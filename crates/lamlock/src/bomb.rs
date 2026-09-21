use core::{mem::ManuallyDrop, ptr::NonNull, sync::atomic::Ordering};

use crate::{node::Node, rawlock::RawLock};

pub struct LightWeightBomb<'a> {
    raw: &'a RawLock,
}

impl<'a> LightWeightBomb<'a> {
    pub fn new(raw: &'a RawLock) -> Self {
        Self { raw }
    }

    pub fn get_raw(&self) -> &'a RawLock {
        self.raw
    }

    pub fn diffuse(self) {
        core::mem::forget(self);
    }
}

impl<'a> Drop for LightWeightBomb<'a> {
    #[cold]
    fn drop(&mut self) {
        self.raw.poison();
    }
}

pub struct HeavyWeightBomb<'a> {
    ignitor: ManuallyDrop<LightWeightBomb<'a>>,
    atom: NonNull<Node>,
    current_completed: bool,
}

impl<'a> Drop for HeavyWeightBomb<'a> {
    #[cold]
    fn drop(&mut self) {
        unsafe {
            ManuallyDrop::drop(&mut self.ignitor);
        }
        loop {
            let next = unsafe { self.atom.as_ref().load_next(Ordering::Acquire) };
            // If the next node is not null, we wake it up and continue to the next iteration.
            if let Some(next) = next {
                if self.current_completed {
                    Node::wake_as_done(self.atom);
                } else {
                    Node::wake_as_poisoned(self.atom);
                }
                self.atom = next;
                self.current_completed = false;
                continue;
            }
            // If we successfully closed the tail, we can stop after waking the last node.
            if self.ignitor.get_raw().try_close(self.atom) {
                if self.current_completed {
                    Node::wake_as_done(self.atom);
                } else {
                    Node::wake_as_poisoned(self.atom);
                }
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

impl<'a> HeavyWeightBomb<'a> {
    pub fn new(lock: &'a RawLock, atom: NonNull<Node>) -> Self {
        Self {
            ignitor: ManuallyDrop::new(LightWeightBomb::new(lock)),
            atom,
            current_completed: false,
        }
    }
    pub fn diffuse(self) {
        core::mem::forget(self);
    }
    pub fn set_current_completed(&mut self, completed: bool) {
        self.current_completed = completed;
    }
    pub fn reset(&mut self, new_atom: NonNull<Node>) {
        self.atom = new_atom;
        self.current_completed = false;
    }
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use crate::node::{self, Node};
    use crate::rawlock::RawLock;
    use core::ptr::NonNull;

    #[test]
    fn test_light_weight_bomb_diffuse() {
        let raw = RawLock::new();
        let bomb = LightWeightBomb::new(&raw);
        bomb.diffuse();
        assert!(!raw.is_poisoned(core::sync::atomic::Ordering::Acquire));
    }

    #[test]
    fn test_light_weight_bomb_poison() {
        let raw = RawLock::new();
        std::thread::scope(|s| {
            let raw = &raw;
            s.spawn(move || {
                LightWeightBomb::new(&raw);
            });
            while !raw.is_poisoned(core::sync::atomic::Ordering::Acquire) {
                core::hint::spin_loop();
            }
        });
    }

    #[test]
    fn test_heavy_weight_bomb() {
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
                        let node = Node::new(|_| true);
                        let this = NonNull::from(&node);
                        if let Some(prev) = raw.swap_tail(this) {
                            unsafe {
                                prev.as_ref().store_next(this);
                            }
                            barrier.wait();
                            assert!(node.wait() == node::POISONED);
                        } else {
                            let _bomb = HeavyWeightBomb::new(raw, this);
                            barrier.wait();
                        }
                    }
                });
            }
        });
    }
}
