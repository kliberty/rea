//! End-to-end tests: run the real binary and check processes, pipes, exit
//! statuses and signals.
//!
//! Most tests use `aliasmux eval TEMPLATE ARGS...` so they don't depend on the
//! contents of the user's alias file. Assumption: a POSIX userland is present
//! (printf, sort, tr, cat, tee, mv, yes, head, sh, ls).

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
    assert_eq!(eval("printf %s {2}", &["a"]).status.code(), Some(2));
}

#[test]
fn splat_passes_rest_through_including_options() {
    // Arguments that look like options belong to the alias, not to clap.
    let out = eval("printf [%s] {2..}", &["a", "-b", "--c"]);
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
fn install_links_aliases_and_dispatches_on_name() {
    let dir = scratch("install");
    // Install creates one link per alias in the embedded file.
    let out = bin().arg("install").arg(&dir).output().unwrap();
    assert!(out.status.success(), "{}", stderr(&out));
    let listed = stdout(&bin().arg("list").output().unwrap()).lines().count();
    assert_eq!(fs::read_dir(&dir).unwrap().count(), listed);

    // A link whose alias doesn't exist reports itself as stale...
    let stale = dir.join("not-an-alias");
    std::os::unix::fs::symlink(env!("CARGO_BIN_EXE_aliasmux"), &stale).unwrap();
    let run = Command::new(&stale).output().unwrap();
    assert_eq!(run.status.code(), Some(127));
    assert!(stderr(&run).contains("no alias named `not-an-alias`"));

    // ...and --prune removes it while leaving the real aliases in place.
    let out = bin().arg("install").arg("--prune").arg(&dir).output().unwrap();
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(!stale.exists());
    assert_eq!(fs::read_dir(&dir).unwrap().count(), listed);
    let _ = fs::remove_dir_all(&dir);
}
