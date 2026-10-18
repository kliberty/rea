//! Parser and expander for alias templates.
//!
//! A template is a tiny shell-*like* command language. It supports only what an
//! alias needs, so that no shell is ever involved at run time:
//!
//! * words, split on unquoted whitespace
//! * `'single'` quotes (fully literal) and `"double"` quotes (literal except for
//!   argument references and the escapes `\"`, `\\`, `\{`, `\}`, `\$`, `` \` ``)
//! * `\x` outside quotes, which makes `x` literal
//! * argument references (see below)
//! * operators: `|` (pipe), `&&`, `||` and `;`, with shell precedence (pipes
//!   bind tighter; the other three are left-associative and equal)
//!
//! Argument references:
//!
//! * `{N}` and `{-N}`: one argument, counted from the front (from 1) or from
//!   the back (`{-1}` is the last)
//! * `{@}`: every argument nothing else uses (no `{N}`, `{-N}` or slice),
//!   zero or more;
//!   `{@+}` is the same but requires at least one
//! * `{A..B}`: a slice, from argument A up to but not including argument B,
//!   each end counted like `{N}` / `{-N}` and either one optional: `{2..}` is
//!   the 2nd onward, `{..-1}` all but the last. Slices may overlap `{N}`, `{-N}`
//!   and each other, `{@}` leaves out what they pass, and they are never empty.
//! * `{0}`: the alias name
//! * `{$NAME}`: environment variable `NAME`, read when the alias runs. Like a
//!   one-argument reference it is always exactly one argv entry, never split
//!   into words, and it takes formats. Unset or empty is an error before
//!   anything runs. It is not an argument, so it doesn't affect the count.
//!
//! Any reference except `{0}` may end in `:` and a format: a file check
//! (`F` file, `D` directory, `E` exists, `N` doesn't exist), a path modifier
//! (`/` basename, `//` parent, `.` without extension, `/.` basename without
//! extension), or a check then a modifier: `{1:F/.}`. Modifiers only edit the
//! text, like fd's `{/}` and friends; checks consult the file system, for
//! every argument, during expansion (before anything runs).
//!
//! A check with no reference, `{:F}` / `{:D}` / `{:E}` / `{:N}`, at the very
//! end of a word checks the whole word instead: `-outfile {1}.jpg{:N}`. When
//! the word repeats (`{@}.jpg{:N}`), each copy is checked.
//!
//! A one-argument reference always yields exactly one argv entry. A word that
//! contains a several-argument reference (`{@}`, `{@+}`, a slice) is repeated
//! once per argument: `-I{@}` gives `-Ia -Ib`, and no entry at all when empty.
//!
//! Characters that a shell treats specially but this language does not (`$`
//! outside `{$NAME}`, backticks, globs, redirections, parentheses, a lone `&`) are rejected unless
//! escaped or quoted. `$` and backticks are rejected inside double quotes too,
//! since sh expands them there. Accepting them silently would be worse: a
//! template written as `ffmpeg -i "$1"` would hand a literal `$1` to ffmpeg.
//!
//! `aliasmux new` and `patch` run this parser when an alias is created, so
//! syntax errors (and templates that no argument count could satisfy) surface
//! then, not when the alias first runs.

use std::collections::HashMap;
use std::ffi::{OsStr, OsString};
use std::fmt;
use std::fs;
use std::io;
use std::iter::Peekable;
use std::path::Path;
use std::str::CharIndices;

/// A one-argument position, as written in `{N}` or `{-N}`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Index {
    /// `{N}`: the Nth argument from the front, N >= 1.
    Front(usize),
    /// `{-N}`: the Nth argument from the back, N >= 1 (`Back(1)` is the last).
    Back(usize),
}

impl Index {
    /// Resolve to a 1-based argument position, or `None` if there are too few
    /// arguments.
    fn position(self, count: usize) -> Option<usize> {
        match self {
            Index::Front(k) => (k <= count).then_some(k),
            Index::Back(k) => (k <= count).then(|| count + 1 - k),
        }
    }

    /// The number as written, ignoring direction (N for both `{N}` and `{-N}`);
    /// sizes the arity search.
    fn magnitude(self) -> usize {
        match self {
            Index::Front(k) | Index::Back(k) => k,
        }
    }

    /// The position as it is written inside braces, e.g. `-2`.
    fn as_written(self) -> String {
        match self {
            Index::Front(k) => k.to_string(),
            Index::Back(k) => format!("-{k}"),
        }
    }
}

impl fmt::Display for Index {
    /// Show the reference as written, e.g. `{-2}`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{{{}}}", self.as_written())
    }
}

/// A reference that stands for several arguments.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Multi {
    /// `{@}` or `{@+}`: the arguments no `{N}`, `{-N}` or slice uses.
    Rest {
        /// True for `{@+}`.
        at_least_one: bool,
    },
    /// `{A..B}`: from A up to but not including B. `None` means from the
    /// first argument / through the last one. The parser stores a `{1..`
    /// start as `None` too, so equal slices compare equal however written.
    /// (No end is equivalent to `None`: `{..-1}` stops before the last.)
    Slice(Option<Index>, Option<Index>),
}

impl Multi {
    /// The set of arguments this refers to, ignoring `{@}` vs `{@+}` (which
    /// differ only in what counts are allowed, not in what they contain).
    ///
    /// References to the same arguments give equal values.
    fn set(self) -> Multi {
        match self {
            Multi::Rest { .. } => Multi::Rest {
                at_least_one: false,
            },
            slice => slice,
        }
    }
}

impl fmt::Display for Multi {
    /// Show the reference as written, e.g. `{2..-2}` or `{@+}`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Multi::Rest {
                at_least_one: false,
            } => f.write_str("{@}"),
            Multi::Rest { at_least_one: true } => f.write_str("{@+}"),
            Multi::Slice(start, end) => {
                let start = start.map(Index::as_written).unwrap_or_default();
                let end = end.map(Index::as_written).unwrap_or_default();
                // `{..}` is not valid syntax, so the all-arguments slice shows as `{1..}`.
                if start.is_empty() && end.is_empty() {
                    f.write_str("{1..}")
                } else {
                    write!(f, "{{{start}..{end}}}")
                }
            }
        }
    }
}

/// A file-system check on an argument, written as a capital letter after the
/// colon: `{1:F}`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Check {
    /// `F`: exists and isn't a directory (symlinks followed). Looser than
    /// `test -f` on purpose: see `Check::run`.
    File,
    /// `D`: an existing directory (symlinks followed, like `test -d`).
    Dir,
    /// `E`: exists, either kind (like `test -e`).
    Exists,
    /// `N`: nothing exists there yet, not even a dangling symlink.
    Absent,
}

impl Check {
    /// The letter this check is written as.
    fn letter(self) -> char {
        match self {
            Check::File => 'F',
            Check::Dir => 'D',
            Check::Exists => 'E',
            Check::Absent => 'N',
        }
    }

    /// What the check requires, for error messages, e.g. "an existing file".
    fn wants(self) -> &'static str {
        match self {
            Check::File => "an existing file",
            Check::Dir => "an existing directory",
            Check::Exists => "an existing path",
            Check::Absent => "a path that doesn't exist yet",
        }
    }

    /// Run the check on one argument, as a path.
    ///
    /// On failure, returns what was found instead, e.g. "is a directory".
    fn run(self, value: &OsStr) -> Result<(), String> {
        let path = Path::new(value);
        // `metadata` follows symlinks, as `test -f` does; `N` looks at the
        // link itself instead (see below).
        let stat = if self == Check::Absent {
            fs::symlink_metadata(path)
        } else {
            fs::metadata(path)
        };
        match (self, stat) {
            // Deviation from `test -f`: anything but a directory passes, so
            // /dev/null, /dev/stdin, FIFOs and bash's `<(...)` (/dev/fd/63, a
            // pipe) work. `F` is there to catch a missing path or a directory
            // passed by mistake; a command that truly needs a regular file
            // reports that itself. A stricter letter could be added later
            // without changing this one.
            (Check::File, Ok(m)) if !m.is_dir() => Ok(()),
            (Check::Dir, Ok(m)) if m.is_dir() => Ok(()),
            (Check::Exists, Ok(_)) => Ok(()),
            // Deviation from `test ! -e`: a dangling symlink counts as
            // existing. `N` is for outputs, and writing through a dangling
            // link would create a file somewhere else entirely.
            (Check::Absent, Ok(_)) => Err("already exists".into()),
            (Check::Absent, Err(err)) if err.kind() == io::ErrorKind::NotFound => Ok(()),
            // Only a directory is left for F.
            (Check::File, Ok(_)) => Err("is a directory".into()),
            (Check::Dir, Ok(m)) if m.is_file() => Err("is a file".into()),
            (Check::Dir, Ok(_)) => Err("is not a directory".into()),
            (_, Err(err)) if err.kind() == io::ErrorKind::NotFound => Err("does not exist".into()),
            // Permission problems and the like: say so rather than guess.
            (_, Err(err)) => Err(format!("can't be checked ({err})")),
        }
    }
}

/// A textual edit of an argument as a path, written after the colon: `{1:/.}`.
/// Purely textual (no file-system access), like fd's placeholders.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Modifier {
    /// `/`: the last component, `dir/photo.jpg` -> `photo.jpg`.
    Basename,
    /// `//`: everything before it, `dir/photo.jpg` -> `dir` (`.` for a bare name).
    Parent,
    /// `.`: without the last extension, `dir/photo.jpg` -> `dir/photo`.
    NoExtension,
    /// `/.`: basename without the last extension, `dir/photo.jpg` -> `photo`.
    Stem,
}

impl Modifier {
    /// How the modifier is written, e.g. `/.`.
    fn as_written(self) -> &'static str {
        match self {
            Modifier::Basename => "/",
            Modifier::Parent => "//",
            Modifier::NoExtension => ".",
            Modifier::Stem => "/.",
        }
    }

    /// Apply the modifier.
    ///
    /// Uses `Path`'s own component logic, so the edge cases follow it (and
    /// match fd): `a.tar.gz` loses only `.gz`, `.bashrc` has no extension, and
    /// a trailing slash is ignored. Where `Path` has no answer (the basename
    /// of `/`), the value is left as it is rather than turned into nothing.
    /// It works on `OsStr` throughout, so non-UTF-8 survives.
    fn apply(self, value: &OsStr) -> OsString {
        let path = Path::new(value);
        match self {
            Modifier::Basename => path.file_name().unwrap_or(value).to_os_string(),
            // A bare name's parent is the current directory, written `.`.
            Modifier::Parent => match path.parent() {
                Some(parent) if parent.as_os_str().is_empty() => OsString::from("."),
                Some(parent) => parent.as_os_str().to_os_string(),
                None => value.to_os_string(),
            },
            Modifier::NoExtension if path.extension().is_some() => {
                path.with_extension("").into_os_string()
            }
            Modifier::NoExtension => value.to_os_string(),
            Modifier::Stem => path.file_stem().unwrap_or(value).to_os_string(),
        }
    }
}

/// The optional `:...` part of a reference: a check, a modifier, or both.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Format {
    /// Checked against the argument itself, before the modifier.
    pub check: Option<Check>,
    /// Applied to the argument's text.
    pub modifier: Option<Modifier>,
}

impl Format {
    /// The format as written, including the colon, e.g. `:F/.`; empty if there
    /// is none.
    fn suffix(self) -> String {
        if self == Format::default() {
            return String::new();
        }
        let check = self
            .check
            .map(Check::letter)
            .map(String::from)
            .unwrap_or_default();
        format!(":{check}{}", self.modifier.map_or("", Modifier::as_written))
    }

    /// Check and edit one argument, returning the text to substitute.
    ///
    /// A failed check is recorded in `findings` rather than returned, and the text
    /// is produced anyway, so `explain` can show the command and every failure
    /// together. `reference` gives the reference as written, for the message.
    fn render(
        self,
        value: &OsStr,
        reference: impl Fn() -> String,
        findings: &mut Findings,
    ) -> OsString {
        if let Some(check) = self.check
            && let Err(found) = check.run(value)
        {
            findings.record(ExpandError::CheckFailed {
                reference: reference(),
                wants: check.wants(),
                value: value.to_os_string(),
                found,
            });
        }
        self.modifier
            .map_or_else(|| value.to_os_string(), |m| m.apply(value))
    }
}

/// A reference as written, e.g. `{1:F}` or `{2..:/.}`, for messages.
///
/// `plain` is the reference without a format, braces included.
fn written(plain: &str, format: Format) -> String {
    // `plain` always ends in `}`; the format goes just before it.
    format!(
        "{}{}}}",
        plain.strip_suffix('}').unwrap_or(plain),
        format.suffix()
    )
}

/// One fragment of a word.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Piece {
    /// Literal text from the template.
    Lit(String),
    /// One argument, with an optional check and modifier.
    Arg(Index, Format),
    /// `{0}`: the alias name.
    Name,
    /// `{$NAME}`: an environment variable, read when the alias runs, with an
    /// optional check and modifier. Always exactly one piece of text: the
    /// value is never split into words.
    Env(String, Format),
    /// Several arguments, each with the same optional check and modifier; the
    /// word is repeated once per argument.
    Multi(Multi, Format),
}

/// A word of a command after parsing.
///
/// Without a [`Piece::Multi`] it becomes exactly one argv entry (an empty piece
/// list is a deliberate empty argument, written `''` or `""`). With one, it
/// becomes one entry per argument the reference stands for, possibly none.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Word {
    /// The pieces, concatenated in order.
    pub pieces: Vec<Piece>,
    /// `{:F}` etc. at the end of the word: checks each finished copy.
    pub check: Option<Check>,
    /// The word as written, for error messages.
    pub source: String,
}

/// One program invocation; the first word is the program.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Command {
    /// The words, in order. Never empty after parsing.
    pub words: Vec<Word>,
}

/// Commands joined by `|`. A single-stage pipeline is just one command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pipeline {
    /// The stages, left to right. Never empty after parsing.
    pub stages: Vec<Command>,
}

/// How a pipeline is chained to the one before it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Op {
    /// `&&`: run only if the previous result succeeded.
    And,
    /// `||`: run only if the previous result failed.
    Or,
    /// `;`: run unconditionally.
    Then,
}

/// The distinct references a template uses, which is all the arity rules and
/// the expander need to know about it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct Shape {
    /// Every distinct `{N}` / `{-N}`.
    indexes: Vec<Index>,
    /// `Some` if `{@}` or `{@+}` is used; true if `{@+}` is, anywhere.
    rest: Option<bool>,
    /// Every distinct slice, as (start, end).
    slices: Vec<(Option<Index>, Option<Index>)>,
}

impl Shape {
    /// Record a reference.
    fn add(&mut self, r: &Ref) {
        match *r {
            // Formats don't change which arguments are used, so they are
            // ignored here: arity is the same with or without them.
            Ref::Arg(index, _) if !self.indexes.contains(&index) => self.indexes.push(index),
            // `{@}` and `{@+}` are the same arguments; one `+` anywhere makes
            // at least one required, which is the safe reading.
            Ref::Multi(Multi::Rest { at_least_one }, _) => {
                self.rest = Some(self.rest.unwrap_or(false) || at_least_one)
            }
            Ref::Multi(Multi::Slice(start, end), _) if !self.slices.contains(&(start, end)) => {
                self.slices.push((start, end))
            }
            _ => {}
        }
    }

    /// The largest N in any `{N}`, `{-N}` or slice end; 0 if there are none.
    fn max_magnitude(&self) -> usize {
        let ends = self
            .slices
            .iter()
            .flat_map(|(start, end)| [*start, *end])
            .flatten();
        self.indexes
            .iter()
            .copied()
            .chain(ends)
            .map(Index::magnitude)
            .max()
            .unwrap_or(0)
    }
}

/// How many runtime arguments a template accepts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Arity {
    /// Accepted counts below `from`, ascending.
    counts: Vec<usize>,
    /// If set, every count from this one upward is accepted.
    from: Option<usize>,
}

impl Arity {
    /// Whether the template can run with `count` arguments.
    pub fn accepts(&self, count: usize) -> bool {
        self.counts.contains(&count) || self.from.is_some_and(|from| count >= from)
    }
}

impl fmt::Display for Arity {
    /// Describe the arity in words, e.g. "exactly 1 argument" or "2 or 4
    /// arguments".
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // The common shapes get their natural phrasing.
        match (self.counts.as_slice(), self.from) {
            ([], Some(0)) => return f.write_str("any number of arguments"),
            ([], Some(1)) => return f.write_str("at least 1 argument"),
            ([], Some(from)) => return write!(f, "at least {from} arguments"),
            ([1], None) => return f.write_str("exactly 1 argument"),
            ([only], None) => return write!(f, "exactly {only} arguments"),
            _ => {}
        }
        // Anything else: runs of consecutive counts, e.g. "2 to 3 or at least 5".
        let mut parts: Vec<String> = Vec::new();
        let mut counts = self.counts.iter().copied().peekable();
        while let Some(start) = counts.next() {
            let mut end = start;
            while let Some(next) = counts.next_if(|&c| c == end + 1) {
                end = next;
            }
            parts.push(if start == end {
                start.to_string()
            } else {
                format!("{start} to {end}")
            });
        }
        if let Some(from) = self.from {
            parts.push(format!("at least {from}"));
        }
        // Join as "a, b or c".
        let (last, init) = parts.split_last().expect("an arity accepts some count");
        let list = if init.is_empty() {
            last.clone()
        } else {
            format!("{} or {last}", init.join(", "))
        };
        write!(f, "{list} arguments")
    }
}

/// A fully parsed template.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Template {
    /// The first pipeline.
    pub head: Pipeline,
    /// Every following pipeline with the operator that precedes it.
    pub tail: Vec<(Op, Pipeline)>,
    /// True when the parser appended `{@}` itself (see [`parse`]).
    pub implicit_rest: bool,
    /// The references used, for arity checks and expansion.
    shape: Shape,
    /// Computed once at parse time; parsing fails if no count is accepted.
    arity: Arity,
}

/// A syntax error, located by byte offset into the template source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseError {
    /// Byte offset of the offending character (or the source length at end of
    /// input). Always on a char boundary, since it comes from `char_indices`.
    /// `None` for problems with the template as a whole, such as an arity that
    /// no argument count satisfies.
    pub offset: Option<usize>,
    /// Human-readable explanation, ideally with a hint at the fix.
    pub message: String,
}

impl ParseError {
    /// An error at a byte offset into the source.
    fn new(offset: usize, message: impl Into<String>) -> Self {
        ParseError {
            offset: Some(offset),
            message: message.into(),
        }
    }

    /// An error about the template as a whole.
    fn whole(message: impl Into<String>) -> Self {
        ParseError {
            offset: None,
            message: message.into(),
        }
    }

    /// Render the message with the source line and a caret under the error, as
    /// a multi-line string for stderr.
    ///
    /// `src` must be the template source the offset refers to.
    pub fn render(&self, src: &str) -> String {
        match self.offset {
            Some(offset) => {
                // Count chars, not bytes, so the caret lines up under multibyte
                // text. Assumption: the terminal shows one column per char,
                // which is wrong for wide CJK glyphs; acceptable for a caret.
                let column = src[..offset].chars().count();
                // Indent both lines equally so the caret sits under the source text.
                format!("{}\n    {}\n    {}^", self.message, src, " ".repeat(column))
            }
            // No single place to point at: show the template without a caret.
            None => format!("{}\n    {}", self.message, src),
        }
    }
}

impl fmt::Display for ParseError {
    /// Show the message alone, with no source context.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

/// A reference as written inside braces.
///
/// Not `Copy`, because of the variable name in [`Ref::Env`].
#[derive(Debug, Clone, PartialEq, Eq)]
enum Ref {
    /// `{N}` or `{-N}`, possibly with a format
    Arg(Index, Format),
    /// `{0}`
    Name,
    /// `{$NAME}`: an environment variable, possibly with a format
    Env(String, Format),
    /// `{@}`, `{@+}` or a slice, possibly with a format
    Multi(Multi, Format),
    /// `{:F}` etc.: a check on the whole word it ends
    WordCheck(Check),
}

/// What the lexer produces: a word or an operator.
#[derive(Debug)]
enum TokenKind {
    Word(Word),
    Pipe,
    And,
    Or,
    Then,
}

/// A token plus where it started, for error messages.
#[derive(Debug)]
struct Token {
    kind: TokenKind,
    offset: usize,
}

/// Accumulates the pieces of one word while lexing.
struct WordBuilder {
    /// Byte offset where the word began.
    start: usize,
    /// Pieces completed so far.
    pieces: Vec<Piece>,
    /// Literal text not yet flushed into `pieces`.
    literal: String,
    /// The several-argument reference in this word, if any (as its set).
    multi: Option<Multi>,
    /// A word check, with the offset of its `{`.
    check: Option<(Check, usize)>,
    /// Whether anything came after the word check (which is an error).
    after_check: bool,
}

impl WordBuilder {
    /// Start an empty word whose first character is at byte offset `start`.
    fn new(start: usize) -> Self {
        WordBuilder {
            start,
            pieces: Vec::new(),
            literal: String::new(),
            multi: None,
            check: None,
            after_check: false,
        }
    }

    /// Append one literal character.
    fn push_char(&mut self, c: char) {
        // Text after a word check is reported when the word ends.
        self.after_check |= self.check.is_some();
        self.literal.push(c);
    }

    /// Append an argument reference whose opening brace is at `offset`.
    ///
    /// Fails if the word would mix two several-argument references, or if the word
    /// already has a word check.
    fn push_ref(&mut self, r: Ref, offset: usize) -> Result<(), ParseError> {
        // A word check must be last (see `finish`), so nothing may follow it,
        // not even another check.
        if let Some((check, at)) = self.check {
            return Err(ParseError::new(
                at,
                format!(
                    "`{{:{}}}` must come at the very end of its word: it checks the whole word",
                    check.letter()
                ),
            ));
        }
        if let Ref::WordCheck(check) = r {
            self.flush();
            self.check = Some((check, offset));
            return Ok(());
        }
        // A word is repeated once per argument of its several-argument
        // reference. Repeating the same one twice is well defined ({@}={@}
        // gives a=a b=b), but two different ones would need a cross product or
        // a zip of unequal lengths, and neither is an obvious reading.
        // Formats don't matter here: `{@}` and `{@:/.}` are the same arguments,
        // so `mv {@} {@://}/old-{@:/}` repeats in step.
        if let Ref::Multi(multi, _) = &r {
            let multi = *multi;
            if let Some(existing) = self.multi.filter(|m| *m != multi.set()) {
                return Err(ParseError::new(
                    offset,
                    format!(
                        "a word can repeat one several-argument reference (like -I{{@}} or {{@}}={{@}}), but not combine two different ones ({existing} and {multi})"
                    ),
                ));
            }
            self.multi = Some(multi.set());
        }
        // Close off pending literal text so ordering is preserved.
        self.flush();
        self.pieces.push(match r {
            Ref::Arg(index, format) => Piece::Arg(index, format),
            Ref::Name => Piece::Name,
            Ref::Env(name, format) => Piece::Env(name, format),
            Ref::Multi(multi, format) => Piece::Multi(multi, format),
            // Returned early above.
            Ref::WordCheck(_) => unreachable!("word checks are not pieces"),
        });
        Ok(())
    }

    /// Move pending literal text into the piece list.
    fn flush(&mut self) {
        // Skip empty literals so `{1}{2}` doesn't gain a useless empty piece.
        if !self.literal.is_empty() {
            self.pieces
                .push(Piece::Lit(std::mem::take(&mut self.literal)));
        }
    }

    /// Finish the word; `source` is the word as written.
    fn finish(mut self, source: &str) -> Result<Word, ParseError> {
        self.flush();
        if let Some((check, at)) = self.check {
            // Constraint on change: only at the end. Elsewhere it reads as
            // belonging to a neighbouring reference (`{1}{:N}.jpg`), and a
            // reference's own check is always about the argument, never the
            // word (`{1:F}` means the same everywhere it appears).
            if self.after_check {
                return Err(ParseError::new(
                    at,
                    format!(
                        "`{{:{}}}` must come at the very end of its word: it checks the whole word",
                        check.letter()
                    ),
                ));
            }
            if self.pieces.is_empty() {
                return Err(ParseError::new(
                    at,
                    "a word check needs a word to check; put it at the end of one, like out.txt{:N}",
                ));
            }
        }
        Ok(Word {
            pieces: self.pieces,
            check: self.check.map(|(check, _)| check),
            source: source.to_owned(),
        })
    }
}

/// Parse one position: `N` or `-N`, N >= 1.
///
/// `body` is the whole reference body, for error messages.
fn parse_index(text: &str, body: &str) -> Result<Index, String> {
    // Only plain decimal digits after an optional `-`, so `{+1}` or `{1x}` fail loudly.
    let (back, digits) = match text.strip_prefix('-') {
        Some(digits) => (true, digits),
        None => (false, text),
    };
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return Err(format!(
            "unknown reference `{{{body}}}`; expected {{N}}, {{-N}}, {{A..B}}, {{@}}, {{@+}} or {{0}}"
        ));
    }
    // Overflow is the only way the parse can fail after the digit check.
    let n: usize = digits
        .parse()
        .map_err(|_| format!("the number in `{{{body}}}` is too large"))?;
    // 0 is only meaningful alone, as `{0}` (handled by the caller).
    if n == 0 {
        return Err(format!(
            "`{{{body}}}`: positions count from 1 at the front and from -1 at the back; 0 is not a position"
        ));
    }
    Ok(if back {
        Index::Back(n)
    } else {
        Index::Front(n)
    })
}

/// Interpret the text between `{` and `}`.
fn parse_ref_body(body: &str) -> Result<Ref, String> {
    // A format, if any, follows the first colon: `{1:F/.}`.
    let Some((target, spec)) = body.split_once(':') else {
        return parse_target(body);
    };
    // No reference before the colon: a check on the whole word.
    if target.is_empty() {
        return parse_word_check(spec, body);
    }
    let format = parse_format(spec, body)?;
    match parse_target(target) {
        Ok(Ref::Arg(index, _)) => Ok(Ref::Arg(index, format)),
        Ok(Ref::Multi(multi, _)) => Ok(Ref::Multi(multi, format)),
        Ok(Ref::Env(name, _)) => Ok(Ref::Env(name, format)),
        // The alias name isn't a path the user supplied, so checking or
        // editing it would only ever be a mistake.
        Ok(Ref::Name) => Err(format!(
            "`{{{body}}}`: {{0}} is the alias name and takes no checks or modifiers"
        )),
        // Unreachable: only parse_word_check makes these. Reported rather than
        // panicking, since this runs on user input.
        Ok(Ref::WordCheck(_)) => Err(format!("internal error parsing `{{{body}}}`")),
        Err(message) => Err(message),
    }
}

/// Parse a word check from the text after the colon: exactly one check
/// letter, no modifier.
///
/// `body` is the whole reference body, for error messages.
fn parse_word_check(spec: &str, body: &str) -> Result<Ref, String> {
    match spec {
        "F" => Ok(Ref::WordCheck(Check::File)),
        "D" => Ok(Ref::WordCheck(Check::Dir)),
        "E" => Ok(Ref::WordCheck(Check::Exists)),
        "N" => Ok(Ref::WordCheck(Check::Absent)),
        // A word check only tests the finished word, so it has nothing to
        // modify; modifiers belong on a reference.
        _ => Err(format!(
            "`{{{body}}}`: a check on a whole word is one of {{:F}}, {{:D}}, {{:E}} or {{:N}}; modifiers need a reference, like {{1:/.}}"
        )),
    }
}

/// Parse a format from the text after the colon: an optional check letter,
/// then an optional modifier.
///
/// `body` is the whole reference body, for error messages.
fn parse_format(spec: &str, body: &str) -> Result<Format, String> {
    let expected = "expected a check (F, D, E or N), a modifier (/, //, . or /.), or a check then a modifier, like :F/.";
    if spec.is_empty() {
        return Err(format!(
            "`{{{body}}}` has nothing after the `:`; {expected}"
        ));
    }
    // At most one check, first. Arbitrary but simple: the four checks are
    // mutually exclusive or redundant (F implies E), so one is always enough.
    let (check, rest) = match spec.chars().next() {
        Some('F') => (Some(Check::File), &spec[1..]),
        Some('D') => (Some(Check::Dir), &spec[1..]),
        Some('E') => (Some(Check::Exists), &spec[1..]),
        Some('N') => (Some(Check::Absent), &spec[1..]),
        _ => (None, spec),
    };
    let modifier = match rest {
        "" => None,
        "/" => Some(Modifier::Basename),
        "//" => Some(Modifier::Parent),
        "." => Some(Modifier::NoExtension),
        "/." => Some(Modifier::Stem),
        _ => {
            return Err(format!(
                "unknown format `:{spec}` in `{{{body}}}`; {expected}"
            ));
        }
    };
    Ok(Format { check, modifier })
}

/// Interpret a reference without its format: the text between `{` and `}`,
/// or before the `:`.
fn parse_target(body: &str) -> Result<Ref, String> {
    match body {
        // `{}` is common in shell idioms (find -exec), so give it a targeted hint.
        "" => Err("`{}` is not an argument reference; quote it ('{}') to pass literal braces, e.g. for find -exec".into()),
        "@" => Ok(Ref::Multi(Multi::Rest { at_least_one: false }, Format::default())),
        "@+" => Ok(Ref::Multi(Multi::Rest { at_least_one: true }, Format::default())),
        "0" => Ok(Ref::Name),
        // An open-ended slice in both directions has two spellings already
        // ({1..} and {@} when nothing else is used); a third adds nothing.
        ".." => Err("`{..}` is not a slice; write `{1..}` for every argument, or `{@}` for those nothing else uses".into()),
        _ if body.starts_with('$') => parse_env_name(&body[1..], body),
        _ => match body.split_once("..") {
            Some((start, end)) => parse_slice(start, end, body),
            None => parse_index(body, body).map(|index| Ref::Arg(index, Format::default())),
        },
    }
}

/// Parse the name in `{$NAME}`, the text after the `$`.
///
/// Names are restricted to what sh accepts (letters, digits and `_`, not
/// starting with a digit). Other names can exist in the environment, but a
/// template naming one is far more likely a typo, such as `{$1}` for `{1}`.
///
/// `body` is the whole reference body, for error messages.
fn parse_env_name(name: &str, body: &str) -> Result<Ref, String> {
    let mut chars = name.chars();
    let valid = match chars.next() {
        Some(first) => {
            (first.is_ascii_alphabetic() || first == '_')
                && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
        }
        None => false,
    };
    if !valid {
        return Err(format!(
            "`{{{body}}}`: an environment variable name is letters, digits and `_`, not starting with a digit"
        ));
    }
    Ok(Ref::Env(name.to_owned(), Format::default()))
}

/// Parse a slice's two ends, the text before and after `..` (either may be
/// empty).
///
/// `body` is the whole reference body, for error messages.
fn parse_slice(start: &str, end: &str, body: &str) -> Result<Ref, String> {
    let start = (!start.is_empty())
        .then(|| parse_index(start, body))
        .transpose()?;
    let end = (!end.is_empty())
        .then(|| parse_index(end, body))
        .transpose()?;
    // Store a `{1..` start as None, so `{1..3}` and `{..3}` are equal (which
    // matters when one word repeats the same slice twice).
    let start = start.filter(|&s| s != Index::Front(1));
    // Ends counted the same way can be compared without knowing the count, so
    // a slice that is always empty is reported now, with the end rule spelled
    // out. Constraint on change: ends were inclusive at first, and `{-1}
    // {..-1}` then passed the last argument twice, against everyone's reading
    // of it (and against Python's [:-1]); exclusive ends it is.
    let always_empty = match (start, end) {
        (Some(Index::Front(a)), Some(Index::Front(b))) => a >= b,
        (Some(Index::Back(a)), Some(Index::Back(b))) => a <= b,
        _ => false,
    };
    if always_empty {
        return Err(format!(
            "`{{{body}}}` is always empty; a slice stops before its end, so {{2..4}} is the 2nd and 3rd"
        ));
    }
    Ok(Ref::Multi(Multi::Slice(start, end), Format::default()))
}

/// Read a reference whose opening brace, at byte offset `open`, has just
/// been consumed.
fn read_ref(chars: &mut Peekable<CharIndices<'_>>, open: usize) -> Result<Ref, ParseError> {
    // Collect until the closing brace; references never contain spaces or
    // nested braces, so either one means the brace was never closed.
    let mut body = String::new();
    loop {
        match chars.next() {
            Some((_, '}')) => break,
            Some((_, c)) if c.is_whitespace() || c == '{' => {
                return Err(ParseError::new(
                    open,
                    "unterminated `{`; write `\\{` for a literal brace",
                ));
            }
            Some((_, c)) => body.push(c),
            None => {
                return Err(ParseError::new(
                    open,
                    "unterminated `{`; write `\\{` for a literal brace",
                ));
            }
        }
    }
    // Errors point at the opening brace, which is where the reader looks.
    parse_ref_body(&body).map_err(|message| ParseError::new(open, message))
}

/// Push the word in progress, if any, onto the token list, leaving `word` as
/// `None`.
///
/// `end` is the byte offset just past the word. Fails if the word is malformed.
fn end_word(
    word: &mut Option<WordBuilder>,
    tokens: &mut Vec<Token>,
    src: &str,
    end: usize,
) -> Result<(), ParseError> {
    // Nothing to do between words, e.g. at repeated whitespace.
    if let Some(builder) = word.take() {
        let offset = builder.start;
        tokens.push(Token {
            kind: TokenKind::Word(builder.finish(&src[offset..end])?),
            offset,
        });
    }
    Ok(())
}

/// The lexer's output.
struct Lexed {
    /// Words and operators, in order.
    tokens: Vec<Token>,
    /// Every reference with the offset of its `{`, for whole-template checks.
    refs: Vec<(Ref, usize)>,
}

/// Split template source into words and operators, collecting references in
/// order.
fn lex(src: &str) -> Result<Lexed, ParseError> {
    let mut tokens = Vec::new();
    let mut refs = Vec::new();
    let mut chars = src.char_indices().peekable();
    // `Some` while inside a word. A word starts at its first character, which
    // may be a quote; that is how `''` becomes an (empty) word at all.
    let mut word: Option<WordBuilder> = None;

    while let Some((offset, c)) = chars.next() {
        match c {
            // Unquoted whitespace separates words and nothing else.
            c if c.is_whitespace() => end_word(&mut word, &mut tokens, src, offset)?,

            // `|` or `||`: operators also end the current word, so `a|b` works.
            '|' => {
                end_word(&mut word, &mut tokens, src, offset)?;
                let kind = if chars.next_if(|&(_, n)| n == '|').is_some() {
                    TokenKind::Or
                } else {
                    TokenKind::Pipe
                };
                tokens.push(Token { kind, offset });
            }

            // Only `&&` exists; a lone `&` would mean "background" in a shell.
            '&' => {
                end_word(&mut word, &mut tokens, src, offset)?;
                if chars.next_if(|&(_, n)| n == '&').is_none() {
                    return Err(ParseError::new(
                        offset,
                        "a lone `&` is not supported (no background jobs); did you mean `&&`?",
                    ));
                }
                tokens.push(Token {
                    kind: TokenKind::And,
                    offset,
                });
            }

            // `;` sequences unconditionally.
            ';' => {
                end_word(&mut word, &mut tokens, src, offset)?;
                tokens.push(Token {
                    kind: TokenKind::Then,
                    offset,
                });
            }

            // Single quotes: everything literal up to the next `'`, exactly as in sh.
            '\'' => {
                let builder = word.get_or_insert_with(|| WordBuilder::new(offset));
                loop {
                    match chars.next() {
                        Some((_, '\'')) => break,
                        Some((_, ch)) => builder.push_char(ch),
                        None => return Err(ParseError::new(offset, "unterminated single quote")),
                    }
                }
            }

            // Double quotes: literal, but references expand and a few escapes work.
            '"' => {
                let builder = word.get_or_insert_with(|| WordBuilder::new(offset));
                loop {
                    match chars.next() {
                        Some((_, '"')) => break,
                        // Backslash escapes only the characters that are special
                        // here; elsewhere it stays literal, like sh, so regexes
                        // such as "\d+" survive unchanged.
                        Some((_, '\\')) => match chars.next() {
                            Some((_, ch @ ('"' | '\\' | '{' | '}' | '$' | '`'))) => {
                                builder.push_char(ch)
                            }
                            Some((_, ch)) => {
                                builder.push_char('\\');
                                builder.push_char(ch);
                            }
                            None => {
                                return Err(ParseError::new(offset, "unterminated double quote"));
                            }
                        },
                        // References work inside double quotes, so "{1}.mkv" is fine.
                        Some((open, '{')) => {
                            let r = read_ref(&mut chars, open)?;
                            // Word checks use no argument; leave them out.
                            if !matches!(r, Ref::WordCheck(_)) {
                                refs.push((r.clone(), open));
                            }
                            builder.push_ref(r, open)?;
                        }
                        // `$` and backticks expand inside double quotes in sh, so
                        // "$1" is the shell habit most likely to slip through;
                        // treating it as literal would silently pass `$1` on.
                        Some((at, ch @ ('$' | '`'))) => {
                            return Err(ParseError::new(
                                at,
                                format!(
                                    "`{ch}` is not expanded here, even in double quotes; use `{{1}}`, `{{2}}`, ... for arguments or `{{$NAME}}` for environment variables, or write `\\{ch}` (or single quotes) to pass it literally"
                                ),
                            ));
                        }
                        Some((_, ch)) => builder.push_char(ch),
                        None => return Err(ParseError::new(offset, "unterminated double quote")),
                    }
                }
            }

            // Unquoted backslash makes the next character literal, whatever it is.
            '\\' => match chars.next() {
                Some((_, ch)) => word
                    .get_or_insert_with(|| WordBuilder::new(offset))
                    .push_char(ch),
                None => {
                    return Err(ParseError::new(
                        offset,
                        "trailing backslash escapes nothing",
                    ));
                }
            },

            // An argument reference, possibly glued to text: {1}.rot.flif
            '{' => {
                let r = read_ref(&mut chars, offset)?;
                // Word checks use no argument; leave them out.
                if !matches!(r, Ref::WordCheck(_)) {
                    refs.push((r.clone(), offset));
                }
                word.get_or_insert_with(|| WordBuilder::new(offset))
                    .push_ref(r, offset)?;
            }

            // A stray `}` is almost always a typo like `{1}}`, so refuse it
            // rather than pass it through.
            '}' => {
                return Err(ParseError::new(
                    offset,
                    "unmatched `}`; write `\\}` for a literal brace",
                ));
            }

            // `$` gets its own hint, since it is what shell habits will produce.
            '$' => {
                return Err(ParseError::new(
                    offset,
                    "`$` is not expanded here; use `{1}`, `{2}`, ... for arguments or `{$NAME}` for environment variables, or quote it ('$') to pass it literally",
                ));
            }

            // Other shell syntax that would silently mean something else here.
            '`' | '(' | ')' | '<' | '>' | '*' | '?' => {
                return Err(ParseError::new(
                    offset,
                    format!(
                        "`{c}` has no special meaning in alias templates (no subshells, redirection or globbing); quote or escape it to pass it literally"
                    ),
                ));
            }

            // Anything else is ordinary word text.
            _ => word
                .get_or_insert_with(|| WordBuilder::new(offset))
                .push_char(c),
        }
    }
    // The last word has no trailing separator to end it.
    end_word(&mut word, &mut tokens, src, src.len())?;
    Ok(Lexed { tokens, refs })
}

/// Recursive-descent parser over the token list.
struct Parser {
    tokens: Peekable<std::vec::IntoIter<Token>>,
    /// Source length, used as the error offset at end of input.
    end: usize,
}

impl Parser {
    /// Byte offset of the next token, or of the end of input; where "expected X"
    /// points.
    fn here(&mut self) -> usize {
        self.tokens.peek().map_or(self.end, |t| t.offset)
    }

    /// Parse `command := word+`.
    fn command(&mut self) -> Result<Command, ParseError> {
        // Take words until an operator or end of input.
        let mut words = Vec::new();
        while let Some(token) = self
            .tokens
            .next_if(|t| matches!(t.kind, TokenKind::Word(_)))
        {
            // The guard above makes this the only possible variant.
            if let TokenKind::Word(w) = token.kind {
                words.push(w);
            }
        }
        // `| |`, a leading `&&`, a trailing `|` etc. all land here.
        if words.is_empty() {
            return Err(ParseError::new(self.here(), "expected a command"));
        }
        Ok(Command { words })
    }

    /// Parse `pipeline := command ("|" command)*`.
    fn pipeline(&mut self) -> Result<Pipeline, ParseError> {
        let mut stages = vec![self.command()?];
        // Each `|` must be followed by another command.
        while self
            .tokens
            .next_if(|t| matches!(t.kind, TokenKind::Pipe))
            .is_some()
        {
            stages.push(self.command()?);
        }
        Ok(Pipeline { stages })
    }

    /// Parse `list := pipeline (op pipeline)* ";"?` into the head pipeline and
    /// the operator-tagged rest.
    fn list(&mut self) -> Result<(Pipeline, Vec<(Op, Pipeline)>), ParseError> {
        let head = self.pipeline()?;
        let mut tail = Vec::new();
        // `pipeline` stops only at end of input or at one of these operators,
        // so whatever comes next (if anything) is an operator.
        while let Some(token) = self.tokens.next() {
            let op = match token.kind {
                TokenKind::And => Op::And,
                TokenKind::Or => Op::Or,
                TokenKind::Then => Op::Then,
                // Unreachable given the above; reported rather than panicking
                // because the tool runs this code on user input.
                _ => {
                    return Err(ParseError::new(
                        token.offset,
                        "internal error: unexpected token",
                    ));
                }
            };
            // A trailing `;` is harmless (sh allows it), so accept it.
            if op == Op::Then && self.tokens.peek().is_none() {
                break;
            }
            tail.push((op, self.pipeline()?));
        }
        Ok((head, tail))
    }
}

/// Reject `{N}` / `{-N}` numbering with gaps, in templates without slices.
///
/// `{3}` without `{2}` is almost certainly a typo. A slice may cover the gap
/// instead (`{1} {2..3} {3}`), but whether it does depends on the argument
/// count, so templates with slices are checked per count in `layout`; this
/// static check only exists to point a caret at the problem when it can.
/// The error points at the highest-numbered reference of a gappy run.
fn check_gaps(refs: &[(Ref, usize)]) -> Result<(), ParseError> {
    // Front and back numbering are checked separately, with the same rule.
    for (back, sign) in [(false, ""), (true, "-")] {
        // The numbers used in this direction, and where the highest first appears.
        let used: Vec<(usize, usize)> = refs
            .iter()
            .filter_map(|(r, offset)| match *r {
                Ref::Arg(Index::Front(k), _) if !back => Some((k, *offset)),
                Ref::Arg(Index::Back(k), _) if back => Some((k, *offset)),
                _ => None,
            })
            .collect();
        let Some(&(highest, at)) = used
            .iter()
            .max_by_key(|&&(k, offset)| (k, std::cmp::Reverse(offset)))
        else {
            continue;
        };
        if let Some(missing) = (1..highest).find(|k| !used.iter().any(|&(u, _)| u == *k)) {
            return Err(ParseError::new(
                at,
                format!(
                    "`{{{sign}{highest}}}` is used but `{{{sign}{missing}}}` is not; every argument must be used, so number them without gaps (or cover the gap with a slice)"
                ),
            ));
        }
    }
    Ok(())
}

/// Why two checks can never both pass on the same path, or `None` if they
/// can.
fn incompatible(a: Check, b: Check) -> Option<&'static str> {
    use Check::*;
    match (a, b) {
        (File, Dir) | (Dir, File) => Some("a path can't be both a file and a directory"),
        (Absent, File | Dir | Exists) | (File | Dir | Exists, Absent) => {
            Some("a path can't both exist and not exist")
        }
        _ => None,
    }
}

/// Reject checks on the same reference that can never all pass, such as
/// `{1:F}` and `{1:N}`.
///
/// Only references that always mean the same arguments are compared (the
/// same `{N}`, the same `{-N}`, the same several-argument reference). Others
/// may overlap only at some counts, like `{1}` and `{-1}`; those are left to
/// fail when they run, rather than being refused for all counts. The error
/// points at the second of a contradictory pair.
fn check_contradictions(refs: &[(Ref, usize)]) -> Result<(), ParseError> {
    /// What a checked reference stands for; equal values are always the same
    /// paths.
    #[derive(PartialEq)]
    enum Target<'a> {
        Arg(Index),
        Multi(Multi),
        Env(&'a str),
    }
    // (what, check, as written) for every checked reference so far.
    let mut seen: Vec<(Target, Check, String)> = Vec::new();
    for (r, offset) in refs {
        let (target, check, text) = match r {
            Ref::Arg(
                index,
                format @ Format {
                    check: Some(check), ..
                },
            ) => (
                Target::Arg(*index),
                *check,
                written(&index.to_string(), *format),
            ),
            Ref::Multi(
                multi,
                format @ Format {
                    check: Some(check), ..
                },
            ) => (
                Target::Multi(multi.set()),
                *check,
                written(&multi.to_string(), *format),
            ),
            Ref::Env(
                name,
                format @ Format {
                    check: Some(check), ..
                },
            ) => (
                Target::Env(name),
                *check,
                written(&format!("{{${name}}}"), *format),
            ),
            _ => continue,
        };
        let clash = seen.iter().find_map(|(t, c, first)| {
            (*t == target)
                .then(|| incompatible(*c, check).map(|why| (first, why)))
                .flatten()
        });
        if let Some((first, why)) = clash {
            return Err(ParseError::new(
                *offset,
                format!("`{text}` contradicts `{first}`: {why}"),
            ));
        }
        seen.push((target, check, text));
    }
    Ok(())
}

/// Parse template source.
///
/// If the template is a single command with no references at all (not even
/// `{0}`), a `{@}` is appended, so `ll = ls -la` passes its arguments through
/// like a shell alias. This is the one implicit rule; it only applies where
/// the meaning is unambiguous. (With several commands, there is no obvious one
/// to receive the arguments, so none does and extra arguments are an error.)
///
/// # Errors
///
/// A syntax error, or a template that breaks the argument rules (a gap in the
/// numbering, an unused argument, contradictory checks, or no argument count
/// that works). The error has a byte offset when there is one place to point
/// at.
pub fn parse(src: &str) -> Result<Template, ParseError> {
    // Lex fully first; the parser then never deals with characters.
    let Lexed { tokens, refs } = lex(src)?;
    // An empty or all-whitespace template has no command to run.
    if tokens.is_empty() {
        return Err(ParseError::new(0, "empty template"));
    }
    let mut parser = Parser {
        tokens: tokens.into_iter().peekable(),
        end: src.len(),
    };
    let (mut head, tail) = parser.list()?;

    // Collect the distinct references, applying the implicit-`{@}` rule above.
    let mut shape = Shape::default();
    for (r, _) in &refs {
        shape.add(r);
    }
    // With slices, a gap may be covered; `layout` decides that per count.
    if shape.slices.is_empty() {
        check_gaps(&refs)?;
    }
    check_contradictions(&refs)?;
    let implicit_rest = refs.is_empty() && tail.is_empty() && head.stages.len() == 1;
    if implicit_rest {
        let rest = Multi::Rest {
            at_least_one: false,
        };
        head.stages[0].words.push(Word {
            pieces: vec![Piece::Multi(rest, Format::default())],
            check: None,
            source: "{@}".into(),
        });
        shape.add(&Ref::Multi(rest, Format::default()));
    }
    let arity = compute_arity(&shape)?;
    Ok(Template {
        head,
        tail,
        implicit_rest,
        shape,
        arity,
    })
}

/// Where each reference lands for one argument count.
struct Layout {
    /// Position of each `{N}` / `{-N}`.
    index: HashMap<Index, usize>,
    /// Positions `{@}` stands for, ascending.
    rest: Vec<usize>,
    /// First and last position of each slice.
    slices: HashMap<(Option<Index>, Option<Index>), (usize, usize)>,
}

impl Layout {
    /// The 1-based positions a several-argument reference stands for, in order.
    fn positions(&self, multi: Multi) -> Vec<usize> {
        match multi {
            Multi::Rest { .. } => self.rest.clone(),
            // Present for every slice of the template once the layout is valid.
            Multi::Slice(start, end) => self
                .slices
                .get(&(start, end))
                .map_or_else(Vec::new, |&(lo, hi)| (lo..=hi).collect()),
        }
    }
}

/// Where a slice's end falls: the slice stops just before this position,
/// which is at most `count + 1`.
///
/// `None` as the end means "through the last". Returns `None` if the end names
/// an argument that can't exist at this count.
fn stop_position(end: Option<Index>, count: usize) -> Option<usize> {
    match end {
        // No end: stop after the last argument.
        None => Some(count + 1),
        // `{..B}` may name the position just past the last argument: with 3
        // arguments, `{..4}` is all three, stopping before a 4th that isn't
        // there. One further would be a gap, so it doesn't fit.
        Some(Index::Front(b)) => (b <= count + 1).then_some(b),
        Some(back) => back.position(count),
    }
}

/// A slice's first and last position, both included, or `None` if it is
/// empty. The slice's end is exclusive.
fn slice_range(slice: (Option<Index>, Option<Index>), count: usize) -> Option<(usize, usize)> {
    let lo = slice.0.map_or(Some(1), |i| i.position(count))?;
    let stop = stop_position(slice.1, count)?;
    // lo >= 1 and stop <= count + 1, so a non-empty range is in bounds.
    (lo < stop).then_some((lo, stop - 1))
}

/// Why one argument count doesn't work.
enum Problem {
    /// These positions would be used by nothing. Kept as positions rather
    /// than text, so the whole-template error can describe them without
    /// naming a particular count (see `describe_unused`).
    Unused(Vec<usize>),
    /// A gap in `{N}` / `{-N}` numbering that no slice covers.
    Gap {
        /// The highest-numbered reference in that direction.
        used: Index,
        /// The first number below it that nothing refers to.
        missing: Index,
    },
    /// Anything else, already phrased.
    Other(String),
}

/// Lay out the references for one argument count, checking the rules that
/// don't involve comparing counts: indexes in range and distinct, `{@+}` and
/// slices non-empty, every argument used.
fn layout(shape: &Shape, count: usize) -> Result<Layout, Problem> {
    // Each position may belong to at most one index ({N}/{-N}); {@} takes
    // whatever neither an index nor a slice uses.
    let mut owner: Vec<Option<Index>> = vec![None; count + 1];
    let mut index = HashMap::new();
    for &i in &shape.indexes {
        let position = i.position(count).ok_or_else(|| {
            Problem::Other(format!("{i} needs at least {} arguments", i.magnitude()))
        })?;
        if let Some(other) = owner[position] {
            return Err(Problem::Other(format!(
                "{other} and {i} would both be argument {position}"
            )));
        }
        owner[position] = Some(i);
        index.insert(i, position);
    }
    // Slices must be non-empty; that is what makes {2..} safe for `rm`.
    let mut slices = HashMap::new();
    for &slice in &shape.slices {
        let range = slice_range(slice, count).ok_or_else(|| {
            Problem::Other(format!("{} would be empty", Multi::Slice(slice.0, slice.1)))
        })?;
        slices.insert(slice, range);
    }
    let in_a_slice = |p: usize| {
        slices
            .values()
            .any(|&(lo, hi): &(usize, usize)| lo <= p && p <= hi)
    };
    // {@}: every position nothing else uses, neither an index nor a slice.
    // Constraint on change: {@} used to exclude only indexes, so in
    // `{1} {2..-2} {@}` it repeated what the slice had already passed; "the
    // rest" means what nothing else mentions, and duplicating on purpose is
    // still possible by writing the slice twice.
    let rest: Vec<usize> = if shape.rest.is_some() {
        (1..=count)
            .filter(|&p| owner[p].is_none() && !in_a_slice(p))
            .collect()
    } else {
        Vec::new()
    };
    if shape.rest == Some(true) && rest.is_empty() {
        return Err(Problem::Other("{@+} would be empty".into()));
    }
    // Gaps in {N} / {-N} numbering must be covered by a slice. {@} does not
    // count: it takes whatever nothing else uses, so letting it fill a gap
    // would silently accept the typo `cp {1} {3} {@}` (with {2} going to {@}).
    for back in [false, true] {
        let numbers: Vec<usize> = shape
            .indexes
            .iter()
            .filter_map(|&i| match i {
                Index::Front(k) if !back => Some(k),
                Index::Back(k) if back => Some(k),
                _ => None,
            })
            .collect();
        let Some(&highest) = numbers.iter().max() else {
            continue;
        };
        let make = |k| {
            if back {
                Index::Back(k)
            } else {
                Index::Front(k)
            }
        };
        for missing in (1..highest).filter(|k| !numbers.contains(k)).map(make) {
            // In range: the higher index was placed above, so this one fits.
            let Some(position) = missing.position(count) else {
                continue;
            };
            if !in_a_slice(position) {
                return Err(Problem::Gap {
                    used: make(highest),
                    missing,
                });
            }
        }
    }
    // Without {@}, every argument must be claimed by an index or covered by a
    // slice; an argument that nothing uses is almost always a mistake (the
    // `flif270 *.flif` case: silently processing only the first file).
    if shape.rest.is_none() {
        let unused: Vec<usize> = (1..=count)
            .filter(|&p| owner[p].is_none() && !slices.values().any(|&(lo, hi)| lo <= p && p <= hi))
            .collect();
        if !unused.is_empty() {
            return Err(Problem::Unused(unused));
        }
    }
    Ok(Layout {
        index,
        rest,
        slices,
    })
}

/// How the slices relate to the indexes and to each other at one count:
/// which indexes each slice contains, and which slices overlap or contain
/// which. Empty slices and out-of-range indexes count as "not contained".
///
/// The result is only meant to be compared with the result at another count.
fn relations(shape: &Shape, count: usize) -> Vec<bool> {
    let ranges: Vec<Option<(usize, usize)>> = shape
        .slices
        .iter()
        .map(|&s| slice_range(s, count))
        .collect();
    let inside = |p: usize, r: Option<(usize, usize)>| r.is_some_and(|(lo, hi)| lo <= p && p <= hi);
    let mut signature = Vec::new();
    for &range in &ranges {
        // Membership of each index in this slice.
        for &i in &shape.indexes {
            signature.push(i.position(count).is_some_and(|p| inside(p, range)));
        }
    }
    for (a, &first) in ranges.iter().enumerate() {
        for &second in &ranges[a + 1..] {
            // Overlap, and containment either way.
            let overlap =
                matches!((first, second), (Some((l1, h1)), Some((l2, h2))) if l1 <= h2 && l2 <= h1);
            let contains = |outer: Option<(usize, usize)>, inner: Option<(usize, usize)>| matches!((outer, inner), (Some((lo, hi)), Some((l, h))) if lo <= l && h <= hi);
            signature.extend([overlap, contains(first, second), contains(second, first)]);
        }
    }
    signature
}

/// Check every rule for one count, including that slices relate to the
/// other references as they do with `plenty` arguments, a count at which every
/// relation has settled (see [`compute_arity`]).
fn check_count(shape: &Shape, count: usize, plenty: usize) -> Result<Layout, Problem> {
    let layout = layout(shape, count)?;
    // E.g. in `{1..4} {-1}`, with 3 arguments the slice would take the last
    // one too, which nobody writing that template intends.
    if relations(shape, count) != relations(shape, plenty) {
        return Err(Problem::Other(format!(
            "with {count} arguments, a slice would share different arguments with the other references than with more"
        )));
    }
    Ok(layout)
}

/// Work out which argument counts a template accepts.
///
/// Let K be the largest number in any reference. From 2K+1 arguments on,
/// front-counted and back-counted positions can never meet and at least one
/// argument sits between them, so every count from there up behaves the same
/// as 2K+1 (the extra arguments are all "middle" ones, treated alike). Checking
/// the counts 0 to 2K+1 therefore decides every count. It is an error if no
/// count works.
fn compute_arity(shape: &Shape) -> Result<Arity, ParseError> {
    let plenty = 2 * shape.max_magnitude() + 1;
    let mut counts: Vec<usize> = (0..plenty)
        .filter(|&n| check_count(shape, n, plenty).is_ok())
        .collect();
    let mut from = check_count(shape, plenty, plenty).is_ok().then_some(plenty);
    // Fold a trailing run of accepted counts into the open-ended part, so
    // "at least 2" isn't spelled "2, 3, 4 or at least 5".
    while let (Some(&last), Some(start)) = (counts.last(), from) {
        if last + 1 != start {
            break;
        }
        counts.pop();
        from = Some(last);
    }
    if counts.is_empty() && from.is_none() {
        // The problem at `plenty` is the one every larger count shares, so it
        // is the one to explain; but `plenty` itself is an internal choice and
        // must not appear in the message.
        let reason = match check_count(shape, plenty, plenty) {
            Err(Problem::Unused(positions)) => describe_unused(shape, &positions, plenty),
            Err(Problem::Gap { used, missing }) => {
                format!("{used} is used but {missing} is not, and no slice covers that argument")
            }
            Err(Problem::Other(text)) => text,
            // Unreachable: `from` would be set.
            Ok(_) => String::new(),
        };
        return Err(ParseError::whole(format!(
            "no number of arguments works with this template: {reason}"
        )));
    }
    Ok(Arity { counts, from })
}

/// Describe arguments that would never be used, without naming a count, e.g.
/// "nothing uses arguments {1} and {-1}".
///
/// `positions` are the unused positions at `plenty` (2K+1), ascending. At that
/// count, positions 1..=K are only reachable as `{N}`, the last K only as
/// `{-N}`, and the one in between stands for every middle argument; so each
/// unused position has a count-independent name, written the way a template
/// would refer to it.
///
/// Constraint on change: this states the problem and nothing more. It used to
/// suggest `{2..}` for a slice ending at `{-1}`, but that is only one fix
/// (adding `{-1}` or `{@}` are others), so the suggestion was more misleading
/// than helpful.
fn describe_unused(shape: &Shape, positions: &[usize], plenty: usize) -> String {
    let k = shape.max_magnitude();
    // Front positions, then the middle (one name for all of it), then the
    // back, which keeps argument order.
    let mut names: Vec<String> = positions
        .iter()
        .filter(|&&p| p <= k)
        .map(|p| format!("{{{p}}}"))
        .collect();
    let middle = positions.iter().any(|&p| p > k && p <= plenty - k);
    if middle {
        names.push("the ones in between".to_owned());
    }
    names.extend(
        positions
            .iter()
            .filter(|&&p| p > plenty - k)
            .map(|&p| format!("{{-{}}}", plenty + 1 - p)),
    );
    match names.split_last() {
        // Only the middle: no reference names it, so say what it is.
        Some((_, [])) if middle => "nothing uses the arguments in between".to_owned(),
        Some((only, [])) => format!("nothing uses argument {only}"),
        Some((last, init)) => format!("nothing uses arguments {} and {last}", init.join(", ")),
        // Unreachable: there is at least one unused position.
        None => "some arguments would never be used".to_owned(),
    }
}

/// One process's argv after expansion.
pub type Argv = Vec<OsString>;

/// A template with arguments substituted: what will actually run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    /// Stages of the first pipeline.
    pub head: Vec<Argv>,
    /// Following pipelines with their operators.
    pub tail: Vec<(Op, Vec<Argv>)>,
}

impl Plan {
    /// The argv when the plan is exactly one command (no pipes or operators), or
    /// `None` if more than one process is involved.
    pub fn as_simple(&self) -> Option<&Argv> {
        match (self.head.as_slice(), self.tail.is_empty()) {
            ([only], true) => Some(only),
            _ => None,
        }
    }
}

/// Why a template could not be expanded with the given arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExpandError {
    /// A number of arguments the template doesn't accept.
    WrongArgCount { arity: Arity, got: usize },
    /// A command expanded to no words at all (e.g. a bare `{@}` with no args).
    EmptyCommand,
    /// A file check failed, e.g. `{1:F}` given a directory.
    CheckFailed {
        /// The reference as written, e.g. `{1:F}`.
        reference: String,
        /// What the check requires, e.g. "an existing file".
        wants: &'static str,
        /// The argument that failed it.
        value: OsString,
        /// What was found instead, e.g. "is a directory".
        found: String,
    },
    /// `{$NAME}` with `NAME` unset, or set to nothing.
    EnvMissing {
        /// The variable's name.
        name: String,
        /// Set but empty, rather than unset.
        empty: bool,
    },
}

impl fmt::Display for ExpandError {
    /// Describe the problem for the user.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ExpandError::WrongArgCount { arity, got } => write!(f, "expected {arity}, got {got}"),
            ExpandError::EmptyCommand => {
                f.write_str("a command expanded to nothing (no program to run)")
            }
            // The value is shown lossily: it's for a person to read, and the
            // real bytes are what the check ran on.
            ExpandError::CheckFailed {
                reference,
                wants,
                value,
                found,
            } => {
                write!(
                    f,
                    "{reference} needs {wants}, but '{}' {found}",
                    value.to_string_lossy()
                )
            }
            ExpandError::EnvMissing { name, empty: false } => write!(f, "{{${name}}} is not set"),
            ExpandError::EnvMissing { name, empty: true } => {
                write!(f, "{{${name}}} is set but empty")
            }
        }
    }
}

/// Reads an environment variable; `std::env::var_os` outside tests.
type EnvLookup<'a> = &'a dyn Fn(&str) -> Option<OsString>;

/// What expansion learns besides the plan: the problems that would stop it,
/// and the environment variables it read.
struct Findings<'a> {
    /// Every distinct problem, in template order.
    problems: Vec<ExpandError>,
    /// Each variable read, in order of first use, with its value.
    env: Vec<(String, Option<OsString>)>,
    /// Where variables are read from.
    lookup: EnvLookup<'a>,
}

impl Findings<'_> {
    /// Record a problem, once.
    ///
    /// The same check on the same argument can appear several times (`{1:F}`
    /// twice); reporting it twice would add nothing.
    fn record(&mut self, err: ExpandError) {
        if !self.problems.contains(&err) {
            self.problems.push(err);
        }
    }

    /// An environment variable's value, read once per expansion so every use
    /// sees the same value.
    fn var(&mut self, name: &str) -> Option<OsString> {
        if let Some((_, value)) = self.env.iter().find(|(n, _)| n == name) {
            return value.clone();
        }
        let value = (self.lookup)(name);
        self.env.push((name.to_owned(), value.clone()));
        value
    }
}

/// A plan together with what would stop it from running.
///
/// Only `explain` wants this: a real run stops at the first problem.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Preview {
    /// The plan, as it would run if every problem were fixed. An unset
    /// variable appears as its reference, e.g. `{$BACKUP_DIR}`.
    pub plan: Plan,
    /// Every distinct problem, in template order: each is an
    /// [`ExpandError::CheckFailed`] or an [`ExpandError::EnvMissing`].
    pub problems: Vec<ExpandError>,
    /// The environment variables the template read and their values, in
    /// order of first use; unset or empty ones are in `problems` instead.
    pub env: Vec<(String, OsString)>,
}

impl Template {
    /// True when the template is one command with no pipes or operators.
    pub fn is_simple(&self) -> bool {
        self.tail.is_empty() && self.head.stages.len() == 1
    }

    /// How many arguments the template accepts.
    pub fn arity(&self) -> &Arity {
        &self.arity
    }

    /// Substitute runtime arguments into the template.
    ///
    /// `name` is the alias name, for `{0}`; `args` excludes it.
    ///
    /// # Errors
    ///
    /// A number of arguments the template doesn't accept, a command that expands
    /// to nothing, the first file check that fails, or an environment variable
    /// that is unset or empty.
    pub fn expand(&self, name: &str, args: &[OsString]) -> Result<Plan, ExpandError> {
        self.expand_with(name, args, &|var| std::env::var_os(var))
    }

    /// [`Template::expand`], reading environment variables from `lookup`.
    fn expand_with(
        &self,
        name: &str,
        args: &[OsString],
        lookup: EnvLookup<'_>,
    ) -> Result<Plan, ExpandError> {
        // Report only the first problem, as before collecting them all;
        // finding the rest costs a few stat calls on an error path.
        let Preview { plan, problems, .. } = self.preview_with(name, args, lookup)?;
        match problems.into_iter().next() {
            Some(first) => Err(first),
            None => Ok(plan),
        }
    }

    /// Substitute runtime arguments, carrying on past failed checks and unset
    /// environment variables.
    ///
    /// For `explain`: shows what would run and everything that would stop it.
    /// Arguments are as for [`Template::expand`].
    ///
    /// # Errors
    ///
    /// A wrong argument count or an empty command, since there is then no plan to
    /// show. Other problems are not errors here; they are in
    /// [`Preview::problems`].
    pub fn preview(&self, name: &str, args: &[OsString]) -> Result<Preview, ExpandError> {
        self.preview_with(name, args, &|var| std::env::var_os(var))
    }

    /// [`Template::preview`], reading environment variables from `lookup`.
    fn preview_with(
        &self,
        name: &str,
        args: &[OsString],
        lookup: EnvLookup<'_>,
    ) -> Result<Preview, ExpandError> {
        // Check the count up front; an accepted count always has a layout.
        let wrong = || ExpandError::WrongArgCount {
            arity: self.arity.clone(),
            got: args.len(),
        };
        if !self.arity.accepts(args.len()) {
            return Err(wrong());
        }
        let layout = layout(&self.shape, args.len()).map_err(|_| wrong())?;
        let mut findings = Findings {
            problems: Vec::new(),
            env: Vec::new(),
            lookup,
        };
        // Expand each pipeline stage by stage, in template order so problems
        // are listed in the order they appear.
        let mut expand_pipeline = |p: &Pipeline| -> Result<Vec<Argv>, ExpandError> {
            p.stages
                .iter()
                .map(|c| expand_command(c, &layout, name, args, &mut findings))
                .collect()
        };
        let head = expand_pipeline(&self.head)?;
        let mut tail = Vec::with_capacity(self.tail.len());
        for (op, p) in &self.tail {
            tail.push((*op, expand_pipeline(p)?));
        }
        // Unset and empty variables are already among the problems.
        let env = findings
            .env
            .into_iter()
            .filter_map(|(name, value)| value.filter(|v| !v.is_empty()).map(|v| (name, v)))
            .collect();
        Ok(Preview {
            plan: Plan { head, tail },
            problems: findings.problems,
            env,
        })
    }
}

/// Expand one command, recording problems in `findings`.
///
/// `layout` says where each reference lands for this argument count, and
/// `args` have already been checked against the arity.
fn expand_command(
    command: &Command,
    layout: &Layout,
    name: &str,
    args: &[OsString],
    findings: &mut Findings,
) -> Result<Argv, ExpandError> {
    let mut argv = Vec::with_capacity(command.words.len());
    for word in &command.words {
        // A several-argument reference repeats the word once per argument it
        // stands for (the parser allows only one distinct one per word).
        let multi = word.pieces.iter().find_map(|p| match p {
            Piece::Multi(m, _) => Some(*m),
            _ => None,
        });
        match multi {
            None => {
                let entry = build_entry(word, layout, name, args, None, findings);
                argv.push(check_word(word, entry, findings));
            }
            Some(multi) => {
                for position in layout.positions(multi) {
                    let entry = build_entry(word, layout, name, args, Some(position), findings);
                    argv.push(check_word(word, entry, findings));
                }
            }
        }
    }
    // Only possible when every word of the command repeated zero times.
    if argv.is_empty() {
        Err(ExpandError::EmptyCommand)
    } else {
        Ok(argv)
    }
}

/// Apply a word's own check (`{:N}` at its end) to one fully expanded copy,
/// recording a failure in `findings`.
///
/// The entry is returned unchanged whether or not the check passed.
fn check_word(word: &Word, entry: OsString, findings: &mut Findings) -> OsString {
    if let Some(check) = word.check
        && let Err(found) = check.run(&entry)
    {
        findings.record(ExpandError::CheckFailed {
            reference: format!("the word {}", word.source),
            wants: check.wants(),
            value: entry.clone(),
            found,
        });
    }
    entry
}

/// Build one argv entry from a word, recording problems in `findings`.
///
/// For a repeated word, `repeat` is the position this copy stands for. The
/// entry is an `OsString` so non-UTF-8 filenames survive.
fn build_entry(
    word: &Word,
    layout: &Layout,
    name: &str,
    args: &[OsString],
    repeat: Option<usize>,
    findings: &mut Findings,
) -> OsString {
    let mut entry = OsString::new();
    for piece in &word.pieces {
        match piece {
            Piece::Lit(text) => entry.push(text),
            Piece::Name => entry.push(name),
            // Unset or empty is a problem: an empty argument is almost never
            // what was meant, and a missing program name can't run. The
            // reference stands in for the value, so `explain` shows where it
            // would go.
            Piece::Env(var, format) => {
                let reference = || written(&format!("{{${var}}}"), *format);
                match findings.var(var) {
                    Some(value) if !value.is_empty() => {
                        entry.push(format.render(&value, reference, findings));
                    }
                    value => {
                        findings.record(ExpandError::EnvMissing {
                            name: var.clone(),
                            empty: value.is_some(),
                        });
                        entry.push(reference());
                    }
                }
            }
            // Positions are 1-based and in range: the layout checked them.
            Piece::Arg(index, format) => {
                entry.push(format.render(
                    &args[layout.index[index] - 1],
                    || written(&index.to_string(), *format),
                    findings,
                ));
            }
            Piece::Multi(multi, format) => {
                let value = &args[repeat.expect("repeated words get a position") - 1];
                entry.push(format.render(value, || written(&multi.to_string(), *format), findings));
            }
        }
    }
    entry
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build an argv list from string literals.
    fn os(items: &[&str]) -> Vec<OsString> {
        items.iter().map(OsString::from).collect()
    }

    /// Parse and expand in one go, panicking on any error.
    fn plan(src: &str, args: &[&str]) -> Plan {
        parse(src).unwrap().expand("t", &os(args)).unwrap()
    }

    /// The single command a simple template expands to.
    fn argv(src: &str, args: &[&str]) -> Argv {
        plan(src, args).head[0].clone()
    }

    /// The arity of a template, as text, e.g. "exactly 1 argument".
    fn arity(src: &str) -> String {
        parse(src)
            .unwrap_or_else(|e| panic!("{src}: {e}"))
            .arity()
            .to_string()
    }

    #[test]
    fn simple_command_gets_implicit_rest() {
        // No references: arguments are appended like a shell alias.
        assert_eq!(argv("ls -la", &["a", "b"]), os(&["ls", "-la", "a", "b"]));
        assert!(parse("ls").unwrap().implicit_rest);
        // `{0}` counts as a reference, so it switches the rule off.
        assert_eq!(arity("echo {0}"), "exactly 0 arguments");
    }

    #[test]
    fn multi_command_without_refs_rejects_args() {
        // No obvious recipient for arguments, so they're an error.
        let t = parse("true | true").unwrap();
        assert!(!t.implicit_rest);
        assert!(matches!(
            t.expand("t", &os(&["x"])),
            Err(ExpandError::WrongArgCount { .. })
        ));
    }

    #[test]
    fn flif270_shape() {
        // The motivating example: one argument used four times across stages.
        let src = "flif -d {1} - | convert -rotate 270 - - | flif -E 14 - {1}.rot.flif && mv {1}.rot.flif {1}";
        let p = plan(src, &["my pic.flif"]);
        assert_eq!(p.head.len(), 3);
        assert_eq!(p.head[0], os(&["flif", "-d", "my pic.flif", "-"]));
        assert_eq!(
            p.head[2],
            os(&["flif", "-E", "14", "-", "my pic.flif.rot.flif"])
        );
        assert_eq!(
            p.tail,
            vec![(
                Op::And,
                vec![os(&["mv", "my pic.flif.rot.flif", "my pic.flif"])]
            )]
        );
    }

    #[test]
    fn quoting_rules() {
        // Single quotes are fully literal, braces and backslashes included.
        assert_eq!(argv(r"echo 'a {1} \n'", &[]), os(&["echo", r"a {1} \n"]));
        // Double quotes expand references and keep unknown escapes literal.
        assert_eq!(
            argv(r#"echo "{1} \d \" \{""#, &["x"]),
            os(&["echo", r#"x \d " {"#])
        );
        // Adjacent quoted and unquoted parts form one word.
        assert_eq!(
            argv(r#"echo a'b c'"d"{1}"#, &["!"]),
            os(&["echo", "ab cd!"])
        );
        // Empty quotes produce an empty argument rather than nothing.
        assert_eq!(
            argv("printf %s '' {1}", &["x"]),
            os(&["printf", "%s", "", "x"])
        );
    }

    #[test]
    fn back_indexes() {
        // {-1} is the last argument, wherever it is.
        assert_eq!(
            argv("cp -t {-1} {@+}", &["a", "b", "dir"]),
            os(&["cp", "-t", "dir", "a", "b"])
        );
        assert_eq!(arity("cp -t {-1} {@+}"), "at least 2 arguments");
        // Front and back together: exact, and never the same argument twice.
        assert_eq!(arity("mv {1} {-1}"), "exactly 2 arguments");
    }

    #[test]
    fn rest_is_what_nothing_else_uses() {
        // Arguments a slice passes aren't repeated by {@}.
        let src = "echo a {2..-2} {1} remaining {@}";
        assert_eq!(
            argv(src, &["1", "2", "3", "4", "5"]),
            os(&["echo", "a", "2", "3", "1", "remaining", "4", "5"])
        );
        // Next to slices, {@} is the "and any others" that may be empty.
        assert_eq!(
            argv("cmd {1} {2..3} {@}", &["a", "b"]),
            os(&["cmd", "a", "b"])
        );
        assert_eq!(
            argv("cmd {..-1} -- {@}", &["a", "b", "c"]),
            os(&["cmd", "a", "b", "--", "c"])
        );
        // A {@+} that a slice always leaves empty is refused up front.
        assert!(
            parse("cmd {1..} {@+}")
                .unwrap_err()
                .message
                .contains("{@+} would be empty")
        );
    }

    #[test]
    fn rest_is_what_indexes_leave() {
        // {@} excludes arguments that {N}/{-N} use, so nothing is passed twice.
        assert_eq!(
            argv("cmd {1} -- {@}", &["a", "b", "c"]),
            os(&["cmd", "a", "--", "b", "c"])
        );
        assert_eq!(argv("cmd {1} -- {@}", &["a"]), os(&["cmd", "a", "--"]));
        assert_eq!(arity("cmd {1} {@}"), "at least 1 argument");
        // {@+} demands at least one.
        assert_eq!(arity("cmd {1} {@+}"), "at least 2 arguments");
        assert!(parse("cmd {@+}").unwrap().expand("t", &[]).is_err());
        assert_eq!(arity("cmd {@}"), "any number of arguments");
    }

    #[test]
    fn slices_stop_before_their_end_and_are_never_empty() {
        // Start included, end not, both counted like {N} and {-N}.
        assert_eq!(
            argv("echo {1} {2..4}", &["a", "b", "c"]),
            os(&["echo", "a", "b", "c"])
        );
        assert_eq!(
            argv("echo {..-1} -- {-1}", &["a", "b", "c"]),
            os(&["echo", "a", "b", "--", "c"])
        );
        // The case that settled it: the last argument, then all the others.
        assert_eq!(
            argv("echo {-1} {..-1}", &["1", "2", "3", "4", "5"]),
            os(&["echo", "5", "1", "2", "3", "4"])
        );
        // {2..} needs at least one argument to stand for: `rm {1} {2..}` with
        // one argument is an error, not `rm a`.
        assert_eq!(arity("cmd {1} {2..}"), "at least 2 arguments");
        // Slices may overlap indexes on purpose.
        assert_eq!(
            argv("echo {1} {1..}", &["a", "b"]),
            os(&["echo", "a", "a", "b"])
        );
    }

    #[test]
    fn slice_overlap_must_not_shift_with_the_count() {
        // With 3 the slice would swallow {-1}; with 5 the 4th would be unused.
        assert_eq!(arity("cmd {1..4} {-1}"), "exactly 4 arguments");
        let t = parse("cmd {1..4} {-1}").unwrap();
        assert_eq!(
            t.expand("t", &os(&["a", "b", "c", "d"])).unwrap().head[0],
            os(&["cmd", "a", "b", "c", "d"])
        );
    }

    #[test]
    fn words_repeat_per_argument() {
        // -I{@} gives one -I per argument, and nothing at all for none.
        assert_eq!(
            argv("gcc -I{@} -c {-1}", &["a", "b", "x.c"]),
            os(&["gcc", "-Ia", "-Ib", "-c", "x.c"])
        );
        assert_eq!(
            argv("gcc -I{@} -c {-1}", &["x.c"]),
            os(&["gcc", "-c", "x.c"])
        );
        // The same reference twice in one word stays in step.
        assert_eq!(argv("env {@}={@}", &["a", "b"]), os(&["env", "a=a", "b=b"]));
        // So does a slice written two ways ({1..-1} is {..-1}).
        assert_eq!(
            argv("echo {1..-1}:{..-1} {-1}", &["a", "b", "c"]),
            os(&["echo", "a:a", "b:b", "c"])
        );
        // A dangerous prefix vanishes rather than standing alone.
        assert_eq!(argv("rm -rf /{@} {0}", &[]), os(&["rm", "-rf", "t"]));
    }

    #[test]
    fn alias_name_reference() {
        assert_eq!(argv("echo {0}: {1}", &["x"]), os(&["echo", "t:", "x"]));
    }

    #[test]
    fn chunked_symlink_case() {
        // poolcmd2 -c 5: several links, then the source last.
        let src = "rm -f {@} && symlink {-1} {@}";
        assert_eq!(arity(src), "at least 1 argument");
        let p = plan(src, &["bin/a", "bin/b", "src"]);
        assert_eq!(p.head[0], os(&["rm", "-f", "bin/a", "bin/b"]));
        assert_eq!(p.tail[0].1[0], os(&["symlink", "src", "bin/a", "bin/b"]));
    }

    #[test]
    fn operators_without_spaces() {
        // Operators end words, so spacing is optional, as in sh.
        let p = plan("a|b&&c||d;e", &[]);
        assert_eq!(p.head, vec![os(&["a"]), os(&["b"])]);
        assert_eq!(
            p.tail.iter().map(|(op, _)| *op).collect::<Vec<_>>(),
            vec![Op::And, Op::Or, Op::Then]
        );
    }

    #[test]
    fn trailing_semicolon_is_allowed() {
        assert_eq!(parse("true;").unwrap().tail.len(), 0);
    }

    #[test]
    fn arity_errors() {
        // Too few and too many are both rejected, with the arity in the message.
        let t = parse("echo {1} {2}").unwrap();
        let err = t.expand("t", &os(&["a"])).unwrap_err();
        assert_eq!(err.to_string(), "expected exactly 2 arguments, got 1");
        assert!(t.expand("t", &os(&["a", "b", "c"])).is_err());
    }

    #[test]
    fn arity_phrasing() {
        // Non-contiguous arities are spelled out rather than approximated.
        let odd = Arity {
            counts: vec![2, 4, 5],
            from: Some(7),
        };
        assert_eq!(odd.to_string(), "2, 4 to 5 or at least 7 arguments");
        assert!(odd.accepts(5) && !odd.accepts(6) && odd.accepts(70));
    }

    #[test]
    fn empty_command_from_rest() {
        // A command that is only {@} needs at least one argument to run.
        assert_eq!(
            parse("{@}").unwrap().expand("t", &[]),
            Err(ExpandError::EmptyCommand)
        );
    }

    #[test]
    fn slices_fill_gaps_in_numbering() {
        // {2} is covered by the slice, so {1} {3} has no gap here.
        assert_eq!(
            argv("echo a {2..} {1} {3}", &["x", "y", "z", "w"]),
            os(&["echo", "a", "y", "z", "w", "x", "z"])
        );
        assert_eq!(arity("echo a {2..} {1} {3}"), "at least 3 arguments");
        // The same from the back: {..-1} covers {-2}.
        assert_eq!(
            argv("echo {-1} {-3} {..-1}", &["a", "b", "c"]),
            os(&["echo", "c", "a", "a", "b"])
        );
        // Still a gap if the slice doesn't reach it, now a whole-template error.
        let err = parse("echo {1} {3} {5..}").unwrap_err();
        assert!(
            err.message
                .ends_with(": {3} is used but {2} is not, and no slice covers that argument"),
            "{}",
            err.message
        );
        // {@} never fills a gap: `cp {1} {3} {@}` stays a typo.
        assert!(parse("cp {1} {3} {@}").is_err());
        // With the gap covered, the last argument is still unused here.
        let err = parse("echo a {2..-1} {1} {3}").unwrap_err();
        assert!(
            err.message.ends_with(": nothing uses argument {-1}"),
            "{}",
            err.message
        );
        // {@} can't fill a gap between references, but it can take what's
        // left at the end, which settles the last argument.
        assert_eq!(arity("echo a {2..-1} {1} {3} {@}"), "at least 4 arguments");
    }

    #[test]
    fn templates_no_count_can_satisfy() {
        // Arguments 1 and 2 are never used, whatever the count.
        let err = parse("cmd {3..}").unwrap_err();
        assert_eq!(err.offset, None);
        assert!(
            err.message
                .ends_with(": nothing uses arguments {1} and {2}"),
            "{}",
            err.message
        );
        // A slice ending at {-1} leaves the last argument out; the message
        // names it as a template would, without an internal count or a fix.
        let err = parse("echo a {2..-1}").unwrap_err();
        assert_eq!(
            err.message,
            "no number of arguments works with this template: nothing uses arguments {1} and {-1}"
        );
        // {1} and {-1} are distinct arguments, but nothing uses the middle.
        assert_eq!(arity("cmd {1} {-1}"), "exactly 2 arguments");
    }

    #[test]
    fn syntax_errors_point_at_the_problem() {
        // Each case: source, and the byte offset the error must report.
        let cases = [
            ("echo $1", 5),
            (r#"ffmpeg -i "$1""#, 11),
            (r#"echo "a `date`""#, 8),
            ("echo {-0}", 5),
            ("echo {x}", 5),
            ("echo {+1}", 5),
            ("echo {1", 5),
            ("echo }", 5),
            ("echo 'oops", 5),
            ("echo {2}", 5),
            ("echo {1} {3} {1}", 9),
            ("echo {-2}", 5),
            ("echo {..}", 5),
            ("echo {3..2}", 5),
            ("echo {2..2}", 5),
            ("echo {-1..-2}", 5),
            ("echo {-1..-1}", 5),
            ("echo {1..0}", 5),
            ("echo {@}{1..}", 8),
            ("a | | b", 4),
            ("a &", 2),
            ("ls *.txt", 3),
            ("cat < f", 4),
            ("find . -exec rm {} ;", 16),
            ("a |", 3),
            ("", 0),
        ];
        for (src, offset) in cases {
            let err = parse(src).expect_err(src);
            assert_eq!(err.offset, Some(offset), "{src}: {}", err.message);
        }
    }

    #[test]
    fn end_rule_is_spelled_out() {
        // Whoever writes an always-empty slice gets the rule with the error.
        assert!(
            parse("echo {2..2}")
                .unwrap_err()
                .message
                .contains("stops before its end")
        );
    }

    #[test]
    fn escaped_specials_are_literal() {
        // Escaping or quoting gets any reserved character through verbatim.
        assert_eq!(
            argv(r"find . -exec rm '{}' \;", &[]),
            os(&["find", ".", "-exec", "rm", "{}", ";"])
        );
        assert_eq!(
            argv(r"echo \$HOME '*' \{1\}", &[]),
            os(&["echo", "$HOME", "*", "{1}"])
        );
        // Inside double quotes `$` and backticks need a backslash, as in sh;
        // single quotes keep them literal with no escaping at all.
        assert_eq!(
            argv(r#"echo "\$1 \`x\`" '$2'"#, &[]),
            os(&["echo", "$1 `x`", "$2"])
        );
    }

    #[test]
    fn non_utf8_arguments_survive() {
        use std::os::unix::ffi::OsStrExt;
        // Filenames need not be UTF-8 on Unix; they must pass through byte-exact.
        let raw = std::ffi::OsStr::from_bytes(b"caf\xe9.mkv").to_os_string();
        let p = parse("ffmpeg -i {1} {1}.out")
            .unwrap()
            .expand("t", std::slice::from_ref(&raw))
            .unwrap();
        assert_eq!(p.head[0][2], raw);
        assert_eq!(p.head[0][3].as_bytes(), b"caf\xe9.mkv.out");
    }

    #[test]
    fn path_modifiers() {
        // The four modifiers on one value.
        let out = argv("echo {1:/} {1://} {1:.} {1:/.}", &["dir/photo.jpg"]);
        assert_eq!(out, os(&["echo", "photo.jpg", "dir", "dir/photo", "photo"]));
        // Edge cases, as in fd: a bare name's parent is `.`, only the last
        // extension goes, a dotfile has none, a trailing slash is ignored,
        // and `/` has no basename or parent to give, so it stays itself.
        assert_eq!(argv("echo {1://}", &["file"]), os(&["echo", "."]));
        assert_eq!(
            argv("echo {1:.} {1:/.}", &["a.tar.gz"]),
            os(&["echo", "a.tar", "a.tar"])
        );
        assert_eq!(
            argv("echo {1:.} {1:/.}", &[".bashrc"]),
            os(&["echo", ".bashrc", ".bashrc"])
        );
        assert_eq!(
            argv("echo {1:/} {1://}", &["dir/sub/"]),
            os(&["echo", "sub", "dir"])
        );
        assert_eq!(argv("echo {1:/} {1://}", &["/"]), os(&["echo", "/", "/"]));
        // Modifiers only change text, so arity is unaffected.
        assert_eq!(arity("cp {1:F} {2:N}"), "exactly 2 arguments");
    }

    #[test]
    fn modifiers_on_several_argument_references() {
        // Applied per item, and words still repeat as usual.
        assert_eq!(
            argv("echo {@:/.}.png", &["a/x.jpg", "b/y.jpg"]),
            os(&["echo", "x.png", "y.png"])
        );
        // One reference with different modifiers in a word stays in step.
        assert_eq!(
            argv("echo {@://}/old-{@:/}", &["a/x", "b/y"]),
            os(&["echo", "a/old-x", "b/old-y"])
        );
        // Slices and back indexes take formats too.
        assert_eq!(
            argv("echo {1://} {2..:/} {-1:/.}", &["p/q", "r/s", "t/u.v"]),
            os(&["echo", "p", "s", "u.v", "u"])
        );
    }

    #[test]
    fn modifiers_keep_non_utf8_bytes() {
        use std::os::unix::ffi::OsStrExt;
        let raw = std::ffi::OsStr::from_bytes(b"dir/caf\xe9.mkv").to_os_string();
        let p = parse("echo {1:/.}")
            .unwrap()
            .expand("t", std::slice::from_ref(&raw))
            .unwrap();
        assert_eq!(p.head[0][1].as_bytes(), b"caf\xe9");
    }

    #[test]
    fn format_syntax_errors() {
        // Each case: source, and the byte offset the error must report.
        let cases = [
            ("echo {1:}", 5),
            ("echo {1:X}", 5),
            ("echo {1:FD}", 5),
            ("echo {1:/./}", 5),
            ("echo {:F}", 5),
            ("echo {0:/}", 5),
        ];
        for (src, offset) in cases {
            let err = parse(src).expect_err(src);
            assert_eq!(err.offset, Some(offset), "{src}: {}", err.message);
        }
    }

    /// A fresh scratch directory for one test.
    ///
    /// Tests run in parallel, so each must pass a different `tag`.
    fn scratch(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("aliasmux-core-{}-{tag}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Expand a one-argument template, returning the error as text if any.
    fn try_expand(src: &str, arg: &std::path::Path) -> Result<Argv, String> {
        let t = parse(src).unwrap();
        t.expand("t", &[arg.as_os_str().to_os_string()])
            .map(|p| p.head[0].clone())
            .map_err(|e| e.to_string())
    }

    #[test]
    fn file_checks() {
        let dir = scratch("checks");
        let (file, sub, missing, dangling) = (
            dir.join("f.txt"),
            dir.join("d"),
            dir.join("nope"),
            dir.join("link"),
        );
        fs::write(&file, "x").unwrap();
        fs::create_dir(&sub).unwrap();
        std::os::unix::fs::symlink(dir.join("gone"), &dangling).unwrap();

        // Passing checks substitute the value as usual.
        assert!(try_expand("cat {1:F}", &file).is_ok());
        assert!(try_expand("ls {1:D}", &sub).is_ok());
        assert!(try_expand("ls {1:E}", &sub).is_ok());
        assert!(try_expand("touch {1:N}", &missing).is_ok());
        // Failing ones say what was wanted and what was found.
        let wants = |src: &str, arg: &std::path::Path| try_expand(src, arg).unwrap_err();
        assert!(
            wants("cat {1:F}", &sub).ends_with("d' is a directory"),
            "{}",
            wants("cat {1:F}", &sub)
        );
        assert!(wants("cat {1:F}", &missing).starts_with("{1:F} needs an existing file, but '"));
        assert!(wants("cat {1:F}", &missing).ends_with("' does not exist"));
        assert!(wants("ls {1:D}", &file).ends_with("' is a file"));
        assert!(wants("ls {1:E}", &missing).ends_with("' does not exist"));
        assert!(wants("touch {1:N}", &file).ends_with("' already exists"));
        // A dangling symlink counts as existing for N (writing through it
        // would create a file elsewhere), but not for F.
        assert!(wants("touch {1:N}", &dangling).ends_with("' already exists"));
        assert!(wants("cat {1:F}", &dangling).ends_with("' does not exist"));
        // F means "not a directory": devices and pipes pass it, D still
        // refuses them, and N sees that they exist.
        let fifo = dir.join("fifo");
        assert!(
            std::process::Command::new("mkfifo")
                .arg(&fifo)
                .status()
                .unwrap()
                .success()
        );
        for special in [std::path::Path::new("/dev/null"), &fifo] {
            assert!(
                try_expand("cat {1:F}", special).is_ok(),
                "{}",
                special.display()
            );
            assert!(wants("ls {1:D}", special).ends_with("' is not a directory"));
            assert!(wants("touch {1:N}", special).ends_with("' already exists"));
        }
        // The check sees the full path; the modifier applies afterwards.
        assert_eq!(
            try_expand("echo {1:F/.}", &file).unwrap(),
            os(&["echo", "f"])
        );
        // Several-argument references check every item and name themselves.
        let t = parse("ls {@:D}").unwrap();
        let err = t
            .expand(
                "t",
                &[sub.clone().into_os_string(), file.clone().into_os_string()],
            )
            .unwrap_err();
        assert!(
            err.to_string()
                .starts_with("{@:D} needs an existing directory"),
            "{err}"
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn word_checks() {
        let dir = scratch("word-checks");
        let (input, output) = (dir.join("photo.png"), dir.join("photo.png.jpg"));
        fs::write(&input, "x").unwrap();
        let src = "magick {1:F} ppm: | cjpeg -outfile {1}.jpg{:N} && mv {1}.jpg {1}";
        // Passes while the output doesn't exist; the check sees the whole word.
        assert!(try_expand(src, &input).is_ok());
        fs::write(&output, "old").unwrap();
        let err = try_expand(src, &input).unwrap_err();
        assert!(
            err.starts_with("the word {1}.jpg{:N} needs a path that doesn't exist yet, but '"),
            "{err}"
        );
        assert!(err.ends_with("photo.png.jpg' already exists"), "{err}");
        // A repeated word is checked once per copy.
        let t = parse("touch {@+}.jpg{:N}").unwrap();
        let other = dir.join("other.png");
        let err = t
            .expand(
                "t",
                &[other.into_os_string(), input.clone().into_os_string()],
            )
            .unwrap_err();
        assert!(
            err.to_string().ends_with("photo.png.jpg' already exists"),
            "{err}"
        );
        // Literal words can be checked too, and keep the implicit {@}.
        let t = parse("cat /no/such/config{:F}").unwrap();
        assert!(t.implicit_rest);
        assert!(
            t.expand("t", &[])
                .unwrap_err()
                .to_string()
                .ends_with("'/no/such/config' does not exist")
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn preview_collects_every_failed_check() {
        let dir = scratch("preview");
        let (input, output) = (dir.join("photo.png"), dir.join("photo.png.jpg"));
        fs::write(&output, "old").unwrap();
        // Two failures: the input is missing and the output exists. The
        // repeated {1:F} is reported once.
        let t = parse("magick {1:F} ppm: | cjpeg -outfile {1}.jpg{:N} && rm {1:F}").unwrap();
        let preview = t.preview("t", &[input.clone().into_os_string()]).unwrap();
        let messages: Vec<String> = preview.problems.iter().map(ToString::to_string).collect();
        assert_eq!(messages.len(), 2, "{messages:?}");
        assert!(
            messages[0].starts_with("{1:F} needs an existing file"),
            "{messages:?}"
        );
        assert!(
            messages[1].starts_with("the word {1}.jpg{:N}"),
            "{messages:?}"
        );
        // The plan is complete regardless.
        assert_eq!(preview.plan.tail.len(), 1);
        assert_eq!(preview.plan.head[1][2], output.clone().into_os_string());
        // expand reports only the first.
        assert_eq!(
            t.expand("t", &[input.into_os_string()])
                .unwrap_err()
                .to_string(),
            messages[0]
        );
        // A wrong count is still an error: there's nothing to preview.
        assert!(matches!(
            t.preview("t", &[]),
            Err(ExpandError::WrongArgCount { .. })
        ));
        let _ = fs::remove_dir_all(&dir);
    }

    /// An environment for tests: `vars` as `NAME=value` pairs.
    fn env_of(vars: &[(&str, &str)]) -> impl Fn(&str) -> Option<OsString> + use<> {
        let vars: Vec<(String, OsString)> = vars
            .iter()
            .map(|(n, v)| (n.to_string(), OsString::from(v)))
            .collect();
        move |name| vars.iter().find(|(n, _)| n == name).map(|(_, v)| v.clone())
    }

    #[test]
    fn env_references() {
        let env = env_of(&[("DEST", "/mnt/my backup"), ("CC", "clang"), ("EMPTY", "")]);
        // One word, spaces and all, never split; glued to text like any reference.
        let t = parse("rsync -ahP {@+} {$DEST} {$DEST}/sub").unwrap();
        let plan = t.expand_with("t", &os(&["a", "b"]), &env).unwrap();
        assert_eq!(
            plan.head[0],
            os(&[
                "rsync",
                "-ahP",
                "a",
                "b",
                "/mnt/my backup",
                "/mnt/my backup/sub"
            ])
        );
        // Not an argument: the count is unaffected.
        assert_eq!(arity("{$CC} -c {1}"), "exactly 1 argument");
        // It's a reference, so like {0} it turns off the implicit {@}.
        assert!(!parse("ls {$HOME}").unwrap().implicit_rest);
        // Inside double quotes too, and with modifiers.
        let t = parse("echo \"{$CC}\" {$DEST:/}").unwrap();
        assert_eq!(
            t.expand_with("t", &[], &env).unwrap().head[0],
            os(&["echo", "clang", "my backup"])
        );
        // Unset and empty are both problems, reported once each.
        let t = parse("{$NOPE} {$EMPTY} {$NOPE}").unwrap();
        let preview = t.preview_with("t", &[], &env).unwrap();
        let messages: Vec<String> = preview.problems.iter().map(ToString::to_string).collect();
        assert_eq!(
            messages,
            ["{$NOPE} is not set", "{$EMPTY} is set but empty"]
        );
        // The reference stands in for the value, so explain shows where it goes.
        assert_eq!(
            preview.plan.head[0],
            os(&["{$NOPE}", "{$EMPTY}", "{$NOPE}"])
        );
        // Only usable values are listed as read; each once, in order of use.
        let t = parse("{$CC} {$DEST} {$CC} {$NOPE}").unwrap();
        let preview = t.preview_with("t", &[], &env).unwrap();
        assert_eq!(
            preview.env,
            [
                ("CC".to_string(), OsString::from("clang")),
                ("DEST".to_string(), OsString::from("/mnt/my backup"))
            ]
        );
        // A real run stops at the first problem.
        let err = t.expand_with("t", &[], &env).unwrap_err();
        assert_eq!(err.to_string(), "{$NOPE} is not set");
    }

    #[test]
    fn env_reference_checks() {
        let dir = scratch("env-checks");
        let dir_str = dir.to_str().unwrap();
        let env = env_of(&[("DIR", dir_str)]);
        assert!(
            parse("ls {$DIR:D}")
                .unwrap()
                .expand_with("t", &[], &env)
                .is_ok()
        );
        let err = parse("cat {$DIR:F}")
            .unwrap()
            .expand_with("t", &[], &env)
            .unwrap_err();
        assert!(
            err.to_string()
                .starts_with("{$DIR:F} needs an existing file, but '"),
            "{err}"
        );
        // Contradictions are refused for variables too.
        let err = parse("cat {$DIR:F} {$DIR:D}").unwrap_err();
        assert_eq!(
            err.message,
            "`{$DIR:D}` contradicts `{$DIR:F}`: a path can't be both a file and a directory"
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn env_reference_syntax_errors() {
        for src in ["echo {$1}", "echo {$}", "echo {$A-B}", "echo {$ÄB}"] {
            let err = parse(src).expect_err(src);
            assert_eq!(err.offset, Some(5), "{src}");
            assert!(
                err.message.contains("an environment variable name is"),
                "{src}: {}",
                err.message
            );
        }
        // Bare `$` is still refused, and names the syntax that works.
        for src in ["echo $HOME", "echo \"$HOME\""] {
            assert!(
                parse(src)
                    .unwrap_err()
                    .message
                    .contains("`{$NAME}` for environment variables"),
                "{src}"
            );
        }
        assert!(parse("echo {$_A1}").is_ok());
    }

    #[test]
    fn word_check_syntax_errors() {
        // Each case: source, and the byte offset the error must report.
        let cases = [
            ("echo {1}{:N}.jpg", 8),
            ("echo {:N}", 5),
            ("echo a{:N}{:F}", 6),
            ("echo a{:/.}", 6),
            ("echo a{:FN}", 6),
            ("echo a{:}", 6),
        ];
        for (src, offset) in cases {
            let err = parse(src).expect_err(src);
            assert_eq!(err.offset, Some(offset), "{src}: {}", err.message);
        }
        // A word check after a closing quote is still at the end.
        assert!(parse("echo \"{1}.jpg\"{:N}").is_ok());
    }

    #[test]
    fn contradictory_checks_are_refused() {
        // The same argument can't be a file and absent, or a file and a directory.
        let err = parse("magick {1:F} ppm: | cjpeg -outfile {1:N}.jpg").unwrap_err();
        assert_eq!(err.offset, Some(35));
        assert_eq!(
            err.message,
            "`{1:N}` contradicts `{1:F}`: a path can't both exist and not exist"
        );
        assert!(
            parse("cp {@:F} {@:D}")
                .unwrap_err()
                .message
                .contains("both a file and a directory")
        );
        // Compatible checks, or checks on different arguments, are fine.
        assert!(parse("cat {1:F} {1:E}").is_ok());
        assert!(parse("cp {1:F} {2:N}").is_ok());
        // References that only sometimes overlap aren't refused for all counts.
        assert!(parse("cp {1:F} {-1:N}").is_ok());
    }

    #[test]
    fn render_places_caret() {
        // The caret line should align with the offending column; whole-template
        // errors show the template without one.
        let err = parse("echo $1").unwrap_err();
        assert!(err.render("echo $1").ends_with("\n    echo $1\n         ^"));
        let whole = parse("cmd {3..}").unwrap_err();
        assert!(whole.render("cmd {3..}").ends_with("\n    cmd {3..}"));
    }
}
