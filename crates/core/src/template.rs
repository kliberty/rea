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
//! * `{@}`: every argument that no `{N}` or `{-N}` uses, zero or more;
//!   `{@+}` is the same but requires at least one
//! * `{A..B}`: a slice, from argument A up to but not including argument B,
//!   each end counted like `{N}` / `{-N}` and either one optional: `{2..}` is
//!   the 2nd onward, `{..-1}` all but the last. Slices claim nothing (they may
//!   overlap other references) and are never empty.
//! * `{0}`: the alias name
//!
//! A one-argument reference always yields exactly one argv entry. A word that
//! contains a several-argument reference (`{@}`, `{@+}`, a slice) is repeated
//! once per argument: `-I{@}` gives `-Ia -Ib`, and no entry at all when empty.
//!
//! Characters that a shell treats specially but this language does not (`$`,
//! backticks, globs, redirections, parentheses, a lone `&`) are rejected unless
//! escaped or quoted. `$` and backticks are rejected inside double quotes too,
//! since sh expands them there. Accepting them silently would be worse: a
//! template written as `ffmpeg -i "$1"` would hand a literal `$1` to ffmpeg.
//!
//! `aliasmux new` and `patch` run this parser when an alias is created, so
//! syntax errors (and templates that no argument count could satisfy) surface
//! then, not when the alias first runs.

use std::collections::HashMap;
use std::ffi::OsString;
use std::fmt;
use std::iter::Peekable;
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
    /// Resolve to a 1-based argument position.
    ///
    /// :param count: how many arguments there are
    /// :return: the position, or `None` if there are too few arguments
    fn position(self, count: usize) -> Option<usize> {
        match self {
            Index::Front(k) => (k <= count).then_some(k),
            Index::Back(k) => (k <= count).then(|| count + 1 - k),
        }
    }

    /// The number as written, ignoring direction; sizes the arity search.
    ///
    /// :return: N for both `{N}` and `{-N}`
    fn magnitude(self) -> usize {
        match self {
            Index::Front(k) | Index::Back(k) => k,
        }
    }

    /// The position as it is written inside braces, e.g. `-2`.
    ///
    /// :return: the text
    fn as_written(self) -> String {
        match self {
            Index::Front(k) => k.to_string(),
            Index::Back(k) => format!("-{k}"),
        }
    }
}

impl fmt::Display for Index {
    /// Show the reference as written, e.g. `{-2}`.
    ///
    /// :param f: destination formatter
    /// :return: formatting result
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{{{}}}", self.as_written())
    }
}

/// A reference that stands for several arguments.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Multi {
    /// `{@}` or `{@+}`: the arguments no `{N}`/`{-N}` uses.
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
    /// :return: a value equal for references to the same arguments
    fn set(self) -> Multi {
        match self {
            Multi::Rest { .. } => Multi::Rest { at_least_one: false },
            slice => slice,
        }
    }
}

impl fmt::Display for Multi {
    /// Show the reference as written, e.g. `{2..-2}` or `{@+}`.
    ///
    /// :param f: destination formatter
    /// :return: formatting result
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Multi::Rest { at_least_one: false } => f.write_str("{@}"),
            Multi::Rest { at_least_one: true } => f.write_str("{@+}"),
            Multi::Slice(start, end) => {
                let start = start.map(Index::as_written).unwrap_or_default();
                let end = end.map(Index::as_written).unwrap_or_default();
                // `{..}` is not valid syntax, so the all-arguments slice shows as `{1..}`.
                if start.is_empty() && end.is_empty() { f.write_str("{1..}") } else { write!(f, "{{{start}..{end}}}") }
            }
        }
    }
}

/// One fragment of a word.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Piece {
    /// Literal text from the template.
    Lit(String),
    /// One argument.
    Arg(Index),
    /// `{0}`: the alias name.
    Name,
    /// Several arguments; the word is repeated once per argument.
    Multi(Multi),
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
    ///
    /// :param r: the reference
    fn add(&mut self, r: Ref) {
        match r {
            Ref::Arg(index) if !self.indexes.contains(&index) => self.indexes.push(index),
            // `{@}` and `{@+}` are the same arguments; one `+` anywhere makes
            // at least one required, which is the safe reading.
            Ref::Multi(Multi::Rest { at_least_one }) => self.rest = Some(self.rest.unwrap_or(false) || at_least_one),
            Ref::Multi(Multi::Slice(start, end)) if !self.slices.contains(&(start, end)) => self.slices.push((start, end)),
            _ => {}
        }
    }

    /// The largest N in any `{N}`, `{-N}` or slice end.
    ///
    /// :return: the magnitude, 0 if there are none
    fn max_magnitude(&self) -> usize {
        let ends = self.slices.iter().flat_map(|(start, end)| [*start, *end]).flatten();
        self.indexes.iter().copied().chain(ends).map(Index::magnitude).max().unwrap_or(0)
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
    /// Whether a number of arguments is accepted.
    ///
    /// :param count: the number of arguments
    /// :return: true if the template can run with that many
    pub fn accepts(&self, count: usize) -> bool {
        self.counts.contains(&count) || self.from.is_some_and(|from| count >= from)
    }
}

impl fmt::Display for Arity {
    /// Describe the arity in words, e.g. "exactly 1 argument" or "2 or 4
    /// arguments".
    ///
    /// :param f: destination formatter
    /// :return: formatting result
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
            parts.push(if start == end { start.to_string() } else { format!("{start} to {end}") });
        }
        if let Some(from) = self.from {
            parts.push(format!("at least {from}"));
        }
        // Join as "a, b or c".
        let (last, init) = parts.split_last().expect("an arity accepts some count");
        let list = if init.is_empty() { last.clone() } else { format!("{} or {last}", init.join(", ")) };
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
    /// Construct an error at a byte offset.
    ///
    /// :param offset: byte offset into the source
    /// :param message: explanation for the user
    /// :return: the error
    fn new(offset: usize, message: impl Into<String>) -> Self {
        ParseError { offset: Some(offset), message: message.into() }
    }

    /// Construct an error about the template as a whole.
    ///
    /// :param message: explanation for the user
    /// :return: the error
    fn whole(message: impl Into<String>) -> Self {
        ParseError { offset: None, message: message.into() }
    }

    /// Render the message with the source line and a caret under the error.
    ///
    /// :param src: the template source the offset refers to
    /// :return: a multi-line string suitable for stderr
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
    /// Show the message alone (no source context).
    ///
    /// :param f: destination formatter
    /// :return: formatting result
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

/// An argument reference as written inside braces.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Ref {
    /// `{N}` or `{-N}`
    Arg(Index),
    /// `{0}`
    Name,
    /// `{@}`, `{@+}` or a slice
    Multi(Multi),
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
}

impl WordBuilder {
    /// Start an empty word.
    ///
    /// :param start: byte offset of the word's first character
    /// :return: the builder
    fn new(start: usize) -> Self {
        WordBuilder { start, pieces: Vec::new(), literal: String::new(), multi: None }
    }

    /// Append one literal character.
    ///
    /// :param c: the character
    fn push_char(&mut self, c: char) {
        self.literal.push(c);
    }

    /// Append an argument reference.
    ///
    /// :param r: the parsed reference
    /// :param offset: byte offset of its opening brace
    /// :return: an error if the word would mix two several-argument references
    fn push_ref(&mut self, r: Ref, offset: usize) -> Result<(), ParseError> {
        // A word is repeated once per argument of its several-argument
        // reference. Repeating the same one twice is well defined ({@}={@}
        // gives a=a b=b), but two different ones would need a cross product or
        // a zip of unequal lengths, and neither is an obvious reading.
        if let Ref::Multi(multi) = r {
            if let Some(existing) = self.multi.filter(|m| *m != multi.set()) {
                return Err(ParseError::new(
                    offset,
                    format!("a word can repeat one several-argument reference (like -I{{@}} or {{@}}={{@}}), but not combine two different ones ({existing} and {multi})"),
                ));
            }
            self.multi = Some(multi.set());
        }
        // Close off pending literal text so ordering is preserved.
        self.flush();
        self.pieces.push(match r {
            Ref::Arg(index) => Piece::Arg(index),
            Ref::Name => Piece::Name,
            Ref::Multi(multi) => Piece::Multi(multi),
        });
        Ok(())
    }

    /// Move pending literal text into the piece list.
    fn flush(&mut self) {
        // Skip empty literals so `{1}{2}` doesn't gain a useless empty piece.
        if !self.literal.is_empty() {
            self.pieces.push(Piece::Lit(std::mem::take(&mut self.literal)));
        }
    }

    /// Finish the word.
    ///
    /// :return: the word
    fn finish(mut self) -> Word {
        self.flush();
        Word { pieces: self.pieces }
    }
}

/// Parse one position: `N` or `-N`, N >= 1.
///
/// :param text: the position text
/// :param body: the whole reference body, for error messages
/// :return: the index, or a message explaining why it is invalid
fn parse_index(text: &str, body: &str) -> Result<Index, String> {
    // Only plain decimal digits after an optional `-`, so `{+1}` or `{1x}` fail loudly.
    let (back, digits) = match text.strip_prefix('-') {
        Some(digits) => (true, digits),
        None => (false, text),
    };
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return Err(format!("unknown reference `{{{body}}}`; expected {{N}}, {{-N}}, {{A..B}}, {{@}}, {{@+}} or {{0}}"));
    }
    // Overflow is the only way the parse can fail after the digit check.
    let n: usize = digits.parse().map_err(|_| format!("the number in `{{{body}}}` is too large"))?;
    // 0 is only meaningful alone, as `{0}` (handled by the caller).
    if n == 0 {
        return Err(format!("`{{{body}}}`: positions count from 1 at the front and from -1 at the back; 0 is not a position"));
    }
    Ok(if back { Index::Back(n) } else { Index::Front(n) })
}

/// Interpret the text between braces.
///
/// :param body: the text between `{` and `}`
/// :return: the reference, or a message explaining why it is invalid
fn parse_ref_body(body: &str) -> Result<Ref, String> {
    match body {
        // `{}` is common in shell idioms (find -exec), so give it a targeted hint.
        "" => Err("`{}` is not an argument reference; quote it ('{}') to pass literal braces, e.g. for find -exec".into()),
        "@" => Ok(Ref::Multi(Multi::Rest { at_least_one: false })),
        "@+" => Ok(Ref::Multi(Multi::Rest { at_least_one: true })),
        "0" => Ok(Ref::Name),
        // An open-ended slice in both directions has two spellings already
        // ({1..} and {@} when nothing else is used); a third adds nothing.
        ".." => Err("`{..}` is not a slice; write `{1..}` for every argument, or `{@}` for those no {N} or {-N} uses".into()),
        _ => match body.split_once("..") {
            Some((start, end)) => parse_slice(start, end, body),
            None => parse_index(body, body).map(Ref::Arg),
        },
    }
}

/// Parse a slice's two ends.
///
/// :param start: text before `..` (may be empty)
/// :param end: text after `..` (may be empty)
/// :param body: the whole reference body, for error messages
/// :return: the slice, or a message explaining why it is invalid
fn parse_slice(start: &str, end: &str, body: &str) -> Result<Ref, String> {
    let start = (!start.is_empty()).then(|| parse_index(start, body)).transpose()?;
    let end = (!end.is_empty()).then(|| parse_index(end, body)).transpose()?;
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
        return Err(format!("`{{{body}}}` is always empty; a slice stops before its end, so {{2..4}} is the 2nd and 3rd"));
    }
    Ok(Ref::Multi(Multi::Slice(start, end)))
}

/// Read a reference whose opening brace has just been consumed.
///
/// :param chars: the character stream, positioned just after `{`
/// :param open: byte offset of the `{`
/// :return: the reference
fn read_ref(chars: &mut Peekable<CharIndices<'_>>, open: usize) -> Result<Ref, ParseError> {
    // Collect until the closing brace; references never contain spaces or
    // nested braces, so either one means the brace was never closed.
    let mut body = String::new();
    loop {
        match chars.next() {
            Some((_, '}')) => break,
            Some((_, c)) if c.is_whitespace() || c == '{' => {
                return Err(ParseError::new(open, "unterminated `{`; write `\\{` for a literal brace"));
            }
            Some((_, c)) => body.push(c),
            None => return Err(ParseError::new(open, "unterminated `{`; write `\\{` for a literal brace")),
        }
    }
    // Errors point at the opening brace, which is where the reader looks.
    parse_ref_body(&body).map_err(|message| ParseError::new(open, message))
}

/// Push the word in progress, if any, onto the token list.
///
/// :param word: the current builder, taken (left as `None`)
/// :param tokens: the output list
fn end_word(word: &mut Option<WordBuilder>, tokens: &mut Vec<Token>) {
    // Nothing to do between words, e.g. at repeated whitespace.
    if let Some(builder) = word.take() {
        let offset = builder.start;
        tokens.push(Token { kind: TokenKind::Word(builder.finish()), offset });
    }
}

/// The lexer's output.
struct Lexed {
    /// Words and operators, in order.
    tokens: Vec<Token>,
    /// Every reference with the offset of its `{`, for whole-template checks.
    refs: Vec<(Ref, usize)>,
}

/// Split template source into words and operators.
///
/// :param src: template source
/// :return: tokens and references, in order
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
            c if c.is_whitespace() => end_word(&mut word, &mut tokens),

            // `|` or `||`: operators also end the current word, so `a|b` works.
            '|' => {
                end_word(&mut word, &mut tokens);
                let kind = if chars.next_if(|&(_, n)| n == '|').is_some() { TokenKind::Or } else { TokenKind::Pipe };
                tokens.push(Token { kind, offset });
            }

            // Only `&&` exists; a lone `&` would mean "background" in a shell.
            '&' => {
                end_word(&mut word, &mut tokens);
                if chars.next_if(|&(_, n)| n == '&').is_none() {
                    return Err(ParseError::new(offset, "a lone `&` is not supported (no background jobs); did you mean `&&`?"));
                }
                tokens.push(Token { kind: TokenKind::And, offset });
            }

            // `;` sequences unconditionally.
            ';' => {
                end_word(&mut word, &mut tokens);
                tokens.push(Token { kind: TokenKind::Then, offset });
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
                            Some((_, ch @ ('"' | '\\' | '{' | '}' | '$' | '`'))) => builder.push_char(ch),
                            Some((_, ch)) => {
                                builder.push_char('\\');
                                builder.push_char(ch);
                            }
                            None => return Err(ParseError::new(offset, "unterminated double quote")),
                        },
                        // References work inside double quotes, so "{1}.mkv" is fine.
                        Some((open, '{')) => {
                            let r = read_ref(&mut chars, open)?;
                            builder.push_ref(r, open)?;
                            refs.push((r, open));
                        }
                        // `$` and backticks expand inside double quotes in sh, so
                        // "$1" is the shell habit most likely to slip through;
                        // treating it as literal would silently pass `$1` on.
                        Some((at, ch @ ('$' | '`'))) => {
                            return Err(ParseError::new(
                                at,
                                format!("`{ch}` is not expanded here, even in double quotes; use `{{1}}`, `{{2}}`, ... for arguments, or write `\\{ch}` (or single quotes) to pass it literally"),
                            ));
                        }
                        Some((_, ch)) => builder.push_char(ch),
                        None => return Err(ParseError::new(offset, "unterminated double quote")),
                    }
                }
            }

            // Unquoted backslash makes the next character literal, whatever it is.
            '\\' => match chars.next() {
                Some((_, ch)) => word.get_or_insert_with(|| WordBuilder::new(offset)).push_char(ch),
                None => return Err(ParseError::new(offset, "trailing backslash escapes nothing")),
            },

            // An argument reference, possibly glued to text: {1}.rot.flif
            '{' => {
                let r = read_ref(&mut chars, offset)?;
                word.get_or_insert_with(|| WordBuilder::new(offset)).push_ref(r, offset)?;
                refs.push((r, offset));
            }

            // A stray `}` is almost always a typo like `{1}}`, so refuse it
            // rather than pass it through.
            '}' => return Err(ParseError::new(offset, "unmatched `}`; write `\\}` for a literal brace")),

            // `$` gets its own hint, since it is what shell habits will produce.
            '$' => {
                return Err(ParseError::new(
                    offset,
                    "`$` is not expanded here; use `{1}`, `{2}`, ... for arguments, or quote it ('$') to pass it literally",
                ));
            }

            // Other shell syntax that would silently mean something else here.
            '`' | '(' | ')' | '<' | '>' | '*' | '?' => {
                return Err(ParseError::new(
                    offset,
                    format!("`{c}` has no special meaning in alias templates (no subshells, redirection or globbing); quote or escape it to pass it literally"),
                ));
            }

            // Anything else is ordinary word text.
            _ => word.get_or_insert_with(|| WordBuilder::new(offset)).push_char(c),
        }
    }
    // The last word has no trailing separator to end it.
    end_word(&mut word, &mut tokens);
    Ok(Lexed { tokens, refs })
}

/// Recursive-descent parser over the token list.
struct Parser {
    tokens: Peekable<std::vec::IntoIter<Token>>,
    /// Source length, used as the error offset at end of input.
    end: usize,
}

impl Parser {
    /// Offset of the next token, or end of input; where "expected X" points.
    ///
    /// :return: byte offset
    fn here(&mut self) -> usize {
        self.tokens.peek().map_or(self.end, |t| t.offset)
    }

    /// Parse `command := word+`.
    ///
    /// :return: the command
    fn command(&mut self) -> Result<Command, ParseError> {
        // Take words until an operator or end of input.
        let mut words = Vec::new();
        while let Some(token) = self.tokens.next_if(|t| matches!(t.kind, TokenKind::Word(_))) {
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
    ///
    /// :return: the pipeline
    fn pipeline(&mut self) -> Result<Pipeline, ParseError> {
        let mut stages = vec![self.command()?];
        // Each `|` must be followed by another command.
        while self.tokens.next_if(|t| matches!(t.kind, TokenKind::Pipe)).is_some() {
            stages.push(self.command()?);
        }
        Ok(Pipeline { stages })
    }

    /// Parse `list := pipeline (op pipeline)* ";"?`.
    ///
    /// :return: head pipeline and the operator-tagged rest
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
                _ => return Err(ParseError::new(token.offset, "internal error: unexpected token")),
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

/// Reject `{N}` / `{-N}` numbering with gaps.
///
/// Every argument must be used, so `{2}` without `{1}` is almost certainly a
/// typo; a slice covering the gap would technically use it, but numbering
/// without gaps is the clearer rule.
///
/// :param refs: every reference with its offset
/// :return: an error pointing at the highest-numbered reference of a gappy run
fn check_gaps(refs: &[(Ref, usize)]) -> Result<(), ParseError> {
    // Front and back numbering are checked separately, with the same rule.
    for (back, sign) in [(false, ""), (true, "-")] {
        // The numbers used in this direction, and where the highest first appears.
        let used: Vec<(usize, usize)> = refs
            .iter()
            .filter_map(|&(r, offset)| match r {
                Ref::Arg(Index::Front(k)) if !back => Some((k, offset)),
                Ref::Arg(Index::Back(k)) if back => Some((k, offset)),
                _ => None,
            })
            .collect();
        let Some(&(highest, at)) = used.iter().max_by_key(|&&(k, offset)| (k, std::cmp::Reverse(offset))) else { continue };
        if let Some(missing) = (1..highest).find(|k| !used.iter().any(|&(u, _)| u == *k)) {
            return Err(ParseError::new(
                at,
                format!("`{{{sign}{highest}}}` is used but `{{{sign}{missing}}}` is not; every argument must be used, so number them without gaps"),
            ));
        }
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
/// :param src: template source
/// :return: the parsed template
pub fn parse(src: &str) -> Result<Template, ParseError> {
    // Lex fully first; the parser then never deals with characters.
    let Lexed { tokens, refs } = lex(src)?;
    // An empty or all-whitespace template has no command to run.
    if tokens.is_empty() {
        return Err(ParseError::new(0, "empty template"));
    }
    let mut parser = Parser { tokens: tokens.into_iter().peekable(), end: src.len() };
    let (mut head, tail) = parser.list()?;
    check_gaps(&refs)?;

    // Collect the distinct references, applying the implicit-`{@}` rule above.
    let mut shape = Shape::default();
    for &(r, _) in &refs {
        shape.add(r);
    }
    let implicit_rest = refs.is_empty() && tail.is_empty() && head.stages.len() == 1;
    if implicit_rest {
        let rest = Multi::Rest { at_least_one: false };
        head.stages[0].words.push(Word { pieces: vec![Piece::Multi(rest)] });
        shape.add(Ref::Multi(rest));
    }
    let arity = compute_arity(&shape)?;
    Ok(Template { head, tail, implicit_rest, shape, arity })
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
    /// The positions a several-argument reference stands for.
    ///
    /// :param multi: the reference
    /// :return: 1-based positions, in order
    fn positions(&self, multi: Multi) -> Vec<usize> {
        match multi {
            Multi::Rest { .. } => self.rest.clone(),
            // Present for every slice of the template once the layout is valid.
            Multi::Slice(start, end) => self.slices.get(&(start, end)).map_or_else(Vec::new, |&(lo, hi)| (lo..=hi).collect()),
        }
    }
}

/// Where a slice's end falls: the slice stops just before this position.
///
/// :param end: the end as written, or `None` for "through the last"
/// :param count: number of arguments
/// :return: the stopping position (up to count + 1), or `None` if the end
///     names an argument that can't exist at this count
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

/// A slice's first and last position, if it is non-empty.
///
/// :param slice: (start, end) ends; the end is exclusive
/// :param count: number of arguments
/// :return: first and last position, both included, or `None` if empty
fn slice_range(slice: (Option<Index>, Option<Index>), count: usize) -> Option<(usize, usize)> {
    let lo = slice.0.map_or(Some(1), |i| i.position(count))?;
    let stop = stop_position(slice.1, count)?;
    // lo >= 1 and stop <= count + 1, so a non-empty range is in bounds.
    (lo < stop).then_some((lo, stop - 1))
}

/// Describe a set of positions for messages, e.g. "arguments 1 and 2".
///
/// :param positions: ascending positions, non-empty
/// :return: the phrase
fn describe_positions(positions: &[usize]) -> String {
    let words: Vec<String> = positions.iter().map(usize::to_string).collect();
    match words.split_last() {
        Some((last, [])) => format!("argument {last}"),
        Some((last, init)) => format!("arguments {} and {last}", init.join(", ")),
        None => "no arguments".into(),
    }
}

/// Why one argument count doesn't work.
enum Problem {
    /// These positions would be used by nothing. Kept as positions rather
    /// than text, so the whole-template error can describe them without
    /// naming a particular count (see `describe_unused`).
    Unused(Vec<usize>),
    /// Anything else, already phrased.
    Other(String),
}

/// Lay out the references for one argument count, checking the rules that
/// don't involve comparing counts: indexes in range and distinct, `{@+}` and
/// slices non-empty, every argument used.
///
/// :param shape: the template's references
/// :param count: number of arguments
/// :return: the layout, or why this count doesn't work
fn layout(shape: &Shape, count: usize) -> Result<Layout, Problem> {
    // Each position may belong to at most one index ({N}/{-N}); {@} takes
    // whatever is left.
    let mut owner: Vec<Option<Index>> = vec![None; count + 1];
    let mut index = HashMap::new();
    for &i in &shape.indexes {
        let position = i.position(count).ok_or_else(|| Problem::Other(format!("{i} needs at least {} arguments", i.magnitude())))?;
        if let Some(other) = owner[position] {
            return Err(Problem::Other(format!("{other} and {i} would both be argument {position}")));
        }
        owner[position] = Some(i);
        index.insert(i, position);
    }
    // {@}: every position no index owns.
    let rest: Vec<usize> = if shape.rest.is_some() { (1..=count).filter(|&p| owner[p].is_none()).collect() } else { Vec::new() };
    if shape.rest == Some(true) && rest.is_empty() {
        return Err(Problem::Other("{@+} would be empty".into()));
    }
    // Slices must be non-empty; that is what makes {2..} safe for `rm`.
    let mut slices = HashMap::new();
    for &slice in &shape.slices {
        let range = slice_range(slice, count).ok_or_else(|| Problem::Other(format!("{} would be empty", Multi::Slice(slice.0, slice.1))))?;
        slices.insert(slice, range);
    }
    // Without {@}, every argument must be claimed by an index or covered by a
    // slice; an argument that nothing uses is almost always a mistake (the
    // `flif270 *.flif` case: silently processing only the first file).
    if shape.rest.is_none() {
        let unused: Vec<usize> =
            (1..=count).filter(|&p| owner[p].is_none() && !slices.values().any(|&(lo, hi)| lo <= p && p <= hi)).collect();
        if !unused.is_empty() {
            return Err(Problem::Unused(unused));
        }
    }
    Ok(Layout { index, rest, slices })
}

/// How the slices relate to the indexes and to each other at one count:
/// which indexes each slice contains, and which slices overlap or contain
/// which. Empty slices and out-of-range indexes count as "not contained".
///
/// :param shape: the template's references
/// :param count: number of arguments
/// :return: a comparable signature
fn relations(shape: &Shape, count: usize) -> Vec<bool> {
    let ranges: Vec<Option<(usize, usize)>> = shape.slices.iter().map(|&s| slice_range(s, count)).collect();
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
            let overlap = matches!((first, second), (Some((l1, h1)), Some((l2, h2))) if l1 <= h2 && l2 <= h1);
            let contains = |outer: Option<(usize, usize)>, inner: Option<(usize, usize)>| {
                matches!((outer, inner), (Some((lo, hi)), Some((l, h))) if lo <= l && h <= hi)
            };
            signature.extend([overlap, contains(first, second), contains(second, first)]);
        }
    }
    signature
}

/// Check every rule for one count, including that slices relate to the
/// other references as they do with plenty of arguments.
///
/// :param shape: the template's references
/// :param count: number of arguments
/// :param plenty: a count at which every relation has settled (see `compute_arity`)
/// :return: the layout, or why this count doesn't work
fn check_count(shape: &Shape, count: usize, plenty: usize) -> Result<Layout, Problem> {
    let layout = layout(shape, count)?;
    // E.g. in `{1..4} {-1}`, with 3 arguments the slice would take the last
    // one too, which nobody writing that template intends.
    if relations(shape, count) != relations(shape, plenty) {
        return Err(Problem::Other(format!("with {count} arguments, a slice would share different arguments with the other references than with more")));
    }
    Ok(layout)
}

/// Work out which argument counts a template accepts.
///
/// Let K be the largest number in any reference. From 2K+1 arguments on,
/// front-counted and back-counted positions can never meet and at least one
/// argument sits between them, so every count from there up behaves the same
/// as 2K+1 (the extra arguments are all "middle" ones, treated alike). Checking
/// the counts 0 to 2K+1 therefore decides every count.
///
/// :param shape: the template's references
/// :return: the arity, or an error if no count works
fn compute_arity(shape: &Shape) -> Result<Arity, ParseError> {
    let plenty = 2 * shape.max_magnitude() + 1;
    let mut counts: Vec<usize> = (0..plenty).filter(|&n| check_count(shape, n, plenty).is_ok()).collect();
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
            Err(Problem::Other(text)) => text,
            // Unreachable: `from` would be set.
            Ok(_) => String::new(),
        };
        return Err(ParseError::whole(format!("no number of arguments works with this template: {reason}")));
    }
    Ok(Arity { counts, from })
}

/// Describe arguments that would never be used, without naming a count.
///
/// At `plenty` (2K+1), positions 1..=K are only reachable as `{N}`, the last K
/// only as `{-N}`, and the one in between stands for every middle argument;
/// so each unused position has a count-independent name.
///
/// :param shape: the template's references
/// :param positions: unused positions at `plenty`, ascending
/// :param plenty: the count they were found at
/// :return: e.g. "argument 1 and the last argument would never be used"
fn describe_unused(shape: &Shape, positions: &[usize], plenty: usize) -> String {
    let k = shape.max_magnitude();
    let front: Vec<usize> = positions.iter().copied().filter(|&p| p <= k).collect();
    let back: Vec<usize> = positions.iter().filter(|&&p| p > plenty - k).map(|&p| plenty + 1 - p).collect();
    let middle = positions.iter().any(|&p| p > k && p <= plenty - k);
    // Front first, then the middle, then the back, in argument order.
    let mut parts = Vec::new();
    if !front.is_empty() {
        parts.push(describe_positions(&front));
    }
    if middle {
        parts.push("the arguments in between".to_owned());
    }
    parts.extend(back.iter().map(|&j| if j == 1 { "the last argument".to_owned() } else { format!("argument -{j}") }));
    let (last, init) = parts.split_last().expect("there is an unused position");
    let list = if init.is_empty() { last.clone() } else { format!("{} and {last}", init.join(", ")) };
    // The usual cause of an unused tail: a slice ending at {-N}, read as if
    // it included its end. Show the spelling that runs through the last.
    let hint = shape
        .slices
        .iter()
        .find(|(_, end)| matches!(end, Some(Index::Back(_))))
        .filter(|_| !back.is_empty())
        .map(|&(start, _)| format!("; a slice stops before its end, so {} runs through the last argument", Multi::Slice(start, None)))
        .unwrap_or_default();
    format!("{list} would never be used{hint}")
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
    /// The argv when the plan is exactly one command (no pipes or operators).
    ///
    /// :return: the single argv, or `None` if more than one process is involved
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
}

impl fmt::Display for ExpandError {
    /// Describe the problem for the user.
    ///
    /// :param f: destination formatter
    /// :return: formatting result
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ExpandError::WrongArgCount { arity, got } => write!(f, "expected {arity}, got {got}"),
            ExpandError::EmptyCommand => f.write_str("a command expanded to nothing (no program to run)"),
        }
    }
}

impl Template {
    /// True when the template is one command with no pipes or operators.
    ///
    /// :return: whether the template is simple
    pub fn is_simple(&self) -> bool {
        self.tail.is_empty() && self.head.stages.len() == 1
    }

    /// How many arguments the template accepts.
    ///
    /// :return: the arity
    pub fn arity(&self) -> &Arity {
        &self.arity
    }

    /// Substitute runtime arguments into the template.
    ///
    /// :param name: the alias name, for `{0}`
    /// :param args: runtime arguments, excluding the alias name
    /// :return: the plan to run
    pub fn expand(&self, name: &str, args: &[OsString]) -> Result<Plan, ExpandError> {
        // Check the count up front; an accepted count always has a layout.
        let wrong = || ExpandError::WrongArgCount { arity: self.arity.clone(), got: args.len() };
        if !self.arity.accepts(args.len()) {
            return Err(wrong());
        }
        let layout = layout(&self.shape, args.len()).map_err(|_| wrong())?;
        // Expand each pipeline stage by stage.
        let expand_pipeline = |p: &Pipeline| -> Result<Vec<Argv>, ExpandError> {
            p.stages.iter().map(|c| expand_command(c, &layout, name, args)).collect()
        };
        let head = expand_pipeline(&self.head)?;
        let tail = self.tail.iter().map(|(op, p)| Ok((*op, expand_pipeline(p)?))).collect::<Result<_, _>>()?;
        Ok(Plan { head, tail })
    }
}

/// Expand one command.
///
/// :param command: the parsed command
/// :param layout: where each reference lands for this argument count
/// :param name: the alias name, for `{0}`
/// :param args: runtime arguments; already checked against the arity
/// :return: the argv
fn expand_command(command: &Command, layout: &Layout, name: &str, args: &[OsString]) -> Result<Argv, ExpandError> {
    let mut argv = Vec::with_capacity(command.words.len());
    for word in &command.words {
        // A several-argument reference repeats the word once per argument it
        // stands for (the parser allows only one distinct one per word).
        let multi = word.pieces.iter().find_map(|p| match p {
            Piece::Multi(m) => Some(*m),
            _ => None,
        });
        match multi {
            None => argv.push(build_entry(word, layout, name, args, None)),
            Some(multi) => {
                for position in layout.positions(multi) {
                    argv.push(build_entry(word, layout, name, args, Some(position)));
                }
            }
        }
    }
    // Only possible when every word of the command repeated zero times.
    if argv.is_empty() { Err(ExpandError::EmptyCommand) } else { Ok(argv) }
}

/// Build one argv entry from a word.
///
/// :param word: the word
/// :param layout: where each reference lands
/// :param name: the alias name, for `{0}`
/// :param args: runtime arguments
/// :param repeat: for a repeated word, the position this copy stands for
/// :return: the entry, as OsString so non-UTF-8 filenames survive
fn build_entry(word: &Word, layout: &Layout, name: &str, args: &[OsString], repeat: Option<usize>) -> OsString {
    let mut entry = OsString::new();
    for piece in &word.pieces {
        match piece {
            Piece::Lit(text) => entry.push(text),
            Piece::Name => entry.push(name),
            // Positions are 1-based and in range: the layout checked them.
            Piece::Arg(index) => entry.push(&args[layout.index[index] - 1]),
            Piece::Multi(_) => entry.push(&args[repeat.expect("repeated words get a position") - 1]),
        }
    }
    entry
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build an argv list from string literals.
    ///
    /// :param items: the strings
    /// :return: owned OsStrings
    fn os(items: &[&str]) -> Vec<OsString> {
        items.iter().map(OsString::from).collect()
    }

    /// Parse and expand in one go, panicking on any error.
    ///
    /// :param src: template source
    /// :param args: runtime arguments
    /// :return: the plan
    fn plan(src: &str, args: &[&str]) -> Plan {
        parse(src).unwrap().expand("t", &os(args)).unwrap()
    }

    /// The single command a simple template expands to.
    ///
    /// :param src: template source
    /// :param args: runtime arguments
    /// :return: its argv
    fn argv(src: &str, args: &[&str]) -> Argv {
        plan(src, args).head[0].clone()
    }

    /// The arity of a template, as text.
    ///
    /// :param src: template source
    /// :return: e.g. "exactly 1 argument"
    fn arity(src: &str) -> String {
        parse(src).unwrap_or_else(|e| panic!("{src}: {e}")).arity().to_string()
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
        assert!(matches!(t.expand("t", &os(&["x"])), Err(ExpandError::WrongArgCount { .. })));
    }

    #[test]
    fn flif270_shape() {
        // The motivating example: one argument used four times across stages.
        let src = "flif -d {1} - | convert -rotate 270 - - | flif -E 14 - {1}.rot.flif && mv {1}.rot.flif {1}";
        let p = plan(src, &["my pic.flif"]);
        assert_eq!(p.head.len(), 3);
        assert_eq!(p.head[0], os(&["flif", "-d", "my pic.flif", "-"]));
        assert_eq!(p.head[2], os(&["flif", "-E", "14", "-", "my pic.flif.rot.flif"]));
        assert_eq!(p.tail, vec![(Op::And, vec![os(&["mv", "my pic.flif.rot.flif", "my pic.flif"])])]);
    }

    #[test]
    fn quoting_rules() {
        // Single quotes are fully literal, braces and backslashes included.
        assert_eq!(argv(r"echo 'a {1} \n'", &[]), os(&["echo", r"a {1} \n"]));
        // Double quotes expand references and keep unknown escapes literal.
        assert_eq!(argv(r#"echo "{1} \d \" \{""#, &["x"]), os(&["echo", r#"x \d " {"#]));
        // Adjacent quoted and unquoted parts form one word.
        assert_eq!(argv(r#"echo a'b c'"d"{1}"#, &["!"]), os(&["echo", "ab cd!"]));
        // Empty quotes produce an empty argument rather than nothing.
        assert_eq!(argv("printf %s '' {1}", &["x"]), os(&["printf", "%s", "", "x"]));
    }

    #[test]
    fn back_indexes() {
        // {-1} is the last argument, wherever it is.
        assert_eq!(argv("cp -t {-1} {@+}", &["a", "b", "dir"]), os(&["cp", "-t", "dir", "a", "b"]));
        assert_eq!(arity("cp -t {-1} {@+}"), "at least 2 arguments");
        // Front and back together: exact, and never the same argument twice.
        assert_eq!(arity("mv {1} {-1}"), "exactly 2 arguments");
    }

    #[test]
    fn rest_is_what_indexes_leave() {
        // {@} excludes arguments that {N}/{-N} use, so nothing is passed twice.
        assert_eq!(argv("cmd {1} -- {@}", &["a", "b", "c"]), os(&["cmd", "a", "--", "b", "c"]));
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
        assert_eq!(argv("echo {1} {2..4}", &["a", "b", "c"]), os(&["echo", "a", "b", "c"]));
        assert_eq!(argv("echo {..-1} -- {-1}", &["a", "b", "c"]), os(&["echo", "a", "b", "--", "c"]));
        // The case that settled it: the last argument, then all the others.
        assert_eq!(argv("echo {-1} {..-1}", &["1", "2", "3", "4", "5"]), os(&["echo", "5", "1", "2", "3", "4"]));
        // {2..} needs at least one argument to stand for: `rm {1} {2..}` with
        // one argument is an error, not `rm a`.
        assert_eq!(arity("cmd {1} {2..}"), "at least 2 arguments");
        // Slices claim nothing, so they may overlap indexes on purpose.
        assert_eq!(argv("echo {1} {1..}", &["a", "b"]), os(&["echo", "a", "a", "b"]));
    }

    #[test]
    fn slice_overlap_must_not_shift_with_the_count() {
        // With 3 the slice would swallow {-1}; with 5 the 4th would be unused.
        assert_eq!(arity("cmd {1..4} {-1}"), "exactly 4 arguments");
        let t = parse("cmd {1..4} {-1}").unwrap();
        assert_eq!(t.expand("t", &os(&["a", "b", "c", "d"])).unwrap().head[0], os(&["cmd", "a", "b", "c", "d"]));
    }

    #[test]
    fn words_repeat_per_argument() {
        // -I{@} gives one -I per argument, and nothing at all for none.
        assert_eq!(argv("gcc -I{@} -c {-1}", &["a", "b", "x.c"]), os(&["gcc", "-Ia", "-Ib", "-c", "x.c"]));
        assert_eq!(argv("gcc -I{@} -c {-1}", &["x.c"]), os(&["gcc", "-c", "x.c"]));
        // The same reference twice in one word stays in step.
        assert_eq!(argv("env {@}={@}", &["a", "b"]), os(&["env", "a=a", "b=b"]));
        // So does a slice written two ways ({1..-1} is {..-1}).
        assert_eq!(argv("echo {1..-1}:{..-1} {-1}", &["a", "b", "c"]), os(&["echo", "a:a", "b:b", "c"]));
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
        assert_eq!(p.tail.iter().map(|(op, _)| *op).collect::<Vec<_>>(), vec![Op::And, Op::Or, Op::Then]);
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
        let odd = Arity { counts: vec![2, 4, 5], from: Some(7) };
        assert_eq!(odd.to_string(), "2, 4 to 5 or at least 7 arguments");
        assert!(odd.accepts(5) && !odd.accepts(6) && odd.accepts(70));
    }

    #[test]
    fn empty_command_from_rest() {
        // A command that is only {@} needs at least one argument to run.
        assert_eq!(parse("{@}").unwrap().expand("t", &[]), Err(ExpandError::EmptyCommand));
    }

    #[test]
    fn templates_no_count_can_satisfy() {
        // Arguments 1 and 2 are never used, whatever the count.
        let err = parse("cmd {3..}").unwrap_err();
        assert_eq!(err.offset, None);
        assert!(err.message.ends_with(": arguments 1 and 2 would never be used"), "{}", err.message);
        // A slice ending at {-1} leaves the last argument out; the message
        // names it without an internal count, and gives the right spelling.
        let err = parse("echo a {2..-1}").unwrap_err();
        assert_eq!(
            err.message,
            "no number of arguments works with this template: argument 1 and the last argument would never be used; \
             a slice stops before its end, so {2..} runs through the last argument"
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
        assert!(parse("echo {2..2}").unwrap_err().message.contains("stops before its end"));
    }

    #[test]
    fn escaped_specials_are_literal() {
        // Escaping or quoting gets any reserved character through verbatim.
        assert_eq!(argv(r"find . -exec rm '{}' \;", &[]), os(&["find", ".", "-exec", "rm", "{}", ";"]));
        assert_eq!(argv(r"echo \$HOME '*' \{1\}", &[]), os(&["echo", "$HOME", "*", "{1}"]));
        // Inside double quotes `$` and backticks need a backslash, as in sh;
        // single quotes keep them literal with no escaping at all.
        assert_eq!(argv(r#"echo "\$1 \`x\`" '$2'"#, &[]), os(&["echo", "$1 `x`", "$2"]));
    }

    #[test]
    fn non_utf8_arguments_survive() {
        use std::os::unix::ffi::OsStrExt;
        // Filenames need not be UTF-8 on Unix; they must pass through byte-exact.
        let raw = std::ffi::OsStr::from_bytes(b"caf\xe9.mkv").to_os_string();
        let p = parse("ffmpeg -i {1} {1}.out").unwrap().expand("t", std::slice::from_ref(&raw)).unwrap();
        assert_eq!(p.head[0][2], raw);
        assert_eq!(p.head[0][3].as_bytes(), b"caf\xe9.mkv.out");
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
