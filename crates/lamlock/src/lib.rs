#![no_std]
#![cfg_attr(all(feature = "nightly", not(miri)), allow(internal_features))]
#![cfg_attr(all(feature = "nightly", not(miri)), feature(core_intrinsics))]
#![doc = include_str!("../README.md")]
#[cfg(feature = "std")]
extern crate std;

mod bomb;
mod futex;
mod node;
mod panic;
mod rawlock;

pub mod nonpoison;
pub mod poison;

pub use poison::Lock;

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
