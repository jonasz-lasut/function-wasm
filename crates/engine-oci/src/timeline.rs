//! Where one run's time goes, for the hand-run benchmark (tests/bench.rs):
//! the engine and the sandbox mark the moments a run passes, and a caller
//! that started a timeline on its thread takes them back after the run.
//! Without the `bench` feature the marks compile to nothing, so a
//! function's binary carries none of this.
//!
//! A run is driven on the thread that called `Engine::run`, and the marks
//! are kept per thread: runs side by side never mix theirs.

#[cfg(feature = "bench")]
pub use on::{Mark, start, take};

#[cfg(feature = "bench")]
pub(crate) use on::{count, mark};

#[cfg(feature = "bench")]
mod on {
    use std::cell::RefCell;
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

    thread_local! {
        static MARKS: RefCell<Option<Vec<Mark>>> = const { RefCell::new(None) };
    }

    /// Starts keeping this thread's marks, forgetting any kept before.
    pub fn start() {
        MARKS.with(|m| *m.borrow_mut() = Some(Vec::new()));
    }

    /// The marks since `start`, in the order they were made, and no more
    /// are kept until the next `start`.
    pub fn take() -> Vec<Mark> {
        MARKS.with(|m| m.borrow_mut().take()).unwrap_or_default()
    }

    pub(crate) fn mark(name: &'static str) {
        count(name, 0);
    }

    pub(crate) fn count(name: &'static str, count: u64) {
        MARKS.with(|m| {
            if let Some(marks) = m.borrow_mut().as_mut() {
                marks.push(Mark {
                    name,
                    at: Instant::now(),
                    count,
                });
            }
        });
    }
}

#[cfg(not(feature = "bench"))]
#[inline(always)]
pub(crate) fn mark(_name: &'static str) {}

#[cfg(not(feature = "bench"))]
#[inline(always)]
pub(crate) fn count(_name: &'static str, _count: u64) {}
