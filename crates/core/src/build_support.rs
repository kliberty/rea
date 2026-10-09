//! Helpers for the build scripts of the multicall and single-alias crates.
//!
//! Both need the same thing: find the alias file, tell Cargo when to rerun,
//! and fail the build with readable errors if any definition is invalid.

use std::env;
use std::fs;
use std::path::PathBuf;
use std::process::exit;

use crate::{aliasfile, template};

/// The validated alias file, ready for a build script to embed or search.
pub struct AliasFile {
    /// Where it was read from, for messages.
    pub path: PathBuf,
    /// Its full contents.
    pub source: String,
}

/// Locate, read and validate the alias file, exiting the build script on any
/// problem.
///
/// The file is `aliases.txt` at the workspace root unless `ALIASMUX_ALIASES`
/// names another (a relative path is taken from the workspace root).
///
/// Assumption: the calling crate lives at `<workspace>/crates/<name>`, so the
/// workspace root is two levels above its manifest. Cargo exposes no stable
/// variable for the workspace root to build scripts.
///
/// :return: the validated file
pub fn load_alias_file() -> AliasFile {
    // Rerun when the variable changes, not just when the default file does.
    println!("cargo::rerun-if-env-changed=ALIASMUX_ALIASES");
    let manifest = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("cargo sets CARGO_MANIFEST_DIR"));
    let root = manifest.join("../..");
    // `join` with an absolute path yields that path, so both forms work.
    let path = env::var_os("ALIASMUX_ALIASES").map_or_else(|| root.join("aliases.txt"), |p| root.join(p));
    println!("cargo::rerun-if-changed={}", path.display());

    // A missing file is a setup error, not something to default around.
    let source = fs::read_to_string(&path).unwrap_or_else(|err| fail(&[format!("cannot read alias file {}: {err}", path.display())]));

    // Gather every problem before failing, so one build reports them all.
    let mut problems = Vec::new();
    match aliasfile::parse(&source) {
        Err(errors) => {
            for err in errors {
                problems.push(format!("{}:{}: {}", path.display(), err.line, err.message));
            }
        }
        Ok(entries) => {
            for entry in entries {
                // `aliasmux` always means management mode, so an alias with
                // that name could never run.
                if entry.name == "aliasmux" {
                    problems.push(format!("{}:{}: `aliasmux` is reserved for the tool itself", path.display(), entry.line));
                }
                if let Err(err) = template::parse(entry.template) {
                    problems.push(format!("{}:{}: alias `{}`: {}", path.display(), entry.line, entry.name, err.render(entry.template)));
                }
            }
        }
    }
    if !problems.is_empty() {
        fail(&problems);
    }
    AliasFile { path, source }
}

/// Print build errors and stop the build script.
///
/// :param problems: one message per problem
/// :return: never
pub fn fail(problems: &[String]) -> ! {
    // Cargo shows a failed build script's stderr, so plain eprintln suffices.
    for problem in problems {
        eprintln!("error: {problem}");
    }
    exit(1);
}

/// The build script's output directory.
///
/// :return: `OUT_DIR`
pub fn out_dir() -> PathBuf {
    PathBuf::from(env::var_os("OUT_DIR").expect("cargo sets OUT_DIR"))
}
