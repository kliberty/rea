//! The aliasmux command line.

use std::ffi::{OsStr, OsString};
use std::fs;
use std::path::{Path, PathBuf};

use clap::{Parser, Subcommand};

use aliasmux_core::{run_alias, slot, template};

/// Prefix for messages, and the program name in `--help`.
const SELF_NAME: &str = env!("CARGO_PKG_NAME");

/// Command aliases with numbered arguments and real pipelines, no shell.
#[derive(Parser)]
#[command(name = SELF_NAME, version)]
struct Cli {
    #[command(subcommand)]
    command: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Run an ad-hoc template, to try one out before adding it to the file.
    Eval {
        /// Template source (quote it for your shell).
        template: String,
        /// Arguments for the template, passed through untouched.
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<OsString>,
    },
    /// Print the commands a template would run, without running them.
    Explain {
        /// Template source (quote it for your shell).
        template: String,
        /// Arguments to expand with.
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<OsString>,
    },
    /// Print the alias definition stored in a patched executable.
    Inspect {
        /// A file produced by `patch`.
        file: PathBuf,
    },
}

/// Entry point for management mode.
///
/// :return: never; exits with the command's status
pub fn main() -> ! {
    // `eval` replaces the process or exits itself; the rest
    // return a status here.
    let status = match Cli::parse().command {
        Cmd::Eval { template, args } => run_alias(SELF_NAME, &template, &args),
        Cmd::Explain { template, args } => explain(&template, &args),
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
/// :param source: template source
/// :param args: arguments to expand with
/// :return: exit status
fn explain(source: &str, args: &[OsString]) -> i32 {
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
        // A marker with garbage after it is a false positive (the tool itself
        // carries the marker bytes as a constant), not a damaged alias.
        Err(slot::SlotError::Corrupt(why)) => fail(&format!("{}: not a patched aliasmux executable (marker found, but {why})", file.display())),
        Err(err) => fail(&format!("{}: {err}", file.display())),
    }
}
