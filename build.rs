//! Build script: validate the alias file, then embed it.
//!
//! Every template is parsed here with the same parser the binary uses, so a
//! typo in an alias fails the build (with a caret under the problem) instead of
//! failing the first time you run that alias.

// The build script only needs the parsing half of these modules; the expansion
// code would otherwise trigger dead-code warnings.
#[allow(dead_code)]
#[path = "src/template.rs"]
mod template;

#[allow(dead_code)]
#[path = "src/aliasfile.rs"]
mod aliasfile;

use std::env;
use std::fs;
use std::path::PathBuf;
use std::process::exit;

/// Validate the alias file and copy it into OUT_DIR for `include_str!`.
fn main() {
    // The definitions can live elsewhere (a dotfiles repo, say): set
    // ALIASMUX_ALIASES to the file. A relative path is taken from the crate
    // root; `join` with an absolute path simply yields that path.
    println!("cargo::rerun-if-env-changed=ALIASMUX_ALIASES");
    let root = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("cargo sets CARGO_MANIFEST_DIR"));
    let path = env::var_os("ALIASMUX_ALIASES").map_or_else(|| root.join("aliases.txt"), |p| root.join(p));
    println!("cargo::rerun-if-changed={}", path.display());
    // Cargo doesn't know about `#[path]` modules, so declare them as inputs too.
    println!("cargo::rerun-if-changed=src/template.rs");
    println!("cargo::rerun-if-changed=src/aliasfile.rs");

    // A missing file is a setup error, not something to default around.
    let source = fs::read_to_string(&path).unwrap_or_else(|err| {
        eprintln!("error: cannot read alias file {}: {err}", path.display());
        exit(1);
    });

    // Gather every problem before failing, so one build reports them all.
    let mut problems = Vec::new();
    let own_name = env::var("CARGO_PKG_NAME").expect("cargo sets CARGO_PKG_NAME");
    match aliasfile::parse(&source) {
        Err(errors) => {
            for err in errors {
                problems.push(format!("{}:{}: {}", path.display(), err.line, err.message));
            }
        }
        Ok(entries) => {
            for entry in entries {
                // An alias with our own name would be unreachable: that name
                // always means management mode.
                if entry.name == own_name {
                    problems.push(format!("{}:{}: `{own_name}` is reserved for the tool itself", path.display(), entry.line));
                }
                if let Err(err) = template::parse(entry.template) {
                    problems.push(format!("{}:{}: alias `{}`: {}", path.display(), entry.line, entry.name, err.render(entry.template)));
                }
            }
        }
    }
    // Cargo shows a failed build script's stderr, so plain eprintln suffices.
    if !problems.is_empty() {
        for problem in &problems {
            eprintln!("error: {problem}");
        }
        exit(1);
    }

    // main.rs embeds this copy, which is why it must be the validated one.
    let out = PathBuf::from(env::var_os("OUT_DIR").expect("cargo sets OUT_DIR")).join("aliases.txt");
    fs::write(&out, source).unwrap_or_else(|err| {
        eprintln!("error: cannot write {}: {err}", out.display());
        exit(1);
    });
}
