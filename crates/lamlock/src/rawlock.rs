use core::{
    ptr::NonNull,
    sync::atomic::{AtomicPtr, AtomicU32, Ordering},
};

use crate::node::Node;

// Without a catcher, an unwinding combiner must still cancel its queue.
#[cfg(feature = "std")]
pub type QueueError = core::convert::Infallible;
#[cfg(not(feature = "std"))]
pub type QueueError = crate::LockPoisoned;

const UNLOCKED: u32 = 0;
const LOCKED: u32 = 1;
#[cfg(not(feature = "std"))]
const POISONED: u32 = 2;

pub struct RawLock {
    status: AtomicU32,
    tail: AtomicPtr<Node>,
}

impl RawLock {
    pub const fn new() -> Self {
        Self {
            status: AtomicU32::new(0),
            tail: AtomicPtr::new(core::ptr::null_mut()),
        }
    }

    pub fn has_tail(&self, ordering: Ordering) -> bool {
        !self.tail.load(ordering).is_null()
    }

    pub fn swap_tail(&self, new_tail: NonNull<Node>) -> Option<NonNull<Node>> {
        let old_tail = self.tail.swap(new_tail.as_ptr(), Ordering::AcqRel);
        NonNull::new(old_tail)
    }

    pub fn try_close(&self, expected: NonNull<Node>) -> bool {
        self.tail
            .compare_exchange(
                expected.as_ptr(),
                core::ptr::null_mut(),
                Ordering::AcqRel,
                Ordering::Relaxed,
            )
            .is_ok()
    }
    pub fn try_acquire(&self) -> Result<bool, QueueError> {
        match self
            .status
            .compare_exchange(UNLOCKED, LOCKED, Ordering::Acquire, Ordering::Relaxed)
        {
            Ok(_) => Ok(true),
            #[cfg(not(feature = "std"))]
            Err(POISONED) => Err(crate::LockPoisoned),
            Err(_) => Ok(false),
        }
    }
    pub fn acquire(&self) -> Result<(), QueueError> {
        while !self.try_acquire()? {
            while self.status.load(Ordering::Relaxed) == LOCKED {
                core::hint::spin_loop();
            }
        }
        Ok(())
    }

    #[cfg(not(feature = "std"))]
    pub fn poison(&self) {
        self.status.store(POISONED, Ordering::Release);
    }

    pub fn release(&self) {
        self.status.store(UNLOCKED, Ordering::Release);
    }

    #[cfg(test)]
    pub fn tail_for_test(&self) -> *mut Node {
        self.tail.load(Ordering::Acquire)
    }
}
