//! A generic alias executable with an empty template slot.
//!
//! `aliasmux patch --stub <this> --out DIR` copies this binary once per alias
//! and writes the alias into [`SLOT`]. Each copy is then a standalone
//! executable: its name on disk doesn't matter, and nothing else is needed at
//! run time (apart from the `.so` files in the dynamic build).

// Exactly one runtime source; both at once would link the runtime twice.
#[cfg(all(feature = "static", feature = "dynamic"))]
compile_error!("enable exactly one of the `static` and `dynamic` features");
#[cfg(not(any(feature = "static", feature = "dynamic")))]
compile_error!("enable one of the `static` or `dynamic` features");

// The runtime under one name, whichever way it is linked.
#[cfg(feature = "static")]
use aliasmux_core as runtime;
#[cfg(feature = "dynamic")]
use aliasmux_shared as runtime;

use runtime::slot::{self, Slot};

/// The template slot that `aliasmux patch` overwrites in each copy.
///
/// It must be a `static` in *this* crate, so that it lands in the stub's own
/// file (see the note in `slot.rs`). `#[used]` keeps it in the binary even if
/// the optimizer could see through every read.
#[used]
static SLOT: Slot = slot::EMPTY;

/// Read the patched alias from [`SLOT`] and run it.
fn main() {
    // Constraint on change: read the slot through `black_box`. To the compiler
    // SLOT is an immutable static whose contents it knows (the empty
    // placeholder), so without this it may constant-fold `len == 0` and
    // compile the stub into "always unpatched". `black_box` makes the address
    // opaque, forcing real loads of whatever bytes the file holds at run time.
    let current: &Slot = std::hint::black_box(&SLOT);
    match slot::decode(current) {
        // The rest of argv (minus our own name) goes to the alias.
        Ok((name, template)) => runtime::run_alias(name, template, &std::env::args_os().skip(1).collect::<Vec<_>>()),
        // Running the raw stub, or a damaged copy: explain, using the shell's
        // "not executable" status since there is nothing to run.
        Err(err) => {
            eprintln!("aliasmux-stub: {err}");
            std::process::exit(126);
        }
    }
}
