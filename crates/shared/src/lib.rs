//! The aliasmux runtime behind a C ABI, built as `libaliasmux_shared.so`.
//!
//! The whole interface is one function, [`aliasmux_run_slot`], declared for C
//! in `c/aliasmux.h`. A stub only has to hold a template slot and pass it in
//! along with its argv, so stubs can be written in any language; the one in
//! `c/stub.c` is a few lines of C.
//!
//! Because the interface is C, Rust's std is linked statically into this
//! library. Stubs need no libstd.so and no matching rustc version: the library
//! can be rebuilt with any compiler and existing stubs keep working.

use std::ffi::{CStr, OsStr, OsString, c_char, c_int};
use std::os::unix::ffi::OsStrExt;

use aliasmux_core::slot::{self, Slot};

/// Run the alias stored in a slot, with the caller's command-line arguments.
///
/// Never returns: it either becomes the target program (`exec`) or exits with
/// the alias's outcome, exactly as the static variants do.
///
/// :param slot: pointer to the caller's template slot
/// :param argc: argument count, as passed to the caller's `main`
/// :param argv: argument vector, as passed to the caller's `main`
/// :return: never
///
/// # Safety
///
/// `slot` must point to `slot::SIZE` readable bytes, and `argv` must hold
/// `argc` valid NUL-terminated strings, as a C `main` receives them.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn aliasmux_run_slot(slot: *const Slot, argc: c_int, argv: *const *const c_char) -> ! {
    // SAFETY: the caller guarantees `slot` covers a whole slot. `Slot` is made
    // only of byte arrays (alignment 1, every bit pattern valid), so any such
    // memory is a valid `Slot`.
    let current = unsafe { &*slot };

    // The marker doubles as the ABI version check: a stub built against a
    // different slot layout would carry a different marker.
    if current.marker != slot::MARKER {
        eprintln!("aliasmux: stub and libaliasmux_shared.so disagree on the slot format; rebuild the stub");
        std::process::exit(126);
    }
    // An unpatched or damaged stub has nothing to run.
    let (name, template) = match slot::decode(current) {
        Ok(found) => found,
        Err(err) => {
            eprintln!("aliasmux-stub: {err}");
            std::process::exit(126);
        }
    };

    // Copy argv[1..] into OsStrings, byte for byte, so non-UTF-8 arguments
    // survive. A negative argc is treated as no arguments.
    let count = usize::try_from(argc).unwrap_or(0);
    let args: Vec<OsString> = (1..count)
        .map(|index| {
            // SAFETY: the caller guarantees `argv[0..argc]` are valid C strings.
            let raw = unsafe { CStr::from_ptr(*argv.add(index)) };
            OsStr::from_bytes(raw.to_bytes()).to_os_string()
        })
        .collect();

    // A panic can't unwind into C: with the "C" ABI, Rust aborts the process
    // instead, which is acceptable for what would be a bug here.
    aliasmux_core::run_alias(name, template, &args)
}
