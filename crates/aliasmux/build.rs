//! Build script: validate the alias file, then embed it.
//!
//! Every template is parsed with the same parser the binary uses, so a typo in
//! an alias fails the build (with a caret under the problem) instead of failing
//! the first time you run that alias.

use std::fs;

use aliasmux_core::build_support;

/// Validate the alias file and copy it into OUT_DIR for `include_str!`.
fn main() {
    let file = build_support::load_alias_file();
    // main.rs embeds this copy, which is why it must be the validated one.
    let out = build_support::out_dir().join("aliases.txt");
    if let Err(err) = fs::write(&out, &file.source) {
        build_support::fail(&[format!("cannot write {}: {err}", out.display())]);
    }
}
