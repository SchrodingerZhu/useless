use core::{ffi::c_ulong, mem::size_of};

use alloc::boxed::Box;
use linux_raw_sys::auxvec::AT_NULL;
use linux_raw_sys::prctl::PR_GET_AUXV;
use syscalls::{Sysno, syscall};

// Maximum number of entries in the auxiliary vector
// It is a rough guess based on typical usage, but can vary by system.
// 64 should be more than enough for most cases.
const MAX_AUXV_ENTRIES: usize = 64;
const MMAP_SIZE: usize = MAX_AUXV_ENTRIES * size_of::<AuxvEntry>();

#[derive(Clone, Copy)]
#[repr(C)]
pub struct AuxvEntry {
    pub key: c_ulong,
    pub value: c_ulong,
}

#[repr(transparent)]
pub struct Auxv(Box<[AuxvEntry; MAX_AUXV_ENTRIES]>);

impl Auxv {
    pub fn new() -> Option<Self> {
        let boxed = Box::new(
            [AuxvEntry {
                key: AT_NULL as c_ulong,
                value: AT_NULL as c_ulong,
            }; MAX_AUXV_ENTRIES],
        );
        unsafe {
            syscall!(Sysno::prctl, PR_GET_AUXV, boxed.as_ptr(), MMAP_SIZE, 0, 0).ok()?;
        }
        Some(Auxv(boxed))
    }
    pub fn iter(&'_ self) -> AuxvIter<'_> {
        AuxvIter::new(self.0.as_ref())
    }
}

pub struct AuxvIter<'a> {
    auxv: &'a [AuxvEntry],
    index: usize,
}

impl<'a> AuxvIter<'a> {
    fn new(auxv: &'a [AuxvEntry]) -> Self {
        AuxvIter { auxv, index: 0 }
    }
}

impl<'a> Iterator for AuxvIter<'a> {
    type Item = AuxvEntry;

    fn next(&mut self) -> Option<Self::Item> {
        let current = self.auxv.get(self.index)?;
        if current.key == AT_NULL as c_ulong {
            return None;
        }
        self.index += 1;
        Some(*current)
    }
}

#[cfg(test)]
mod tests {
    use linux_raw_sys::auxvec::AT_RANDOM;

    use super::*;
    #[test]
    fn test_mmap_auxv() {
        if cfg!(miri) {
            return;
        }
        let auxv = Auxv::new();
        assert!(auxv.is_some(), "Failed to mmap auxiliary vector");
    }

    #[test]
    fn test_auxv_iter() {
        if cfg!(miri) {
            return;
        }
        let auxv = Auxv::new().expect("Failed to mmap auxiliary vector");
        assert!(
            auxv.iter().any(|x| x.key == AT_RANDOM as c_ulong),
            "AT_RANDOM not found in auxiliary vector"
        );
    }
}
