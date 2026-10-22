//! A generic alias executable with an empty template slot.
//!
//! `rea new` and `rea patch` copy this binary once per alias and
//! write the alias into [`SLOT`]. Each copy is then a standalone executable:
//! its name on disk doesn't matter, and nothing else is needed at run time.
//!
//! Deviation from a normal Rust program: `#![no_main]`. The C runtime calls
//! our `main` directly, skipping std's setup before `main`, which is most of
//! what an alias costs beyond the exec itself. Measured with
//! `perf stat -e instructions:k` on a static build, a stub with std's setup
//! cost more than twice the kernel work of a C++ wrapper doing the same job.
//! What that setup did, and what replaces it:
//!
//! * Find the main thread's stack bounds for overflow detection (glibc reads
//!   and parses `/proc/self/maps`, musl probes with `mremap`), and install a
//!   SIGSEGV/SIGBUS handler on an alternate stack to report overflows. Dropped:
//!   a stack overflow becomes a plain segfault, and the stub never recurses
//!   deeply.
//! * Reopen closed fds 0-2 on `/dev/null`, and ignore SIGPIPE: both done only
//!   when a supervisor runs, by [`run::ensure_standard_fds`] and
//!   [`run::ignore_sigpipe`]. A single command is exec'd with descriptors and
//!   signal dispositions exactly as the alias received them.
//! * Capture argv for `std::env::args`: replaced by reading `argv` here, since
//!   std captures it without its setup only on glibc.
//!
//! For the same reason the stub has its own allocator, [`Arena`], which keeps
//! the C library's heap from being set up at all in the usual case.
//!
//! Constraint on change: this binary must not rely on anything std
//! initializes before `main`.
//!
//! [`run::ensure_standard_fds`]: rea_core::run::ensure_standard_fds
//! [`run::ignore_sigpipe`]: rea_core::run::ignore_sigpipe

#![no_main]

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::UnsafeCell;
use std::ffi::{CStr, OsString, c_char, c_int};
use std::os::unix::ffi::OsStringExt;
use std::sync::atomic::{AtomicUsize, Ordering};

use rea_core::slot::{self, Slot};

/// Size of the stub's allocation arena.
///
/// Arbitrary, but generous: an alias run allocates a few kilobytes (the parsed
/// template, the arguments, the argv to exec). Anything beyond it goes to the
/// system allocator, so a huge argument list still works.
const ARENA_SIZE: usize = 64 * 1024;

/// A bump allocator over a static buffer, falling back to the system
/// allocator when the buffer is full.
///
/// Why: the first allocation from the C library's allocator sets up its heap
/// (with musl, two `brk` and two `mmap` calls), which was a measurable part of
/// an alias run. The buffer is zero-initialized, so it lives in `.bss`: it
/// costs nothing in the file, and only the pages actually used are ever
/// faulted in.
///
/// Memory is never returned, except that freeing or growing the most recent
/// allocation reuses its space. That suits a process that execs or exits
/// within microseconds; the supervisor's allocations don't grow while it
/// waits for its children.
///
/// Constraint on change: `used` comes *before* `bytes`, so it shares a page
/// with the start of the buffer, which the first allocation writes anyway.
/// After the buffer, it sat alone on a page 64 KiB further on, costing that
/// page's faults on every run (`perf trace -F all` showed them).
#[repr(C, align(16))]
struct Arena {
    /// Bytes of `bytes` in use, from the start.
    used: AtomicUsize,
    /// The memory handed out.
    bytes: UnsafeCell<[u8; ARENA_SIZE]>,
}

// SAFETY: `bytes` is only ever handed out in disjoint pieces, and `used`,
// which decides the pieces, is only changed atomically.
unsafe impl Sync for Arena {}

impl Arena {
    /// Address of the first byte of the buffer.
    fn base(&self) -> usize {
        self.bytes.get() as usize
    }

    /// Whether `ptr` points into the buffer (so the arena owns it).
    fn owns(&self, ptr: *mut u8) -> bool {
        (self.base()..self.base() + ARENA_SIZE).contains(&(ptr as usize))
    }
}

// SAFETY: every pointer returned is either a piece of `bytes` that no other
// live allocation overlaps (claimed by a successful compare-and-swap on
// `used`), aligned as requested, or one from `System`; each is released to
// whichever allocator it came from.
unsafe impl GlobalAlloc for Arena {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // Ordering: `used` only has to hand out each byte once, which any
        // atomic compare-and-swap guarantees; nothing is published through it.
        //
        // Start by guessing 0 rather than loading `used`: a compare-and-swap
        // is a write even when it fails, so the first touch of the page is a
        // write. Reading an untouched page first maps the shared zero page,
        // then the write faults a second time for a real one. A wrong guess
        // costs one failed compare-and-swap, which returns the real value.
        let mut used = 0;
        loop {
            // Find the next address in the arena that matches the requested
            // alignment, or fall back to the system allocator if there's no room.
            let alloc_addr_start =
                (self.base() + used + layout.align() - 1) & !(layout.align() - 1);
            let new_used = alloc_addr_start - self.base() + layout.size();
            if new_used > ARENA_SIZE {
                // SAFETY: forwarding the caller's valid layout.
                return unsafe { System.alloc(layout) };
            }

            // Claim it, unless another thread moved `used` first; then retry from there.
            match self.used.compare_exchange_weak(
                used,
                new_used,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => return alloc_addr_start as *mut u8,
                Err(now) => used = now,
            }
        }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        if self.owns(ptr) {
            // Where the block sits in the buffer, and the offset just past it.
            let block_start = ptr as usize - self.base();
            let block_end = block_start + layout.size();
            // If this was the last allocation, `used` ends exactly at the block's end:
            // move it back to the block's start. Otherwise, leave it alone; the bytes
            // simply stay unused.
            let _ = self.used.compare_exchange(
                block_end,
                block_start,
                Ordering::Relaxed,
                Ordering::Relaxed,
            );
        } else {
            // SAFETY: the pointer came from `System` with this layout.
            unsafe { System.dealloc(ptr, layout) }
        }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        if !self.owns(ptr) {
            // SAFETY: the pointer came from `System` with this layout.
            return unsafe { System.realloc(ptr, layout, new_size) };
        }
        // The last allocation can grow or shrink in place, which is the
        // common case of a string or vector being built up.
        let start = ptr as usize - self.base();
        if start + new_size <= ARENA_SIZE
            && self
                .used
                .compare_exchange(
                    start + layout.size(),
                    start + new_size,
                    Ordering::Relaxed,
                    Ordering::Relaxed,
                )
                .is_ok()
        {
            return ptr;
        }
        // Otherwise allocate anew and copy, possibly into the system heap.
        // SAFETY: the caller guarantees `new_size` is valid for this alignment.
        let new_layout = unsafe { Layout::from_size_align_unchecked(new_size, layout.align()) };
        // SAFETY: a valid layout, per the caller's guarantee.
        let new = unsafe { self.alloc(new_layout) };
        if !new.is_null() {
            // SAFETY: both blocks are valid for the smaller of the two sizes
            // and don't overlap (the old one is still allocated).
            unsafe { std::ptr::copy_nonoverlapping(ptr, new, layout.size().min(new_size)) };
            // SAFETY: releasing the old block with the layout it was made with.
            unsafe { self.dealloc(ptr, layout) };
        }
        new
    }
}

/// Every allocation in the stub goes through the arena first.
#[global_allocator]
static ALLOCATOR: Arena = Arena {
    used: AtomicUsize::new(0),
    bytes: UnsafeCell::new([0; ARENA_SIZE]),
};

/// The template slot that `rea patch` overwrites in each copy.
///
/// It must be a `static` in this binary, not in rea-core (see the note
/// in `slot.rs`). `#[used]` keeps it in the binary even if
/// the optimizer could see through every read.
#[used]
static SLOT: Slot = slot::EMPTY;

/// Read the patched alias from [`SLOT`] and run it.
///
/// Called by the C runtime with the process's `argc` and `argv`. Never
/// returns normally: the alias execs, or exits with its outcome.
#[unsafe(no_mangle)]
extern "C" fn main(argc: c_int, argv: *const *const c_char) -> c_int {
    // The rest of argv (minus our own name) goes to the alias. Copied as raw
    // bytes, so non-UTF-8 arguments survive.
    let args: Vec<OsString> = (1..usize::try_from(argc).unwrap_or(0))
        .map(|i| {
            // SAFETY: the C runtime passes `argc` pointers to valid
            // NUL-terminated strings, which live for the whole process.
            let arg = unsafe { CStr::from_ptr(*argv.add(i)) };
            OsString::from_vec(arg.to_bytes().to_vec())
        })
        .collect();
    // Constraint on change: read the slot through `black_box`. To the compiler
    // SLOT is an immutable static whose contents it knows (the empty
    // placeholder), so without this it may constant-fold `len == 0` and
    // compile the stub into "always unpatched". `black_box` makes the address
    // opaque, forcing real loads of whatever bytes the file holds at run time.
    let current: &Slot = std::hint::black_box(&SLOT);
    match slot::decode(current) {
        Ok((name, template)) => rea_core::run_stored_alias(name, template, &args),
        // Running the raw stub, or a damaged copy: explain, using the shell's
        // "not executable" status since there is nothing to run.
        Err(err) => {
            eprintln!("rea-stub: {err}");
            126
        }
    }
}
