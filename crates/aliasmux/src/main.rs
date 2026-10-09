//! aliasmux: command aliases with numbered arguments and real pipelines, no
//! shell.
//!
//! This is the tool that creates and inspects aliases. Each alias it creates
//! is a copy of `aliasmux-stub` (src/bin/aliasmux-stub.rs) with the alias
//! written into it, so the aliases themselves never run this binary.

mod cli;

/// Hand over to the command-line interface.
fn main() {
    cli::main();
}
