//! Runtime shared by every aliasmux variant: the template language, the alias
//! file format, process execution, and the slot format for patched stubs.
//!
//! The variants differ only in how a template reaches [`run_alias`]:
//!
//! * multicall: one binary embeds the whole alias file and picks by argv[0]
//! * patched stub: a prebuilt binary with a template written into its data
//! * shared library: a patched C stub that calls this crate in a `.so`
//! * single: one binary compiled per alias with its template as a constant

pub mod aliasfile;
pub mod build_support;
pub mod run;
pub mod slot;
pub mod template;

use std::ffi::OsString;

/// Exit status for usage errors (wrong argument count, bad template). Follows
/// the shell convention of 2 for misuse.
pub const USAGE_ERROR: i32 = 2;

/// Parse, expand and run one template.
///
/// :param name: alias name, used as the prefix of every error message
/// :param source: the template source
/// :param args: runtime arguments, excluding the alias name
/// :return: never; exits with the alias's outcome
pub fn run_alias(name: &str, source: &str, args: &[OsString]) -> ! {
    // Templates baked in at build time were validated then, so this only fails
    // for ad-hoc templates (`aliasmux eval`) or a corrupted patched stub.
    let parsed = template::parse(source).unwrap_or_else(|err| {
        eprintln!("{name}: invalid template: {}", err.render(source));
        std::process::exit(USAGE_ERROR);
    });
    // Wrong argument counts get the template as a usage line, since the
    // template is the most precise description of what the alias wants.
    let plan = parsed.expand(args).unwrap_or_else(|err| {
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
