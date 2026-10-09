//! Parser and expander for alias templates.
//!
//! A template is a tiny shell-*like* command language. It supports only what an
//! alias needs, so that no shell is ever involved at run time:
//!
//! * words, split on unquoted whitespace
//! * `'single'` quotes (fully literal) and `"double"` quotes (literal except for
//!   argument references and the escapes `\"`, `\\`, `\{`, `\}`, `\$`, `` \` ``)
//! * `\x` outside quotes, which makes `x` literal
//! * argument references: `{N}` (the Nth argument, 1-based), `{N..}` (the Nth
//!   argument onward, one word each) and `{@}` (the same as `{1..}`)
//! * operators: `|` (pipe), `&&`, `||` and `;`, with shell precedence (pipes
//!   bind tighter; the other three are left-associative and equal)
//!
//! Characters that a shell treats specially but this language does not (`$`,
//! backticks, globs, redirections, parentheses, a lone `&`) are rejected unless
//! escaped or quoted. `$` and backticks are rejected inside double quotes too,
//! since sh expands them there. Accepting them silently would be worse: a
//! template written as `ffmpeg -i "$1"` would hand a literal `$1` to ffmpeg.
//!
//! `aliasmux new` and `patch` run this parser when an alias is created, so
//! syntax errors surface then, not when the alias first runs.

use std::ffi::OsString;
use std::fmt;
use std::iter::Peekable;
use std::str::CharIndices;

/// One fragment of a word: literal text, or a single argument substituted in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Piece {
    /// Literal text from the template.
    Lit(String),
    /// The Nth runtime argument, 1-based (so `Arg(1)` is `{1}`).
    Arg(usize),
}

/// A word of a command after parsing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Word {
    /// Pieces concatenated into exactly one argv entry. An empty list is a
    /// deliberate empty-string argument (written `''` or `""`).
    Concat(Vec<Piece>),
    /// Runtime arguments from this 1-based index onward, one argv entry each.
    /// Expands to zero entries when there are no such arguments.
    Splat(usize),
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

/// A fully parsed template.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Template {
    /// The first pipeline.
    pub head: Pipeline,
    /// Every following pipeline with the operator that precedes it.
    pub tail: Vec<(Op, Pipeline)>,
    /// True when the parser appended `{@}` itself (see [`parse`]).
    pub implicit_splat: bool,
}

/// How many runtime arguments a template accepts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Arity {
    /// Fewest arguments accepted.
    pub min: usize,
    /// Most arguments accepted; `None` when a splat absorbs any extras.
    pub max: Option<usize>,
}

impl fmt::Display for Arity {
    /// Describe the arity in words, e.g. "exactly 1 argument".
    ///
    /// :param f: destination formatter
    /// :return: formatting result
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Pick the singular form only for exactly one, matching English usage.
        let noun = |n: usize| if n == 1 { "argument" } else { "arguments" };
        // A missing maximum means a splat is present, so only a floor applies.
        match self.max {
            Some(max) => write!(f, "exactly {max} {}", noun(max)),
            None => write!(f, "at least {} {}", self.min, noun(self.min)),
        }
    }
}

/// A syntax error, located by byte offset into the template source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseError {
    /// Byte offset of the offending character (or the source length at end of
    /// input). Always on a char boundary, since it comes from `char_indices`.
    pub offset: usize,
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
        ParseError { offset, message: message.into() }
    }

    /// Render the message with the source line and a caret under the error.
    ///
    /// :param src: the template source the offset refers to
    /// :return: a multi-line string suitable for stderr
    pub fn render(&self, src: &str) -> String {
        // Count chars, not bytes, so the caret lines up under multibyte text.
        // Assumption: the terminal shows one column per char, which is wrong for
        // wide CJK glyphs; acceptable for an error caret.
        let column = src[..self.offset].chars().count();
        // Indent both lines equally so the caret sits under the source text.
        format!("{}\n    {}\n    {}^", self.message, src, " ".repeat(column))
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
    /// `{N}`
    Arg(usize),
    /// `{N..}` or `{@}`
    Splat(usize),
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
    /// A splat seen in this word, with its offset for error reporting.
    splat: Option<(usize, usize)>,
}

impl WordBuilder {
    /// Start an empty word.
    ///
    /// :param start: byte offset of the word's first character
    /// :return: the builder
    fn new(start: usize) -> Self {
        WordBuilder { start, pieces: Vec::new(), literal: String::new(), splat: None }
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
    /// :return: an error if the word would contain two splats
    fn push_ref(&mut self, r: Ref, offset: usize) -> Result<(), ParseError> {
        match r {
            Ref::Arg(n) => {
                // Close off pending literal text so ordering is preserved.
                self.flush();
                self.pieces.push(Piece::Arg(n));
            }
            Ref::Splat(from) => {
                // A second splat would otherwise silently replace the first.
                if self.splat.is_some() {
                    return Err(ParseError::new(offset, "only one `{N..}` / `{@}` is allowed per word"));
                }
                self.splat = Some((from, offset));
            }
        }
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
    /// :return: the word, or an error if a splat was mixed with other content
    fn finish(mut self) -> Result<Word, ParseError> {
        self.flush();
        match self.splat {
            // A splat yields a variable number of words, so it cannot also be
            // glued to text: `x{@}` has no single sensible meaning. (Shells
            // glue the prefix to the first element only, a frequent surprise.)
            Some((from, offset)) => {
                if self.pieces.is_empty() {
                    Ok(Word::Splat(from))
                } else {
                    Err(ParseError::new(offset, "`{N..}` / `{@}` must be a whole word on its own, not joined to other text"))
                }
            }
            // No splat: everything concatenates into one argv entry.
            None => Ok(Word::Concat(self.pieces)),
        }
    }
}

/// Interpret the text between braces.
///
/// :param body: the text between `{` and `}`
/// :return: the reference, or a message explaining why it is invalid
fn parse_ref_body(body: &str) -> Result<Ref, String> {
    // `{}` is common in shell idioms (find -exec), so give it a targeted hint.
    if body.is_empty() {
        return Err("`{}` is not an argument reference; quote it ('{}') to pass literal braces, e.g. for find -exec".into());
    }
    // `{@}` is shorthand for "all arguments".
    if body == "@" {
        return Ok(Ref::Splat(1));
    }
    // A trailing `..` turns `{N}` into "N onward".
    let (digits, is_splat) = match body.strip_suffix("..") {
        Some(digits) => (digits, true),
        None => (body, false),
    };
    // Only plain decimal digits are accepted, so `{+1}` or `{1x}` fail loudly.
    let unknown = || format!("unknown reference `{{{body}}}`; expected `{{N}}`, `{{N..}}` or `{{@}}`");
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return Err(unknown());
    }
    // Overflow is the only way the parse can fail after the digit check.
    let n: usize = digits.parse().map_err(|_| format!("argument number in `{{{body}}}` is too large"))?;
    // There is no `{0}`: unlike bash's `$0`, the alias name isn't an argument.
    if n == 0 {
        return Err("arguments are numbered from 1; there is no `{0}`".into());
    }
    Ok(if is_splat { Ref::Splat(n) } else { Ref::Arg(n) })
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
/// :return: an error if the word is malformed
fn end_word(word: &mut Option<WordBuilder>, tokens: &mut Vec<Token>) -> Result<(), ParseError> {
    // Nothing to do between words, e.g. at repeated whitespace.
    if let Some(builder) = word.take() {
        let offset = builder.start;
        tokens.push(Token { kind: TokenKind::Word(builder.finish()?), offset });
    }
    Ok(())
}

/// Split template source into words and operators.
///
/// :param src: template source
/// :return: tokens in order
fn lex(src: &str) -> Result<Vec<Token>, ParseError> {
    let mut tokens = Vec::new();
    let mut chars = src.char_indices().peekable();
    // `Some` while inside a word. A word starts at its first character, which
    // may be a quote; that is how `''` becomes an (empty) word at all.
    let mut word: Option<WordBuilder> = None;

    while let Some((offset, c)) = chars.next() {
        match c {
            // Unquoted whitespace separates words and nothing else.
            c if c.is_whitespace() => end_word(&mut word, &mut tokens)?,

            // `|` or `||`: operators also end the current word, so `a|b` works.
            '|' => {
                end_word(&mut word, &mut tokens)?;
                let kind = if chars.next_if(|&(_, n)| n == '|').is_some() { TokenKind::Or } else { TokenKind::Pipe };
                tokens.push(Token { kind, offset });
            }

            // Only `&&` exists; a lone `&` would mean "background" in a shell.
            '&' => {
                end_word(&mut word, &mut tokens)?;
                if chars.next_if(|&(_, n)| n == '&').is_none() {
                    return Err(ParseError::new(offset, "a lone `&` is not supported (no background jobs); did you mean `&&`?"));
                }
                tokens.push(Token { kind: TokenKind::And, offset });
            }

            // `;` sequences unconditionally.
            ';' => {
                end_word(&mut word, &mut tokens)?;
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
    end_word(&mut word, &mut tokens)?;
    Ok(tokens)
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

/// Parse template source.
///
/// If the template is a single command with no argument references at all, a
/// `{@}` is appended, so `ll = ls -la` passes its arguments through like a
/// shell alias. This is the one implicit rule; it only applies where the
/// meaning is unambiguous. (With several commands, there is no obvious one to
/// receive the arguments, so none does and extra arguments are an error.)
///
/// :param src: template source
/// :return: the parsed template
pub fn parse(src: &str) -> Result<Template, ParseError> {
    // Lex fully first; the parser then never deals with characters.
    let tokens = lex(src)?;
    // An empty or all-whitespace template has no command to run.
    if tokens.is_empty() {
        return Err(ParseError::new(0, "empty template"));
    }
    let mut parser = Parser { tokens: tokens.into_iter().peekable(), end: src.len() };
    let (head, tail) = parser.list()?;
    let mut template = Template { head, tail, implicit_splat: false };

    // Apply the implicit-`{@}` rule documented above. An arity of exactly zero
    // means no references of either kind, since any splat makes `max` `None`.
    let no_refs = template.arity() == Arity { min: 0, max: Some(0) };
    if no_refs && template.is_simple() {
        template.head.stages[0].words.push(Word::Splat(1));
        template.implicit_splat = true;
    }
    Ok(template)
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
    /// Too few or too many arguments.
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
    /// Every pipeline, head first.
    ///
    /// :return: iterator over pipelines
    fn pipelines(&self) -> impl Iterator<Item = &Pipeline> {
        std::iter::once(&self.head).chain(self.tail.iter().map(|(_, p)| p))
    }

    /// Every word of every command, in source order.
    ///
    /// :return: iterator over words
    fn words(&self) -> impl Iterator<Item = &Word> {
        self.pipelines().flat_map(|p| p.stages.iter()).flat_map(|c| c.words.iter())
    }

    /// True when the template is one command with no pipes or operators.
    ///
    /// :return: whether the template is simple
    pub fn is_simple(&self) -> bool {
        self.tail.is_empty() && self.head.stages.len() == 1
    }

    /// Work out how many arguments the template accepts.
    ///
    /// The minimum is the highest index referenced. Gaps are allowed (a template
    /// may use `{2}` but not `{1}`), but `{2}` still requires two arguments.
    /// Without a splat, extra arguments are an error rather than ignored: for
    /// `flif270 *.flif`, silently processing only the first file would be the
    /// worst possible outcome.
    ///
    /// :return: the arity
    pub fn arity(&self) -> Arity {
        let mut min = 0;
        let mut unbounded = false;
        for word in self.words() {
            match word {
                // A plain reference requires its argument to exist.
                Word::Concat(pieces) => {
                    for piece in pieces {
                        if let Piece::Arg(n) = piece {
                            min = min.max(*n);
                        }
                    }
                }
                // `{N..}` may be empty but requires the ones before it, so that
                // `cmd {1} {3..}` can't skip `{2}` silently.
                Word::Splat(from) => {
                    unbounded = true;
                    min = min.max(from - 1);
                }
            }
        }
        Arity { min, max: if unbounded { None } else { Some(min) } }
    }

    /// Substitute runtime arguments into the template.
    ///
    /// :param args: runtime arguments, excluding the alias name
    /// :return: the plan to run
    pub fn expand(&self, args: &[OsString]) -> Result<Plan, ExpandError> {
        // Check the count up front, so expansion below can index without checks.
        let arity = self.arity();
        if args.len() < arity.min || arity.max.is_some_and(|max| args.len() > max) {
            return Err(ExpandError::WrongArgCount { arity, got: args.len() });
        }
        // Expand each pipeline stage by stage.
        let expand_pipeline =
            |p: &Pipeline| -> Result<Vec<Argv>, ExpandError> { p.stages.iter().map(|c| expand_command(c, args)).collect() };
        let head = expand_pipeline(&self.head)?;
        let tail = self.tail.iter().map(|(op, p)| Ok((*op, expand_pipeline(p)?))).collect::<Result<_, _>>()?;
        Ok(Plan { head, tail })
    }
}

/// Expand one command.
///
/// :param command: the parsed command
/// :param args: runtime arguments; already checked against the arity
/// :return: the argv
fn expand_command(command: &Command, args: &[OsString]) -> Result<Argv, ExpandError> {
    let mut argv = Vec::with_capacity(command.words.len());
    for word in &command.words {
        match word {
            // Each splatted argument becomes its own argv entry, unmodified.
            Word::Splat(from) => argv.extend(args.iter().skip(from - 1).cloned()),
            // Concatenate pieces as OsString, so non-UTF-8 filenames survive.
            Word::Concat(pieces) => {
                let mut entry = OsString::new();
                for piece in pieces {
                    match piece {
                        Piece::Lit(text) => entry.push(text),
                        // In bounds: `expand` checked len >= arity.min >= n.
                        Piece::Arg(n) => entry.push(&args[n - 1]),
                    }
                }
                argv.push(entry);
            }
        }
    }
    // Only possible when the whole command is a splat with nothing to splat.
    if argv.is_empty() { Err(ExpandError::EmptyCommand) } else { Ok(argv) }
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
        parse(src).unwrap().expand(&os(args)).unwrap()
    }

    #[test]
    fn simple_command_gets_implicit_splat() {
        // No references: arguments are appended like a shell alias.
        let p = plan("ls -la", &["a", "b"]);
        assert_eq!(p.as_simple().unwrap(), &os(&["ls", "-la", "a", "b"]));
    }

    #[test]
    fn multi_command_without_refs_rejects_args() {
        // No obvious recipient for arguments, so they're an error.
        let t = parse("true | true").unwrap();
        assert!(!t.implicit_splat);
        assert!(matches!(t.expand(&os(&["x"])), Err(ExpandError::WrongArgCount { .. })));
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
        assert_eq!(plan(r"echo 'a {1} \n'", &[]).head[0], os(&["echo", r"a {1} \n"]));
        // Double quotes expand references and keep unknown escapes literal.
        assert_eq!(plan(r#"echo "{1} \d \" \{""#, &["x"]).head[0], os(&["echo", r#"x \d " {"#]));
        // Adjacent quoted and unquoted parts form one word.
        assert_eq!(plan(r#"echo a'b c'"d"{1}"#, &["!"]).head[0], os(&["echo", "ab cd!"]));
        // Empty quotes produce an empty argument rather than nothing.
        assert_eq!(plan("printf %s '' {1}", &["x"]).head[0], os(&["printf", "%s", "", "x"]));
    }

    #[test]
    fn splats() {
        // `{2..}` takes the rest, one word each; `{@}` takes everything.
        assert_eq!(plan("cmd {1} -- {2..}", &["a", "b", "c"]).head[0], os(&["cmd", "a", "--", "b", "c"]));
        assert_eq!(plan("cmd {@}", &[]).head[0], os(&["cmd"]));
        // Arity reflects the floor from the splat start.
        assert_eq!(parse("cmd {3..}").unwrap().arity(), Arity { min: 2, max: None });
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
        // Too few and too many are both rejected, with the arity in the error.
        let t = parse("echo {2}").unwrap();
        assert_eq!(t.expand(&os(&["a"])), Err(ExpandError::WrongArgCount { arity: Arity { min: 2, max: Some(2) }, got: 1 }));
        assert!(t.expand(&os(&["a", "b", "c"])).is_err());
    }

    #[test]
    fn empty_command_from_splat() {
        // A command that is only a splat needs at least one argument to run.
        assert_eq!(parse("{@}").unwrap().expand(&[]), Err(ExpandError::EmptyCommand));
    }

    #[test]
    fn syntax_errors_point_at_the_problem() {
        // Each case: source, and the byte offset the error must report.
        let cases = [
            ("echo $1", 5),
            (r#"ffmpeg -i "$1""#, 11),
            (r#"echo "a `date`""#, 8),
            ("echo {0}", 5),
            ("echo {x}", 5),
            ("echo {1", 5),
            ("echo }", 5),
            ("echo 'oops", 5),
            ("echo x{@}", 6),
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
            assert_eq!(err.offset, offset, "{src}: {}", err.message);
        }
    }

    #[test]
    fn escaped_specials_are_literal() {
        // Escaping or quoting gets any reserved character through verbatim.
        assert_eq!(plan(r"find . -exec rm '{}' \;", &[]).head[0], os(&["find", ".", "-exec", "rm", "{}", ";"]));
        assert_eq!(plan(r"echo \$HOME '*' \{1\}", &[]).head[0], os(&["echo", "$HOME", "*", "{1}"]));
        // Inside double quotes `$` and backticks need a backslash, as in sh;
        // single quotes keep them literal with no escaping at all.
        assert_eq!(plan(r#"echo "\$1 \`x\`" '$2'"#, &[]).head[0], os(&["echo", "$1 `x`", "$2"]));
    }

    #[test]
    fn non_utf8_arguments_survive() {
        use std::os::unix::ffi::OsStrExt;
        // Filenames need not be UTF-8 on Unix; they must pass through byte-exact.
        let raw = std::ffi::OsStr::from_bytes(b"caf\xe9.mkv").to_os_string();
        let p = parse("ffmpeg -i {1} {1}.out").unwrap().expand(std::slice::from_ref(&raw)).unwrap();
        assert_eq!(p.head[0][2], raw);
        assert_eq!(p.head[0][3].as_bytes(), b"caf\xe9.mkv.out");
    }

    #[test]
    fn render_places_caret() {
        // The caret line should align with the offending column.
        let err = parse("echo $1").unwrap_err();
        assert!(err.render("echo $1").ends_with("\n    echo $1\n         ^"));
    }
}
