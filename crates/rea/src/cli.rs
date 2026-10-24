//! The rea command line: create, update, list and inspect aliases.
//!
//! An alias is a copy of the `rea-stub` executable with its definition
//! written into the stub's slot (see rea_core::slot). Creating one needs
//! no compiler: the definition is validated here, at creation time, with the
//! same parser the alias runs, so a bad template never becomes an executable.

use std::collections::{HashMap, HashSet};
use std::ffi::{OsStr, OsString};
use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};

use clap::{CommandFactory, Parser, Subcommand, ValueHint};
use clap_complete::Shell;

use rea_core::{aliasfile, run_alias, slot, template};

/// Prefix for messages, and the program name in `--help`.
const SELF_NAME: &str = env!("CARGO_PKG_NAME");

/// File name of the stub, expected next to this executable by default.
const STUB_NAME: &str = "rea-stub";

/// Largest file that directory scans (`list`, `patch --prune`) will read when
/// looking for aliases. Arbitrary, but far above any stub's size, so the limit
/// only spares reading big unrelated files that happen to share the directory.
const SCAN_LIMIT: u64 = 64 * 1024 * 1024;

/// The template language, shown by `rea --help`, `new --help` and
/// `eval --help`. Constraint on change: keep it in step with the parser
/// (rea_core::template) and the README's syntax table.
const SYNTAX: &str = concat!(
    "\
Template syntax:
  Words and quoting work as in sh: '...' is literal, \"...\" is literal except
  for {...} references and the escapes \\\" \\\\ \\{ \\} \\$ \\`, and \\x is a literal x.

  {1} {2} ...     arguments counted from the front
  {-1} {-2} ...   arguments counted from the back: {-1} is the last
  {@}             every argument nothing else uses (no {N}, {-N} or
                  slice), zero or more
  {@+}            the same, but at least one is required
  {A..B}          a slice: from argument A up to but not including B, each
                  counted like {N} or {-N}. Either end may be left out:
                  {2..} is the 2nd onward, {..-1} all but the last. Slices
                  are never empty, and may overlap the other references.
                  With slices, {@} must be one run at the start or the end.
  {0}             the alias name
  {$NAME}         environment variable NAME, as one argument, never split;
                  unset or empty is an error before anything runs

  Checks and modifiers go after a colon, on any of the above but {0}:
  {1:/}           basename                 dir/photo.jpg -> photo.jpg
  {1://}          parent directory         dir/photo.jpg -> dir
  {1:.}           without the extension    dir/photo.jpg -> dir/photo
  {1:/.}          basename, no extension   dir/photo.jpg -> photo
  {1:F} {1:D}     must be an existing file / directory; a file is anything
                  but a directory, so /dev/null and <(...) pass
  {1:E} {1:N}     must exist / must not exist yet
  {1:F/.}         both, check first: dir/photo.jpg must be a file -> photo
  {1:D//}         dir/sub must be a directory -> dir
  The check is on the argument itself, before the modifier. Modifiers only
  edit the text. Checks look at the file system, for every argument, before
  anything runs, so a file an earlier step creates doesn't count yet.

  {:F} {:D} {:E} {:N}  at the very end of a word, check the whole word:
                  -outfile {1}.jpg{:N} checks photo.png.jpg, not photo.png.
                  A repeated word ({@}.jpg{:N}) is checked once per copy.

  a | b           pipe
  a && b          run b only if a succeeded
  a || b          run b only if a failed
  a ; b           run b regardless
  (&&, || and ; have equal precedence and group left to right, as in sh.)

  A one-argument reference is always exactly one argument, spaces and all.
  A word containing {@}, {@+} or a slice is repeated once per argument:
  -I{@} gives -Ia -Ib, and nothing at all when {@} is empty.

  Every argument must be used, and numbering has no gaps: {3} needs {2},
  or a slice covering it ({@} doesn't count). Numbers go up to \
",
    rea_core::max_position!(),
    ", in
  references and slices alike. The count is exact unless {@},
  {@+} or a slice is used. A single command with no references (not even
  {0} or {$NAME}) gets {@} appended, like a shell alias.

  $ outside {$NAME}, backticks, globs, redirection, ( ) and a lone & are errors rather than
  passed through; quote or escape them to pass them literally.

Examples:
  rea new mkvize 'ffmpeg -i {1} -y -c:v copy -c:a copy {1}.mkv'
  rea new cpto 'cp -t {-1} {@+}'
  rea new incl 'gcc -I{@} -c {-1}'
  rea new relink 'rm -f {@} && symlink {-1} {@}'
  rea new thumb 'convert {1:F} -resize 50% {1://}/{1:/.}-thumb.jpg'
  rea new tardir 'tar -C {1:D//} -czf {1:/}.tar.gz {1:/}'
  rea new backup 'rsync -ahP {@+} {$BACKUP_DIR:D}'"
);

/// Command aliases with numbered arguments and real pipelines, no shell.
///
/// Each alias is a small standalone executable. DIR defaults to
/// $REA_ALIAS_DIR, or else the directory this program is in, and the stub to
/// `rea-stub` next to this program.
#[derive(Parser)]
// `-h` shows the short help with a pointer; `--help` shows the syntax too.
#[command(name = SELF_NAME, version, after_help = "Run `rea --help` for the template syntax.", after_long_help = SYNTAX)]
struct Cli {
    #[command(subcommand)]
    command: Cmd,
}

/// Where aliases go and what they are made from; shared by `new` and `patch`.
#[derive(clap::Args)]
struct Output {
    /// Directory for the aliases (default: $REA_ALIAS_DIR, or else the
    /// directory this program is in).
    #[arg(short = 'o', long = "out", value_name = "DIR", value_hint = ValueHint::DirPath)]
    dir: Option<PathBuf>,
    /// The stub executable to copy (default: rea-stub next to this program).
    #[arg(long, value_name = "PATH", value_hint = ValueHint::FilePath)]
    stub: Option<PathBuf>,
    /// Allow replacing an existing file that is not a rea alias (another
    /// program, say). Existing aliases are always updated without this, and
    /// unchanged ones are never rewritten, with or without it.
    // Constraint on change: deliberately not called --force, which reads as
    // "rewrite everything" and was confusing. (A plain comment, not `///`, so
    // it stays out of --help.)
    #[arg(long)]
    replace_non_aliases: bool,
}

#[derive(Subcommand)]
enum Cmd {
    /// Create (or update) one alias.
    #[command(after_long_help = SYNTAX)]
    New {
        /// Alias name, which is also its file name.
        name: String,
        /// Template, e.g. 'ffmpeg -i {1} -c copy {1}.mkv' (quote it for your shell).
        template: String,
        #[command(flatten)]
        output: Output,
    },
    /// Create or update every alias defined in an alias file.
    ///
    /// The file is validated completely before anything is written. Aliases
    /// whose definition is unchanged are left untouched.
    Patch {
        /// Alias file: one `name = template` per line, `#` for comments.
        #[arg(value_hint = ValueHint::FilePath)]
        file: PathBuf,
        /// Only these aliases from the file (default: all of them).
        names: Vec<String>,
        #[command(flatten)]
        output: Output,
        /// Also remove aliases in DIR that the file no longer defines.
        #[arg(long, conflicts_with = "names")]
        prune: bool,
        /// Print what would be written and removed, without changing anything.
        #[arg(long)]
        dry_run: bool,
    },
    /// Update an alias file from the aliases in a directory.
    ///
    /// The counterpart of `patch`. An alias the file already defines keeps
    /// its line, with only the template replaced if it changed; aliases the
    /// file lacks are appended, in name order, under a `# added by rea save`
    /// comment. Comments, blank lines, order, and entries with no alias in
    /// DIR are kept. A file that doesn't parse is refused, and an unchanged
    /// one is not rewritten.
    ///
    /// The file is replaced by renaming a new one over it, or, if it has
    /// other hard links or an owner a new file wouldn't get, rewritten in
    /// place with a backup beside it until the write is done. A symlink is
    /// followed, so the file it points to is the one updated.
    Save {
        /// Alias file to update (created if missing).
        #[arg(value_hint = ValueHint::FilePath)]
        file: PathBuf,
        /// Directory to read the aliases from (default: $REA_ALIAS_DIR, or
        /// else the directory this program is in).
        #[arg(value_hint = ValueHint::DirPath)]
        dir: Option<PathBuf>,
        /// Print what would change, without writing.
        #[arg(long)]
        dry_run: bool,
    },
    /// Validate an alias file without writing anything.
    Check {
        /// Alias file to check.
        #[arg(value_hint = ValueHint::FilePath)]
        file: PathBuf,
    },
    /// List the aliases in a directory, in alias-file format.
    ///
    /// The output is a valid alias file, so `list DIR > aliases.txt` followed
    /// by `patch aliases.txt` recreates the same aliases.
    List {
        /// Directory to scan (default: $REA_ALIAS_DIR, or else the directory
        /// this program is in).
        #[arg(value_hint = ValueHint::DirPath)]
        dir: Option<PathBuf>,
    },
    /// Print the definition stored in alias executables.
    ///
    /// A bare name (no `/`) is looked up on $PATH like a command, then in the
    /// default alias directory.
    Inspect {
        /// Alias executables: paths, or bare names to look up.
        #[arg(required = true, value_name = "ALIAS", value_hint = ValueHint::FilePath)]
        files: Vec<PathBuf>,
    },
    /// Print the commands an alias would run, without running them.
    ///
    /// File checks that would fail and unset environment variables are listed
    /// after the commands. The exit status is the alias's own: 2 for any of
    /// those, a wrong argument count or an invalid template.
    ///
    /// A bare name (no `/`) is looked up on $PATH like a command, then in the
    /// default alias directory.
    Explain {
        /// Treat TARGET as template source rather than an alias executable.
        #[arg(short = 't', long = "template")]
        is_template: bool,
        /// Alias executable: a path, or a bare name to look up (template
        /// source, with -t).
        #[arg(value_hint = ValueHint::FilePath)]
        target: String,
        /// Arguments to expand with.
        #[arg(trailing_var_arg = true, allow_hyphen_values = true, value_hint = ValueHint::AnyPath)]
        args: Vec<OsString>,
    },
    /// Run a template directly, to try it out before creating an alias.
    ///
    /// With `-` as the template, it is read from stdin instead (one line), which
    /// then leaves the template's commands with stdin already at end of file.
    #[command(after_long_help = SYNTAX)]
    Eval {
        /// Template source (quote it for your shell), or `-` to read it from stdin.
        template: String,
        /// Arguments for the template, passed through untouched.
        #[arg(trailing_var_arg = true, allow_hyphen_values = true, value_hint = ValueHint::AnyPath)]
        args: Vec<OsString>,
    },
    /// Print a tab-completion script for a shell.
    ///
    /// The script completes subcommands, options and file names, for the name
    /// this program was run as (so a renamed or symlinked copy gets its own).
    /// Install it where the shell looks, for example:
    ///
    ///   bash  rea completions bash > ~/.local/share/bash-completion/completions/rea
    ///   zsh   rea completions zsh > ~/.zfunc/_rea
    ///         (with `fpath+=~/.zfunc` before `compinit` in ~/.zshrc)
    ///   fish  rea completions fish > ~/.config/fish/completions/rea.fish
    ///
    /// For a renamed copy, use its name in the file names too: bash and zsh find
    /// the script by the command's name. Or load it on every start instead, e.g.
    /// `source <(rea completions bash)` in ~/.bashrc. Regenerate it after
    /// upgrading, in case options changed.
    #[command(verbatim_doc_comment)]
    Completions {
        /// The shell to write the script for.
        shell: Shell,
    },
}

/// Entry point. Never returns: exits with the subcommand's status.
pub fn main() -> ! {
    // `eval` replaces the process or exits itself; the rest return a status.
    let status = match Cli::parse().command {
        Cmd::New {
            name,
            template,
            output,
        } => new(&name, &template, &output),
        Cmd::Patch {
            file,
            names,
            output,
            prune,
            dry_run,
        } => patch(&file, &names, &output, prune, dry_run),
        Cmd::Save { file, dir, dry_run } => save(&file, dir.as_deref(), dry_run),
        Cmd::Check { file } => check(&file),
        Cmd::List { dir } => list(dir.as_deref()),
        Cmd::Inspect { files } => inspect(&files),
        Cmd::Explain {
            is_template,
            target,
            args,
        } => explain(is_template, &target, &args),
        Cmd::Eval { template, args } => eval(template, &args),
        Cmd::Completions { shell } => completions(shell),
    };
    std::process::exit(status);
}

/// Print the completion script for `shell` to stdout.
///
/// The script is tied to a command name, so it uses the name this program
/// was run as, falling back to the package name if `argv[0]` has none.
fn completions(shell: Shell) -> i32 {
    let name = std::env::args_os()
        .next()
        .and_then(|arg0| {
            Path::new(&arg0)
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
        })
        .unwrap_or_else(|| SELF_NAME.to_owned());
    clap_complete::generate(shell, &mut Cli::command(), name, &mut io::stdout());
    0
}

/// Print an error and return exit status 1.
fn fail(message: &str) -> i32 {
    eprintln!("{SELF_NAME}: {message}");
    1
}

// ---------------------------------------------------------------------------
// Validation
// ---------------------------------------------------------------------------

/// Check one alias definition completely, as creation requires.
///
/// Returns an explanation of the first problem, or `None` if the definition is
/// valid.
fn definition_problem(name: &str, template: &str) -> Option<String> {
    // Same name rules as the alias file, so both routes accept the same names.
    if let Some(problem) = aliasfile::name_problem(name) {
        return Some(problem);
    }
    // These two would shadow the tool and its stub in the same directory.
    if name == SELF_NAME || name == STUB_NAME {
        return Some(format!("`{name}` is reserved for rea itself"));
    }
    // The real parser, with a caret under the error.
    if let Err(err) = template::parse(template) {
        return Some(format!("alias `{name}`: {}", err.render(template)));
    }
    // Last: the definition must fit in the stub's slot.
    slot::encode(name, template).err()
}

/// Read and fully validate an alias file.
///
/// `source` receives the file's contents, which the returned entries borrow. On
/// failure, returns every problem found, formatted as `path:line: ...`.
fn load_alias_file<'a>(
    path: &Path,
    source: &'a mut String,
) -> Result<Vec<aliasfile::Entry<'a>>, Vec<String>> {
    *source = fs::read_to_string(path)
        .map_err(|err| vec![format!("cannot read {}: {err}", path.display())])?;
    // Collect every problem in the file, so one run shows every typo: line
    // problems (format, names, duplicates), plus the templates of every line
    // that was otherwise fine.
    let (entries, errors) = aliasfile::parse_lenient(source);
    let mut problems: Vec<(usize, String)> =
        errors.into_iter().map(|e| (e.line, e.message)).collect();
    problems.extend(
        entries
            .iter()
            .filter_map(|e| definition_problem(e.name, e.template).map(|p| (e.line, p))),
    );
    // Report in file order, whichever check found each problem.
    problems.sort_by_key(|(line, _)| *line);
    if problems.is_empty() {
        Ok(entries)
    } else {
        Err(problems
            .into_iter()
            .map(|(line, p)| format!("{}:{line}: {p}", path.display()))
            .collect())
    }
}

/// Report every problem from [`load_alias_file`] and return exit status 1.
fn fail_all(problems: &[String]) -> i32 {
    for problem in problems {
        fail(problem);
    }
    1
}

/// `eval`: run a template, from the argument or (with `-`) from stdin.
///
/// Never returns: it runs the template, or exits with an error.
fn eval(template: String, args: &[OsString]) -> ! {
    // `-` follows the usual "read stdin" convention. A template that really is
    // a lone `-` would only try to run a program named `-`, so nothing useful
    // is lost; `\-` still spells that if anyone needs it.
    let source = if template == "-" {
        template_from_stdin().unwrap_or_else(|err| {
            fail(&err);
            std::process::exit(rea_core::USAGE_ERROR);
        })
    } else {
        template
    };
    run_alias(SELF_NAME, &source, args)
}

/// Read a one-line template from stdin, without its trailing line ending.
///
/// The whole input is read (it must end before the template can run), so the
/// template's commands inherit a stdin that is already at end of file.
fn template_from_stdin() -> Result<String, String> {
    let mut text = String::new();
    io::stdin()
        .read_to_string(&mut text)
        .map_err(|err| match err.kind() {
            // Templates are text; the alias slot and file format both require UTF-8.
            io::ErrorKind::InvalidData => "the template on stdin is not valid UTF-8".to_owned(),
            _ => format!("cannot read the template from stdin: {err}"),
        })?;
    // `echo` and heredocs end the line (or add blank lines), so trailing line
    // endings are dropped; any other whitespace is harmless to the parser.
    let line = text.trim_end_matches(['\n', '\r']);
    // Constraint on change: refuse embedded newlines instead of accepting
    // them. The template parser treats a newline as plain whitespace, so two
    // lines would silently become one command, where sh would run two. One
    // line per template also matches the alias file and the stub's slot.
    if line.contains('\n') {
        return Err("the template on stdin must be a single line (use `;`, `&&` or `||` to run several commands)".into());
    }
    Ok(line.to_owned())
}

// ---------------------------------------------------------------------------
// Files: the stub, existing aliases, writing
// ---------------------------------------------------------------------------

/// The directory this program lives in, where the stub is found, and the
/// default for DIR when `$REA_ALIAS_DIR` doesn't set one.
///
/// Assumption: on Linux, `current_exe` resolves symlinks (it reads
/// /proc/self/exe), so a symlinked `rea` still finds the stub beside the
/// real file. Defaulting DIR here is an arbitrary convenience: installing the
/// tool in ~/.local/bin makes new aliases land there too.
fn own_dir() -> Result<PathBuf, String> {
    let exe =
        std::env::current_exe().map_err(|err| format!("cannot locate this program: {err}"))?;
    exe.parent()
        .map(Path::to_path_buf)
        .ok_or_else(|| "cannot locate this program's directory".into())
}

/// The environment variable that sets the default alias directory.
const ALIAS_DIR_VAR: &str = "REA_ALIAS_DIR";

/// The default alias directory: `$REA_ALIAS_DIR`, or else the directory
/// this program is in.
///
/// Every command that has a default directory uses this, so they agree:
/// `new` and `patch` write there, `list` scans it, and a bare alias name is
/// looked for there last. The variable is for installs that put this program
/// somewhere aliases don't belong, such as `cargo install` into ~/.cargo/bin.
/// An empty value counts as unset, rather than meaning the current directory,
/// and a relative one is relative to the current directory, as with `-o`.
fn default_dir() -> Result<PathBuf, String> {
    match std::env::var_os(ALIAS_DIR_VAR) {
        Some(dir) if !dir.is_empty() => Ok(PathBuf::from(dir)),
        _ => own_dir(),
    }
}

/// A stub, read into memory, with its slot located.
struct Stub {
    /// The whole file.
    image: Vec<u8>,
    /// Byte offset of its slot.
    offset: usize,
}

impl Stub {
    /// Load and check a stub: the one at `path`, or by default the one next to
    /// this program.
    fn load(path: Option<&Path>) -> Result<Stub, String> {
        let path = match path {
            Some(path) => path.to_path_buf(),
            None => own_dir()?.join(STUB_NAME),
        };
        let image = fs::read(&path).map_err(|err| {
            format!(
                "cannot read stub {}: {err} (use --stub to point at it)",
                path.display()
            )
        })?;
        // locate() only accepts a well-formed slot, so this program, which
        // carries the marker bytes as a constant, is refused here. A slot
        // that holds an alias is fine: any alias also works as a stub.
        let offset = slot::locate(&image)
            .map_err(|err| format!("{}: not a rea stub ({err})", path.display()))?;
        Ok(Stub { image, offset })
    }

    /// The complete executable for an alias made from this stub.
    ///
    /// `name` and `template` must already be validated.
    fn patched(&self, name: &str, template: &str) -> Result<Vec<u8>, String> {
        let mut copy = self.image.clone();
        copy[self.offset..self.offset + slot::SIZE].copy_from_slice(&slot::encode(name, template)?);
        Ok(copy)
    }
}

/// Read the alias definition, `(name, template)`, out of a file's contents.
///
/// Returns `None` for anything that isn't an alias, including the bare stub
/// (empty slot) and the tool itself (marker but no slot).
fn alias_in(image: &[u8]) -> Option<(&str, &str)> {
    let offset = slot::locate(image).ok()?;
    slot::decode_at(image, offset).ok()
}

/// What is currently at an alias's path.
enum Existing {
    /// Nothing.
    Missing,
    /// A rea alias; holds the file's bytes for an unchanged-check.
    Alias(Vec<u8>),
    /// Something else (another program, the stub, a directory...).
    Other,
}

/// Classify the file at a path.
fn existing(path: &Path) -> io::Result<Existing> {
    match fs::read(path) {
        Ok(bytes) if alias_in(&bytes).is_some() => Ok(Existing::Alias(bytes)),
        Ok(_) => Ok(Existing::Other),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(Existing::Missing),
        // Reading a directory fails with EISDIR; it is "something else".
        Err(err) if err.kind() == io::ErrorKind::IsADirectory => Ok(Existing::Other),
        Err(err) => Err(err),
    }
}

/// Write bytes as a new executable file (mode 0755), replacing any file at
/// `path`.
fn write_executable(path: &Path, bytes: &[u8]) -> io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    // Set the mode at creation so the file is never briefly non-executable.
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o755)
        .open(path)?;
    file.write_all(bytes)
}

/// What writing an alias did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Wrote {
    Created,
    /// An alias with a different definition was updated.
    Updated,
    /// An alias with the same definition was rewritten onto a different
    /// stub, as after upgrading rea.
    StubOnly,
    /// Same bytes already there; the file (and its mtime) were left alone.
    Unchanged,
    /// A non-alias file was replaced, which only `--replace-non-aliases` allows.
    Replaced,
}

/// Write one alias into a directory, refusing to clobber unrelated files.
///
/// `dir` must exist, and `name` and `template` must already be validated. A file
/// that isn't an alias is replaced only with `replace_non_aliases`. With
/// `dry_run`, everything is decided and checked the same way, refusals
/// included, and the result says what would have been written, but nothing
/// is.
fn write_alias(
    stub: &Stub,
    dir: &Path,
    name: &str,
    template: &str,
    replace_non_aliases: bool,
    dry_run: bool,
) -> Result<Wrote, String> {
    let target = dir.join(name);
    let bytes = stub.patched(name, template)?;
    // Decide based on what's there now. Only aliases are updated freely; the
    // tool, the stub, or any other program needs --replace-non-aliases.
    let wrote = match existing(&target)
        .map_err(|err| format!("cannot read {}: {err}", target.display()))?
    {
        Existing::Missing => Wrote::Created,
        Existing::Alias(old) if old == bytes => return Ok(Wrote::Unchanged),
        // Same definition in different bytes: only the stub changed.
        Existing::Alias(old) if alias_in(&old) == Some((name, template)) => Wrote::StubOnly,
        Existing::Alias(_) => Wrote::Updated,
        Existing::Other if replace_non_aliases => Wrote::Replaced,
        Existing::Other => {
            return Err(format!(
                "{} exists and is not a rea alias; use --replace-non-aliases to replace it",
                target.display()
            ));
        }
    };
    if dry_run {
        return Ok(wrote);
    }
    // Write to a temporary name, then rename: replacing a running executable
    // in place fails with ETXTBSY, and a half-written file must never be left
    // under the alias's name. The leading dot keeps it out of normal listings.
    let temp = dir.join(format!(".{name}.rea-tmp"));
    if let Err(err) = write_executable(&temp, &bytes).and_then(|()| fs::rename(&temp, &target)) {
        let _ = fs::remove_file(&temp);
        return Err(format!("cannot write {}: {err}", target.display()));
    }
    Ok(wrote)
}

/// Every alias in a directory, as `(path, name, template)`, sorted by name,
/// then path.
fn scan(dir: &Path) -> Result<Vec<(PathBuf, String, String)>, String> {
    let listing =
        fs::read_dir(dir).map_err(|err| format!("cannot list {}: {err}", dir.display()))?;
    let mut found = Vec::new();
    for entry in listing.flatten() {
        let path = entry.path();
        // Regular files of plausible size only; see SCAN_LIMIT.
        let plausible = entry
            .metadata()
            .is_ok_and(|m| m.is_file() && m.len() >= slot::SIZE as u64 && m.len() <= SCAN_LIMIT);
        if !plausible {
            continue;
        }
        // Unreadable files are simply not ours to report on.
        let Ok(image) = fs::read(&path) else { continue };
        if let Some((name, template)) = alias_in(&image) {
            found.push((path, name.to_owned(), template.to_owned()));
        }
    }
    found.sort_by(|a, b| (&a.1, &a.0).cmp(&(&b.1, &b.0)));
    Ok(found)
}

/// Resolve DIR (the `--out` option) and create it if needed.
fn output_dir(dir: Option<&Path>) -> Result<PathBuf, String> {
    let dir = dir.map_or_else(default_dir, |d| Ok(d.to_path_buf()))?;
    fs::create_dir_all(&dir).map_err(|err| format!("cannot create {}: {err}", dir.display()))?;
    Ok(dir)
}

/// Print what happened to one alias, or with `dry_run`, what would have.
fn report(wrote: Wrote, path: &Path, dry_run: bool) {
    let path = path.display();
    // Unchanged aliases are only counted, so a large rerun stays quiet.
    // Replacing a program that isn't an alias is the one change that loses
    // something, so it says so.
    let line = match (wrote, dry_run) {
        (Wrote::Unchanged, _) => return,
        (Wrote::Created, false) => format!("created {path}"),
        (Wrote::Updated, false) => format!("updated {path}"),
        (Wrote::StubOnly, false) => format!("updated {path} (stub only)"),
        (Wrote::Replaced, false) => format!("replaced {path}, which was not a rea alias"),
        (Wrote::Created, true) => format!("would create {path}"),
        (Wrote::Updated, true) => format!("would update {path}"),
        (Wrote::StubOnly, true) => format!("would update {path} (stub only)"),
        (Wrote::Replaced, true) => format!("would replace {path}, which is not a rea alias"),
    };
    println!("{line}");
}

// ---------------------------------------------------------------------------
// Commands
// ---------------------------------------------------------------------------

/// `new`: create or update one alias. Returns the exit status.
fn new(name: &str, template: &str, output: &Output) -> i32 {
    // Validate before touching the filesystem at all.
    if let Some(problem) = definition_problem(name, template) {
        return fail(&problem);
    }
    let prepared = Stub::load(output.stub.as_deref())
        .and_then(|stub| Ok((stub, output_dir(output.dir.as_deref())?)));
    let (stub, dir) = match prepared {
        Ok(prepared) => prepared,
        Err(err) => return fail(&err),
    };
    match write_alias(
        &stub,
        &dir,
        name,
        template,
        output.replace_non_aliases,
        false,
    ) {
        // Say "unchanged" here, unlike in `patch`: with one alias, silence
        // would look like nothing happened for no reason.
        Ok(Wrote::Unchanged) => {
            println!("unchanged {}", dir.join(name).display());
            0
        }
        Ok(wrote) => {
            report(wrote, &dir.join(name), false);
            0
        }
        Err(err) => fail(&err),
    }
}

/// `patch`: create or update the aliases of an alias file.
///
/// `names` limits it to those aliases; empty means all of them. With `prune`,
/// aliases the file no longer defines are removed. Returns the exit status.
fn patch(file: &Path, names: &[String], output: &Output, prune: bool, dry_run: bool) -> i32 {
    // Validate the whole file first, so a typo anywhere writes nothing.
    let mut source = String::new();
    let entries = match load_alias_file(file, &mut source) {
        Ok(entries) => entries,
        Err(problems) => return fail_all(&problems),
    };
    // A requested name must exist in the file; better an error than a silent skip.
    let defined: HashSet<&str> = entries.iter().map(|e| e.name).collect();
    if let Some(missing) = names.iter().find(|n| !defined.contains(n.as_str())) {
        return fail(&format!("{} defines no alias `{missing}`", file.display()));
    }
    // A dry run doesn't create DIR either; a missing one just means every
    // alias would be created.
    let dir = if dry_run {
        output
            .dir
            .as_deref()
            .map_or_else(default_dir, |d| Ok(d.to_path_buf()))
    } else {
        output_dir(output.dir.as_deref())
    };
    let prepared = Stub::load(output.stub.as_deref()).and_then(|stub| Ok((stub, dir?)));
    let (stub, dir) = match prepared {
        Ok(prepared) => prepared,
        Err(err) => return fail(&err),
    };

    // Write the selected aliases, counting outcomes for the summary.
    let (mut created, mut updated, mut stub_only, mut replaced) = (0, 0, 0, 0);
    let (mut unchanged, mut failures) = (0, 0);
    for entry in entries
        .iter()
        .filter(|e| names.is_empty() || names.iter().any(|n| n == e.name))
    {
        match write_alias(
            &stub,
            &dir,
            entry.name,
            entry.template,
            output.replace_non_aliases,
            dry_run,
        ) {
            Ok(wrote) => {
                report(wrote, &dir.join(entry.name), dry_run);
                match wrote {
                    Wrote::Created => created += 1,
                    Wrote::Updated => updated += 1,
                    // Counted as updates, with the breakdown in the summary.
                    Wrote::StubOnly => {
                        updated += 1;
                        stub_only += 1;
                    }
                    Wrote::Replaced => replaced += 1,
                    Wrote::Unchanged => unchanged += 1,
                }
            }
            Err(err) => failures += fail(&err),
        }
    }

    // Prune by the name stored inside each alias, not its file name, so a
    // renamed copy of a still-defined alias is kept. Only files that hold an
    // alias are candidates; nothing else in DIR is ever removed.
    let mut removed = 0;
    // A directory a dry run didn't create has nothing in it to prune.
    if prune && (dir.exists() || !dry_run) {
        match scan(&dir) {
            Ok(found) => {
                for (path, name, _) in found {
                    if defined.contains(name.as_str()) {
                        continue;
                    }
                    if dry_run {
                        println!("would remove {}", path.display());
                        removed += 1;
                        continue;
                    }
                    match fs::remove_file(&path) {
                        Ok(()) => {
                            println!("removed {}", path.display());
                            removed += 1;
                        }
                        Err(err) => {
                            failures += fail(&format!("cannot remove {}: {err}", path.display()))
                        }
                    }
                }
            }
            Err(err) => failures += fail(&err),
        }
    }
    // The summary goes to stderr, so stdout lists only what changed. One
    // count per verb on the lines above, and a dry run says so first, since
    // the counts are of what would happen.
    let stub_note = if stub_only > 0 {
        format!(" ({stub_only} stub only)")
    } else {
        String::new()
    };
    eprintln!(
        "{}{created} created, {updated} updated{stub_note}, {replaced} replaced, \
         {unchanged} unchanged, {removed} removed, {failures} failed",
        if dry_run { "dry run: " } else { "" }
    );
    if failures > 0 { 1 } else { 0 }
}

/// `check`: validate an alias file. Returns the exit status.
fn check(file: &Path) -> i32 {
    let mut source = String::new();
    match load_alias_file(file, &mut source) {
        Ok(entries) => {
            println!("{}: {} aliases, all valid", file.display(), entries.len());
            0
        }
        Err(problems) => fail_all(&problems),
    }
}

/// `list`: print a directory's aliases as an alias file. Returns the exit
/// status.
fn list(dir: Option<&Path>) -> i32 {
    let dir = match dir.map_or_else(default_dir, |d| Ok(d.to_path_buf())) {
        Ok(dir) => dir,
        Err(err) => return fail(&err),
    };
    let found = match scan(&dir) {
        Ok(found) => found,
        Err(err) => return fail(&err),
    };
    for (path, name, template) in &found {
        // A file not named after its alias is worth knowing about, and a
        // comment line keeps the output a valid alias file.
        if path.file_name() != Some(OsStr::new(name)) {
            println!("# {} holds alias `{name}`", path.display());
        }
        println!("{name} = {template}");
    }
    0
}

/// `save`: bring an alias file up to date with a directory's aliases.
///
/// Returns the exit status: 1 if the file can't be updated.
fn save(file: &Path, dir: Option<&Path>, dry_run: bool) -> i32 {
    let dir = match dir.map_or_else(default_dir, |d| Ok(d.to_path_buf())) {
        Ok(dir) => dir,
        Err(err) => return fail(&err),
    };
    let found = match scan(&dir) {
        Ok(found) => found,
        Err(err) => return fail(&err),
    };
    let aliases = match distinct_aliases(&found) {
        Ok(aliases) => aliases,
        Err(problems) => return fail_all(&problems),
    };
    // Follow a symlink, so the file it points to is updated and the link
    // stays a link. A missing file is created.
    let target = match fs::symlink_metadata(file) {
        Ok(_) => match fs::canonicalize(file) {
            Ok(real) => real,
            Err(err) => return fail(&format!("cannot resolve {}: {err}", file.display())),
        },
        Err(err) if err.kind() == io::ErrorKind::NotFound => file.to_path_buf(),
        Err(err) => return fail(&format!("cannot read {}: {err}", file.display())),
    };
    let source = if target.exists() {
        match fs::read_to_string(&target) {
            Ok(source) => source,
            Err(err) => return fail(&format!("cannot read {}: {err}", target.display())),
        }
    } else {
        String::new()
    };
    let merged = match aliasfile::merge(&source, &aliases) {
        Ok(merged) => merged,
        Err(errors) => {
            let problems: Vec<String> = errors
                .iter()
                .map(|e| format!("{}:{}: {}", file.display(), e.line, e.message))
                .collect();
            return fail_all(&problems);
        }
    };

    // Write first, so the lines below only claim changes that happened.
    let changed = merged.text != source || !target.exists();
    if changed && !dry_run {
        if let Err(err) = crate::replace::replace(&target, merged.text.as_bytes()) {
            return fail(&err);
        }
    }
    // What changed goes to stdout, like `patch`, and the summary to stderr.
    let (update, add) = if dry_run {
        ("would update", "would add")
    } else {
        ("updated", "added")
    };
    for (name, line) in &merged.updated {
        println!("{update} {name} (line {line})");
    }
    for name in &merged.added {
        println!("{add} {name}");
    }
    eprintln!(
        "{}{} updated, {} added, {} unchanged",
        if dry_run { "dry run: " } else { "" },
        merged.updated.len(),
        merged.added.len(),
        merged.unchanged
    );
    0
}

/// The aliases found in a directory as `(name, template)` pairs, one per
/// name, for writing to an alias file.
///
/// Two files may hold the same alias, a copy under another name, say; that
/// is fine when they agree. Different templates under one name can't both
/// go in the file, and neither can one with a line break, which the file
/// format has no way to hold.
fn distinct_aliases(found: &[(PathBuf, String, String)]) -> Result<Vec<(&str, &str)>, Vec<String>> {
    // Name -> the first file holding it, and its template.
    let mut first: HashMap<&str, (&Path, &str)> = HashMap::new();
    let mut aliases = Vec::new();
    let mut problems = Vec::new();
    for (path, name, template) in found {
        if template.contains(['\n', '\r']) {
            problems.push(format!(
                "{}: alias `{name}` has a line break in its template",
                path.display()
            ));
            continue;
        }
        match first.get(name.as_str()) {
            None => {
                first.insert(name, (path, template));
                aliases.push((name.as_str(), template.as_str()));
            }
            Some((other, kept)) if kept.trim() != template.trim() => problems.push(format!(
                "alias `{name}` differs between {} and {}",
                other.display(),
                path.display()
            )),
            Some(_) => {}
        }
    }
    if problems.is_empty() {
        Ok(aliases)
    } else {
        Err(problems)
    }
}

/// `inspect`: print the definitions stored in alias executables.
///
/// Returns the exit status: 1 if any file is not an alias.
fn inspect(files: &[PathBuf]) -> i32 {
    let mut failures = 0;
    for file in files {
        match find_alias(file).and_then(|path| read_definition(&path)) {
            Ok((name, template)) => println!("{name} = {template}"),
            Err(err) => failures += fail(&err),
        }
    }
    if failures > 0 { 1 } else { 0 }
}

/// The file to read for an alias given on the command line, found the way
/// a shell finds a command.
///
/// A target containing `/` is a path, used as given. A bare name is looked
/// up in each `$PATH` directory in order, and the first executable file
/// wins, so `explain NAME` reads the file that running NAME would run, even
/// when that file isn't an alias. As in sh, an empty `$PATH` entry means the
/// current directory. If no entry has the name, the default alias directory
/// is tried last, for aliases made where `$PATH` doesn't look.
fn find_alias(target: &Path) -> Result<PathBuf, String> {
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::PermissionsExt;

    // An empty target is left to fail as a path, with the read error.
    let bytes = target.as_os_str().as_bytes();
    if bytes.is_empty() || bytes.contains(&b'/') {
        return Ok(target.to_path_buf());
    }
    // Executable regular files only, as the shell's search; metadata follows
    // symlinks, so a symlinked alias counts.
    let is_command = |path: &Path| {
        fs::metadata(path)
            .is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
    };
    // Unset is no directories; set but empty is one empty entry, as in sh.
    let mut dirs: Vec<PathBuf> = std::env::var_os("PATH")
        .map(|var| {
            std::env::split_paths(&var)
                .map(|dir| {
                    if dir.as_os_str().is_empty() {
                        PathBuf::from(".")
                    } else {
                        dir
                    }
                })
                .collect()
        })
        .unwrap_or_default();
    // The default alias directory comes last, so it never shadows `$PATH`.
    // Failing to locate it only loses the fallback.
    let fallback = default_dir().ok();
    dirs.extend(fallback.clone());
    if let Some(found) = dirs
        .iter()
        .map(|dir| dir.join(target))
        .find(|path| is_command(path))
    {
        return Ok(found);
    }
    let name = target.display();
    Err(match fallback {
        Some(dir) => format!("{name}: not found on PATH or in {}", dir.display()),
        None => format!("{name}: not found on PATH"),
    })
}

/// Read the definition stored in an alias executable, as `(name, template)`,
/// or say why the file is not an alias.
fn read_definition(file: &Path) -> Result<(String, String), String> {
    let image = fs::read(file).map_err(|err| format!("cannot read {}: {err}", file.display()))?;
    // Same lookup the stub does at run time, but on the file instead of memory.
    // locate() only accepts a well-formed slot, so a copy of the marker bytes
    // with other data after it (this program has one) isn't mistaken for a
    // damaged alias.
    let offset = slot::locate(&image)
        .map_err(|err| format!("{}: not a rea alias ({err})", file.display()))?;
    match slot::decode_at(&image, offset) {
        Ok((name, template)) => Ok((name.to_owned(), template.to_owned())),
        Err(err) => Err(format!("{}: {err}", file.display())),
    }
}

/// Quote a word so the printed command can be pasted into sh (or a template).
///
/// The word is single-quoted unless it is plainly safe.
fn quote(word: &OsStr) -> String {
    let text = word.to_string_lossy();
    // Arbitrary conservative set of characters that need no quoting anywhere.
    let safe = !text.is_empty()
        && text
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_./=:,+@%".contains(c));
    if safe {
        text.into_owned()
    } else {
        // Inside single quotes only `'` itself is special; `'\''` ends the
        // quote, adds an escaped quote, and reopens. Works in sh and templates.
        format!("'{}'", text.replace('\'', r"'\''"))
    }
}

/// Format one argv as a pasteable line of quoted words.
fn format_argv(argv: &template::Argv) -> String {
    argv.iter().map(|w| quote(w)).collect::<Vec<_>>().join(" ")
}

/// `explain`: print the expanded plan, one process per line.
///
/// `target` is an alias executable, or template source with `is_template`.
/// Failed file checks and unset environment variables are listed after the
/// plan rather than replacing it, and the variables it read are shown.
/// Returns the exit status.
fn explain(is_template: bool, target: &str, args: &[OsString]) -> i32 {
    // Resolve the name (for `{0}`) and template from whichever form was given.
    // A bare template has no alias name, so it gets ours, as with `eval`.
    let (name, source) = if is_template {
        (SELF_NAME.to_owned(), target.to_owned())
    } else {
        match find_alias(Path::new(target)).and_then(|path| read_definition(&path)) {
            Ok(definition) => definition,
            Err(err) => return fail(&err),
        }
    };
    // Report errors exactly as a real run would, status included, but without
    // exiting the process from inside a helper. A file that isn't an alias
    // (above) is the tool's own error, so it keeps status 1.
    let parsed = match template::parse(&source) {
        Ok(parsed) => parsed,
        Err(err) => {
            fail(&format!("invalid template: {}", err.render(&source)));
            return rea_core::USAGE_ERROR;
        }
    };
    // Failed checks and unset variables don't stop the explanation: the plan
    // is shown, then what would stop it from running.
    let template::Preview {
        plan,
        problems,
        env,
    } = match parsed.preview(&name, args) {
        Ok(preview) => preview,
        Err(err) => {
            fail(&err.to_string());
            return rea_core::USAGE_ERROR;
        }
    };
    // Stages are indented under their pipeline so the structure reads at a glance.
    let print_pipeline = |prefix: &str, stages: &[template::Argv]| {
        for (index, argv) in stages.iter().enumerate() {
            let lead = if index == 0 { prefix } else { "  |" };
            println!("{lead} {}", format_argv(argv));
        }
    };
    // The head pipeline has no operator; align it with the ones that do.
    print_pipeline("  ", &plan.head);
    for (op, stages) in &plan.tail {
        let prefix = match op {
            template::Op::And => "&&",
            template::Op::Or => "||",
            template::Op::Then => " ;",
        };
        print_pipeline(prefix, stages);
    }
    // Make the "single command means exec" behavior visible.
    if plan.as_simple().is_some() {
        println!("(single command: runs via exec, no supervisor process)");
    }
    // Values from the environment are the one input not on the command line,
    // so show where they came from.
    if !env.is_empty() {
        let values: Vec<String> = env
            .iter()
            .map(|(var, value)| format!("{var}={}", quote(value)))
            .collect();
        println!("(from the environment: {})", values.join(" "));
    }
    if problems.is_empty() {
        return 0;
    }
    // Same wording as the alias's own error, and the same status, so
    // `explain` stays a faithful dry run for scripts.
    for err in &problems {
        eprintln!("{SELF_NAME}: would not run: {err}");
    }
    rea_core::USAGE_ERROR
}
