//! aliasmux: compiled command aliases without a shell.
//!
//! As a multicall binary, it holds every alias: each alias is a symlink to it,
//! and the name it was invoked under (argv[0]) selects the template. Invoked
//! under its own name, it is the management tool instead (see `cli.rs`),
//! which also produces the other variants (patched per-alias stubs).

mod cli;

use std::path::Path;

use aliasmux_core::{aliasfile, run_alias};

/// Alias definitions, embedded at build time. build.rs validated every entry
/// and copied the file into OUT_DIR, so each template here is known to parse.
pub const ALIASES: &str = include_str!(concat!(env!("OUT_DIR"), "/aliases.txt"));

/// The name under which this binary acts as its own management tool. Assumes
/// the binary is named after the package, which is Cargo's default.
pub const SELF_NAME: &str = env!("CARGO_PKG_NAME");

/// Dispatch on argv[0]: run an alias, or the management CLI.
fn main() {
    let mut args = std::env::args_os();
    // argv[0] can be absent (execve with an empty argv); treat that as our own
    // name, which leads to the management CLI's usage message.
    let argv0 = args.next().unwrap_or_else(|| SELF_NAME.into());
    // Only the final path component matters: `~/bin/flif270` is `flif270`.
    let invoked = Path::new(&argv0).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();

    // Our own name: management mode. The arguments go through clap there; in
    // alias mode they must not, since they belong to the target program.
    if invoked == SELF_NAME {
        cli::main();
    }
    // Any other name must be an alias. A miss usually means the symlink
    // outlived its definition, so say how to clean that up.
    match aliasfile::find(ALIASES, &invoked) {
        Some(template) => run_alias(&invoked, template, &args.collect::<Vec<_>>()),
        None => {
            eprintln!("{SELF_NAME}: no alias named `{invoked}` (a stale symlink? `{SELF_NAME} install --prune DIR` removes those)");
            std::process::exit(127);
        }
    }
}
