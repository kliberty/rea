//! End-to-end tests: run the real binary and check processes, pipes, exit
//! statuses and signals.
//!
//! Runtime behavior is tested through `aliasmux eval TEMPLATE ARGS...`, which
//! runs a template exactly as a patched alias would. Assumption: a POSIX
//! userland is present (printf, sort, tr, cat, tee, mv, yes, head, sh, ls).

use std::fs;
use std::os::unix::process::ExitStatusExt;
use std::path::PathBuf;
use std::process::{Command, Output};

/// The binary under test.
///
/// :return: a command for it
fn bin() -> Command {
    Command::new(env!("CARGO_BIN_EXE_aliasmux"))
}

/// Run an ad-hoc template.
///
/// :param template: template source
/// :param args: arguments for it
/// :return: the captured output
fn eval(template: &str, args: &[&str]) -> Output {
    bin().arg("eval").arg(template).args(args).output().expect("binary runs")
}

/// Stdout as a string.
///
/// :param out: captured output
/// :return: stdout, lossily decoded
fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// Stderr as a string.
///
/// :param out: captured output
/// :return: stderr, lossily decoded
fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

/// A fresh, empty scratch directory unique to one test.
///
/// :param tag: test-specific suffix; tests run in parallel, so it must differ
/// :return: the directory path
fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("aliasmux-test-{}-{tag}", std::process::id()));
    // Leftovers from an aborted run with the same pid are possible; start clean.
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("create scratch dir");
    dir
}

#[test]
fn simple_command_appends_arguments() {
    // No references, one command: arguments are appended, like a shell alias.
    let out = eval("printf %s-", &["a", "b"]);
    assert_eq!(stdout(&out), "a-b-");
    assert!(out.status.success());
}

#[test]
fn numbered_argument_reused_with_spaces_intact() {
    // `{1}` twice, and the space in the argument never splits it.
    let out = eval(r"printf '[%s][%s]\n' {1} {1}.x", &["a b"]);
    assert_eq!(stdout(&out), "[a b][a b.x]\n");
}

#[test]
fn pipeline_stages_are_connected() {
    let out = eval(r"printf 'b\na\n' | sort | tr a-z A-Z", &[]);
    assert_eq!(stdout(&out), "A\nB\n");
    assert!(out.status.success());
}

#[test]
fn and_or_sequencing() {
    // `false && x` skips x; the failure then lets `|| y` run.
    let out = eval("false && printf no || printf yes ; printf !", &[]);
    assert_eq!(stdout(&out), "yes!");
    assert!(out.status.success());
}

#[test]
fn pipeline_reports_upstream_failure() {
    // pipefail-style: plain sh would report `cat`'s 0 here.
    let out = eval("sh -c 'exit 3' | cat", &[]);
    assert_eq!(out.status.code(), Some(3));
}

#[test]
fn upstream_sigpipe_is_success_and_does_not_hang() {
    // `yes` dies of SIGPIPE once `head` exits. That must neither hang (pipe
    // ends closed properly, SIGPIPE at default in children) nor count as failure.
    let out = eval("yes | head -n 2", &[]);
    assert_eq!(stdout(&out), "y\ny\n");
    assert!(out.status.success(), "{:?}", out.status);
}

#[test]
fn missing_command_is_127() {
    // Both via the direct exec path and inside a pipeline.
    let single = eval("no-such-command-aliasmux", &[]);
    assert_eq!(single.status.code(), Some(127));
    assert!(stderr(&single).contains("no-such-command-aliasmux: command not found"));
    let piped = eval("no-such-command-aliasmux | cat", &[]);
    assert_eq!(piped.status.code(), Some(127));
}

#[test]
fn argument_count_is_checked() {
    // Too many: the `flif270 *.flif` case must not silently drop files.
    let many = eval("printf %s {1}", &["a", "b"]);
    assert_eq!(many.status.code(), Some(2));
    assert!(stderr(&many).contains("expected exactly 1 argument, got 2"), "{}", stderr(&many));
    // Too few.
    let few = eval("printf %s {1} {2}", &["a"]);
    assert_eq!(few.status.code(), Some(2));
    assert!(stderr(&few).contains("expected exactly 2 arguments, got 1"), "{}", stderr(&few));
}

#[test]
fn rest_passes_arguments_through_including_options() {
    // Arguments that look like options belong to the alias, not to clap.
    let out = eval("printf [%s] {@}", &["-b", "--c"]);
    assert_eq!(stdout(&out), "[-b][--c]");
}

#[test]
fn template_errors_are_reported() {
    let out = eval("echo $1", &["x"]);
    assert_eq!(out.status.code(), Some(2));
    assert!(stderr(&out).contains("use `{1}`"));
}

#[test]
fn rewrite_in_place_like_flif270() {
    // Same shape as flif270: transform through a pipe into a temp file, then
    // replace the original, for a filename with a space in it.
    let dir = scratch("rewrite");
    let file = dir.join("my file.txt");
    fs::write(&file, "hello\n").unwrap();
    let template = "cat {1} | tr a-z A-Z | tee {1}.tmp && mv {1}.tmp {1}";
    let out = eval(template, &[file.to_str().unwrap()]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(fs::read_to_string(&file).unwrap(), "HELLO\n");
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn failed_pipeline_does_not_clobber_original() {
    // The upstream `cat` fails, so `mv` must not run. Under plain sh semantics
    // (last stage decides) it would, replacing the file with empty output.
    let dir = scratch("clobber");
    let file = dir.join("keep.txt");
    fs::write(&file, "original\n").unwrap();
    let template = "cat {1}.missing | tr a-z A-Z | tee {1}.tmp && mv {1}.tmp {1}";
    let out = eval(template, &[file.to_str().unwrap()]);
    assert!(!out.status.success());
    assert_eq!(fs::read_to_string(&file).unwrap(), "original\n");
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn death_by_signal_is_propagated() {
    // A stage killed by SIGTERM makes the supervisor die by SIGTERM too, so the
    // caller sees the real cause rather than an exit code. ($$ is literal here
    // because it is single-quoted; sh expands it.)
    let out = eval("sh -c 'kill -TERM $$' | cat", &[]);
    assert_eq!(out.status.signal(), Some(libc_sigterm()));
}

/// SIGTERM's number, without a libc dev-dependency just for one constant.
///
/// :return: 15, which is SIGTERM on every Linux architecture
fn libc_sigterm() -> i32 {
    15
}

#[test]
fn explain_prints_quoted_plan() {
    let out = bin()
        .args(["explain", "-t", "flif -d {1} - | flif -E 14 - {1}.rot && mv {1}.rot {1}", "a b"])
        .output()
        .unwrap();
    let text = stdout(&out);
    // Each process on its own line, quoted so it can be pasted into sh.
    assert!(text.contains("   flif -d 'a b' -\n"), "{text}");
    assert!(text.contains("  | flif -E 14 - 'a b.rot'\n"), "{text}");
    assert!(text.contains("&& mv 'a b.rot' 'a b'\n"), "{text}");
}

#[test]
fn inherited_sigint_ignore_reaches_children() {
    // Started with SIGINT ignored (as a background job in a script is), the
    // stages must inherit that: the stage sends itself SIGINT and must survive.
    // `trap "" INT` makes sh ignore SIGINT, and `exec` keeps that across exec.
    let template = r#"sh -c 'kill -INT $$; echo survived' | cat"#;
    let script = format!("trap '' INT; exec \"$0\" eval '{}'", template.replace('\'', r"'\''"));
    let out = Command::new("sh").args(["-c", &script, env!("CARGO_BIN_EXE_aliasmux")]).output().unwrap();
    assert_eq!(stdout(&out), "survived\n", "{}", stderr(&out));
    assert!(out.status.success(), "{:?}", out.status);
}

#[test]
fn default_sigint_still_reaches_children() {
    // The normal case: SIGINT at its default, so the stage dies by it, and the
    // supervisor reports that by dying of SIGINT too.
    let out = eval(r#"sh -c 'kill -INT $$; echo survived' | cat"#, &[]);
    assert_eq!(stdout(&out), "");
    assert_eq!(out.status.signal(), Some(2));
}

// ---------------------------------------------------------------------------
// Creating aliases. These run `new` and `patch` without --stub, so they also
// check that the stub is found next to the tool (Cargo builds both bins of
// this package into the same directory).
// ---------------------------------------------------------------------------

/// Run `aliasmux` with arguments.
///
/// :param args: the arguments
/// :return: the captured output
fn tool(args: &[&OsStr]) -> Output {
    bin().args(args).output().expect("binary runs")
}

/// Shorthand for building an argument list of mixed strings and paths.
macro_rules! args {
    ($($a:expr),* $(,)?) => { [$(std::ffi::OsStr::new($a)),*] };
}

use std::ffi::OsStr;

#[test]
fn new_creates_a_standalone_alias_and_updates_only_on_change() {
    let dir = scratch("new");
    let d = dir.to_str().unwrap();
    // Create: the alias runs on its own, from its own file.
    let out = tool(&args!["new", "greet", "printf [%s] {1}", "-o", d]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(stdout(&out).starts_with("created "));
    let run = Command::new(dir.join("greet")).arg("a b").output().unwrap();
    assert_eq!(stdout(&run), "[a b]");
    // The definition reads back out of the binary.
    assert_eq!(stdout(&tool(&args!["inspect", dir.join("greet").as_os_str()])), "greet = printf [%s] {1}\n");

    // Same definition again: reported unchanged, file untouched.
    let before = fs::metadata(dir.join("greet")).unwrap().modified().unwrap();
    let out = tool(&args!["new", "greet", "printf [%s] {1}", "-o", d]);
    assert!(stdout(&out).starts_with("unchanged "), "{}", stdout(&out));
    assert_eq!(fs::metadata(dir.join("greet")).unwrap().modified().unwrap(), before);

    // A new definition updates it in place.
    let out = tool(&args!["new", "greet", "printf -%s- {1}", "-o", d]);
    assert!(out.status.success() && stdout(&out).starts_with("updated "), "{}", stderr(&out));
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn new_validates_before_writing() {
    let dir = scratch("new-invalid");
    // Bad template: caret-pointed error, and no file created.
    let out = tool(&args!["new", "bad", "echo $1", "-o", dir.to_str().unwrap()]);
    assert_eq!(out.status.code(), Some(1));
    assert!(stderr(&out).contains("use `{1}`"), "{}", stderr(&out));
    // Bad and reserved names are refused the same way.
    assert!(!tool(&args!["new", "a/b", "true", "-o", dir.to_str().unwrap()]).status.success());
    assert!(!tool(&args!["new", "aliasmux-stub", "true", "-o", dir.to_str().unwrap()]).status.success());
    assert_eq!(fs::read_dir(&dir).unwrap().count(), 0);
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn new_replaces_non_alias_files_only_when_told_to() {
    let dir = scratch("clobber-guard");
    let d = dir.to_str().unwrap();
    // An ordinary file, the tool itself, and the bare stub are all off limits.
    fs::write(dir.join("notes"), "precious").unwrap();
    fs::copy(env!("CARGO_BIN_EXE_aliasmux"), dir.join("tool-copy")).unwrap();
    fs::copy(env!("CARGO_BIN_EXE_aliasmux-stub"), dir.join("stub-copy")).unwrap();
    for name in ["notes", "tool-copy", "stub-copy"] {
        let out = tool(&args!["new", name, "true", "-o", d]);
        assert_eq!(out.status.code(), Some(1), "{name}");
        assert!(stderr(&out).contains("is not an aliasmux alias"), "{name}: {}", stderr(&out));
    }
    assert_eq!(fs::read_to_string(dir.join("notes")).unwrap(), "precious");
    // --replace-non-aliases is the explicit way through.
    let out = tool(&args!["new", "notes", "true", "-o", d, "--replace-non-aliases"]);
    assert!(stdout(&out).starts_with("replaced "), "{}", stderr(&out));
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn patch_validates_the_whole_file_before_writing() {
    let dir = scratch("patch-invalid");
    let file = dir.join("aliases.txt");
    // One good line and two bad ones, failing different checks (template and
    // name): both reported in one run, in file order, and nothing written.
    fs::write(&file, "good = true\nbad = echo $1\nalso bad = x\n").unwrap();
    let out_dir = dir.join("bin");
    let out = tool(&args!["patch", file.as_os_str(), "-o", out_dir.as_os_str()]);
    assert_eq!(out.status.code(), Some(1));
    let err = stderr(&out);
    assert!(err.contains("aliases.txt:2:") && err.contains("aliases.txt:3:"), "{err}");
    assert!(!out_dir.join("good").exists());
    // `check` reports the same problems.
    assert_eq!(tool(&args!["check", file.as_os_str()]).status.code(), Some(1));
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn patch_writes_only_changes_and_prunes_removed_aliases() {
    let dir = scratch("patch-prune");
    let (file, out_dir) = (dir.join("aliases.txt"), dir.join("bin"));
    let patch = |extra: &[&str]| {
        let mut args = vec![OsStr::new("patch"), file.as_os_str(), OsStr::new("-o"), out_dir.as_os_str()];
        args.extend(extra.iter().map(OsStr::new));
        tool(&args)
    };
    fs::write(&file, "keep = true\nchange = printf a\ngone = false\n").unwrap();
    assert!(patch(&[]).status.success());
    // An unrelated file in the directory must survive pruning.
    fs::write(out_dir.join("unrelated"), "x").unwrap();
    let keep_mtime = fs::metadata(out_dir.join("keep")).unwrap().modified().unwrap();

    fs::write(&file, "keep = true\nchange = printf b\n").unwrap();
    let out = patch(&["--prune"]);
    assert!(out.status.success(), "{}", stderr(&out));
    // Only the changed alias is rewritten; the deleted one is removed.
    let lines: Vec<String> = stdout(&out).lines().map(|l| l.replace(out_dir.to_str().unwrap(), "DIR")).collect();
    assert_eq!(lines, ["updated DIR/change", "removed DIR/gone"]);
    assert!(stderr(&out).contains("1 written, 1 unchanged, 1 removed, 0 failed"), "{}", stderr(&out));
    assert_eq!(fs::metadata(out_dir.join("keep")).unwrap().modified().unwrap(), keep_mtime);
    assert!(out_dir.join("unrelated").exists());
    assert_eq!(stdout(&Command::new(out_dir.join("change")).output().unwrap()), "b");
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn list_output_is_an_alias_file_that_recreates_the_aliases() {
    let dir = scratch("round-trip");
    let (first, second) = (dir.join("first"), dir.join("second"));
    let defs = "pipe = printf 'b\\na\\n' | sort\nsay = printf %s {1}\n";
    fs::write(dir.join("aliases.txt"), defs).unwrap();
    assert!(tool(&args![OsStr::new("patch"), dir.join("aliases.txt").as_os_str(), OsStr::new("-o"), first.as_os_str()]).status.success());
    // `list` prints the definitions back, sorted by name...
    let listed = stdout(&tool(&args![OsStr::new("list"), first.as_os_str()]));
    assert_eq!(listed, "pipe = printf 'b\\na\\n' | sort\nsay = printf %s {1}\n");
    // ...and patching from that output reproduces identical executables.
    fs::write(dir.join("listed.txt"), &listed).unwrap();
    assert!(tool(&args![OsStr::new("patch"), dir.join("listed.txt").as_os_str(), OsStr::new("-o"), second.as_os_str()]).status.success());
    for name in ["pipe", "say"] {
        assert_eq!(fs::read(first.join(name)).unwrap(), fs::read(second.join(name)).unwrap(), "{name}");
    }
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn the_tool_itself_is_refused_as_a_stub() {
    // aliasmux carries the marker bytes in its patching code. Treating that as
    // a slot would overwrite 4 KiB of the program, so it must refuse.
    let dir = scratch("stub-guard");
    let out = tool(&args!["new", "x", "true", "-o", dir.to_str().unwrap(), "--stub", env!("CARGO_BIN_EXE_aliasmux")]);
    assert_eq!(out.status.code(), Some(1));
    assert!(stderr(&out).contains("not an aliasmux stub"), "{}", stderr(&out));
    assert_eq!(fs::read_dir(&dir).unwrap().count(), 0);
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn explain_reads_an_alias_executable() {
    let dir = scratch("explain-file");
    assert!(tool(&args!["new", "mv2", "mv {1} {1}.bak", "-o", dir.to_str().unwrap()]).status.success());
    let out = tool(&args![OsStr::new("explain"), dir.join("mv2").as_os_str(), OsStr::new("a b")]);
    assert_eq!(stdout(&out), "   mv 'a b' 'a b.bak'\n(single command: runs via exec, no supervisor process)\n");
    let _ = fs::remove_dir_all(&dir);
}

// ---------------------------------------------------------------------------
// `eval -`: the template read from stdin.
// ---------------------------------------------------------------------------

/// Run `aliasmux eval - ARGS...` with `input` on stdin.
///
/// :param input: bytes to feed on stdin
/// :param args: arguments for the template
/// :return: the captured output
fn eval_stdin(input: &[u8], args: &[&str]) -> Output {
    use std::io::Write;
    use std::process::Stdio;
    let mut child = bin().arg("eval").arg("-").args(args).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
    // Dropping stdin after writing closes it, so the read sees end of file.
    child.stdin.take().unwrap().write_all(input).unwrap();
    child.wait_with_output().unwrap()
}

#[test]
fn eval_reads_the_template_from_stdin() {
    // Arguments still come from the command line, options included.
    let out = eval_stdin(b"printf [%s] {1} {2..}\n", &["a b", "-x"]);
    assert_eq!(stdout(&out), "[a b][-x]", "{}", stderr(&out));
    // Trailing line endings (CRLF, blank lines) are dropped.
    assert_eq!(stdout(&eval_stdin(b"printf ok\r\n\n\n", &[])), "ok");
}

#[test]
fn eval_stdin_rejects_multiple_lines_and_bad_input() {
    // Two lines would otherwise be parsed as one command; refuse instead.
    let out = eval_stdin(b"printf a\nprintf b\n", &[]);
    assert_eq!(out.status.code(), Some(2));
    assert!(stderr(&out).contains("must be a single line"), "{}", stderr(&out));
    assert_eq!(stdout(&out), "");
    // Non-UTF-8 input and template errors are usage errors too.
    assert_eq!(eval_stdin(b"echo \xff\n", &[]).status.code(), Some(2));
    assert!(stderr(&eval_stdin(b"echo $1\n", &[])).contains("use `{1}`"));
}

#[test]
fn eval_stdin_leaves_commands_with_an_exhausted_stdin() {
    // Documented consequence: the template consumed stdin, so `cat` reads
    // nothing (rather than, say, blocking or seeing the template text).
    let out = eval_stdin(b"cat\n", &[]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(stdout(&out), "");
}

// ---------------------------------------------------------------------------
// Several-argument references, end to end.
// ---------------------------------------------------------------------------

#[test]
fn chunked_names_with_back_index_and_rest() {
    // The poolcmd2 -c case: any number of names, then a shared argument last.
    let explained = bin().args(["explain", "-t", "rm -f {@} && symlink {-1} {@}", "l1", "l2", "src"]).output().unwrap();
    assert_eq!(stdout(&explained), "   rm -f l1 l2\n&& symlink src l1 l2\n");
    // And for real, with commands that take several names: recreate two
    // files (one stale), then use the last argument on its own.
    let dir = scratch("chunk");
    let (a, b) = (dir.join("a"), dir.join("b"));
    fs::write(&a, "stale").unwrap();
    let out = eval("rm -f {@} && touch {@} && printf %s {-1}", &[a.to_str().unwrap(), b.to_str().unwrap(), "done"]);
    assert_eq!(stdout(&out), "done", "{}", stderr(&out));
    assert_eq!(fs::read_to_string(&a).unwrap(), "");
    assert!(b.exists());
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn word_repetition_and_alias_name() {
    // -I{@} repeats per argument; {0} is the alias name (ours, for eval).
    let out = eval("printf [%s] {0} -I{@}", &["a", "b"]);
    assert_eq!(stdout(&out), "[aliasmux][-Ia][-Ib]");
    // {@+} refuses to run with nothing to stand for.
    let none = eval("printf %s {@+}", &[]);
    assert_eq!(none.status.code(), Some(2));
    assert!(stderr(&none).contains("expected at least 1 argument, got 0"), "{}", stderr(&none));
}

#[test]
fn new_rejects_templates_no_count_satisfies() {
    // Arguments 1 and 2 would never be used: refused at creation, no file.
    let dir = scratch("unsatisfiable");
    let out = tool(&args!["new", "bad", "echo {3..}", "-o", dir.to_str().unwrap()]);
    assert_eq!(out.status.code(), Some(1));
    assert!(stderr(&out).contains("no number of arguments works"), "{}", stderr(&out));
    assert_eq!(fs::read_dir(&dir).unwrap().count(), 0);
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn template_syntax_is_in_the_long_help() {
    // `--help` (long) carries the full syntax; `-h` points at it.
    for args in [&["--help"][..], &["new", "--help"], &["eval", "--help"]] {
        let help = stdout(&bin().args(args).output().unwrap());
        assert!(help.contains("{A..B}") && help.contains("{@+}") && help.contains("up to but not including"), "{args:?}: {help}");
        // Checks and modifiers, including the two combined.
        assert!(help.contains("{1:/.}") && help.contains("{1:F/.}") && help.contains("{1:D//}"), "{args:?}: {help}");
    }
    assert!(stdout(&bin().arg("-h").output().unwrap()).contains("aliasmux --help"));
}

#[test]
fn closed_stdout_ends_quietly_instead_of_panicking() {
    // Like any Unix tool, `aliasmux explain ... | head` must just stop when the
    // reader goes away. The output (~200 KB) is far larger than a pipe buffer,
    // so the write is certain to happen after `head -c 1` has exited.
    let script = r#""$0" explain -t 'echo {@}' $(seq 100000 130000) | head -c 1 >/dev/null"#;
    let out = Command::new("sh").args(["-c", script, env!("CARGO_BIN_EXE_aliasmux")]).output().unwrap();
    assert!(!stderr(&out).contains("panicked"), "{}", stderr(&out));
    assert_eq!(stderr(&out), "");
}

// ---------------------------------------------------------------------------
// Path modifiers and file checks.
// ---------------------------------------------------------------------------

#[test]
fn modifiers_through_explain() {
    let out = bin().args(["explain", "-t", "convert {1} {1://}/{1:/.}-thumb.jpg", "pics/a b.png"]).output().unwrap();
    assert_eq!(stdout(&out), "   convert 'pics/a b.png' 'pics/a b-thumb.jpg'\n(single command: runs via exec, no supervisor process)\n");
}

#[test]
fn failed_check_stops_everything_before_it_starts() {
    // All checks run before the first command, so `touch` never runs here.
    let dir = scratch("check-first");
    let (made, missing) = (dir.join("made"), dir.join("missing"));
    let out = eval("touch {1} && cat {2:F}", &[made.to_str().unwrap(), missing.to_str().unwrap()]);
    assert_eq!(out.status.code(), Some(2));
    let err = stderr(&out);
    assert!(err.contains("{2:F} needs an existing file, but '") && err.contains("' does not exist"), "{err}");
    assert!(!made.exists(), "the first command must not have run");
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn new_rejects_bad_formats() {
    let dir = scratch("bad-format");
    let out = tool(&args!["new", "x", "cat {1:Q}", "-o", dir.to_str().unwrap()]);
    assert_eq!(out.status.code(), Some(1));
    assert!(stderr(&out).contains("unknown format `:Q`"), "{}", stderr(&out));
    assert_eq!(fs::read_dir(&dir).unwrap().count(), 0);
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn word_check_guards_an_output_before_anything_runs() {
    // Like the convert-jpg alias: transform into {1}.out, then replace {1}.
    let dir = scratch("word-check-run");
    let file = dir.join("f.txt");
    fs::write(&file, "hello\n").unwrap();
    let template = "cat {1:F} | tr a-z A-Z | tee {1}.out{:N} && mv {1}.out {1}";
    let first = eval(template, &[file.to_str().unwrap()]);
    assert!(first.status.success(), "{}", stderr(&first));
    assert_eq!(fs::read_to_string(&file).unwrap(), "HELLO\n");
    // With the output already there, nothing runs at all.
    fs::write(dir.join("f.txt.out"), "keep").unwrap();
    let second = eval(template, &[file.to_str().unwrap()]);
    assert_eq!(second.status.code(), Some(2));
    assert!(stderr(&second).contains("the word {1}.out{:N} needs a path that doesn't exist yet"), "{}", stderr(&second));
    assert_eq!(fs::read_to_string(dir.join("f.txt.out")).unwrap(), "keep");
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn explain_shows_the_plan_despite_failed_checks() {
    let dir = scratch("explain-checks");
    let input = dir.join("photo.png");
    fs::write(&input, "x").unwrap();
    fs::write(dir.join("photo.png.jpg"), "old").unwrap();
    let out = tool(&args![
        OsStr::new("explain"),
        OsStr::new("-t"),
        OsStr::new("magick {1:F} ppm: | cjpeg -outfile {1}.jpg{:N} && mv {1}.jpg {2:D}"),
        input.as_os_str(),
        dir.join("missing").as_os_str()
    ]);
    // The whole plan is printed, then each failure, and the status is the
    // alias's own for a failed check.
    assert_eq!(out.status.code(), Some(2));
    assert_eq!(stdout(&out).lines().count(), 3, "{}", stdout(&out));
    let err = stderr(&out);
    assert_eq!(err.lines().count(), 2, "{err}");
    assert!(err.contains("would not run: the word {1}.jpg{:N} needs a path that doesn't exist yet"), "{err}");
    assert!(err.contains("would not run: {2:D} needs an existing directory"), "{err}");
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn explain_exits_as_the_alias_would() {
    // A wrong count and an invalid template are usage errors for the alias,
    // so explain gives the same 2.
    let few = bin().args(["explain", "-t", "cp {1} {2}", "a"]).output().unwrap();
    assert_eq!(few.status.code(), Some(2), "{}", stderr(&few));
    let invalid = bin().args(["explain", "-t", "echo $HOME"]).output().unwrap();
    assert_eq!(invalid.status.code(), Some(2), "{}", stderr(&invalid));
    // Something that isn't an alias at all is the tool's own error.
    let missing = bin().args(["explain", "/no/such/alias"]).output().unwrap();
    assert_eq!(missing.status.code(), Some(1), "{}", stderr(&missing));
}
