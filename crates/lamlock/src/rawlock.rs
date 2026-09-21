use core::{
    ptr::NonNull,
    sync::atomic::{AtomicPtr, AtomicU32, Ordering},
};

use crate::{LockPoisoned, LockResult, node::Node};

const UNLOCKED: u32 = 0;
const LOCKED: u32 = 1;
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

    pub fn poison(&self) {
        self.status.store(POISONED, Ordering::Release);
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
    pub fn try_acquire(&self) -> LockResult<bool> {
        match self
            .status
            .compare_exchange(UNLOCKED, LOCKED, Ordering::Acquire, Ordering::Relaxed)
        {
            Ok(_) => Ok(true),
            Err(LOCKED) => Ok(false),
            Err(_) => Err(LockPoisoned),
        }
    }
    pub fn acquire(&self) -> LockResult<()> {
        loop {
            match self.status.compare_exchange(
                UNLOCKED,
                LOCKED,
                Ordering::Acquire,
                Ordering::Relaxed,
            ) {
                Ok(_) => return Ok(()),
                Err(LOCKED) => {
                    while self.status.load(Ordering::Relaxed) == LOCKED {
                        core::hint::spin_loop();
                    }
                }
                Err(_) => return Err(LockPoisoned),
            }
        }
    }
    pub fn release(&self) {
        self.status.store(UNLOCKED, Ordering::Release);
    }

    #[cfg(test)]
    pub fn tail_for_test(&self) -> *mut Node {
        self.tail.load(Ordering::Acquire)
    }

    #[cfg(test)]
    pub fn is_poisoned(&self, ordering: Ordering) -> bool {
        self.status.load(ordering) == POISONED
    }
}
