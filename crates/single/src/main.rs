//! One alias, compiled into its own executable.
//!
//! The template is a compile-time constant chosen by `ALIASMUX_NAME` (see
//! build.rs). This is the closest match to the original alias_maker.py: one
//! generated program per alias, needing the compiler each time.

// Defines `NAME` and `TEMPLATE`.
include!(concat!(env!("OUT_DIR"), "/alias.rs"));

/// Run the baked-in alias.
fn main() {
    // An empty name means the crate was built without choosing an alias.
    if NAME.is_empty() {
        eprintln!("aliasmux-single: built without ALIASMUX_NAME; rebuild with ALIASMUX_NAME=<alias>");
        std::process::exit(126);
    }
    // Everything after our own argv[0] belongs to the alias.
    aliasmux_core::run_alias(NAME, TEMPLATE, &std::env::args_os().skip(1).collect::<Vec<_>>());
}
