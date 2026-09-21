use core::{
    ffi::{CStr, c_char, c_int, c_void},
    ptr::NonNull,
};

use linux_raw_sys::{
    ctypes::c_uint,
    elf::{Elf_Dyn, Elf_Ehdr, Elf_Phdr, Elf_Sym, Elf_Verdaux, Elf_Verdef, VER_FLG_BASE},
    elf_uapi::Elf64_Shdr,
};

pub type VdsoFunc = unsafe extern "C" fn(*mut c_void, usize, c_uint, *mut c_void, usize) -> c_int;

#[repr(transparent)]
#[derive(Debug, Clone, Copy)]
struct VerdefRef(NonNull<Elf_Verdef>);

impl core::ops::Deref for VerdefRef {
    type Target = Elf_Verdef;

    fn deref(&self) -> &Self::Target {
        unsafe { self.0.as_ref() }
    }
}

impl VerdefRef {
    fn aux(&self) -> Option<&Elf_Verdaux> {
        if self.vd_aux == 0 {
            None
        } else {
            let offset = self.vd_aux as usize;
            let ptr = unsafe { self.0.byte_add(offset) };
            Some(unsafe { ptr.cast().as_ref() })
        }
    }
    fn as_ptr(&self) -> *mut Elf_Verdef {
        self.0.as_ptr()
    }
}

struct VerdefIter(Option<VerdefRef>);

impl Iterator for VerdefIter {
    type Item = VerdefRef;

    fn next(&mut self) -> Option<Self::Item> {
        let current = self.0?;
        let offset = current.vd_next;
        if offset == 0 {
            self.0 = None;
        } else {
            self.0 = Some(VerdefRef(unsafe {
                NonNull::new_unchecked(current.as_ptr().byte_add(offset as usize))
            }));
        }
        Some(current)
    }
}

const fn get_name_and_version() -> Option<(&'static CStr, &'static CStr)> {
    if cfg!(target_arch = "aarch64") {
        return Some((c"__kernel_getrandom", c"LINUX_2.6.39"));
    }

    if cfg!(target_arch = "x86_64") {
        return Some((c"__vdso_getrandom", c"LINUX_2.6"));
    }

    None
}

struct SymbolTable {
    strtab: NonNull<c_char>,
    symtab: &'static [Elf_Sym],
    versym: &'static [u16],
    verdef: VerdefRef,
    vdso_addr: NonNull<c_void>,
}

impl SymbolTable {
    unsafe fn load(phdr: PhdrInfo, symbol_count: usize) -> Option<Self> {
        let mut strtab = None;
        let mut symtab = None;
        let mut versym = None;
        let mut verdef = None;
        for dy in phdr.dyn_iter() {
            match dy.d_tag as u32 {
                linux_raw_sys::elf_uapi::DT_STRTAB => {
                    strtab = Some(NonNull::new(
                        (phdr.vdso_addr.as_ptr() as usize + unsafe { dy.d_un.d_val } as usize)
                            as *mut c_char,
                    )?);
                }
                linux_raw_sys::elf_uapi::DT_SYMTAB => {
                    symtab = Some(NonNull::new(
                        (phdr.vdso_addr.as_ptr() as usize + unsafe { dy.d_un.d_val } as usize)
                            as *mut Elf_Sym,
                    )?);
                }
                linux_raw_sys::elf_uapi::DT_VERSYM => {
                    versym = Some(NonNull::new(
                        (phdr.vdso_addr.as_ptr() as usize + unsafe { dy.d_un.d_val } as usize)
                            as *mut u16,
                    )?);
                }
                linux_raw_sys::elf_uapi::DT_VERDEF => {
                    verdef = Some(VerdefRef(NonNull::new(
                        (phdr.vdso_addr.as_ptr() as usize + unsafe { dy.d_un.d_val } as usize)
                            as *mut Elf_Verdef,
                    )?));
                }
                _ => continue,
            }
        }
        Some(Self {
            strtab: strtab?,
            symtab: unsafe { core::slice::from_raw_parts(symtab?.as_ptr(), symbol_count) },
            versym: unsafe { core::slice::from_raw_parts(versym?.as_ptr(), symbol_count) },
            verdef: verdef?,
            vdso_addr: phdr.vdso_addr,
        })
    }
    fn verdef_iter(&self) -> VerdefIter {
        VerdefIter(Some(self.verdef))
    }
    fn find_version(&self, index: usize) -> Option<&CStr> {
        let identifier = self.versym.get(index)? & 0x7FFF;
        for verdef in self.verdef_iter() {
            if verdef.vd_flags & VER_FLG_BASE != 0 {
                continue;
            }
            if verdef.vd_ndx & 0x7FFF == identifier {
                let aux = verdef.aux()?;
                return Some(unsafe {
                    CStr::from_ptr(self.strtab.as_ptr().add(aux.vda_name as usize))
                });
            }
        }
        None
    }
    fn find_symbol(&self) -> Option<VdsoFunc> {
        let (target_name, target_version) = get_name_and_version()?;
        for (i, sym) in self.symtab.iter().enumerate() {
            let name_offset = sym.st_name as usize;
            let name = unsafe { CStr::from_ptr(self.strtab.as_ptr().add(name_offset)) };
            if name == target_name {
                let version = self.find_version(i)?;
                if version == target_version {
                    let addr = unsafe { self.vdso_addr.byte_add(sym.st_value) };
                    return Some(unsafe {
                        core::mem::transmute::<NonNull<c_void>, VdsoFunc>(addr)
                    });
                }
            }
        }
        None
    }
}

#[repr(transparent)]
struct DynIter(Option<NonNull<Elf_Dyn>>);

impl Iterator for DynIter {
    type Item = &'static Elf_Dyn;

    fn next(&mut self) -> Option<Self::Item> {
        let current = self.0?;
        if unsafe { current.as_ref().d_tag == linux_raw_sys::elf_uapi::DT_NULL as usize } {
            return None;
        }
        self.0 = Some(unsafe { current.add(1) });
        Some(unsafe { current.as_ref() })
    }
}

struct PhdrInfo {
    vdso_addr: NonNull<c_void>,
    vdso_dyn: NonNull<Elf_Dyn>,
}

impl PhdrInfo {
    unsafe fn load(ehdr: NonNull<Elf_Ehdr>) -> Option<Self> {
        let phoff = unsafe { ehdr.as_ref().e_phoff };
        let phnum = unsafe { ehdr.as_ref().e_phnum } as usize;
        let ptr = unsafe { ehdr.cast::<Elf_Phdr>().byte_add(phoff) };
        let phdr_array = unsafe { core::slice::from_raw_parts(ptr.as_ptr(), phnum) };
        let mut vdso_dyn = None;
        let mut vdso_addr = None;
        for ph in phdr_array {
            if ph.p_type == linux_raw_sys::elf_uapi::PT_DYNAMIC {
                vdso_dyn.replace(unsafe { ehdr.cast::<Elf_Dyn>().byte_add(ph.p_offset) });
                continue;
            } else if ph.p_type == linux_raw_sys::elf_uapi::PT_LOAD
                && ph.p_flags & linux_raw_sys::elf_uapi::PF_X != 0
            {
                vdso_addr.replace(unsafe {
                    ehdr.cast()
                        .byte_offset(ph.p_offset as isize - ph.p_vaddr as isize)
                });
            }
        }
        Some(Self {
            vdso_addr: vdso_addr?,
            vdso_dyn: vdso_dyn?,
        })
    }
    fn dyn_iter(&self) -> DynIter {
        DynIter(Some(self.vdso_dyn))
    }
}

struct ElfShdrArray(
    #[cfg(target_pointer_width = "64")] &'static [Elf64_Shdr],
    #[cfg(target_pointer_width = "32")] &'static [Elf32_Shdr],
);

impl ElfShdrArray {
    fn symbol_count(&self) -> usize {
        for shdr in self.0.iter() {
            if shdr.sh_type == linux_raw_sys::elf_uapi::SHT_DYNSYM {
                return shdr.sh_size as usize / shdr.sh_entsize as usize;
            }
        }
        0
    }
    unsafe fn load(ehdr: NonNull<Elf_Ehdr>) -> Option<Self> {
        let shnum = unsafe { ehdr.as_ref().e_shnum } as usize;
        let shoff = unsafe { ehdr.as_ref().e_shoff };
        let ptr = unsafe { ehdr.cast().byte_add(shoff) };
        let slice = unsafe { core::slice::from_raw_parts(ptr.as_ptr(), shnum) };
        Some(Self(slice))
    }
}

pub fn get_function_and_page_size() -> Option<(VdsoFunc, usize)> {
    let auxv = crate::auxv::Auxv::new()?;
    let mut func = None;
    let mut page_size = None;
    for entry in auxv.iter() {
        if entry.key == linux_raw_sys::auxvec::AT_SYSINFO_EHDR.into() {
            unsafe {
                let ehdr = NonNull::new(entry.value as *mut Elf_Ehdr)?;
                let shdr = ElfShdrArray::load(ehdr)?;
                let symbol_count = shdr.symbol_count();
                let phdr_info = PhdrInfo::load(ehdr)?;
                let symbol_table = SymbolTable::load(phdr_info, symbol_count)?;
                func = symbol_table.find_symbol();
            }
        }
        if entry.key == linux_raw_sys::auxvec::AT_PAGESZ.into() {
            page_size = Some(entry.value as usize);
        }
    }
    Some((func?, page_size?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_get_function() {
        if cfg!(miri) {
            return;
        }
        let (_func, page_size) =
            get_function_and_page_size().expect("Failed to get VDSO function and page size");
        assert!(page_size > 0, "Page size should be greater than 0");
    }
}
