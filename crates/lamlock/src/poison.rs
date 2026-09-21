//! Flat-combining locks that reject ordinary tasks after a task panics.

use core::sync::atomic::{AtomicBool, Ordering};

use crate::{LockPoisoned, LockResult, nonpoison};

/// A poisonable flat-combining lock.
/// Create a new `Lock` with the [`Lock::new`] method.
/// To get access to the data, you can use the [`Lock::run`] method.
pub struct Lock<T> {
    inner: nonpoison::Lock<T>,
    poisoned: AtomicBool,
}

impl<T> Lock<T> {
    /// Create a new lock with the given data.
    pub const fn new(data: T) -> Self {
        Self {
            inner: nonpoison::Lock::new(data),
            poisoned: AtomicBool::new(false),
        }
    }

    /// Get mutable access to the data, even if the lock is poisoned.
    ///
    /// The exclusive borrow guarantees that no other thread can access the lock.
    /// This does not clear poison or repair data left inconsistent by a panic.
    pub fn get_mut(&mut self) -> &mut T {
        self.inner.get_mut()
    }

    /// Wait until the lock is available, then poison it.
    /// Return error if the lock is already poisoned.
    pub fn poison(&self) -> LockResult<()> {
        let poisoned = &self.poisoned;
        self.inner
            .try_run(|_| {
                if poisoned.load(Ordering::Relaxed) {
                    return Err(LockPoisoned);
                }
                poisoned.store(true, Ordering::Relaxed);
                Ok(())
            })
            .map_err(|_| LockPoisoned)?
    }

    /// Schedule a recovery closure, invoking it only if the lock is poisoned.
    /// Returns `true` if the closure returns `true` and poison is cleared.
    /// Returns `false` if the lock was not poisoned or the closure returns `false`.
    /// A panicking closure leaves the lock poisoned.
    ///
    /// Without `std`, returns [`LockPoisoned`] if an uncaught combiner panic
    /// disabled the underlying queue; recovery cannot execute in that case.
    pub fn try_unpoison<F>(&self, f: F) -> LockResult<bool>
    where
        F: FnOnce(&mut T) -> bool + Send,
    {
        let poisoned = &self.poisoned;
        self.inner
            .try_run(move |data| {
                if !poisoned.load(Ordering::Relaxed) || !f(data) {
                    return false;
                }
                poisoned.store(false, Ordering::Relaxed);
                true
            })
            .map_err(|_| LockPoisoned)
    }

    /// Schedules a closure to run on the lock's data.
    /// If the lock is poisoned when the task executes, returns [`LockPoisoned`]
    /// without invoking the closure. Its captures are dropped on the executing thread.
    ///
    /// With `std`, delegated panics are caught and resumed on the requester.
    /// The panic hook and the closure's destructors run on the executing thread.
    /// Completed tasks retain their results; later tasks return [`LockPoisoned`].
    ///
    /// Without `std`, a panic unwinds the executing thread. The heavy bomb
    /// cancels the remaining queue, whose requesters receive [`LockPoisoned`].
    ///
    /// ```rust
    /// use lamlock::Lock;
    /// let lock = Lock::new(0);
    /// lock.run(|data| {
    ///     *data += 1;
    /// }).unwrap();
    /// ```
    #[inline(always)]
    pub fn run<F, R>(&self, f: F) -> LockResult<R>
    where
        F: FnOnce(&mut T) -> R + Send,
        R: Send,
    {
        let poisoned = &self.poisoned;
        self.inner
            .try_run(move |data| {
                if poisoned.load(Ordering::Relaxed) {
                    return Err(LockPoisoned);
                }
                // Leave the flag set if the closure unwinds.
                poisoned.store(true, Ordering::Relaxed);
                let result = f(data);
                poisoned.store(false, Ordering::Relaxed);
                Ok(result)
            })
            .map_err(|_| LockPoisoned)?
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nonpoison::tests::wait_for_new_tail;

    extern crate std;

    #[test]
    fn try_unpoison_checks_state_and_recovery_result() {
        let lock = Lock::new(1);
        assert!(!lock.try_unpoison(|_| panic!("healthy lock")).unwrap());
        lock.poison().unwrap();
        assert!(
            !lock
                .try_unpoison(|data| {
                    *data = 2;
                    false
                })
                .unwrap()
        );
        assert!(lock.run(|_| ()).is_err());
        assert!(
            lock.try_unpoison(|data| {
                assert_eq!(*data, 2);
                *data = 3;
                true
            })
            .unwrap()
        );
        assert_eq!(lock.run(|data| *data).unwrap(), 3);
        assert!(!lock.try_unpoison(|_| panic!("already recovered")).unwrap());
    }

    #[test]
    fn try_unpoison_panic_preserves_poison() {
        let lock = Lock::new(0);
        lock.poison().unwrap();
        let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            lock.try_unpoison(|data| {
                *data = 1;
                panic!("recovery failed");
            })
        }))
        .unwrap_err();
        assert_eq!(panic.downcast_ref::<&str>(), Some(&"recovery failed"));
        assert!(lock.run(|_| ()).is_err());
        assert!(
            lock.try_unpoison(|data| {
                assert_eq!(*data, 1);
                true
            })
            .unwrap()
        );
        assert_eq!(lock.run(|data| *data).unwrap(), 1);
    }

    #[cfg(feature = "std")]
    #[test]
    fn queued_recovery_allows_following_tasks() {
        let lock = Lock::new(0);
        lock.inner.raw.acquire().unwrap();
        std::thread::scope(|scope| {
            let combiner = scope.spawn(|| {
                let requester = std::thread::current().id();
                let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    lock.run(|data| {
                        *data = 1;
                        panic!("poison before recovery");
                    })
                }))
                .unwrap_err();
                assert_eq!(
                    panic.downcast_ref::<&str>(),
                    Some(&"poison before recovery")
                );
                requester
            });
            let first = wait_for_new_tail(&lock.inner, core::ptr::null_mut());
            let rejected = scope.spawn(|| {
                assert!(lock.run(|_| panic!("poisoned task")).is_err());
            });
            let second = wait_for_new_tail(&lock.inner, first);
            let recovery = scope.spawn(|| {
                let mut executor = None;
                assert!(
                    lock.try_unpoison(|data| {
                        executor = Some(std::thread::current().id());
                        assert_eq!(*data, 1);
                        *data = 10;
                        true
                    })
                    .unwrap()
                );
                executor.unwrap()
            });
            let third = wait_for_new_tail(&lock.inner, second);
            let following = scope.spawn(|| {
                lock.run(|data| {
                    *data += 1;
                    (*data, std::thread::current().id())
                })
                .unwrap()
            });
            wait_for_new_tail(&lock.inner, third);
            lock.inner.raw.release();
            let combiner_id = combiner.join().unwrap();
            rejected.join().unwrap();
            assert_eq!(recovery.join().unwrap(), combiner_id);
            assert_eq!(following.join().unwrap(), (11, combiner_id));
        });
        assert_eq!(lock.run(|data| *data).unwrap(), 11);
    }

    #[cfg(feature = "std")]
    #[test]
    fn queued_panic_returns_to_requester() {
        use std::{
            panic::{AssertUnwindSafe, catch_unwind, panic_any},
            sync::mpsc,
            thread,
        };

        struct DroppedOn {
            name: &'static str,
            events: mpsc::Sender<(&'static str, thread::ThreadId)>,
        }
        impl Drop for DroppedOn {
            fn drop(&mut self) {
                self.events
                    .send((self.name, thread::current().id()))
                    .unwrap();
            }
        }

        let mut lock = Lock::new(0usize);
        let (events, received) = mpsc::channel();
        // Hold the raw lock until A, B, and C have linked their stack nodes in
        // that order. A must then combine B's panic and cancel C's task.
        lock.inner.raw.acquire().unwrap();
        let (combiner_id, panicking_id, _cancelled_id) = thread::scope(|scope| {
            let lock = &lock;
            let result_drop = DroppedOn {
                name: "result",
                events: events.clone(),
            };
            let combiner = scope.spawn(move || {
                let requester = thread::current().id();
                let result = lock
                    .run(move |data| {
                        assert_eq!(thread::current().id(), requester);
                        *data += 1;
                        result_drop
                    })
                    .unwrap();
                // B's panic must not unwind A or discard A's successful result.
                assert!(lock.run(|_| ()).is_err());
                drop(result);
                requester
            });
            let first = wait_for_new_tail(&lock.inner, core::ptr::null_mut());

            let payload = DroppedOn {
                name: "payload",
                events: events.clone(),
            };
            let task_local = DroppedOn {
                name: "task",
                events: events.clone(),
            };
            let panicking = scope.spawn(move || {
                let requester = thread::current().id();
                let panic = catch_unwind(AssertUnwindSafe(|| {
                    let _ = lock.run(move |data| {
                        assert_ne!(thread::current().id(), requester);
                        let _local = task_local;
                        *data += 10;
                        panic_any(payload);
                    });
                }))
                .unwrap_err();
                // Preserve the original concrete payload, not a new poison panic.
                let payload = panic.downcast::<DroppedOn>().unwrap();
                assert_eq!(payload.name, "payload");
                drop(payload);
                requester
            });
            let second = wait_for_new_tail(&lock.inner, first);

            let cancelled_capture = DroppedOn {
                name: "cancelled",
                events: events.clone(),
            };
            let cancelled = scope.spawn(move || {
                let requester = thread::current().id();
                assert!(
                    lock.run(move |data| {
                        drop(cancelled_capture);
                        *data += 100;
                    })
                    .is_err()
                );
                requester
            });
            wait_for_new_tail(&lock.inner, second);
            lock.inner.raw.release();

            (
                combiner.join().unwrap(),
                panicking.join().unwrap(),
                cancelled.join().unwrap(),
            )
        });

        assert_eq!(*lock.get_mut(), 11);
        drop(events);
        let mut events: std::vec::Vec<_> = received.into_iter().collect();
        events.sort_by_key(|(name, _)| *name);
        assert_eq!(
            events,
            std::vec![
                ("cancelled", combiner_id),
                ("payload", panicking_id),
                ("result", combiner_id),
                ("task", combiner_id),
            ]
        );
    }

    #[test]
    fn combiner_own_panic_cancels_waiter() {
        let mut lock = Lock::new(0usize);
        lock.inner.raw.acquire().unwrap();
        std::thread::scope(|scope| {
            let lock = &lock;
            let combiner = scope.spawn(|| {
                let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    let _ = lock.run(|data| {
                        *data = 1;
                        panic!("combiner's own task");
                    });
                }))
                .unwrap_err();
                assert_eq!(panic.downcast_ref::<&str>(), Some(&"combiner's own task"));
            });
            let first = wait_for_new_tail(&lock.inner, core::ptr::null_mut());
            let waiter = scope.spawn(|| {
                assert!(lock.run(|data| *data = 2).is_err());
            });
            wait_for_new_tail(&lock.inner, first);
            lock.inner.raw.release();
            combiner.join().unwrap();
            waiter.join().unwrap();
        });
        assert_eq!(*lock.get_mut(), 1);
        assert!(!lock.inner.raw.has_tail(Ordering::Acquire));
        assert!(lock.poison().is_err());
    }

    #[cfg(not(feature = "std"))]
    #[test]
    fn uncaught_delegated_panic_drops_closures_once() {
        use core::sync::atomic::AtomicUsize;

        struct CountDrop<'a>(&'a AtomicUsize);
        impl Drop for CountDrop<'_> {
            fn drop(&mut self) {
                self.0.fetch_add(1, Ordering::Relaxed);
            }
        }

        let mut lock = Lock::new(0);
        let started_drops = AtomicUsize::new(0);
        let cancelled_drops = AtomicUsize::new(0);
        lock.inner.raw.acquire().unwrap();
        std::thread::scope(|scope| {
            let lock = &lock;
            let combiner = scope.spawn(|| {
                let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    lock.run(|data| *data += 1)
                }))
                .unwrap_err();
                assert_eq!(panic.downcast_ref::<&str>(), Some(&"delegated task"));
            });
            let first = wait_for_new_tail(&lock.inner, core::ptr::null_mut());
            let started_capture = CountDrop(&started_drops);
            let started = scope.spawn(move || {
                assert!(
                    lock.run(move |data| {
                        let _capture = started_capture;
                        *data += 10;
                        panic!("delegated task");
                    })
                    .is_err()
                );
            });
            let second = wait_for_new_tail(&lock.inner, first);
            let cancelled_capture = CountDrop(&cancelled_drops);
            let cancelled = scope.spawn(move || {
                assert!(
                    lock.run(move |data| {
                        drop(cancelled_capture);
                        *data += 100;
                    })
                    .is_err()
                );
            });
            wait_for_new_tail(&lock.inner, second);
            lock.inner.raw.release();
            combiner.join().unwrap();
            started.join().unwrap();
            cancelled.join().unwrap();
        });
        assert_eq!(*lock.get_mut(), 11);
        assert_eq!(started_drops.load(Ordering::Relaxed), 1);
        assert_eq!(cancelled_drops.load(Ordering::Relaxed), 1);
        assert!(!lock.inner.raw.has_tail(Ordering::Acquire));
        assert!(lock.run(|_| ()).is_err());
        assert!(lock.try_unpoison(|_| panic!("disabled queue")).is_err());
    }

    #[test]
    fn slow_path_drops_closure_once() {
        use core::sync::atomic::AtomicUsize;

        struct CountDrop<'a>(&'a AtomicUsize);
        impl Drop for CountDrop<'_> {
            fn drop(&mut self) {
                self.0.fetch_add(1, Ordering::Relaxed);
            }
        }

        // Exercise both a rejected closure and one consumed by a panic.
        for execute in [false, true] {
            let lock = Lock::new(());
            let drops = AtomicUsize::new(0);
            let captured = CountDrop(&drops);
            if !execute {
                lock.poison().unwrap();
            }
            lock.inner.raw.acquire().unwrap();
            let outcome = std::thread::scope(|scope| {
                let requester = scope.spawn(|| {
                    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        lock.run(move |_| {
                            let _captured = captured;
                            panic!("consumed closure");
                        })
                    }))
                });
                while !lock.inner.raw.has_tail(Ordering::Acquire) {
                    std::thread::yield_now();
                }
                lock.inner.raw.release();
                requester.join().unwrap()
            });
            if execute {
                assert!(outcome.is_err());
            } else {
                assert!(outcome.unwrap().is_err());
            }
            assert_eq!(drops.load(Ordering::Relaxed), 1);
        }
    }

    #[test]
    fn smoke_test() {
        let lock = Lock::new(0);
        lock.run(|data| {
            *data += 1;
        })
        .unwrap();
        assert_eq!(lock.run(|x| *x).unwrap(), 1);
    }

    #[test]
    fn multi_thread_test() {
        let cnt = 100;
        let lock = Lock::new(0);
        std::thread::scope(|scope| {
            for i in 0..cnt {
                let lock = &lock;
                scope.spawn(move || {
                    lock.run(|data| {
                        *data += cnt - i;
                    })
                    .unwrap();
                });
            }
        });

        assert_eq!(lock.run(|x| *x).unwrap(), cnt * (cnt + 1) / 2);
    }

    #[test]
    #[should_panic]
    fn mutli_thread_panic_chain_test() {
        let cnt = 100;
        let lock = Lock::new(0);
        std::thread::scope(|scope| {
            for i in 0..cnt {
                let lock = &lock;
                scope.spawn(move || {
                    lock.run(|data| {
                        *data += cnt - i;
                        if i == cnt / 2 {
                            panic!("panic chain");
                        }
                    })
                    .unwrap();
                });
            }
        });
    }

    #[test]
    fn multi_thread_poison_is_permanent() {
        let mut lock = Lock::new(std::string::String::new());
        lock.poison().unwrap();
        std::thread::scope(|scope| {
            for _ in 0..100 {
                let lock = &lock;
                scope.spawn(move || {
                    assert!(lock.run(|x| x.push('A')).is_err());
                    assert!(lock.poison().is_err());
                });
            }
        });
        assert!(lock.get_mut().is_empty());
    }

    #[test]
    fn get_mut_preserves_poison_after_panic() {
        let mut lock = Lock::new(std::string::String::new());
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _ = lock.run(|data| {
                data.push('A');
                panic!("poison the lock");
            });
        }));
        assert!(result.is_err());
        assert_eq!(lock.get_mut(), "A");
        lock.get_mut().clear();
        assert!(lock.run(|data| data.push('B')).is_err());
        assert!(lock.get_mut().is_empty());
    }
}
