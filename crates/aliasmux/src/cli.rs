//! Management commands, used when the binary is invoked under its own name.
//!
//! clap is only ever reached in this mode. Alias invocations never touch it,
//! both because their arguments belong to the target program and to keep alias
//! startup minimal.

use std::collections::HashSet;
use std::ffi::{OsStr, OsString};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use clap::{Parser, Subcommand};

use aliasmux_core::{aliasfile, run_alias, slot, template};

use crate::{ALIASES, SELF_NAME};

/// Compiled command aliases: numbered arguments and real pipelines, no shell.
#[derive(Parser)]
#[command(name = SELF_NAME, version)]
struct Cli {
    #[command(subcommand)]
    command: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// List every alias and its template.
    List,
    /// Show one alias's template and how many arguments it takes.
    Show {
        /// Alias name.
        name: String,
    },
    /// Run an alias by name, without needing its symlink.
    Run {
        /// Alias name.
        name: String,
        /// Arguments for the alias, passed through untouched (options included).
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<OsString>,
    },
    /// Run an ad-hoc template, to try one out before adding it to the file.
    Eval {
        /// Template source (quote it for your shell).
        template: String,
        /// Arguments for the template, passed through untouched.
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<OsString>,
    },
    /// Print the commands an alias would run, without running them.
    Explain {
        /// Treat TARGET as template source rather than an alias name.
        #[arg(short = 't', long = "template")]
        is_template: bool,
        /// Alias name (or template source, with -t).
        target: String,
        /// Arguments to expand with.
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<OsString>,
    },
    /// Write one `main` source file per alias, for the aliasmux-single crate.
    ///
    /// Files are rewritten only when their content changes, and generated
    /// files for removed aliases are deleted, so Cargo rebuilds exactly the
    /// aliases that changed. Prints `BIN<tab>ALIAS` per alias on stdout.
    GenSingle {
        /// Directory for the sources, normally crates/single/src/bin.
        dir: PathBuf,
    },
    /// Make one standalone executable per alias by patching a stub binary.
    Patch {
        /// The stub to copy (aliasmux-stub, static or dynamic build).
        #[arg(long)]
        stub: PathBuf,
        /// Directory for the generated executables.
        #[arg(long)]
        out: PathBuf,
        /// Aliases to generate; all of them if none are given.
        names: Vec<String>,
    },
    /// Print the alias definition stored in a patched executable.
    Inspect {
        /// A file produced by `patch`.
        file: PathBuf,
    },
    /// Create a symlink in DIR for every alias, pointing at this binary.
    Install {
        /// Directory to put the links in, normally one on your PATH.
        dir: PathBuf,
        /// Replace existing files that have an alias's name.
        #[arg(long)]
        force: bool,
        /// Also remove links in DIR that point at this binary but whose alias
        /// no longer exists.
        #[arg(long)]
        prune: bool,
    },
}

/// Entry point for management mode.
///
/// :return: never; exits with the command's status
pub fn main() -> ! {
    // `run` and `eval` replace the process or exit themselves; the rest
    // return a status here.
    let status = match Cli::parse().command {
        Cmd::List => list(),
        Cmd::Show { name } => show(&name),
        Cmd::Run { name, args } => match aliasfile::find(ALIASES, &name) {
            Some(source) => run_alias(&name, source, &args),
            None => fail(&format!("no alias named `{name}`")),
        },
        Cmd::Eval { template, args } => run_alias(SELF_NAME, &template, &args),
        Cmd::Explain { is_template, target, args } => explain(is_template, &target, &args),
        Cmd::Install { dir, force, prune } => install(&dir, force, prune),
        Cmd::Patch { stub, out, names } => patch(&stub, &out, &names),
        Cmd::GenSingle { dir } => gen_single(&dir),
        Cmd::Inspect { file } => inspect(&file),
    };
    std::process::exit(status);
}

/// Print an error and return the failure status.
///
/// :param message: what went wrong
/// :return: exit status 1
fn fail(message: &str) -> i32 {
    eprintln!("{SELF_NAME}: {message}");
    1
}

/// All embedded entries.
///
/// :return: the entries; validated at build time, so parsing cannot fail here
fn entries() -> Vec<aliasfile::Entry<'static>> {
    // An Err here would mean build.rs let a bad file through, i.e. a bug.
    aliasfile::parse(ALIASES).expect("embedded alias file was validated by build.rs")
}

/// `list`: names and templates, aligned.
///
/// :return: exit status
fn list() -> i32 {
    let entries = entries();
    // Pad names to a common width so the templates line up.
    let width = entries.iter().map(|e| e.name.len()).max().unwrap_or(0);
    for entry in &entries {
        println!("{:width$}  {}", entry.name, entry.template);
    }
    0
}

/// `show`: template plus arity.
///
/// :param name: alias name
/// :return: exit status
fn show(name: &str) -> i32 {
    let Some(source) = aliasfile::find(ALIASES, name) else {
        return fail(&format!("no alias named `{name}`"));
    };
    // Validated at build time; see `entries`.
    let parsed = template::parse(source).expect("embedded templates were validated by build.rs");
    println!("{name} = {source}");
    println!("takes {}", parsed.arity());
    // The implicit rule is invisible in the source, so call it out.
    if parsed.implicit_splat {
        println!("(no argument references, so arguments are appended to the command)");
    }
    0
}

/// Quote a word so the printed command can be pasted into sh (or a template).
///
/// :param word: one argv entry
/// :return: the word, single-quoted unless it is plainly safe
fn quote(word: &OsStr) -> String {
    let text = word.to_string_lossy();
    // Arbitrary conservative set of characters that need no quoting anywhere.
    let safe = !text.is_empty() && text.chars().all(|c| c.is_ascii_alphanumeric() || "-_./=:,+@%".contains(c));
    if safe {
        text.into_owned()
    } else {
        // Inside single quotes only `'` itself is special; `'\''` ends the
        // quote, adds an escaped quote, and reopens. Works in sh and templates.
        format!("'{}'", text.replace('\'', r"'\''"))
    }
}

/// Format one argv as a pasteable line.
///
/// :param argv: the argv
/// :return: space-joined quoted words
fn format_argv(argv: &template::Argv) -> String {
    argv.iter().map(|w| quote(w)).collect::<Vec<_>>().join(" ")
}

/// `explain`: print the expanded plan, one process per line.
///
/// :param is_template: whether `target` is template source rather than a name
/// :param target: alias name or template source
/// :param args: arguments to expand with
/// :return: exit status
fn explain(is_template: bool, target: &str, args: &[OsString]) -> i32 {
    // Resolve the template source from whichever form was given.
    let source = if is_template {
        target
    } else {
        match aliasfile::find(ALIASES, target) {
            Some(source) => source,
            None => return fail(&format!("no alias named `{target}`")),
        }
    };
    // Report errors exactly as a real run would, but without exiting the
    // process from inside a helper.
    let parsed = match template::parse(source) {
        Ok(parsed) => parsed,
        Err(err) => return fail(&format!("invalid template: {}", err.render(source))),
    };
    let plan = match parsed.expand(args) {
        Ok(plan) => plan,
        Err(err) => return fail(&err.to_string()),
    };
    // Stages are indented under their pipeline so the structure reads at a glance.
    let print_pipeline = |prefix: &str, stages: &[template::Argv]| {
        for (index, argv) in stages.iter().enumerate() {
            let lead = if index == 0 { prefix } else { "  |" };
            println!("{lead} {}", format_argv(argv));
        }
    };
    // The head pipeline has no operator; align it with the ones that do.
    print_pipeline("  ", &plan.head);
    for (op, stages) in &plan.tail {
        let prefix = match op {
            template::Op::And => "&&",
            template::Op::Or => "||",
            template::Op::Then => " ;",
        };
        print_pipeline(prefix, stages);
    }
    // Make the "single command means exec" behavior visible.
    if plan.as_simple().is_some() {
        println!("(single command: runs via exec, no supervisor process)");
    }
    0
}

/// Whether `link` is a symlink that resolves to `target`.
///
/// :param link: path to check
/// :param target: canonical path of this binary
/// :return: true if `link` is a symlink to `target`
fn links_to(link: &Path, target: &Path) -> bool {
    // Require a symlink, so hard links or copies are never treated as ours.
    let is_symlink = fs::symlink_metadata(link).is_ok_and(|m| m.file_type().is_symlink());
    // Canonicalize resolves the whole chain, so relative links work too.
    is_symlink && fs::canonicalize(link).is_ok_and(|resolved| resolved == target)
}

/// `install`: create (and optionally prune) alias symlinks in `dir`.
///
/// :param dir: directory for the links
/// :param force: replace existing non-matching files
/// :param prune: remove our links whose alias no longer exists
/// :return: exit status; 1 if anything was skipped or failed
fn install(dir: &Path, force: bool, prune: bool) -> i32 {
    // Links point at the canonical binary path, so they keep working when
    // reached through a different symlinked route to the same file.
    let exe = match std::env::current_exe().and_then(fs::canonicalize) {
        Ok(exe) => exe,
        Err(err) => return fail(&format!("cannot locate this binary: {err}")),
    };
    let names: HashSet<&str> = entries().iter().map(|e| e.name).collect();
    let mut failures = 0;

    // Sort for stable, readable output; the set's order is arbitrary.
    let mut sorted: Vec<&str> = names.iter().copied().collect();
    sorted.sort_unstable();
    for name in sorted {
        let link = dir.join(name);
        // Decide what to do about whatever is already at that path.
        match fs::symlink_metadata(&link) {
            // Already ours: nothing to do, and not worth printing.
            Ok(_) if links_to(&link, &exe) => continue,
            // Something else, explicitly allowed to be replaced.
            Ok(_) if force => {
                if let Err(err) = fs::remove_file(&link) {
                    failures += fail(&format!("cannot replace {}: {err}", link.display()));
                    continue;
                }
            }
            // Something else, not ours to touch without --force.
            Ok(_) => {
                failures += fail(&format!("{} exists and is not a link to {SELF_NAME}; use --force to replace it", link.display()));
                continue;
            }
            // Free path: the normal case.
            Err(err) if err.kind() == io::ErrorKind::NotFound => {}
            Err(err) => {
                failures += fail(&format!("cannot check {}: {err}", link.display()));
                continue;
            }
        }
        // Create the link and say so, so the user can see what changed.
        match std::os::unix::fs::symlink(&exe, &link) {
            Ok(()) => println!("linked {}", link.display()),
            Err(err) => failures += fail(&format!("cannot create {}: {err}", link.display())),
        }
    }

    // Pruning only ever removes symlinks to this exact binary, never our own
    // name, and never anything still defined.
    if prune {
        let listing = match fs::read_dir(dir) {
            Ok(listing) => listing,
            Err(err) => return fail(&format!("cannot list {}: {err}", dir.display())),
        };
        for entry in listing.flatten() {
            let path = entry.path();
            let name = entry.file_name();
            let name = name.to_string_lossy();
            // Skip live aliases, our own name, and anything not linked to us.
            if names.contains(name.as_ref()) || name == SELF_NAME || !links_to(&path, &exe) {
                continue;
            }
            match fs::remove_file(&path) {
                Ok(()) => println!("removed stale {}", path.display()),
                Err(err) => failures += fail(&format!("cannot remove {}: {err}", path.display())),
            }
        }
    }
    // Any failure makes the whole command fail, so scripts notice.
    if failures > 0 { 1 } else { 0 }
}

/// `patch`: write one patched copy of the stub per alias.
///
/// :param stub: path to the stub binary
/// :param out: output directory (created if missing)
/// :param names: aliases to generate, or empty for all
/// :return: exit status; 1 if any alias failed
fn patch(stub: &Path, out: &Path, names: &[String]) -> i32 {
    // Read the stub once and locate its slot; every output is a copy of it.
    let image = match fs::read(stub) {
        Ok(image) => image,
        Err(err) => return fail(&format!("cannot read {}: {err}", stub.display())),
    };
    let offset = match slot::locate(&image) {
        Ok(offset) => offset,
        Err(err) => return fail(&format!("{}: {err}", stub.display())),
    };
    // A marker alone is not proof of a slot: any binary containing the
    // patching code (aliasmux itself) has the marker bytes as a constant, and
    // overwriting there would corrupt the program. A real slot is empty (a
    // fresh stub) or holds a valid alias (re-patching an existing alias).
    if let Err(slot::SlotError::Corrupt(why)) = slot::decode_at(&image, offset) {
        return fail(&format!("{}: not an aliasmux stub (marker found, but {why})", stub.display()));
    }
    if let Err(err) = fs::create_dir_all(out) {
        return fail(&format!("cannot create {}: {err}", out.display()));
    }

    // No names means every alias; otherwise only the named ones, in order.
    let all = entries();
    let selected: Vec<(&str, &str)> = if names.is_empty() {
        all.iter().map(|e| (e.name, e.template)).collect()
    } else {
        let mut selected = Vec::new();
        for name in names {
            match aliasfile::find(ALIASES, name) {
                Some(template) => selected.push((name.as_str(), template)),
                None => return fail(&format!("no alias named `{name}`")),
            }
        }
        selected
    };

    let mut failures = 0;
    for (name, template) in selected {
        // Encoding fails only on size; the template itself was validated at
        // build time, like every embedded one.
        let bytes = match slot::encode(name, template) {
            Ok(bytes) => bytes,
            Err(err) => {
                failures += fail(&err);
                continue;
            }
        };
        let mut copy = image.clone();
        copy[offset..offset + slot::SIZE].copy_from_slice(&bytes);
        // Write to a temporary name, then rename: replacing a running
        // executable in place fails with ETXTBSY, and a half-written file
        // must never be left under the alias's name.
        let target = out.join(name);
        let temp = out.join(format!(".{name}.tmp"));
        let written = write_executable(&temp, &copy).and_then(|()| fs::rename(&temp, &target));
        match written {
            Ok(()) => println!("patched {} ({} bytes)", target.display(), copy.len()),
            Err(err) => {
                let _ = fs::remove_file(&temp);
                failures += fail(&format!("cannot write {}: {err}", target.display()));
            }
        }
    }
    if failures > 0 { 1 } else { 0 }
}

/// Write bytes to a new file with mode 0755.
///
/// :param path: destination; replaced if it exists
/// :param bytes: contents
/// :return: I/O result
fn write_executable(path: &Path, bytes: &[u8]) -> io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    // Set the mode at creation so the file is never briefly non-executable.
    let mut file = fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o755).open(path)?;
    file.write_all(bytes)
}

/// `inspect`: show the definition stored in a patched executable.
///
/// :param file: the executable
/// :return: exit status
fn inspect(file: &Path) -> i32 {
    let image = match fs::read(file) {
        Ok(image) => image,
        Err(err) => return fail(&format!("cannot read {}: {err}", file.display())),
    };
    // Same lookup the stub does at run time, but on the file instead of memory.
    let offset = match slot::locate(&image) {
        Ok(offset) => offset,
        Err(err) => return fail(&format!("{}: {err}", file.display())),
    };
    match slot::decode_at(&image, offset) {
        Ok((name, template)) => {
            println!("{name} = {template}");
            0
        }
        // A marker with garbage after it is a false positive (see `patch`),
        // not a damaged alias, so say that rather than "corrupt".
        Err(slot::SlotError::Corrupt(why)) => fail(&format!("{}: not a patched aliasmux executable (marker found, but {why})", file.display())),
        Err(err) => fail(&format!("{}: {err}", file.display())),
    }
}

/// First line of every file `gen-single` writes. Pruning deletes only files
/// that start with it, so hand-written files in the directory are never touched.
const GENERATED_MARKER: &str = "// @generated by `aliasmux gen-single`";

/// The Cargo bin target name for an alias.
///
/// Cargo derives a crate name from each bin name, and crate names can't
/// contain `.` or `+`, which alias names may. Those two are spelled out; every
/// other allowed alias character is already valid. The build script copies
/// each binary back to its alias name, so this name is only seen by Cargo.
///
/// :param alias: alias name (already validated by the alias file parser)
/// :return: a valid bin target name
fn bin_name(alias: &str) -> String {
    alias.replace('.', "_dot_").replace('+', "_plus_")
}

/// The generated `main` for one alias.
///
/// :param alias: alias name
/// :param template: template source (validated at build time)
/// :return: the Rust source text
fn single_source(alias: &str, template: &str) -> String {
    // `{:?}` on a str yields a valid Rust string literal, escapes included.
    // The template also appears in a comment, which is safe because templates
    // are single lines (the alias file format guarantees it).
    format!(
        "{GENERATED_MARKER} from the alias file. Do not edit:\n\
         // change the alias and rerun it (scripts/build_variants.py does).\n\
         //\n\
         // {alias} = {template}\n\
         \n\
         /// Run the `{alias}` alias.\n\
         fn main() {{\n    \
             aliasmux_single::run({alias:?}, {template:?});\n\
         }}\n"
    )
}

/// `gen-single`: write per-alias sources, only where they changed.
///
/// :param dir: output directory (created if missing)
/// :return: exit status
fn gen_single(dir: &Path) -> i32 {
    if let Err(err) = fs::create_dir_all(dir) {
        return fail(&format!("cannot create {}: {err}", dir.display()));
    }

    // Map every alias to its bin name first, so a collision (say `a.b` and an
    // alias literally named `a_dot_b`) is reported before anything is written.
    let entries = entries();
    let mut owners: std::collections::HashMap<String, &str> = std::collections::HashMap::new();
    for entry in &entries {
        if let Some(other) = owners.insert(bin_name(entry.name), entry.name) {
            return fail(&format!("aliases `{other}` and `{}` both map to the Cargo bin name `{}`", entry.name, bin_name(entry.name)));
        }
    }

    let mut failures = 0;
    for entry in &entries {
        let bin = bin_name(entry.name);
        let path = dir.join(format!("{bin}.rs"));
        let source = single_source(entry.name, entry.template);
        // Unchanged files keep their mtime, which is what lets Cargo skip
        // recompiling (and relinking) every alias that didn't change.
        if fs::read_to_string(&path).is_ok_and(|old| old == source) {
            println!("{bin}\t{}", entry.name);
            continue;
        }
        match fs::write(&path, source) {
            Ok(()) => {
                eprintln!("wrote {}", path.display());
                println!("{bin}\t{}", entry.name);
            }
            Err(err) => failures += fail(&format!("cannot write {}: {err}", path.display())),
        }
    }

    // Delete generated sources whose alias is gone, so its binary stops being
    // built. Anything without the marker is someone's own file: leave it.
    let listing = match fs::read_dir(dir) {
        Ok(listing) => listing,
        Err(err) => return fail(&format!("cannot list {}: {err}", dir.display())),
    };
    for file in listing.flatten() {
        let path = file.path();
        let Some(stem) = path.file_stem().and_then(OsStr::to_str) else { continue };
        let generated = fs::read_to_string(&path).is_ok_and(|text| text.starts_with(GENERATED_MARKER));
        if path.extension() == Some(OsStr::new("rs")) && generated && !owners.contains_key(stem) {
            match fs::remove_file(&path) {
                Ok(()) => eprintln!("removed stale {}", path.display()),
                Err(err) => failures += fail(&format!("cannot remove {}: {err}", path.display())),
            }
        }
    }
    if failures > 0 { 1 } else { 0 }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bin_names_avoid_characters_cargo_rejects() {
        // Plain names are unchanged; `.` and `+` are spelled out.
        assert_eq!(bin_name("flif270"), "flif270");
        assert_eq!(bin_name("my-alias_2"), "my-alias_2");
        assert_eq!(bin_name("g++.v2"), "g_plus__plus__dot_v2");
    }

    #[test]
    fn generated_source_embeds_name_and_template_as_literals() {
        // Quotes and backslashes in a template must come out as valid Rust.
        let source = single_source("q", r#"echo "a\b" {1}"#);
        assert!(source.starts_with(GENERATED_MARKER));
        assert!(source.contains(r#"aliasmux_single::run("q", "echo \"a\\b\" {1}");"#), "{source}");
    }
}
