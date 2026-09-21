use core::{
    ptr::NonNull,
    sync::atomic::{AtomicU32, Ordering},
};

#[cfg(not(miri))]
use linux_raw_sys::general::{FUTEX_WAIT_PRIVATE, FUTEX_WAKE_PRIVATE};
#[cfg(not(miri))]
use syscalls::{Sysno, raw_syscall};

#[repr(transparent)]
pub struct Futex(AtomicU32);

impl core::ops::Deref for Futex {
    type Target = AtomicU32;

    #[inline(always)]
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl Futex {
    #[inline(always)]
    pub const fn new(value: u32) -> Self {
        Self(AtomicU32::new(value))
    }

    #[inline(always)]
    pub fn wait(this: NonNull<Self>, value: u32) {
        #[cfg(not(miri))]
        while unsafe { this.as_ref().load(Ordering::Acquire) == value } {
            // The waiter owns this node, so its futex word stays alive while
            // the kernel checks it and waits. A null timeout waits indefinitely.
            let _ = unsafe {
                raw_syscall!(
                    Sysno::futex,
                    this.as_ptr().cast::<u32>(),
                    FUTEX_WAIT_PRIVATE,
                    value,
                    0
                )
            };
            // Recheck the state after spurious wakes, EAGAIN, and EINTR.
        }

        #[cfg(miri)]
        while unsafe { this.as_ref().load(Ordering::Acquire) == value } {
            core::hint::spin_loop();
        }
    }

    #[inline(always)]
    pub fn notify(this: NonNull<Self>, new_val: u32, #[allow(unused)] old_val: u32) {
        #[cfg(not(miri))]
        {
            let address = this.as_ptr().cast::<u32>();
            // Publishing the new state lets the waiter reclaim the node.
            // This swap must be our last access to its memory; only the saved
            // raw address may be used afterward, without creating references.
            if unsafe { this.as_ref().swap(new_val, Ordering::AcqRel) == old_val } {
                Self::wake(address);
            }
        }

        #[cfg(miri)]
        unsafe {
            this.as_ref().store(new_val, Ordering::Release);
        }
    }

    #[cfg(not(miri))]
    #[inline(always)]
    fn wake(address: *mut u32) {
        // SAFETY: Ordinary FUTEX_WAKE_PRIVATE uses the address as a queue key
        // within this process; it does not access the futex word. The address
        // may therefore be dangling or unmapped. Reuse can wake an unrelated
        // futex, whose consumer must handle spurious wakeups by rechecking its
        // condition. Errors caused by reuse are also harmless for this wake.
        // See https://github.com/bminor/glibc/blob/master/sysdeps/nptl/futex-internal.h
        // (futex_wake), which explicitly permits destroyed/reused/unmapped futexes.
        let _ = unsafe { raw_syscall!(Sysno::futex, address, FUTEX_WAKE_PRIVATE, 1) };
    }
}

#[cfg(all(test, not(miri), target_pointer_width = "64"))]
mod tests {
    use super::*;
    use linux_raw_sys::general::{MAP_ANONYMOUS, MAP_PRIVATE, PROT_READ, PROT_WRITE};
    use syscalls::syscall;

    #[test]
    fn wake_accepts_unmapped_address() {
        let len = core::mem::size_of::<u32>();
        let address = unsafe {
            syscall!(
                Sysno::mmap,
                0,
                len,
                PROT_READ | PROT_WRITE,
                MAP_ANONYMOUS | MAP_PRIVATE,
                -1isize,
                0
            )
        }
        .unwrap();
        unsafe { syscall!(Sysno::munmap, address, len) }.unwrap();

        // Model a late notification after the waiting thread's stack is gone.
        Futex::wake(address as *mut u32);
    }
}
