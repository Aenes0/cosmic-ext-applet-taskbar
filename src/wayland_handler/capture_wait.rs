// Copyright 2023 System76 <info@system76.com>, 2026 Aenes0
// SPDX-License-Identifier: GPL-3.0-only

//! Waiting for the compositor during a thumbnail capture: a timeout, `stopped`
//! as a failure, and exactly one answer per request. No Wayland in here.

use std::sync::{Condvar, Mutex, MutexGuard};
use std::time::Duration;

/// How long one wait on the compositor may last before the capture is dropped.
pub const TIMEOUT: Duration = Duration::from_secs(2);

/// Error text of a wait that ran out of time.
pub const TIMED_OUT: &str = "timed out";

/// Whether the buffer and memfd of a finished wait may be freed. Not after a
/// timeout: the frame is still pending, and a compositor that finds its
/// buffer gone may crash. Leaking them (bounded) is the safe choice.
pub fn should_release(res: &Result<(), String>) -> bool {
    !matches!(res, Err(why) if why == TIMED_OUT)
}

pub struct Inner<F> {
    pub formats: Option<F>,
    pub res: Option<Result<(), String>>,
    pub stopped: bool,
}

impl<F> Default for Inner<F> {
    fn default() -> Self {
        Self {
            formats: None,
            res: None,
            stopped: false,
        }
    }
}

/// State shared between the compositor callbacks and the capture thread.
pub struct Shared<F> {
    condvar: Condvar,
    inner: Mutex<Inner<F>>,
}

impl<F> Default for Shared<F> {
    fn default() -> Self {
        Self {
            condvar: Condvar::new(),
            inner: Mutex::new(Inner::default()),
        }
    }
}

impl<F> Shared<F> {
    fn lock(&self) -> MutexGuard<'_, Inner<F>> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Changes the shared state and wakes the waiting thread.
    pub fn update<U: FnOnce(&mut Inner<F>)>(&self, f: U) {
        f(&mut self.lock());
        self.condvar.notify_all();
    }

    /// Waits for the buffer formats; fails when stopped or after `timeout`.
    pub fn wait_formats(&self, timeout: Duration) -> Result<F, String> {
        self.wait(timeout, |i| i.formats.is_some(), |i| i.formats.take())
    }

    /// Waits for the copy result; fails when stopped or after `timeout`.
    pub fn wait_result(&self, timeout: Duration) -> Result<(), String> {
        self.wait(timeout, |i| i.res.is_some(), |i| i.res.take())
            .and_then(|res| res)
    }

    fn wait<T>(
        &self,
        timeout: Duration,
        ready: impl Fn(&Inner<F>) -> bool,
        take: impl FnOnce(&mut Inner<F>) -> Option<T>,
    ) -> Result<T, String> {
        let (mut inner, _) = self
            .condvar
            .wait_timeout_while(self.lock(), timeout, |i| !i.stopped && !ready(i))
            .unwrap_or_else(|e| e.into_inner());
        let stopped = inner.stopped;
        take(&mut inner).ok_or_else(|| {
            if stopped {
                "capture session stopped".to_string()
            } else {
                TIMED_OUT.to_string()
            }
        })
    }
}

/// Holds `done` until used; reports `None` if dropped while still holding it.
pub struct DoneGuard<R>(Option<Box<dyn FnOnce(Option<R>) + Send>>);

impl<R> DoneGuard<R> {
    pub fn new(done: impl FnOnce(Option<R>) + Send + 'static) -> Self {
        Self(Some(Box::new(done)))
    }

    pub fn finish(&mut self, res: Option<R>) {
        if let Some(done) = self.0.take() {
            done(res);
        }
    }
}

impl<R> Drop for DoneGuard<R> {
    fn drop(&mut self) {
        self.finish(None);
    }
}

/// Runs `job` through `spawn` and gives its result to `done` exactly once,
/// also when the task never runs (failed spawn).
pub fn run_once<R, J, F, S>(job: J, done: F, spawn: S)
where
    R: Send + 'static,
    J: FnOnce() -> Option<R> + Send + 'static,
    F: FnOnce(Option<R>) + Send + 'static,
    S: FnOnce(Box<dyn FnOnce() + Send>),
{
    let mut guard = DoneGuard::new(done);
    spawn(Box::new(move || {
        let res = job();
        guard.finish(res);
    }));
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Instant;

    const SHORT: Duration = Duration::from_millis(50);

    #[test]
    fn formats_already_there_are_taken() {
        let s = Shared::<u32>::default();
        s.update(|i| i.formats = Some(7));
        assert_eq!(s.wait_formats(SHORT), Ok(7));
    }

    #[test]
    fn formats_arriving_later_wake_the_waiter() {
        let s = Arc::new(Shared::<u32>::default());
        let s2 = s.clone();
        let t = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(20));
            s2.update(|i| i.formats = Some(9));
        });
        assert_eq!(s.wait_formats(Duration::from_secs(2)), Ok(9));
        let _ = t.join();
    }

    #[test]
    fn silence_times_out_instead_of_hanging() {
        let s = Shared::<u32>::default();
        let t0 = Instant::now();
        assert_eq!(s.wait_result(SHORT), Err(TIMED_OUT.to_string()));
        assert!(t0.elapsed() >= SHORT);
        assert!(t0.elapsed() < Duration::from_secs(1));
    }

    #[test]
    fn stopped_releases_the_wait_as_failure() {
        let s = Shared::<u32>::default();
        s.update(|i| i.stopped = true);
        let t0 = Instant::now();
        assert_eq!(
            s.wait_result(Duration::from_secs(5)),
            Err("capture session stopped".to_string())
        );
        assert!(t0.elapsed() < Duration::from_secs(1));
    }

    #[test]
    fn ready_and_failed_results_pass_through() {
        let s = Shared::<u32>::default();
        s.update(|i| i.res = Some(Ok(())));
        assert_eq!(s.wait_result(SHORT), Ok(()));
        s.update(|i| i.res = Some(Err("boom".to_string())));
        assert_eq!(s.wait_result(SHORT), Err("boom".to_string()));
    }

    #[test]
    fn only_a_timeout_keeps_the_buffers() {
        assert!(should_release(&Ok(())));
        assert!(should_release(&Err("capture failed".to_string())));
        assert!(should_release(&Err("capture session stopped".to_string())));
        assert!(!should_release(&Err(TIMED_OUT.to_string())));
    }

    #[test]
    fn a_new_wait_after_a_timeout_works() {
        let s = Shared::<u32>::default();
        assert!(s.wait_result(SHORT).is_err());
        s.update(|i| i.res = Some(Ok(())));
        assert_eq!(s.wait_result(SHORT), Ok(()));
    }

    fn counter() -> (Arc<AtomicUsize>, impl FnOnce(Option<u8>) + Send + 'static) {
        let n = Arc::new(AtomicUsize::new(0));
        let n2 = n.clone();
        (n, move |_| {
            n2.fetch_add(1, Ordering::SeqCst);
        })
    }

    #[test]
    fn done_runs_once_with_the_result() {
        let (n, done) = counter();
        run_once(|| Some(1u8), done, |task| task());
        assert_eq!(n.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn done_runs_once_when_the_job_fails() {
        let (n, done) = counter();
        run_once(|| None, done, |task| task());
        assert_eq!(n.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn done_runs_once_when_the_task_is_dropped_unrun() {
        let (n, done) = counter();
        run_once(|| Some(1u8), done, drop);
        assert_eq!(n.load(Ordering::SeqCst), 1);
    }
}
