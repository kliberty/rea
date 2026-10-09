//! Shared entry point for the generated per-alias binaries in `src/bin`.
//!
//! Each generated `src/bin/<alias>.rs` is a one-line `main` that calls
//! [`run`] with its alias baked in as string constants. Keeping the real code
//! here means the generated files stay trivial, and a change to them never
//! requires recompiling this library (only that one binary).

/// Run an alias with this process's command-line arguments.
///
/// :param name: alias name, for error messages
/// :param template: template source; validated when the file was generated
/// :return: never; execs the target or exits with the alias's outcome
pub fn run(name: &str, template: &str) -> ! {
    // Everything after our own argv[0] belongs to the alias.
    aliasmux_core::run_alias(name, template, &std::env::args_os().skip(1).collect::<Vec<_>>())
}
