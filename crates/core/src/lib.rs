//! Runtime for rea aliases: the template language, the alias file format,
//! process execution, and the slot format that holds an alias in its stub.
//!
//! Each alias is a copy of the `rea-stub` executable with one alias
//! written into its slot; the stub reads it back and calls [`run_alias`].

pub mod aliasfile;
pub mod run;
pub mod slot;
pub mod template;

use std::ffi::OsString;

/// Exit status for usage errors (wrong argument count, bad template). Follows
/// the shell convention of 2 for misuse.
pub const USAGE_ERROR: i32 = 2;

/// Parse, expand and run one template.
///
/// `name` is the alias name, used as the prefix of every error message; `args`
/// excludes it. Never returns: exits with the alias's outcome.
pub fn run_alias(name: &str, source: &str, args: &[OsString]) -> ! {
    run_parsed(name, source, args, template::parse(source));
}

/// Run the template stored in an alias, which `rea new` or `patch`
/// already validated.
///
/// Like [`run_alias`], but parses with [`template::parse_for_run`], which
/// skips re-proving that some argument count works. Never returns.
pub fn run_stored_alias(name: &str, source: &str, args: &[OsString]) -> ! {
    run_parsed(name, source, args, template::parse_for_run(source));
}

/// Report a parse failure, or expand and run the template.
///
/// Never returns: exits with the alias's outcome.
fn run_parsed(
    name: &str,
    source: &str,
    args: &[OsString],
    parsed: Result<template::Template, template::ParseError>,
) -> ! {
    // Templates are validated when an alias is created, so this only fails
    // for ad-hoc templates (`rea eval`) or a damaged stub.
    let parsed = parsed.unwrap_or_else(|err| {
        eprintln!("{name}: invalid template: {}", err.render(source));
        std::process::exit(USAGE_ERROR);
    });
    // Wrong argument counts get the template as a usage line, since the
    // template is the most precise description of what the alias wants.
    let plan = parsed.expand(name, args).unwrap_or_else(|err| {
        eprintln!("{name}: {err}\n    {name} = {source}");
        std::process::exit(USAGE_ERROR);
    });

    // One command: become it, with no supervisor process left over.
    if let Some(argv) = plan.as_simple() {
        run::exec_single(name, argv);
    }
    // Several processes: supervise them, then exit as the result dictates.
    run::ensure_standard_fds();
    run::ignore_sigpipe();
    run::restore_default_sigchld();
    run::shield_from_terminal_signals();
    run::exit_with(run::run_plan(name, &plan));
}
