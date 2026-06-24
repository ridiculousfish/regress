//! Executable memory for the JIT: copy generated code into fresh pages, flip
//! them to read+execute, and flush the instruction cache so the CPU sees the
//! freshly written instructions.
//!
//! We try the simple portable path first — allocate read/write pages with the
//! `region` crate, write the code, then `region::protect` to read/execute. On
//! Apple Silicon this works for ad-hoc-signed binaries (cargo test/bench) that
//! don't opt into the hardened runtime; if a future signed/entitled build
//! rejects the `mprotect`, this is the seam to escalate to `MAP_JIT` +
//! `pthread_jit_write_protect_np` (see the plan).

use super::JitError;
use region::{Allocation, Protection};

/// A page-aligned, read+execute region holding generated machine code. The
/// entry point is at offset 0. Dropping it unmaps the pages, so the owner must
/// outlive every call through `entry_ptr`.
pub(crate) struct ExecBuffer {
    /// Keeps the mapping alive; `Drop` unmaps. Never read directly after
    /// construction — we hand out raw pointers into it.
    alloc: Allocation,
    /// The number of code bytes actually written (≤ `alloc.len()`, which is
    /// page-rounded).
    len: usize,
}

impl ExecBuffer {
    /// Allocate RW pages, copy `code` in, protect read+execute, and flush the
    /// instruction cache. `code` must be non-empty.
    pub(crate) fn new(code: &[u8]) -> Result<Self, JitError> {
        debug_assert!(!code.is_empty());
        // `region::alloc` rounds up to whole pages and zero-fills.
        let mut alloc = region::alloc(code.len(), Protection::READ_WRITE)?;
        // SAFETY: the allocation is at least `code.len()` bytes and currently
        // writable; src and dst don't overlap (fresh mapping).
        unsafe {
            core::ptr::copy_nonoverlapping(code.as_ptr(), alloc.as_mut_ptr::<u8>(), code.len());
        }
        // SAFETY: protecting our own freshly-allocated mapping.
        unsafe {
            region::protect(alloc.as_ptr::<u8>(), alloc.len(), Protection::READ_EXECUTE)?;
        }
        flush_icache(alloc.as_ptr::<u8>(), code.len());
        Ok(Self {
            alloc,
            len: code.len(),
        })
    }

    /// Raw pointer to the entry point (offset 0). Valid for the lifetime of
    /// `self`.
    pub(crate) fn entry_ptr(&self) -> *const u8 {
        self.alloc.as_ptr::<u8>()
    }

    /// Number of code bytes written.
    #[allow(dead_code)]
    pub(crate) fn len(&self) -> usize {
        self.len
    }
}

impl core::fmt::Debug for ExecBuffer {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("ExecBuffer")
            .field("entry", &self.alloc.as_ptr::<u8>())
            .field("len", &self.len)
            .finish()
    }
}

/// Make instructions written as data visible to instruction fetch. Required on
/// aarch64 (separate I/D caches, no coherence guarantee); a no-op on x86-64,
/// whose instruction cache is coherent with stores.
#[inline]
fn flush_icache(ptr: *const u8, len: usize) {
    #[cfg(all(target_arch = "aarch64", target_os = "macos"))]
    {
        // SAFETY: Darwin's documented cache-flush entry point; `ptr..ptr+len`
        // is our own mapping.
        unsafe { sys_icache_invalidate(ptr as *mut core::ffi::c_void, len) };
    }
    #[cfg(all(target_arch = "aarch64", not(target_os = "macos")))]
    {
        // The compiler-rt builtin: flush [start, end). LLVM/GCC lower the
        // necessary `dc cvau` / `ic ivau` / barriers.
        // SAFETY: range is our own mapping.
        unsafe { __clear_cache(ptr as *mut core::ffi::c_char, (ptr as usize + len) as *mut core::ffi::c_char) };
    }
    #[cfg(not(target_arch = "aarch64"))]
    {
        // x86-64: coherent instruction cache, nothing to do.
        let _ = (ptr, len);
    }
}

#[cfg(all(target_arch = "aarch64", target_os = "macos"))]
unsafe extern "C" {
    fn sys_icache_invalidate(start: *mut core::ffi::c_void, len: usize);
}

#[cfg(all(target_arch = "aarch64", not(target_os = "macos")))]
unsafe extern "C" {
    fn __clear_cache(start: *mut core::ffi::c_char, end: *mut core::ffi::c_char);
}
