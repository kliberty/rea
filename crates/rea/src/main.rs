//! rea: command aliases with numbered arguments and real pipelines, no
//! shell.
//!
//! This is the tool that creates and inspects aliases. Each alias it creates
//! is a copy of `rea-stub` (src/bin/rea-stub.rs) with the alias
//! written into it, so the aliases themselves never run this binary.

mod cli;
mod replace;

/// Hand over to the command-line interface.
fn main() {
    // Before any output, so `rea list | head` ends quietly (see there).
    rea_core::run::restore_default_sigpipe();
    cli::main();
}
