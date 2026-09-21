#![no_std]
#![cfg_attr(all(feature = "nightly", not(miri)), allow(internal_features))]
#![cfg_attr(all(feature = "nightly", not(miri)), feature(core_intrinsics))]
#![doc = include_str!("../README.md")]
#[cfg(feature = "std")]
extern crate std;

use core::{
    cell::{Cell, UnsafeCell},
    mem::MaybeUninit,
    ptr::NonNull,
    sync::atomic::Ordering,
};

use crate::node::Node;
mod bomb;
mod futex;
mod node;
mod panic;
mod rawlock;

/// Error type for when a lock is poisoned.
#[derive(Debug, Clone, Copy, Default)]
pub struct LockPoisoned;

pub type LockResult<T> = Result<T, LockPoisoned>;

impl core::fmt::Display for LockPoisoned {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "Lock is poisoned")
    }
}

impl core::error::Error for LockPoisoned {}

/// The `Lock` struct is a thread-safe, poisonable lock that allows for safe concurrent access to data.
/// Create a new `Lock` with the [`Lock::new`] method.
/// To get access to the data, you can use the [`Lock::run`] method.
pub struct Lock<T> {
    raw: rawlock::RawLock,
    data: UnsafeCell<T>,
}

unsafe impl<T: Send> Sync for Lock<T> {}

impl<T> Lock<T> {
    /// Create a new lock with the given data.
    pub const fn new(data: T) -> Self {
        Self {
            raw: rawlock::RawLock::new(),
            data: UnsafeCell::new(data),
        }
    }
    /// Get mutable access to the data, even if the lock is poisoned.
    ///
    /// The exclusive borrow guarantees that no other thread can access the lock.
    /// This does not clear poison or repair data left inconsistent by a panic.
    pub fn get_mut(&mut self) -> &mut T {
        self.data.get_mut()
    }

    /// Wait until the lock is available, then poison it.
    /// Poisoning is permanent: subsequent calls to [`Lock::run`] return [`LockPoisoned`].
    /// Return error if the lock is already poisoned.
    pub fn poison(&self) -> Result<(), LockPoisoned> {
        self.raw.acquire()?;
        self.raw.poison();
        Ok(())
    }

    #[inline(never)]
    fn run_slowly<F, R>(&self, f: F) -> LockResult<R>
    where
        F: FnOnce(&mut T) -> R + Send,
        R: Send,
    {
        #[repr(C)]
        struct CombinedNode<'a, T, F, R> {
            node: Node,
            closure: MaybeUninit<F>,
            data: &'a UnsafeCell<T>,
            result: Cell<MaybeUninit<Result<R, panic::Payload>>>,
        }
        unsafe fn execute<T, F, R>(this: NonNull<Node>) -> bool
        where
            F: FnOnce(&mut T) -> R,
        {
            let this = this.cast::<CombinedNode<T, F, R>>();
            // SAFETY: The combiner executes each node at most once. Ownership
            // moves out of the slot and into this invocation, including on panic.
            let closure = unsafe { this.as_ref().closure.assume_init_read() };
            // A failed task leaves the lock poisoned and no further tasks run.
            // The mutable reference exists only inside the caught invocation.
            let result = panic::catch(core::panic::AssertUnwindSafe(|| {
                let data = unsafe { &mut *this.as_ref().data.get() };
                closure(data)
            }));
            let success = result.is_ok();
            unsafe { this.as_ref().result.set(MaybeUninit::new(result)) };
            success
        }
        let mut combined_node = CombinedNode {
            node: Node::new(execute::<T, F, R>),
            closure: MaybeUninit::new(f),
            data: &self.data,
            result: Cell::new(MaybeUninit::uninit()),
        };
        let this = NonNull::from(&combined_node).cast();
        if let Err(poisoned) = Node::attach(this, &self.raw) {
            // SAFETY: An error means this task never ran and the queue no longer
            // accesses its node. The closure slot still owns F. This branch is
            // not reached when attach unwinds after consuming F without std.
            unsafe { combined_node.closure.assume_init_drop() };
            return Err(poisoned);
        }
        // SAFETY: Successful attachment means the task stored an outcome, even
        // if that outcome is a caught panic. The completion wake synchronizes
        // the write, and no other thread accesses this node after attach returns.
        match unsafe { combined_node.result.into_inner().assume_init() } {
            Ok(result) => Ok(result),
            Err(payload) => panic::resume(payload),
        }
    }

    /// Schedules a closure to run on the lock's data.
    /// The locking strategy splits into two paths:
    /// 1. If the lock is not poisoned and can be acquired immediately, it runs the closure directly.
    ///    On the fast path, the closure is not spilled into the node.
    /// 2. If the lock cannot be acquired immediately, it schedules the closure to run later.
    ///
    /// If the lock is poisoned, returns [`LockPoisoned`] without running the closure.
    ///
    /// With the default `std` feature, an unwinding panic in the closure is resumed
    /// on the thread calling `run`, even when another thread executes the closure.
    /// The panic hook and the closure's destructors run on the executing thread.
    /// Completed tasks retain their results; tasks not yet executed return
    /// [`LockPoisoned`]. Poisoning remains permanent.
    ///
    /// Without `std`, a panic unwinds the executing thread, which can be another
    /// task's requester. With an aborting panic runtime, panics abort as usual.
    /// ```rust
    /// use lamlock::Lock;
    /// let lock = Lock::new(0);
    /// lock.run(|data| {
    ///   *data += 1;
    /// }).unwrap();
    /// ```
    #[inline(always)]
    pub fn run<F, R>(&self, f: F) -> LockResult<R>
    where
        F: FnOnce(&mut T) -> R + Send,
        R: Send,
    {
        if !self.raw.has_tail(Ordering::Relaxed) && self.raw.try_acquire()? {
            let bomb = bomb::LightWeightBomb::new(&self.raw);
            let result = f(unsafe { &mut *self.data.get() });
            self.raw.release();
            bomb.diffuse();
            return Ok(result);
        }
        self.run_slowly(f)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    extern crate std;

    fn wait_for_new_tail(lock: &Lock<usize>, previous: *mut Node) -> *mut Node {
        let start = std::time::Instant::now();
        loop {
            let tail = lock.raw.tail_for_test();
            if tail != previous && !tail.is_null() {
                // The tail swap precedes linking the predecessor. Wait for both
                // so the combiner cannot hand off at an unfinished link.
                // SAFETY: These tests hold the raw lock throughout queue setup;
                // no queued node can complete or be reclaimed until release.
                if previous.is_null()
                    || unsafe { (*previous).load_next(Ordering::Acquire) } == NonNull::new(tail)
                {
                    return tail;
                }
            }
            assert!(start.elapsed() < std::time::Duration::from_secs(30));
            std::thread::yield_now();
        }
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
        lock.raw.acquire().unwrap();
        let (combiner_id, panicking_id, cancelled_id) = thread::scope(|scope| {
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
            let first = wait_for_new_tail(lock, core::ptr::null_mut());

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
            let second = wait_for_new_tail(lock, first);

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
            wait_for_new_tail(lock, second);
            lock.raw.release();

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
                ("cancelled", cancelled_id),
                ("payload", panicking_id),
                ("result", combiner_id),
                ("task", combiner_id),
            ]
        );
    }

    #[test]
    fn combiner_own_panic_cancels_waiter() {
        let mut lock = Lock::new(0usize);
        lock.raw.acquire().unwrap();
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
            let first = wait_for_new_tail(lock, core::ptr::null_mut());
            let waiter = scope.spawn(|| {
                assert!(lock.run(|data| *data = 2).is_err());
            });
            wait_for_new_tail(lock, first);
            lock.raw.release();
            combiner.join().unwrap();
            waiter.join().unwrap();
        });
        assert_eq!(*lock.get_mut(), 1);
        assert!(!lock.raw.has_tail(Ordering::Acquire));
        assert!(lock.poison().is_err());
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

        // Exercise both an unexecuted closure and one consumed by a panic,
        // including when no_std lets the panic unwind out of attach directly.
        for execute in [false, true] {
            let lock = Lock::new(());
            let drops = AtomicUsize::new(0);
            let captured = CountDrop(&drops);
            if !execute {
                lock.poison().unwrap();
            }
            let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                lock.run_slowly(move |_| {
                    let _captured = captured;
                    panic!("consumed closure");
                })
            }));
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
