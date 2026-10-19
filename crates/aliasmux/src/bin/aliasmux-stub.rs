//! A generic alias executable with an empty template slot.
//!
//! `aliasmux new` and `aliasmux patch` copy this binary once per alias and
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
//! Constraint on change: this binary must not rely on anything std
//! initializes before `main`.
//!
//! [`run::ensure_standard_fds`]: aliasmux_core::run::ensure_standard_fds
//! [`run::ignore_sigpipe`]: aliasmux_core::run::ignore_sigpipe

#![no_main]

use std::ffi::{CStr, OsString, c_char, c_int};
use std::os::unix::ffi::OsStringExt;

use aliasmux_core::slot::{self, Slot};

/// The template slot that `aliasmux patch` overwrites in each copy.
///
/// It must be a `static` in this binary, not in aliasmux-core (see the note
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
        Ok((name, template)) => aliasmux_core::run_alias(name, template, &args),
        // Running the raw stub, or a damaged copy: explain, using the shell's
        // "not executable" status since there is nothing to run.
        Err(err) => {
            eprintln!("aliasmux-stub: {err}");
            126
        }
    }
}
