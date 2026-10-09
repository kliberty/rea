//! The aliasmux runtime as one shared library.
//!
//! This crate only re-exports `aliasmux-core`. Building it as a `dylib` puts
//! the whole runtime into `libaliasmux_shared-<hash>.so`, so dynamically linked
//! stubs contain little more than their template slot.
//!
//! It is a separate crate, rather than `aliasmux-core` itself gaining a
//! `dylib` crate type, so that the static builds never compile a dylib and
//! keep LTO (which rustc refuses to combine with dylib outputs here).

pub use aliasmux_core::*;
