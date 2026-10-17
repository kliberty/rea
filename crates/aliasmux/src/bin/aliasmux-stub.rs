//! A generic alias executable with an empty template slot.
//!
//! `aliasmux new` and `aliasmux patch` copy this binary once per alias and
//! write the alias into [`SLOT`]. Each copy is then a standalone executable:
//! its name on disk doesn't matter, and nothing else is needed at run time.

use aliasmux_core::slot::{self, Slot};

/// The template slot that `aliasmux patch` overwrites in each copy.
///
/// It must be a `static` in this binary, not in aliasmux-core (see the note
/// in `slot.rs`). `#[used]` keeps it in the binary even if
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
        Ok((name, template)) => aliasmux_core::run_alias(
            name,
            template,
            &std::env::args_os().skip(1).collect::<Vec<_>>(),
        ),
        // Running the raw stub, or a damaged copy: explain, using the shell's
        // "not executable" status since there is nothing to run.
        Err(err) => {
            eprintln!("aliasmux-stub: {err}");
            std::process::exit(126);
        }
    }
}
