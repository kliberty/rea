//! Build script: pick one alias by `ALIASMUX_NAME` and bake it into the binary.
//!
//! Each build of this crate produces the executable for one alias, so making
//! all of them means one build per alias (scripts/build_variants.py loops).

use std::env;
use std::fs;

use aliasmux_core::{aliasfile, build_support};

/// Validate the alias file, then write `NAME` and `TEMPLATE` constants for
/// `main.rs` to include.
fn main() {
    // The whole file is validated, not just the chosen alias, so this crate
    // and the multicall binary agree on what is a valid file.
    let file = build_support::load_alias_file();
    println!("cargo::rerun-if-env-changed=ALIASMUX_NAME");
    let name = env::var("ALIASMUX_NAME").unwrap_or_default();

    // Without a name (e.g. `cargo build --workspace`), still build, but to a
    // binary that explains itself, so workspace-wide commands keep working.
    let template = if name.is_empty() {
        println!("cargo::warning=ALIASMUX_NAME is not set; the aliasmux-single binary will only print an error");
        String::new()
    } else {
        match aliasfile::find(&file.source, &name) {
            Some(template) => template.to_owned(),
            None => build_support::fail(&[format!("ALIASMUX_NAME={name}: no such alias in {}", file.path.display())]),
        }
    };

    // `{:?}` on a str yields a valid Rust string literal, escapes included.
    let code = format!("pub const NAME: &str = {name:?};\npub const TEMPLATE: &str = {template:?};\n");
    let out = build_support::out_dir().join("alias.rs");
    if let Err(err) = fs::write(&out, code) {
        build_support::fail(&[format!("cannot write {}: {err}", out.display())]);
    }
}
