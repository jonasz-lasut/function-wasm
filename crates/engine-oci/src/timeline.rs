//! Where one run's time goes, for the hand-run benchmark (tests/bench.rs):
//! the engine and the sandbox mark the moments a run passes, and a caller
//! that started a timeline on its thread takes them back after the run.
//! Without the `bench` feature the marks compile to nothing, so a
//! function's binary carries none of this.
//!
//! A run is driven on the thread that called `Engine::run`, and the marks
//! are kept per thread: runs side by side never mix theirs. What an
//! answered run leaves to another thread (`run.rs`) marks the same
//! timeline: the engine hands that thread the caller's with `handle` and
//! `adopt`.

#[cfg(feature = "bench")]
pub use on::{Mark, start, take};

#[cfg(feature = "bench")]
pub(crate) use on::{Handle, adopt, count, handle, mark};

#[cfg(feature = "bench")]
mod on {
    use std::cell::RefCell;
    use std::sync::{Arc, Mutex};
    use std::time::Instant;

    /// One moment of a run, or one count taken at it.
    #[derive(Clone, Copy, Debug)]
    pub struct Mark {
        pub name: &'static str,
        pub at: Instant,
        /// How many times a wait looked before it found what it waited
        /// for; zero for a plain moment.
        pub count: u64,
    }

    /// A thread's timeline, for another thread to mark.
    pub(crate) type Handle = Option<Arc<Mutex<Vec<Mark>>>>;

    thread_local! {
        static MARKS: RefCell<Handle> = const { RefCell::new(None) };
    }

    /// Starts keeping this thread's marks, forgetting any kept before.
    pub fn start() {
        MARKS.with(|m| *m.borrow_mut() = Some(Arc::default()));
    }

    /// The marks since `start`, in the order they were made, and no more
    /// are kept until the next `start`.
    pub fn take() -> Vec<Mark> {
        MARKS
            .with(|m| m.borrow_mut().take())
            .map(|marks| std::mem::take(&mut *marks.lock().expect("poisoned")))
            .unwrap_or_default()
    }

    /// This thread's timeline, if it keeps one.
    pub(crate) fn handle() -> Handle {
        MARKS.with(|m| m.borrow().clone())
    }

    /// Marks `timeline` from this thread on.
    pub(crate) fn adopt(timeline: Handle) {
        MARKS.with(|m| *m.borrow_mut() = timeline);
    }

    pub(crate) fn mark(name: &'static str) {
        count(name, 0);
    }

    pub(crate) fn count(name: &'static str, count: u64) {
        MARKS.with(|m| {
            if let Some(marks) = m.borrow().as_ref() {
                marks.lock().expect("poisoned").push(Mark {
                    name,
                    at: Instant::now(),
                    count,
                });
            }
        });
    }
}

#[cfg(not(feature = "bench"))]
mod off {
    /// Nothing: no timeline is kept without the `bench` feature.
    #[derive(Clone, Copy)]
    pub(crate) struct Handle;

    #[inline(always)]
    pub(crate) fn mark(_name: &'static str) {}

    #[inline(always)]
    pub(crate) fn count(_name: &'static str, _count: u64) {}

    #[inline(always)]
    pub(crate) fn handle() -> Handle {
        Handle
    }

    #[inline(always)]
    pub(crate) fn adopt(_timeline: Handle) {}
}

#[cfg(not(feature = "bench"))]
pub(crate) use off::{Handle, adopt, count, handle, mark};
