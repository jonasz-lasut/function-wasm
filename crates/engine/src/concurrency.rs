//! Run governance - the Rust port of the Go engine's fair.go, mempool.go
//! and concurrency.go: the global run bound served round-robin by module
//! key, the aggregate run-memory pool, and the per-step slot bound. Waits
//! are blocking and bounded by the run's own deadline.

use std::collections::{HashMap, VecDeque};
use std::sync::mpsc::{Receiver, RecvTimeoutError, SyncSender, sync_channel};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

/// The tail of every wait refusal; the leading phrase is the contract
/// operators grep for, the cause is worded naturally.
const DEADLINE_EXCEEDED: &str = "deadline exceeded";

/// A round-robin slot scheduler: requests with different keys are served in
/// turn so one hot module cannot take every slot from every other. With one
/// key in use it degrades to a FIFO.
#[derive(Debug)]
pub(crate) struct FairScheduler {
    total: usize,
    state: Mutex<SchedulerState>,
}

#[derive(Debug)]
struct SchedulerState {
    active: usize,
    queues: HashMap<String, VecDeque<Waiter>>,
    /// Round-robin order of keys with pending waiters.
    order: VecDeque<String>,
}

#[derive(Debug)]
struct Waiter {
    tx: SyncSender<()>,
    id: u64,
}

#[derive(Debug)]
pub(crate) struct SlotGuard<'a> {
    scheduler: &'a FairScheduler,
}

impl Drop for SlotGuard<'_> {
    fn drop(&mut self) {
        let mut s = self.scheduler.state.lock().expect("poisoned");
        s.active -= 1;
        self.scheduler.wake_next(&mut s);
    }
}

impl FairScheduler {
    pub(crate) fn new(slots: usize) -> FairScheduler {
        FairScheduler {
            total: slots,
            state: Mutex::new(SchedulerState {
                active: 0,
                queues: HashMap::new(),
                order: VecDeque::new(),
            }),
        }
    }

    /// Waits for a slot until deadline. A wait the deadline cuts short held
    /// nothing; a grant that races the deadline is handed on to the next
    /// waiter rather than leaked.
    pub(crate) fn acquire(&self, key: &str, deadline: Instant) -> Result<SlotGuard<'_>, String> {
        static IDS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let (tx, rx): (SyncSender<()>, Receiver<()>) = sync_channel(1);
        {
            let mut s = self.state.lock().expect("poisoned");
            if s.active < self.total {
                s.active += 1;
                return Ok(SlotGuard { scheduler: self });
            }
            let id = IDS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            if !s.queues.contains_key(key) {
                s.order.push_back(key.to_string());
            }
            s.queues
                .entry(key.to_string())
                .or_default()
                .push_back(Waiter { tx, id });
            drop(s);
            let timeout = deadline.saturating_duration_since(Instant::now());
            match rx.recv_timeout(timeout) {
                Ok(()) => return Ok(SlotGuard { scheduler: self }),
                Err(RecvTimeoutError::Timeout | RecvTimeoutError::Disconnected) => {
                    let mut s = self.state.lock().expect("poisoned");
                    // The grant may have landed in the same instant the
                    // deadline fired: reconcile under the lock so it is
                    // handed on, never dropped.
                    match rx.try_recv() {
                        Ok(()) => {
                            s.active -= 1;
                            self.wake_next(&mut s);
                        }
                        Err(_) => Self::remove_waiter(&mut s, key, id),
                    }
                }
            }
        }
        Err(format!("waiting for a run slot: {DEADLINE_EXCEEDED}"))
    }

    /// Admits the next waiter in round-robin order; called with the lock
    /// held (a freed slot rotates to the next key, FIFO within a key).
    fn wake_next(&self, s: &mut SchedulerState) {
        while let Some(key) = s.order.pop_front() {
            let Some(q) = s.queues.get_mut(&key) else {
                continue;
            };
            let Some(w) = q.pop_front() else {
                s.queues.remove(&key);
                continue;
            };
            let empty = q.is_empty();
            s.active += 1;
            // The send always succeeds once into the cap-1 channel; a waiter
            // whose deadline fired takes the value back and hands the slot on.
            let _ = w.tx.send(());
            if empty {
                s.queues.remove(&key);
            } else {
                // Rotate: the next slot goes to a different key.
                s.order.push_back(key);
            }
            return;
        }
    }

    fn remove_waiter(s: &mut SchedulerState, key: &str, id: u64) {
        if let Some(q) = s.queues.get_mut(key) {
            q.retain(|w| w.id != id);
            if q.is_empty() {
                s.queues.remove(key);
                s.order.retain(|k| k != key);
            }
        }
    }
}

/// A counting semaphore over bytes: a run reserves its module's initial
/// linear memory before it starts and every growth beyond it as the guest
/// grows, releasing the whole reservation when its store drops; a
/// reservation that cannot fit waits under the run's deadline. Callers pair
/// reserve and release themselves (the engine's RunLimiter owns that).
#[derive(Debug)]
pub(crate) struct MemPool {
    total: u64,
    used: Mutex<u64>,
    freed: Condvar,
}

impl MemPool {
    pub(crate) fn new(total: u64) -> MemPool {
        MemPool {
            total,
            used: Mutex::new(0),
            freed: Condvar::new(),
        }
    }

    pub(crate) fn reserve(&self, n: u64, deadline: Instant) -> Result<(), String> {
        let mut used = self.used.lock().expect("poisoned");
        loop {
            if *used + n <= self.total {
                *used += n;
                return Ok(());
            }
            let timeout = deadline.saturating_duration_since(Instant::now());
            if timeout.is_zero() {
                return Err(format!(
                    "waiting for {} of run memory (--max-total-run-memory {}): {DEADLINE_EXCEEDED}",
                    format_bytes(n),
                    format_bytes(self.total)
                ));
            }
            let (guard, result) = self.freed.wait_timeout(used, timeout).expect("poisoned");
            used = guard;
            if result.timed_out() && *used + n > self.total {
                return Err(format!(
                    "waiting for {} of run memory (--max-total-run-memory {}): {DEADLINE_EXCEEDED}",
                    format_bytes(n),
                    format_bytes(self.total)
                ));
            }
        }
    }

    pub(crate) fn release(&self, n: u64) {
        let mut used = self.used.lock().expect("poisoned");
        *used -= n;
        // One release can admit several runs: every waiter re-checks.
        self.freed.notify_all();
    }
}

/// Bounds how many runs of a given key execute at once - the per-step
/// concurrency limit, keyed by module digest so two Compositions naming the
/// same module share the bound. The slot count is fixed at a key's first
/// acquire and governs the key until its entry goes idle: replacing the
/// channel while runs hold it would let both bounds run at once.
pub struct StepSlots {
    entries: Mutex<HashMap<String, Arc<StepEntry>>>,
}

#[derive(Debug)]
struct StepEntry {
    capacity: usize,
    held: Mutex<usize>,
    freed: Condvar,
    last_seen: Mutex<Instant>,
}

#[derive(Debug)]
pub struct StepGuard {
    entry: Arc<StepEntry>,
}

impl Drop for StepGuard {
    fn drop(&mut self) {
        let mut held = self.entry.held.lock().expect("poisoned");
        *held -= 1;
        self.entry.freed.notify_one();
    }
}

impl Default for StepSlots {
    fn default() -> Self {
        Self::new()
    }
}

impl StepSlots {
    pub fn new() -> StepSlots {
        StepSlots {
            entries: Mutex::new(HashMap::new()),
        }
    }

    /// Waits for one of the key's n slots until deadline. n sizes the entry
    /// only on the key's first acquire; a later different n reuses it
    /// (first-seen wins, until the entry is swept idle).
    pub fn acquire(&self, key: &str, n: usize, deadline: Instant) -> Result<StepGuard, String> {
        let entry = {
            let mut entries = self.entries.lock().expect("poisoned");
            let e = entries.entry(key.to_string()).or_insert_with(|| {
                Arc::new(StepEntry {
                    capacity: n,
                    held: Mutex::new(0),
                    freed: Condvar::new(),
                    last_seen: Mutex::new(Instant::now()),
                })
            });
            *e.last_seen.lock().expect("poisoned") = Instant::now();
            Arc::clone(e)
        };
        let mut held = entry.held.lock().expect("poisoned");
        loop {
            if *held < entry.capacity {
                *held += 1;
                drop(held);
                return Ok(StepGuard { entry });
            }
            let timeout = deadline.saturating_duration_since(Instant::now());
            if timeout.is_zero() {
                return Err(format!(
                    "waiting for one of this step's {n} run slots (limits.concurrency): {DEADLINE_EXCEEDED}"
                ));
            }
            let (guard, result) = entry.freed.wait_timeout(held, timeout).expect("poisoned");
            held = guard;
            if result.timed_out() && *held >= entry.capacity {
                return Err(format!(
                    "waiting for one of this step's {n} run slots (limits.concurrency): {DEADLINE_EXCEEDED}"
                ));
            }
        }
    }

    /// Removes entries not seen for ten minutes; a held entry is never
    /// truly idle and is kept.
    pub fn sweep_idle(&self) {
        let cutoff = Instant::now() - Duration::from_secs(10 * 60);
        let mut entries = self.entries.lock().expect("poisoned");
        entries.retain(|_, e| {
            *e.held.lock().expect("poisoned") > 0
                || *e.last_seen.lock().expect("poisoned") >= cutoff
        });
    }
}

/// Bytes with binary-SI suffixes, as the Go engine's formatBytes renders
/// them in wait refusals.
pub(crate) fn format_bytes(b: u64) -> String {
    if b >= 1 << 30 && b.is_multiple_of(1 << 30) {
        return format!("{}Gi", b >> 30);
    }
    if b >= 1 << 20 && b.is_multiple_of(1 << 20) {
        return format!("{}Mi", b >> 20);
    }
    if b >= 1 << 10 && b.is_multiple_of(1 << 10) {
        return format!("{}Ki", b >> 10);
    }
    b.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn soon() -> Instant {
        Instant::now() + Duration::from_millis(100)
    }

    #[test]
    fn the_scheduler_round_robins_keys() {
        let s = Arc::new(FairScheduler::new(1));
        let held = s.acquire("hot", soon()).expect("first slot");

        // Queue waiters: two for the hot key, one for the cold - the cold
        // key must be served before the hot key's second waiter.
        let order = Arc::new(Mutex::new(Vec::new()));
        let mut handles = Vec::new();
        for (key, delay) in [("hot", 0u64), ("hot", 10), ("cold", 20)] {
            let s = Arc::clone(&s);
            let order = Arc::clone(&order);
            handles.push(std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(delay));
                let g = s
                    .acquire(key, Instant::now() + Duration::from_secs(5))
                    .expect("slot");
                order.lock().expect("poisoned").push(key);
                std::thread::sleep(Duration::from_millis(10));
                drop(g);
            }));
        }
        std::thread::sleep(Duration::from_millis(50));
        drop(held);
        for h in handles {
            h.join().expect("join");
        }
        assert_eq!(*order.lock().expect("poisoned"), vec!["hot", "cold", "hot"]);
    }

    #[test]
    fn a_slot_wait_times_out_with_the_go_message() {
        let s = FairScheduler::new(1);
        let _held = s.acquire("a", soon()).expect("slot");
        let err = s
            .acquire("b", Instant::now() + Duration::from_millis(30))
            .expect_err("timeout");
        assert_eq!(err, "waiting for a run slot: deadline exceeded");
    }

    #[test]
    fn the_pool_admits_when_memory_frees() {
        let p = Arc::new(MemPool::new(100));
        p.reserve(80, soon()).expect("fits");
        let err = p
            .reserve(40, Instant::now() + Duration::from_millis(30))
            .expect_err("full");
        assert_eq!(
            err,
            "waiting for 40 of run memory (--max-total-run-memory 100): deadline exceeded"
        );
        let p2 = Arc::clone(&p);
        let waiter = std::thread::spawn(move || {
            p2.reserve(40, Instant::now() + Duration::from_secs(5))
                .is_ok()
        });
        std::thread::sleep(Duration::from_millis(30));
        p.release(80);
        assert!(waiter.join().expect("join"));
    }

    #[test]
    fn step_slots_pin_their_first_capacity() {
        let s = StepSlots::new();
        let deadline = Instant::now() + Duration::from_millis(30);
        let _one = s.acquire("sha256:m", 1, deadline).expect("first");
        // A later acquire with a larger n still runs under the first bound.
        let err = s.acquire("sha256:m", 5, deadline).expect_err("pinned");
        assert_eq!(
            err,
            "waiting for one of this step's 5 run slots (limits.concurrency): deadline exceeded"
        );
    }
}
