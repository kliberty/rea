//! aliasmux: compiled command aliases without a shell.
//!
//! One binary holds every alias, busybox style: each alias is a symlink to it,
//! and the name it was invoked under (argv[0]) selects the template. Invoked
//! under its own name, it is a small management tool instead (see `cli.rs`).

mod aliasfile;
mod cli;
mod run;
mod template;

use std::ffi::OsString;
use std::path::Path;

/// Alias definitions, embedded at build time. build.rs validated every entry
/// and copied the file into OUT_DIR, so each template here is known to parse.
pub const ALIASES: &str = include_str!(concat!(env!("OUT_DIR"), "/aliases.txt"));

/// The name under which this binary acts as its own management tool. Assumes
/// the binary is named after the package, which is Cargo's default for
/// `src/main.rs`.
pub const SELF_NAME: &str = env!("CARGO_PKG_NAME");

/// Exit status for usage errors (wrong argument count, bad template). Follows
/// the shell convention of 2 for misuse.
const USAGE_ERROR: i32 = 2;

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

/// Parse, expand and run one template.
///
/// :param name: alias name, used as the prefix of every error message
/// :param source: the template source
/// :param args: runtime arguments, excluding the alias name
/// :return: never; exits with the alias's outcome
pub fn run_alias(name: &str, source: &str, args: &[OsString]) -> ! {
    // Embedded templates were validated by build.rs, so this only fails for
    // ad-hoc templates from `aliasmux eval`.
    let template = template::parse(source).unwrap_or_else(|err| {
        eprintln!("{name}: invalid template: {}", err.render(source));
        std::process::exit(USAGE_ERROR);
    });
    // Wrong argument counts get the template as a usage line, since the
    // template is the most precise description of what the alias wants.
    let plan = template.expand(args).unwrap_or_else(|err| {
        eprintln!("{name}: {err}\n    {name} = {source}");
        std::process::exit(USAGE_ERROR);
    });

    // One command: become it, with no supervisor process left over.
    if let Some(argv) = plan.as_simple() {
        run::exec_single(name, argv);
    }
    // Several processes: supervise them, then exit as the result dictates.
    run::shield_from_terminal_signals();
    run::exit_with(run::run_plan(name, &plan));
}
