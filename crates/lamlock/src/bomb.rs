#[cfg(not(feature = "std"))]
use crate::node::Node;
#[cfg(not(feature = "std"))]
use core::{ptr::NonNull, sync::atomic::Ordering};

use crate::rawlock::RawLock;

pub struct UnlockGuard<'a> {
    raw: &'a RawLock,
}

impl<'a> UnlockGuard<'a> {
    pub fn new(raw: &'a RawLock) -> Self {
        Self { raw }
    }
}

impl Drop for UnlockGuard<'_> {
    fn drop(&mut self) {
        self.raw.release();
    }
}

#[cfg(not(feature = "std"))]
pub struct Bomb<'a> {
    raw: &'a RawLock,
    atom: NonNull<Node>,
}

#[cfg(not(feature = "std"))]
impl<'a> Drop for Bomb<'a> {
    #[cold]
    fn drop(&mut self) {
        self.raw.poison();
        loop {
            let next = unsafe { self.atom.as_ref().load_next(Ordering::Acquire) };
            // If the next node is not null, we wake it up and continue to the next iteration.
            if let Some(next) = next {
                Node::wake_as_poisoned(self.atom);
                self.atom = next;
                continue;
            }
            // If we successfully closed the tail, we can stop after waking the last node.
            if self.raw.try_close(self.atom) {
                Node::wake_as_poisoned(self.atom);
                break;
            }
            // Otherwise, we know that the next will be updated since there are nodes waiting.
            // Unlike the combining path in the normal case, we continue to wake up further nodes.
            // This should end soon as the lock is poisoned. New nodes will not attach to the tail.
            while unsafe { self.atom.as_ref().load_next(Ordering::Relaxed).is_none() } {
                core::hint::spin_loop();
            }
        }
    }
}

#[cfg(not(feature = "std"))]
impl<'a> Bomb<'a> {
    pub fn new(lock: &'a RawLock, atom: NonNull<Node>) -> Self {
        Self { raw: lock, atom }
    }
    pub fn diffuse(self) {
        core::mem::forget(self);
    }
    pub fn reset(&mut self, new_atom: NonNull<Node>) {
        self.atom = new_atom;
    }
}

#[cfg(all(test, not(feature = "std")))]
mod tests {
    extern crate std;
    use super::*;
    use crate::node::{self, Node};

    #[test]
    fn test_bomb() {
        const NUM_THREADS: usize = 10;
        let barrier = std::sync::Barrier::new(NUM_THREADS);
        let raw = RawLock::new();
        std::thread::scope(|s| {
            let raw = &raw;
            let barrier = &barrier;
            for _ in 0..NUM_THREADS {
                s.spawn({
                    let raw = raw;
                    move || {
                        let node = Node::new(|_| ());
                        let this = NonNull::from(&node);
                        if let Some(prev) = raw.swap_tail(this) {
                            unsafe {
                                prev.as_ref().store_next(this);
                            }
                            barrier.wait();
                            assert!(node.wait() == node::POISONED);
                        } else {
                            let _bomb = Bomb::new(raw, this);
                            barrier.wait();
                        }
                    }
                });
            }
        });
    }
}
