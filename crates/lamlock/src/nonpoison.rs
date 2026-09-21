//! Flat-combining locks that remain usable after a task panics.

use core::{
    cell::{Cell, UnsafeCell},
    mem::MaybeUninit,
    ptr::NonNull,
    sync::atomic::Ordering,
};

use crate::{bomb::UnlockGuard, node::Node, panic, rawlock};

/// A nonpoisoning flat-combining lock.
/// Create a new `Lock` with the [`Lock::new`] method.
/// To get access to the data, you can use the [`Lock::run`] method.
pub struct Lock<T> {
    pub(crate) raw: rawlock::RawLock,
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
    /// Get mutable access to the data through an exclusive borrow.
    pub fn get_mut(&mut self) -> &mut T {
        self.data.get_mut()
    }

    #[inline(never)]
    fn run_slowly<F, R>(&self, f: F) -> Result<R, rawlock::QueueError>
    where
        F: FnOnce(&mut T) -> R + Send,
        R: Send,
    {
        #[repr(C)]
        struct CombinedNode<'a, T, F, R> {
            node: Node,
            closure: MaybeUninit<F>,
            #[cfg(not(feature = "std"))]
            started: Cell<bool>,
            data: &'a UnsafeCell<T>,
            result: Cell<MaybeUninit<Result<R, panic::Payload>>>,
        }
        unsafe fn execute<T, F, R>(this: NonNull<Node>)
        where
            F: FnOnce(&mut T) -> R,
        {
            let this = this.cast::<CombinedNode<T, F, R>>();
            #[cfg(not(feature = "std"))]
            unsafe {
                this.as_ref().started.set(true);
            }
            // SAFETY: The combiner executes each node at most once. Ownership
            // moves out of the slot and into this invocation, including on panic.
            let closure = unsafe { this.as_ref().closure.assume_init_read() };
            // The mutable reference exists only inside the caught invocation.
            let result = panic::catch(core::panic::AssertUnwindSafe(|| {
                let data = unsafe { &mut *this.as_ref().data.get() };
                closure(data)
            }));
            unsafe { this.as_ref().result.set(MaybeUninit::new(result)) };
        }
        let mut combined_node = CombinedNode {
            node: Node::new(execute::<T, F, R>),
            closure: MaybeUninit::new(f),
            #[cfg(not(feature = "std"))]
            started: Cell::new(false),
            data: &self.data,
            result: Cell::new(MaybeUninit::uninit()),
        };
        let this = NonNull::from(&combined_node).cast();
        if let Err(cancelled) = Node::attach(this, &self.raw) {
            #[cfg(not(feature = "std"))]
            if combined_node.started.get() {
                return Err(cancelled);
            }
            // The node is detached; drop only a closure that was not consumed.
            unsafe { combined_node.closure.assume_init_drop() };
            return Err(cancelled);
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
    /// An uncontended call executes directly on the requesting thread.
    /// Otherwise, a combiner may execute the closure on its behalf.
    ///
    /// With `std`, delegated panics are caught and resumed on the requester.
    /// The panic hook and the closure's destructors run on the executing thread.
    /// Subsequent tasks continue to execute, even after a task panics.
    ///
    /// Without `std`, an unwinding combiner cancels and disables its queue.
    /// Canceled and subsequent calls panic because their tasks cannot execute.
    /// A panic on the direct fast path still releases the lock normally.
    ///
    /// ```rust
    /// use lamlock::nonpoison::Lock;
    /// let lock = Lock::new(0);
    /// lock.run(|data| *data += 1);
    /// assert_eq!(lock.run(|data| *data), 1);
    /// ```
    #[inline(always)]
    pub fn run<F, R>(&self, f: F) -> R
    where
        F: FnOnce(&mut T) -> R + Send,
        R: Send,
    {
        self.try_run(f)
            .unwrap_or_else(|_| panic!("combiner unwound without panic transport"))
    }

    #[inline(always)]
    pub(crate) fn try_run<F, R>(&self, f: F) -> Result<R, rawlock::QueueError>
    where
        F: FnOnce(&mut T) -> R + Send,
        R: Send,
    {
        if !self.raw.has_tail(Ordering::Relaxed) && self.raw.try_acquire()? {
            let guard = UnlockGuard::new(&self.raw);
            let result = f(unsafe { &mut *self.data.get() });
            drop(guard);
            return Ok(result);
        }
        self.run_slowly(f)
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    extern crate std;
    use std::{
        panic::{AssertUnwindSafe, catch_unwind},
        thread,
    };

    pub(crate) fn wait_for_new_tail<T>(lock: &Lock<T>, previous: *mut Node) -> *mut Node {
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

    #[test]
    fn fast_path_panic_releases_waiting_queue() {
        let lock = Lock::new(0);
        let (entered, entered_rx) = std::sync::mpsc::channel();
        thread::scope(|scope| {
            let holder = scope.spawn(|| {
                let panic = catch_unwind(AssertUnwindSafe(|| {
                    lock.run(|data| {
                        *data = 1;
                        entered.send(()).unwrap();
                        wait_for_new_tail(&lock, core::ptr::null_mut());
                        panic!("fast path");
                    });
                }))
                .unwrap_err();
                assert_eq!(panic.downcast_ref::<&str>(), Some(&"fast path"));
            });
            entered_rx.recv().unwrap();
            let waiter = scope.spawn(|| {
                lock.run(|data| {
                    *data += 1;
                    *data
                })
            });
            holder.join().unwrap();
            assert_eq!(waiter.join().unwrap(), 2);
        });
        assert_eq!(lock.run(|data| *data), 2);
    }

    #[cfg(feature = "std")]
    #[test]
    fn queued_panic_preserves_results_and_continues() {
        // Cover both a panic in the combiner's own task and a delegated panic.
        for panic_first in [false, true] {
            let lock = Lock::new(0);
            lock.raw.acquire().unwrap();
            thread::scope(|scope| {
                let combiner = scope.spawn(|| {
                    let requester = thread::current().id();
                    let result = catch_unwind(AssertUnwindSafe(|| {
                        lock.run(|data| {
                            assert_eq!(thread::current().id(), requester);
                            *data += 1;
                            if panic_first {
                                panic!("first task");
                            }
                            *data
                        })
                    }));
                    if panic_first {
                        assert_eq!(
                            result.unwrap_err().downcast_ref::<&str>(),
                            Some(&"first task")
                        );
                    } else {
                        assert_eq!(result.unwrap(), 1);
                    }
                    requester
                });
                let first = wait_for_new_tail(&lock, core::ptr::null_mut());
                let second = scope.spawn(|| {
                    let requester = thread::current().id();
                    let result = catch_unwind(AssertUnwindSafe(|| {
                        lock.run(|data| {
                            assert_ne!(thread::current().id(), requester);
                            *data += 10;
                            if !panic_first {
                                panic!("second task");
                            }
                            *data
                        })
                    }));
                    if panic_first {
                        assert_eq!(result.unwrap(), 11);
                    } else {
                        assert_eq!(
                            result.unwrap_err().downcast_ref::<&str>(),
                            Some(&"second task")
                        );
                    }
                });
                let second_node = wait_for_new_tail(&lock, first);
                let third = scope.spawn(|| {
                    lock.run(|data| {
                        *data += 100;
                        (*data, thread::current().id())
                    })
                });
                wait_for_new_tail(&lock, second_node);
                lock.raw.release();
                let combiner_id = combiner.join().unwrap();
                second.join().unwrap();
                assert_eq!(third.join().unwrap(), (111, combiner_id));
            });
            assert!(!lock.raw.has_tail(Ordering::Acquire));
            assert_eq!(lock.run(|data| *data), 111);
        }
    }
}
