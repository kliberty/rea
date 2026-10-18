//! The aliasmux command line: create, update, list and inspect aliases.
//!
//! An alias is a copy of the `aliasmux-stub` executable with its definition
//! written into the stub's slot (see aliasmux_core::slot). Creating one needs
//! no compiler: the definition is validated here, at creation time, with the
//! same parser the alias runs, so a bad template never becomes an executable.

use std::collections::HashSet;
use std::ffi::{OsStr, OsString};
use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};

use clap::{Parser, Subcommand};

use aliasmux_core::{aliasfile, run_alias, slot, template};

/// Prefix for messages, and the program name in `--help`.
const SELF_NAME: &str = env!("CARGO_PKG_NAME");

/// File name of the stub, expected next to this executable by default.
const STUB_NAME: &str = "aliasmux-stub";

/// Largest file that directory scans (`list`, `patch --prune`) will read when
/// looking for aliases. Arbitrary, but far above any stub's size, so the limit
/// only spares reading big unrelated files that happen to share the directory.
const SCAN_LIMIT: u64 = 64 * 1024 * 1024;

/// The template language, shown by `aliasmux --help`, `new --help` and
/// `eval --help`. Constraint on change: keep it in step with the parser
/// (aliasmux_core::template) and the README's syntax table.
const SYNTAX: &str = "\
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
  {0}             the alias name

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
  or a slice covering it ({@} doesn't count). The count is exact unless {@},
  {@+} or a slice is used. A single command with no references gets {@}
  appended, like a shell alias.

  $, backticks, globs, redirection, ( ) and a lone & are errors rather than
  passed through; quote or escape them to pass them literally.

Examples:
  aliasmux new mkvize 'ffmpeg -i {1} -y -c:v copy -c:a copy {1}.mkv'
  aliasmux new cpto 'cp -t {-1} {@+}'
  aliasmux new incl 'gcc -I{@} -c {-1}'
  aliasmux new relink 'rm -f {@} && symlink {-1} {@}'
  aliasmux new thumb 'convert {1:F} -resize 50% {1://}/{1:/.}-thumb.jpg'
  aliasmux new tardir 'tar -C {1:D//} -czf {1:/}.tar.gz {1:/}'";

/// Command aliases with numbered arguments and real pipelines, no shell.
///
/// Each alias is a small standalone executable. DIR defaults to the directory
/// this program is in, and the stub to `aliasmux-stub` next to it.
#[derive(Parser)]
// `-h` shows the short help with a pointer; `--help` shows the syntax too.
#[command(name = SELF_NAME, version, after_help = "Run `aliasmux --help` for the template syntax.", after_long_help = SYNTAX)]
struct Cli {
    #[command(subcommand)]
    command: Cmd,
}

/// Where aliases go and what they are made from; shared by `new` and `patch`.
#[derive(clap::Args)]
struct Output {
    /// Directory for the aliases (default: the directory this program is in).
    #[arg(short = 'o', long = "out", value_name = "DIR")]
    dir: Option<PathBuf>,
    /// The stub executable to copy (default: aliasmux-stub next to this program).
    #[arg(long, value_name = "PATH")]
    stub: Option<PathBuf>,
    /// Allow replacing an existing file that is not an aliasmux alias (another
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
        file: PathBuf,
        /// Only these aliases from the file (default: all of them).
        names: Vec<String>,
        #[command(flatten)]
        output: Output,
        /// Also remove aliases in DIR that the file no longer defines.
        #[arg(long, conflicts_with = "names")]
        prune: bool,
    },
    /// Validate an alias file without writing anything.
    Check {
        /// Alias file to check.
        file: PathBuf,
    },
    /// List the aliases in a directory, in alias-file format.
    ///
    /// The output is a valid alias file, so `list DIR > aliases.txt` followed
    /// by `patch aliases.txt` recreates the same aliases.
    List {
        /// Directory to scan (default: the directory this program is in).
        dir: Option<PathBuf>,
    },
    /// Print the definition stored in alias executables.
    Inspect {
        /// Alias executables.
        #[arg(required = true)]
        files: Vec<PathBuf>,
    },
    /// Print the commands an alias would run, without running them.
    ///
    /// File checks that would fail are listed after the commands. The exit
    /// status is the alias's own: 2 for a failed check, a wrong argument
    /// count or an invalid template.
    Explain {
        /// Treat TARGET as template source rather than an alias executable.
        #[arg(short = 't', long = "template")]
        is_template: bool,
        /// Alias executable (or template source, with -t).
        target: String,
        /// Arguments to expand with.
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
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
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<OsString>,
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
        } => patch(&file, &names, &output, prune),
        Cmd::Check { file } => check(&file),
        Cmd::List { dir } => list(dir.as_deref()),
        Cmd::Inspect { files } => inspect(&files),
        Cmd::Explain {
            is_template,
            target,
            args,
        } => explain(is_template, &target, &args),
        Cmd::Eval { template, args } => eval(template, &args),
    };
    std::process::exit(status);
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
        return Some(format!("`{name}` is reserved for aliasmux itself"));
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
            std::process::exit(aliasmux_core::USAGE_ERROR);
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

/// The directory this program lives in, which is the default for DIR and for
/// finding the stub.
///
/// Assumption: on Linux, `current_exe` resolves symlinks (it reads
/// /proc/self/exe), so a symlinked `aliasmux` still finds the stub beside the
/// real file. Defaulting DIR here is an arbitrary convenience: installing the
/// tool in ~/.local/bin makes new aliases land there too.
fn own_dir() -> Result<PathBuf, String> {
    let exe =
        std::env::current_exe().map_err(|err| format!("cannot locate this program: {err}"))?;
    exe.parent()
        .map(Path::to_path_buf)
        .ok_or_else(|| "cannot locate this program's directory".into())
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
        let offset = slot::locate(&image).map_err(|err| format!("{}: {err}", path.display()))?;
        // A marker alone is not proof of a slot: this program carries the
        // marker bytes too, as a constant. A real stub's slot is empty, or
        // holds a valid alias (any alias also works as a stub).
        if let Err(slot::SlotError::Corrupt(why)) = slot::decode_at(&image, offset) {
            return Err(format!(
                "{}: not an aliasmux stub (marker found, but {why})",
                path.display()
            ));
        }
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
    /// An aliasmux alias; holds the file's bytes for an unchanged-check.
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
    Updated,
    /// Same bytes already there; the file (and its mtime) were left alone.
    Unchanged,
    /// A non-alias file was replaced, which only `--replace-non-aliases` allows.
    Replaced,
}

/// Write one alias into a directory, refusing to clobber unrelated files.
///
/// `dir` must exist, and `name` and `template` must already be validated. A file
/// that isn't an alias is replaced only with `replace_non_aliases`.
fn write_alias(
    stub: &Stub,
    dir: &Path,
    name: &str,
    template: &str,
    replace_non_aliases: bool,
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
        Existing::Alias(_) => Wrote::Updated,
        Existing::Other if replace_non_aliases => Wrote::Replaced,
        Existing::Other => {
            return Err(format!(
                "{} exists and is not an aliasmux alias; use --replace-non-aliases to replace it",
                target.display()
            ));
        }
    };
    // Write to a temporary name, then rename: replacing a running executable
    // in place fails with ETXTBSY, and a half-written file must never be left
    // under the alias's name. The leading dot keeps it out of normal listings.
    let temp = dir.join(format!(".{name}.aliasmux-tmp"));
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
    let dir = match dir {
        Some(dir) => dir.to_path_buf(),
        None => own_dir()?,
    };
    fs::create_dir_all(&dir).map_err(|err| format!("cannot create {}: {err}", dir.display()))?;
    Ok(dir)
}

/// Print what happened to one alias.
fn report(wrote: Wrote, path: &Path) {
    // Unchanged aliases are only counted, so a large rerun stays quiet.
    let verb = match wrote {
        Wrote::Created => "created",
        Wrote::Updated => "updated",
        Wrote::Replaced => "replaced",
        Wrote::Unchanged => return,
    };
    println!("{verb} {}", path.display());
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
    match write_alias(&stub, &dir, name, template, output.replace_non_aliases) {
        // Say "unchanged" here, unlike in `patch`: with one alias, silence
        // would look like nothing happened for no reason.
        Ok(Wrote::Unchanged) => {
            println!("unchanged {}", dir.join(name).display());
            0
        }
        Ok(wrote) => {
            report(wrote, &dir.join(name));
            0
        }
        Err(err) => fail(&err),
    }
}

/// `patch`: create or update the aliases of an alias file.
///
/// `names` limits it to those aliases; empty means all of them. With `prune`,
/// aliases the file no longer defines are removed. Returns the exit status.
fn patch(file: &Path, names: &[String], output: &Output, prune: bool) -> i32 {
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
    let prepared = Stub::load(output.stub.as_deref())
        .and_then(|stub| Ok((stub, output_dir(output.dir.as_deref())?)));
    let (stub, dir) = match prepared {
        Ok(prepared) => prepared,
        Err(err) => return fail(&err),
    };

    // Write the selected aliases, counting outcomes for the summary.
    let (mut written, mut unchanged, mut failures) = (0, 0, 0);
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
        ) {
            Ok(Wrote::Unchanged) => unchanged += 1,
            Ok(wrote) => {
                report(wrote, &dir.join(entry.name));
                written += 1;
            }
            Err(err) => failures += fail(&err),
        }
    }

    // Prune by the name stored inside each alias, not its file name, so a
    // renamed copy of a still-defined alias is kept. Only files that hold an
    // alias are candidates; nothing else in DIR is ever removed.
    let mut removed = 0;
    if prune {
        match scan(&dir) {
            Ok(found) => {
                for (path, name, _) in found {
                    if defined.contains(name.as_str()) {
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
    // The summary goes to stderr, so stdout lists only what changed.
    eprintln!("{written} written, {unchanged} unchanged, {removed} removed, {failures} failed");
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
    let dir = match dir.map_or_else(own_dir, |d| Ok(d.to_path_buf())) {
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

/// `inspect`: print the definitions stored in alias executables.
///
/// Returns the exit status: 1 if any file is not an alias.
fn inspect(files: &[PathBuf]) -> i32 {
    let mut failures = 0;
    for file in files {
        match read_definition(file) {
            Ok((name, template)) => println!("{name} = {template}"),
            Err(err) => failures += fail(&err),
        }
    }
    if failures > 0 { 1 } else { 0 }
}

/// Read the definition stored in an alias executable, as `(name, template)`,
/// or say why the file is not an alias.
fn read_definition(file: &Path) -> Result<(String, String), String> {
    let image = fs::read(file).map_err(|err| format!("cannot read {}: {err}", file.display()))?;
    // Same lookup the stub does at run time, but on the file instead of memory.
    let offset = slot::locate(&image).map_err(|err| format!("{}: {err}", file.display()))?;
    match slot::decode_at(&image, offset) {
        Ok((name, template)) => Ok((name.to_owned(), template.to_owned())),
        // A marker with garbage after it is a false positive (this program
        // carries the marker bytes as a constant), not a damaged alias.
        Err(slot::SlotError::Corrupt(why)) => Err(format!(
            "{}: not an aliasmux alias (marker found, but {why})",
            file.display()
        )),
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
/// Failed file checks are listed after the plan rather than replacing it.
/// Returns the exit status.
fn explain(is_template: bool, target: &str, args: &[OsString]) -> i32 {
    // Resolve the name (for `{0}`) and template from whichever form was given.
    // A bare template has no alias name, so it gets ours, as with `eval`.
    let (name, source) = if is_template {
        (SELF_NAME.to_owned(), target.to_owned())
    } else {
        match read_definition(Path::new(target)) {
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
            return aliasmux_core::USAGE_ERROR;
        }
    };
    // Failed checks don't stop the explanation: the plan is shown, then what
    // would stop it from running.
    let template::Preview {
        plan,
        failed_checks,
    } = match parsed.preview(&name, args) {
        Ok(preview) => preview,
        Err(err) => {
            fail(&err.to_string());
            return aliasmux_core::USAGE_ERROR;
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
    if failed_checks.is_empty() {
        return 0;
    }
    // Same wording as the alias's own error, and the same status, so
    // `explain` stays a faithful dry run for scripts.
    for err in &failed_checks {
        eprintln!("{SELF_NAME}: would not run: {err}");
    }
    aliasmux_core::USAGE_ERROR
}
