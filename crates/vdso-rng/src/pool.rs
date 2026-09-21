use core::{ffi::c_void, ptr::NonNull};

use alloc::vec::Vec;
use crossbeam_queue::SegQueue;
use lamlock::Lock;

use crate::{config::Config, utils};

#[repr(transparent)]
#[derive(Debug, Clone, Copy)]
pub struct Ptr(pub(crate) NonNull<c_void>);

unsafe impl Send for Ptr {}

/// A shared state block pool for `vDSO`-based `getrandom` operations.
///
/// This pool maintains a free list of opaque state blocks. Threads can rent a block
/// from the pool and use it to generate random bytes. The pool is [`Sync`] and supports
/// concurrent access. Under debug assertions, [`Pool::drop`] detects potential misuse,
/// such as double drops.
///
/// The pool itself offers only [`Pool::new`]. To interact with the pool, see [`crate::LocalState`],
/// which wraps individual state blocks for random generation.
///
/// ```rust
/// use vdso_rng::Pool;
/// let _pool = Pool::new().unwrap();
/// ```
///
/// ### Memory Behavior
/// The pool grows monotonically with system parallelism. Opaque state blocks are stored
/// in memory-mapped pages that are not backed by swap. When the system is under memory pressure,
/// the OS may reclaim these pages, which is generally safe.
///
/// ## Safety
/// - **Not async-signal-safe**: Using the pool in signal handlers may cause deadlocks.
/// - **Fork safety**: After `fork`, the kernel wipes the random states to avoid leaks.
///   However, we do **not** guarantee correctness of pool usage across forks.
pub struct Pool {
    pub(crate) config: Config,
    mmaps: Lock<Vec<Ptr>>,
    freelist: SegQueue<Ptr>,
}

impl Pool {
    pub fn new() -> Result<Self, crate::Error> {
        let (function, page_size) =
            crate::vdso::get_function_and_page_size().ok_or(crate::Error::NotSupported)?;
        let config = unsafe { Config::new(function, page_size) };
        let mmaps = Lock::new(Vec::new());
        let freelist = SegQueue::new();
        Ok(Self {
            config,
            mmaps,
            freelist,
        })
    }
    fn grow(
        mmaps: &mut Vec<Ptr>,
        config: &Config,
        freelist: &SegQueue<Ptr>,
    ) -> Result<(), crate::Error> {
        let page = utils::mmap(
            config.page_size * config.pages_per_block,
            config.params.mmap_prot,
            config.params.mmap_flags,
        )
        .ok_or(crate::Error::AllocationFailure)?;
        mmaps.push(Ptr(page));
        unsafe {
            for p in 0..config.pages_per_block {
                let page_ptr = page.byte_add(p * config.page_size);
                for s in 0..config.states_per_page {
                    let state_ptr =
                        page_ptr.byte_add(s * config.params.size_of_opaque_states as usize);
                    freelist.push(Ptr(state_ptr));
                }
            }
        }
        Ok(())
    }
    pub(crate) fn get(&self) -> Result<Ptr, crate::Error> {
        if let Some(ptr) = self.freelist.pop() {
            return Ok(ptr);
        }
        self.mmaps
            .run(|mmaps| {
                // Since the mmaps is locked, this loop should terminates in finite amount of time.
                loop {
                    match self.freelist.pop() {
                        Some(ptr) => return Ok(ptr),
                        None => {
                            Self::grow(mmaps, &self.config, &self.freelist)?;
                            continue;
                        }
                    }
                }
            })
            .unwrap_or(Err(crate::Error::PoolPoisoned))
    }
    pub(crate) fn recycle(&self, ptr: Ptr) {
        self.freelist.push(ptr);
    }
}

impl Drop for Pool {
    fn drop(&mut self) {
        #[cfg(debug_assertions)]
        let mut counter = 0;
        while self.freelist.pop().is_some() {
            #[cfg(debug_assertions)]
            {
                counter += 1;
            }
        }
        let mmaps = self.mmaps.get_mut();
        #[cfg(debug_assertions)]
        debug_assert_eq!(
            counter,
            self.config.pages_per_block * self.config.states_per_page * mmaps.len(),
            "Freelist should contain all states from all mmaps"
        );
        for ptr in mmaps.drain(..) {
            unsafe { utils::munmap(ptr.0, self.config.page_size * self.config.pages_per_block) };
        }
    }
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;

    #[test]
    fn pool_smoke_test() {
        let pool = Pool::new().expect("Failed to create pool");
        let ptr = pool.get().expect("Failed to get pointer from pool");
        pool.recycle(ptr);
    }

    #[test]
    fn pool_multi_thread_test() {
        let parallelism = std::thread::available_parallelism().unwrap();
        let pool = Pool::new().expect("Failed to create pool with VDSO function and page size");
        std::thread::scope(|scope| {
            for _ in 0..parallelism.get() {
                scope.spawn(|| {
                    let ptrs = (0..16)
                        .map(|_| pool.get().expect("Failed to get pointer from pool"))
                        .collect::<Vec<_>>();
                    for ptr in ptrs {
                        pool.recycle(ptr);
                    }
                });
            }
        });
    }
}
