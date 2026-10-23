//! End-to-end tests: run the real binary and check processes, pipes, exit
//! statuses and signals.
//!
//! Runtime behavior is tested through `rea eval TEMPLATE ARGS...`, which
//! runs a template exactly as a patched alias would. Assumption: a POSIX
//! userland is present (printf, sort, tr, cat, tee, mv, yes, head, sh, ls).

use std::fs;
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// A command for the binary under test.
fn bin() -> Command {
    Command::new(env!("CARGO_BIN_EXE_rea"))
}

/// Run an ad-hoc template and capture its output.
fn eval(template: &str, args: &[&str]) -> Output {
    bin()
        .arg("eval")
        .arg(template)
        .args(args)
        .output()
        .expect("binary runs")
}

/// Stdout as a string, lossily decoded.
fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// Stderr as a string, lossily decoded.
fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

/// A fresh, empty scratch directory unique to one test.
///
/// Tests run in parallel, so each must pass a different `tag`.
fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rea-test-{}-{tag}", std::process::id()));
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
    let single = eval("no-such-command-rea", &[]);
    assert_eq!(single.status.code(), Some(127));
    assert!(stderr(&single).contains("no-such-command-rea: command not found"));
    let piped = eval("no-such-command-rea | cat", &[]);
    assert_eq!(piped.status.code(), Some(127));
}

#[test]
fn argument_count_is_checked() {
    // Too many: the `flif270 *.flif` case must not silently drop files.
    let many = eval("printf %s {1}", &["a", "b"]);
    assert_eq!(many.status.code(), Some(2));
    assert!(
        stderr(&many).contains("expected exactly 1 argument, got 2"),
        "{}",
        stderr(&many)
    );
    // Too few.
    let few = eval("printf %s {1} {2}", &["a"]);
    assert_eq!(few.status.code(), Some(2));
    assert!(
        stderr(&few).contains("expected exactly 2 arguments, got 1"),
        "{}",
        stderr(&few)
    );
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

/// SIGTERM's number (15 on every Linux architecture), without a libc
/// dev-dependency just for one constant.
fn libc_sigterm() -> i32 {
    15
}

#[test]
fn explain_prints_quoted_plan() {
    let out = bin()
        .args([
            "explain",
            "-t",
            "flif -d {1} - | flif -E 14 - {1}.rot && mv {1}.rot {1}",
            "a b",
        ])
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
    let script = format!(
        "trap '' INT; exec \"$0\" eval '{}'",
        template.replace('\'', r"'\''")
    );
    let out = Command::new("sh")
        .args(["-c", &script, env!("CARGO_BIN_EXE_rea")])
        .output()
        .unwrap();
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

/// Run an ad-hoc template with SIGCHLD ignored from the start, as a parent
/// that ignores it would leave it.
fn eval_with_sigchld_ignored(template: &str) -> Output {
    let mut command = bin();
    command.arg("eval").arg(template);
    // SAFETY: `signal` is async-signal-safe, so it may run between fork and
    // exec. SIG_IGN survives the exec; a handler wouldn't.
    unsafe {
        command.pre_exec(|| {
            libc::signal(libc::SIGCHLD, libc::SIG_IGN);
            Ok(())
        });
    }
    command.output().expect("binary runs")
}

#[test]
fn inherited_sigchld_ignore_keeps_exit_statuses() {
    // With SIGCHLD ignored, children are reaped by the kernel and waiting for
    // them fails. Each waited-for step must still report its real status.
    let out = eval_with_sigchld_ignored("true && echo ok");
    assert_eq!(stdout(&out), "ok\n", "{}", stderr(&out));
    assert!(out.status.success(), "{:?}", out.status);

    let out = eval_with_sigchld_ignored("true | true");
    assert!(out.status.success(), "{:?}", out.status);

    let out = eval_with_sigchld_ignored("true | sh -c 'exit 3'");
    assert_eq!(out.status.code(), Some(3), "{}", stderr(&out));

    let out = eval_with_sigchld_ignored("false || echo recovered");
    assert_eq!(stdout(&out), "recovered\n", "{}", stderr(&out));
}

// ---------------------------------------------------------------------------
// Creating aliases. These run `new` and `patch` without --stub, so they also
// check that the stub is found next to the tool (Cargo builds both bins of
// this package into the same directory).
// ---------------------------------------------------------------------------

/// Run `rea` with arguments and capture its output.
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
    assert_eq!(
        stdout(&tool(&args!["inspect", dir.join("greet").as_os_str()])),
        "greet = printf [%s] {1}\n"
    );

    // Same definition again: reported unchanged, file untouched.
    let before = fs::metadata(dir.join("greet")).unwrap().modified().unwrap();
    let out = tool(&args!["new", "greet", "printf [%s] {1}", "-o", d]);
    assert!(stdout(&out).starts_with("unchanged "), "{}", stdout(&out));
    assert_eq!(
        fs::metadata(dir.join("greet")).unwrap().modified().unwrap(),
        before
    );

    // A new definition updates it in place.
    let out = tool(&args!["new", "greet", "printf -%s- {1}", "-o", d]);
    assert!(
        out.status.success() && stdout(&out).starts_with("updated "),
        "{}",
        stderr(&out)
    );
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
    assert!(
        !tool(&args!["new", "a/b", "true", "-o", dir.to_str().unwrap()])
            .status
            .success()
    );
    assert!(
        !tool(&args![
            "new",
            "rea-stub",
            "true",
            "-o",
            dir.to_str().unwrap()
        ])
        .status
        .success()
    );
    assert_eq!(fs::read_dir(&dir).unwrap().count(), 0);
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn new_replaces_non_alias_files_only_when_told_to() {
    let dir = scratch("clobber-guard");
    let d = dir.to_str().unwrap();
    // An ordinary file, the tool itself, and the bare stub are all off limits.
    fs::write(dir.join("notes"), "precious").unwrap();
    fs::copy(env!("CARGO_BIN_EXE_rea"), dir.join("tool-copy")).unwrap();
    fs::copy(env!("CARGO_BIN_EXE_rea-stub"), dir.join("stub-copy")).unwrap();
    for name in ["notes", "tool-copy", "stub-copy"] {
        let out = tool(&args!["new", name, "true", "-o", d]);
        assert_eq!(out.status.code(), Some(1), "{name}");
        assert!(
            stderr(&out).contains("is not a rea alias"),
            "{name}: {}",
            stderr(&out)
        );
    }
    assert_eq!(fs::read_to_string(dir.join("notes")).unwrap(), "precious");
    // --replace-non-aliases is the explicit way through.
    let out = tool(&args![
        "new",
        "notes",
        "true",
        "-o",
        d,
        "--replace-non-aliases"
    ]);
    assert!(stdout(&out).starts_with("replaced "), "{}", stderr(&out));
    assert!(
        stdout(&out).ends_with(", which was not a rea alias\n"),
        "{}",
        stdout(&out)
    );
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn patch_tells_stub_only_updates_from_definition_changes() {
    let dir = scratch("patch-stub-only");
    let file = dir.join("aliases.txt");
    let out_dir = dir.join("aliases");
    fs::write(&file, "same = true\nchange = printf a\n").unwrap();
    let patch = |extra: &[&OsStr]| {
        let mut command = bin();
        command
            .arg("patch")
            .arg(&file)
            .arg("-o")
            .arg(&out_dir)
            .args(extra);
        command.output().unwrap()
    };
    assert!(patch(&[]).status.success());

    // A different stub, as after an upgrade: the same one plus a byte.
    let mut image = fs::read(env!("CARGO_BIN_EXE_rea-stub")).unwrap();
    image.push(0);
    let stub = dir.join("rea-stub");
    fs::write(&stub, image).unwrap();
    fs::write(&file, "same = true\nchange = printf b\n").unwrap();
    let out = patch(&[OsStr::new("--stub"), stub.as_os_str()]);
    assert!(out.status.success(), "{}", stderr(&out));
    let lines: Vec<String> = stdout(&out)
        .lines()
        .map(|l| l.replace(out_dir.to_str().unwrap(), "DIR"))
        .collect();
    assert_eq!(
        lines,
        ["updated DIR/same (stub only)", "updated DIR/change"]
    );
    assert!(
        stderr(&out).starts_with("0 created, 2 updated (1 stub only), 0 replaced"),
        "{}",
        stderr(&out)
    );
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn patch_dry_run_refuses_and_replaces_as_a_real_run_would() {
    let dir = scratch("patch-dry-run");
    let file = dir.join("aliases.txt");
    let out_dir = dir.join("new-dir");
    fs::write(&file, "notes = true\nother = false\n").unwrap();
    let patch = |extra: &[&str]| {
        let mut command = bin();
        command
            .arg("patch")
            .arg(&file)
            .arg("-o")
            .arg(&out_dir)
            .args(extra);
        command.output().unwrap()
    };

    // A missing DIR isn't created; everything in it would be.
    let out = patch(&["--dry-run"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(stdout(&out).lines().count(), 2);
    assert!(stdout(&out).lines().all(|l| l.starts_with("would create ")));
    assert!(!out_dir.exists());

    // A file that isn't an alias is refused exactly as it would be...
    fs::create_dir_all(&out_dir).unwrap();
    fs::write(out_dir.join("notes"), "precious").unwrap();
    let out = patch(&["--dry-run"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(
        stderr(&out).contains("is not a rea alias"),
        "{}",
        stderr(&out)
    );
    assert!(
        stderr(&out).contains(
            "dry run: 1 created, 0 updated, 0 replaced, 0 unchanged, 0 removed, 1 failed"
        )
    );
    // ...and with --replace-non-aliases, reported as a replacement, untouched.
    let out = patch(&["--dry-run", "--replace-non-aliases"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(
        stdout(&out).contains("would replace ")
            && stdout(&out).contains(", which is not a rea alias"),
        "{}",
        stdout(&out)
    );
    assert!(
        stderr(&out).contains("dry run: 1 created, 0 updated, 1 replaced"),
        "{}",
        stderr(&out)
    );
    assert_eq!(
        fs::read_to_string(out_dir.join("notes")).unwrap(),
        "precious"
    );
    assert!(!out_dir.join("other").exists());
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
    assert!(
        err.contains("aliases.txt:2:") && err.contains("aliases.txt:3:"),
        "{err}"
    );
    assert!(!out_dir.join("good").exists());
    // `check` reports the same problems.
    assert_eq!(
        tool(&args!["check", file.as_os_str()]).status.code(),
        Some(1)
    );
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn patch_writes_only_changes_and_prunes_removed_aliases() {
    let dir = scratch("patch-prune");
    let (file, out_dir) = (dir.join("aliases.txt"), dir.join("bin"));
    let patch = |extra: &[&str]| {
        let mut args = vec![
            OsStr::new("patch"),
            file.as_os_str(),
            OsStr::new("-o"),
            out_dir.as_os_str(),
        ];
        args.extend(extra.iter().map(OsStr::new));
        tool(&args)
    };
    fs::write(&file, "keep = true\nchange = printf a\ngone = false\n").unwrap();
    assert!(patch(&[]).status.success());
    // An unrelated file in the directory must survive pruning.
    fs::write(out_dir.join("unrelated"), "x").unwrap();
    let keep_mtime = fs::metadata(out_dir.join("keep"))
        .unwrap()
        .modified()
        .unwrap();

    fs::write(&file, "keep = true\nchange = printf b\n").unwrap();
    // A dry run reports the same changes and makes none of them.
    let changed = |out: &Output| -> Vec<String> {
        stdout(out)
            .lines()
            .map(|l| l.replace(out_dir.to_str().unwrap(), "DIR"))
            .collect()
    };
    let out = patch(&["--prune", "--dry-run"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(
        changed(&out),
        ["would update DIR/change", "would remove DIR/gone"]
    );
    assert!(
        stderr(&out).contains(
            "dry run: 0 created, 1 updated, 0 replaced, 1 unchanged, 1 removed, 0 failed"
        ),
        "{}",
        stderr(&out)
    );
    assert!(out_dir.join("gone").exists());
    let out = patch(&["--prune"]);
    assert!(out.status.success(), "{}", stderr(&out));
    // Only the changed alias is rewritten; the deleted one is removed.
    assert_eq!(changed(&out), ["updated DIR/change", "removed DIR/gone"]);
    assert!(
        stderr(&out)
            .starts_with("0 created, 1 updated, 0 replaced, 1 unchanged, 1 removed, 0 failed"),
        "{}",
        stderr(&out)
    );
    assert_eq!(
        fs::metadata(out_dir.join("keep"))
            .unwrap()
            .modified()
            .unwrap(),
        keep_mtime
    );
    assert!(out_dir.join("unrelated").exists());
    assert_eq!(
        stdout(&Command::new(out_dir.join("change")).output().unwrap()),
        "b"
    );
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn list_output_is_an_alias_file_that_recreates_the_aliases() {
    let dir = scratch("round-trip");
    let (first, second) = (dir.join("first"), dir.join("second"));
    let defs = "pipe = printf 'b\\na\\n' | sort\nsay = printf %s {1}\n";
    fs::write(dir.join("aliases.txt"), defs).unwrap();
    assert!(
        tool(&args![
            OsStr::new("patch"),
            dir.join("aliases.txt").as_os_str(),
            OsStr::new("-o"),
            first.as_os_str()
        ])
        .status
        .success()
    );
    // `list` prints the definitions back, sorted by name...
    let listed = stdout(&tool(&args![OsStr::new("list"), first.as_os_str()]));
    assert_eq!(
        listed,
        "pipe = printf 'b\\na\\n' | sort\nsay = printf %s {1}\n"
    );
    // ...and patching from that output reproduces identical executables.
    fs::write(dir.join("listed.txt"), &listed).unwrap();
    assert!(
        tool(&args![
            OsStr::new("patch"),
            dir.join("listed.txt").as_os_str(),
            OsStr::new("-o"),
            second.as_os_str()
        ])
        .status
        .success()
    );
    for name in ["pipe", "say"] {
        assert_eq!(
            fs::read(first.join(name)).unwrap(),
            fs::read(second.join(name)).unwrap(),
            "{name}"
        );
    }
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn positions_above_the_cap_are_refused() {
    // The unit tests run with a smaller cap; this checks the real one, which
    // the binary under test is built with. Refused at parse time, so it's fast.
    let template = format!("echo {{{}}}", rea_core::max_position!() + 1);
    let out = tool(&args!("eval", &template));
    let expected = concat!("positions go up to ", rea_core::max_position!());
    assert!(stderr(&out).contains(expected), "{}", stderr(&out));
    assert_eq!(out.status.code(), Some(2));
}

#[test]
fn the_tool_itself_is_refused_as_a_stub() {
    // rea carries the marker bytes in its patching code. Treating that as
    // a slot would overwrite 4 KiB of the program, so it must refuse.
    let dir = scratch("stub-guard");
    let out = tool(&args![
        "new",
        "x",
        "true",
        "-o",
        dir.to_str().unwrap(),
        "--stub",
        env!("CARGO_BIN_EXE_rea")
    ]);
    assert_eq!(out.status.code(), Some(1));
    assert!(stderr(&out).contains("not a rea stub"), "{}", stderr(&out));
    assert_eq!(fs::read_dir(&dir).unwrap().count(), 0);
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn explain_reads_an_alias_executable() {
    let dir = scratch("explain-file");
    assert!(
        tool(&args![
            "new",
            "mv2",
            "mv {1} {1}.bak",
            "-o",
            dir.to_str().unwrap()
        ])
        .status
        .success()
    );
    let out = tool(&args![
        OsStr::new("explain"),
        dir.join("mv2").as_os_str(),
        OsStr::new("a b")
    ]);
    assert_eq!(
        stdout(&out),
        "   mv 'a b' 'a b.bak'\n(single command: runs via exec, no supervisor process)\n"
    );
    let _ = fs::remove_dir_all(&dir);
}

/// Run `rea` with arguments and `$PATH` set to `path`.
fn tool_with_path(args: &[&OsStr], path: &OsStr) -> Output {
    bin()
        .args(args)
        .env("PATH", path)
        .output()
        .expect("binary runs")
}

#[test]
fn bare_names_are_looked_up_on_path() {
    let dir = scratch("bare-names");
    let aliases = dir.join("aliases");
    let first = dir.join("first");
    fs::create_dir_all(&first).unwrap();
    let new = tool(&args![
        "new",
        "mv2",
        "mv {1} {1}.bak",
        "-o",
        aliases.to_str().unwrap()
    ]);
    assert!(new.status.success(), "{}", stderr(&new));
    let path = std::env::join_paths([&first, &aliases]).unwrap();

    // Found in a later $PATH directory, by `explain` and `inspect` alike.
    let out = tool_with_path(&args!["explain", "mv2", "a b"], &path);
    assert!(
        stdout(&out).starts_with("   mv 'a b' 'a b.bak'\n"),
        "{}",
        stderr(&out)
    );
    let out = tool_with_path(&args!["inspect", "mv2"], &path);
    assert_eq!(stdout(&out), "mv2 = mv {1} {1}.bak\n", "{}", stderr(&out));

    // The first executable on $PATH wins, as when running the name, even if
    // it isn't an alias; the error says which file was read.
    let shadow = first.join("mv2");
    fs::write(&shadow, "#!/bin/sh\n").unwrap();
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(&shadow, fs::Permissions::from_mode(0o755)).unwrap();
    let out = tool_with_path(&args!["explain", "mv2"], &path);
    assert_eq!(out.status.code(), Some(1));
    let expected = format!("{}: not a rea alias", shadow.display());
    assert!(stderr(&out).contains(&expected), "{}", stderr(&out));

    // A file that isn't executable is skipped, as the shell skips it.
    fs::set_permissions(&shadow, fs::Permissions::from_mode(0o644)).unwrap();
    let out = tool_with_path(&args!["inspect", "mv2"], &path);
    assert_eq!(stdout(&out), "mv2 = mv {1} {1}.bak\n", "{}", stderr(&out));

    // A bare name in the current directory isn't used unless $PATH says so,
    // but a path to it is.
    let out = bin()
        .args(["inspect", "mv2"])
        .env("PATH", &first)
        .current_dir(&aliases)
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert!(
        stderr(&out).contains("mv2: not found on PATH or in "),
        "{}",
        stderr(&out)
    );
    let out = bin()
        .args(["inspect", "./mv2"])
        .env("PATH", &first)
        .current_dir(&aliases)
        .output()
        .unwrap();
    assert_eq!(stdout(&out), "mv2 = mv {1} {1}.bak\n", "{}", stderr(&out));
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn alias_dir_variable_sets_the_default_directory() {
    let dir = scratch("alias-dir-var");
    let aliases = dir.join("aliases");
    let other = dir.join("other");
    let empty = dir.join("empty");
    fs::create_dir_all(&empty).unwrap();
    let run = |args: &[&str]| {
        bin()
            .args(args)
            .env("REA_ALIAS_DIR", &aliases)
            .env("PATH", &empty)
            .output()
            .unwrap()
    };

    // `new` writes there, and creates it.
    let out = run(&["new", "mv2", "mv {1} {1}.bak"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(aliases.join("mv2").is_file());
    // `-o` still wins over the variable.
    let out = run(&[
        "new",
        "cp2",
        "cp {1} {1}.bak",
        "-o",
        other.to_str().unwrap(),
    ]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(other.join("cp2").is_file() && !aliases.join("cp2").exists());

    // `list` lists it, and a bare name not on $PATH is found there.
    assert_eq!(stdout(&run(&["list"])), "mv2 = mv {1} {1}.bak\n");
    let out = run(&["inspect", "mv2"]);
    assert_eq!(stdout(&out), "mv2 = mv {1} {1}.bak\n", "{}", stderr(&out));

    // An empty value counts as unset: the default is this program's directory.
    let out = bin()
        .args(["inspect", "nosuch"])
        .env("REA_ALIAS_DIR", "")
        .env("PATH", &empty)
        .output()
        .unwrap();
    let own = PathBuf::from(env!("CARGO_BIN_EXE_rea"));
    let expected = format!(
        "not found on PATH or in {}",
        own.parent().unwrap().display()
    );
    assert!(stderr(&out).contains(&expected), "{}", stderr(&out));
    let _ = fs::remove_dir_all(&dir);
}

// ---------------------------------------------------------------------------
// `save`: updating an alias file from a directory.
// ---------------------------------------------------------------------------

/// A scratch directory with aliases `ll` and `enc` in `DIR/aliases`.
fn save_fixture(tag: &str) -> (PathBuf, PathBuf) {
    let dir = scratch(tag);
    let aliases = dir.join("aliases");
    for (name, template) in [("ll", "ls -la"), ("enc", "ffmpeg -i {1} {1}.mkv")] {
        let out = tool(&args![
            "new",
            name,
            template,
            "-o",
            aliases.to_str().unwrap()
        ]);
        assert!(out.status.success(), "{}", stderr(&out));
    }
    (dir, aliases)
}

/// Run `rea save FILE DIR` plus extra arguments.
fn save(file: &Path, aliases: &Path, extra: &[&str]) -> Output {
    bin()
        .arg("save")
        .arg(file)
        .arg(aliases)
        .args(extra)
        .output()
        .unwrap()
}

/// Names in `dir`, sorted, to check that no temporary files are left.
fn names_in(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

#[test]
fn save_updates_lines_in_place_and_appends_new_aliases() {
    let (dir, aliases) = save_fixture("save-merge");
    let file = dir.join("aliases.txt");
    let original = "# mine\nll    = ls -l\n\nold = echo kept\n";
    fs::write(&file, original).unwrap();

    // A dry run reports and writes nothing.
    let out = save(&file, &aliases, &["--dry-run"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(stdout(&out), "would update ll (line 2)\nwould add enc\n");
    assert_eq!(fs::read_to_string(&file).unwrap(), original);

    let out = save(&file, &aliases, &[]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(stdout(&out), "updated ll (line 2)\nadded enc\n");
    assert_eq!(
        fs::read_to_string(&file).unwrap(),
        "# mine\nll    = ls -la\n\nold = echo kept\n\n# added by rea save\nenc = ffmpeg -i {1} {1}.mkv\n"
    );
    // Saving again changes nothing, and the result patches cleanly.
    let out = save(&file, &aliases, &[]);
    assert_eq!(stderr(&out), "0 updated, 0 added, 2 unchanged\n");
    let check = tool(&args!["check", file.as_os_str()]);
    assert!(check.status.success(), "{}", stderr(&check));
    assert_eq!(names_in(&dir), ["aliases", "aliases.txt"]);
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn save_creates_a_missing_file_and_refuses_one_it_cannot_parse() {
    let (dir, aliases) = save_fixture("save-new");
    let file = dir.join("new.txt");
    let out = save(&file, &aliases, &[]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(
        fs::read_to_string(&file).unwrap(),
        "enc = ffmpeg -i {1} {1}.mkv\nll = ls -la\n"
    );

    // A duplicate name is reported with its line, and nothing is written.
    let bad = "ll = ls\nll = ls -l\n";
    fs::write(&file, bad).unwrap();
    let out = save(&file, &aliases, &[]);
    assert_eq!(out.status.code(), Some(1));
    assert!(
        stderr(&out).contains("new.txt:2: alias `ll` is already defined"),
        "{}",
        stderr(&out)
    );
    assert_eq!(fs::read_to_string(&file).unwrap(), bad);
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn save_keeps_symlinks_hard_links_and_permissions() {
    use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};
    let (dir, aliases) = save_fixture("save-links");

    // A symlink stays a link; the file it points to is updated.
    let real = dir.join("real.txt");
    let link = dir.join("link.txt");
    fs::write(&real, "ll = ls\n").unwrap();
    symlink(&real, &link).unwrap();
    let out = save(&link, &aliases, &[]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(
        fs::symlink_metadata(&link)
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert!(
        fs::read_to_string(&real)
            .unwrap()
            .starts_with("ll = ls -la\n")
    );

    // A lone file is replaced by rename (a new inode), keeping its mode.
    fs::set_permissions(&real, fs::Permissions::from_mode(0o640)).unwrap();
    fs::write(&real, "ll = ls\n").unwrap();
    let before = fs::metadata(&real).unwrap().ino();
    assert!(save(&real, &aliases, &[]).status.success());
    let meta = fs::metadata(&real).unwrap();
    assert_ne!(meta.ino(), before);
    assert_eq!(meta.permissions().mode() & 0o777, 0o640);

    // With a second hard link it is rewritten in place, so both names see the
    // new contents and the inode is the same.
    let hard = dir.join("hard.txt");
    fs::hard_link(&real, &hard).unwrap();
    fs::write(&real, "ll = ls\n").unwrap();
    let before = fs::metadata(&real).unwrap().ino();
    assert!(save(&real, &aliases, &[]).status.success());
    assert_eq!(fs::metadata(&real).unwrap().ino(), before);
    assert!(
        fs::read_to_string(&hard)
            .unwrap()
            .starts_with("ll = ls -la\n")
    );

    // No temporary file or backup is left behind.
    assert_eq!(
        names_in(&dir),
        ["aliases", "hard.txt", "link.txt", "real.txt"]
    );
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn save_refuses_aliases_that_disagree() {
    let (dir, aliases) = save_fixture("save-conflict");
    // A copy under another file name that holds a different `ll`.
    let out = tool(&args!["new", "ll", "ls -1", "-o", dir.to_str().unwrap()]);
    assert!(out.status.success(), "{}", stderr(&out));
    fs::rename(dir.join("ll"), aliases.join("ll-copy")).unwrap();
    let file = dir.join("aliases.txt");
    let out = save(&file, &aliases, &[]);
    assert_eq!(out.status.code(), Some(1));
    assert!(
        stderr(&out).contains("alias `ll` differs between"),
        "{}",
        stderr(&out)
    );
    assert!(!file.exists());
    let _ = fs::remove_dir_all(&dir);
}

// ---------------------------------------------------------------------------
// `eval -`: the template read from stdin.
// ---------------------------------------------------------------------------

/// Run `rea eval - ARGS...` with `input` on stdin and capture its output.
fn eval_stdin(input: &[u8], args: &[&str]) -> Output {
    use std::io::Write;
    use std::process::Stdio;
    let mut child = bin()
        .arg("eval")
        .arg("-")
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
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
    assert!(
        stderr(&out).contains("must be a single line"),
        "{}",
        stderr(&out)
    );
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
    let explained = bin()
        .args([
            "explain",
            "-t",
            "rm -f {@} && symlink {-1} {@}",
            "l1",
            "l2",
            "src",
        ])
        .output()
        .unwrap();
    assert_eq!(stdout(&explained), "   rm -f l1 l2\n&& symlink src l1 l2\n");
    // And for real, with commands that take several names: recreate two
    // files (one stale), then use the last argument on its own.
    let dir = scratch("chunk");
    let (a, b) = (dir.join("a"), dir.join("b"));
    fs::write(&a, "stale").unwrap();
    let out = eval(
        "rm -f {@} && touch {@} && printf %s {-1}",
        &[a.to_str().unwrap(), b.to_str().unwrap(), "done"],
    );
    assert_eq!(stdout(&out), "done", "{}", stderr(&out));
    assert_eq!(fs::read_to_string(&a).unwrap(), "");
    assert!(b.exists());
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn word_repetition_and_alias_name() {
    // -I{@} repeats per argument; {0} is the alias name (ours, for eval).
    let out = eval("printf [%s] {0} -I{@}", &["a", "b"]);
    assert_eq!(stdout(&out), "[rea][-Ia][-Ib]");
    // {@+} refuses to run with nothing to stand for.
    let none = eval("printf %s {@+}", &[]);
    assert_eq!(none.status.code(), Some(2));
    assert!(
        stderr(&none).contains("expected at least 1 argument, got 0"),
        "{}",
        stderr(&none)
    );
}

#[test]
fn new_rejects_templates_no_count_satisfies() {
    // Arguments 1 and 2 would never be used: refused at creation, no file.
    let dir = scratch("unsatisfiable");
    let out = tool(&args![
        "new",
        "bad",
        "echo {3..}",
        "-o",
        dir.to_str().unwrap()
    ]);
    assert_eq!(out.status.code(), Some(1));
    assert!(
        stderr(&out).contains("no number of arguments works"),
        "{}",
        stderr(&out)
    );
    assert_eq!(fs::read_dir(&dir).unwrap().count(), 0);
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn template_syntax_is_in_the_long_help() {
    // `--help` (long) carries the full syntax; `-h` points at it.
    for args in [&["--help"][..], &["new", "--help"], &["eval", "--help"]] {
        let help = stdout(&bin().args(args).output().unwrap());
        assert!(
            help.contains("{A..B}")
                && help.contains("{@+}")
                && help.contains("up to but not including"),
            "{args:?}: {help}"
        );
        // Checks and modifiers, including the two combined.
        assert!(
            help.contains("{1:/.}") && help.contains("{1:F/.}") && help.contains("{1:D//}"),
            "{args:?}: {help}"
        );
    }
    assert!(stdout(&bin().arg("-h").output().unwrap()).contains("rea --help"));
}

#[test]
fn closed_stdout_ends_quietly_instead_of_panicking() {
    // Like any Unix tool, `rea explain ... | head` must just stop when the
    // reader goes away. The output (~200 KB) is far larger than a pipe buffer,
    // so the write is certain to happen after `head -c 1` has exited.
    let script = r#""$0" explain -t 'echo {@}' $(seq 100000 130000) | head -c 1 >/dev/null"#;
    let out = Command::new("sh")
        .args(["-c", script, env!("CARGO_BIN_EXE_rea")])
        .output()
        .unwrap();
    assert!(!stderr(&out).contains("panicked"), "{}", stderr(&out));
    assert_eq!(stderr(&out), "");
}

// ---------------------------------------------------------------------------
// Path modifiers and file checks.
// ---------------------------------------------------------------------------

#[test]
fn modifiers_through_explain() {
    let out = bin()
        .args([
            "explain",
            "-t",
            "convert {1} {1://}/{1:/.}-thumb.jpg",
            "pics/a b.png",
        ])
        .output()
        .unwrap();
    assert_eq!(
        stdout(&out),
        "   convert 'pics/a b.png' 'pics/a b-thumb.jpg'\n(single command: runs via exec, no supervisor process)\n"
    );
}

#[test]
fn failed_check_stops_everything_before_it_starts() {
    // All checks run before the first command, so `touch` never runs here.
    let dir = scratch("check-first");
    let (made, missing) = (dir.join("made"), dir.join("missing"));
    let out = eval(
        "touch {1} && cat {2:F}",
        &[made.to_str().unwrap(), missing.to_str().unwrap()],
    );
    assert_eq!(out.status.code(), Some(2));
    let err = stderr(&out);
    assert!(
        err.contains("{2:F} needs an existing file, but '") && err.contains("' does not exist"),
        "{err}"
    );
    assert!(!made.exists(), "the first command must not have run");
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn new_rejects_bad_formats() {
    let dir = scratch("bad-format");
    let out = tool(&args!["new", "x", "cat {1:Q}", "-o", dir.to_str().unwrap()]);
    assert_eq!(out.status.code(), Some(1));
    assert!(
        stderr(&out).contains("unknown format `:Q`"),
        "{}",
        stderr(&out)
    );
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
    assert!(
        stderr(&second).contains("the word {1}.out{:N} needs a path that doesn't exist yet"),
        "{}",
        stderr(&second)
    );
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
    assert!(
        err.contains("would not run: the word {1}.jpg{:N} needs a path that doesn't exist yet"),
        "{err}"
    );
    assert!(
        err.contains("would not run: {2:D} needs an existing directory"),
        "{err}"
    );
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn explain_exits_as_the_alias_would() {
    // A wrong count and an invalid template are usage errors for the alias,
    // so explain gives the same 2.
    let few = bin()
        .args(["explain", "-t", "cp {1} {2}", "a"])
        .output()
        .unwrap();
    assert_eq!(few.status.code(), Some(2), "{}", stderr(&few));
    let invalid = bin()
        .args(["explain", "-t", "echo $HOME"])
        .output()
        .unwrap();
    assert_eq!(invalid.status.code(), Some(2), "{}", stderr(&invalid));
    // Something that isn't an alias at all is the tool's own error.
    let missing = bin().args(["explain", "/no/such/alias"]).output().unwrap();
    assert_eq!(missing.status.code(), Some(1), "{}", stderr(&missing));
}

#[test]
fn env_references_at_run_time() {
    let dir = scratch("env-run");
    let marker = dir.join("ran");
    // Set: the value is one argument, spaces and all.
    let out = bin()
        .args(["eval", "printf '%s|' {$GREETING}"])
        .env("GREETING", "hello world")
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(stdout(&out), "hello world|");
    // Unset: nothing runs, not even the steps before it.
    let template = format!("touch {} && echo {{$REA_TEST_UNSET}}", marker.display());
    let out = bin()
        .args(["eval", &template])
        .env_remove("REA_TEST_UNSET")
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    assert!(
        stderr(&out).contains("{$REA_TEST_UNSET} is not set"),
        "{}",
        stderr(&out)
    );
    assert!(!marker.exists());
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn explain_shows_environment_values() {
    let out = bin()
        .args(["explain", "-t", "rsync -ahP {@+} {$BACKUP_DIR}", "a"])
        .env("BACKUP_DIR", "/mnt/my backup")
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(
        stdout(&out),
        "   rsync -ahP a '/mnt/my backup'\n\
         (single command: runs via exec, no supervisor process)\n\
         (from the environment: BACKUP_DIR='/mnt/my backup')\n"
    );
    // Unset: the plan is still shown, with the reference in its place.
    let out = bin()
        .args(["explain", "-t", "rsync -ahP {@+} {$BACKUP_DIR}", "a"])
        .env_remove("BACKUP_DIR")
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    assert!(
        stdout(&out).starts_with("   rsync -ahP a '{$BACKUP_DIR}'\n"),
        "{}",
        stdout(&out)
    );
    assert!(
        stderr(&out).contains("would not run: {$BACKUP_DIR} is not set"),
        "{}",
        stderr(&out)
    );
}

#[test]
fn alias_runs_with_standard_fds_closed() {
    let dir = scratch("closed-fds");
    let out_dir = dir.to_str().unwrap();
    for (name, template) in [("say", "printf abc"), ("sayall", "printf abc | cat")] {
        let made = tool(&args!["new", name, template, "-o", out_dir]);
        assert!(made.status.success(), "{}", stderr(&made));
    }
    let run = |command: String| {
        let script = format!("{command} >&-");
        Command::new("sh").args(["-c", &script]).output().unwrap()
    };
    // A single command is exec'd with its descriptors untouched, so with
    // stdout closed it fails exactly as it does when run directly.
    let direct = run("printf abc".into());
    let alias = run(dir.join("say").display().to_string());
    assert!(!direct.status.success());
    assert_eq!(
        alias.status.code(),
        direct.status.code(),
        "{}",
        stderr(&alias)
    );
    // A pipeline reopens closed descriptors on /dev/null before making its
    // pipes, as std's runtime did, so the last stage writes there instead.
    let piped = run(dir.join("sayall").display().to_string());
    assert!(piped.status.success(), "{}", stderr(&piped));
    // Arguments still arrive intact, non-UTF-8 bytes included.
    let made = tool(&args![
        "new",
        "show",
        "printf '%s|' {@}",
        "-o",
        dir.to_str().unwrap()
    ]);
    assert!(made.status.success(), "{}", stderr(&made));
    use std::os::unix::ffi::OsStrExt;
    let raw = OsStr::from_bytes(b"caf\xe9");
    let out = Command::new(dir.join("show"))
        .args([OsStr::new("a b"), raw])
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(out.stdout, b"a b|caf\xe9|");
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn alias_handles_arguments_beyond_its_arena() {
    // The stub allocates from a 64 KiB static buffer first and falls back to
    // the system allocator; about 200 KB of arguments goes well past it, and
    // through the copy when a block outgrows the buffer.
    let dir = scratch("big-args");
    let made = tool(&args![
        "new",
        "count",
        "printf '%s\\n' {@+} | wc -l",
        "-o",
        dir.to_str().unwrap()
    ]);
    assert!(made.status.success(), "{}", stderr(&made));
    let args: Vec<String> = (0..2000).map(|i| format!("{i:0>100}")).collect();
    let out = Command::new(dir.join("count"))
        .args(&args)
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(stdout(&out).trim(), "2000");
    // The single-command path too: the exec'd program sees every argument.
    let made = tool(&args![
        "new",
        "each",
        "printf '%s\\n' {@}",
        "-o",
        dir.to_str().unwrap()
    ]);
    assert!(made.status.success(), "{}", stderr(&made));
    let out = Command::new(dir.join("each")).args(&args).output().unwrap();
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(stdout(&out), args.join("\n") + "\n");
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn completion_script_is_for_the_name_run_as() {
    // The plain binary: the script registers itself for `rea`.
    let out = tool(&args!["completions", "bash"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(
        stdout(&out).contains("complete -F _rea"),
        "{}",
        stdout(&out)
    );
    // A renamed copy (here a symlink) gets a script for its own name, since
    // the shell looks scripts up by the command typed.
    let dir = scratch("completions");
    let renamed = dir.join("rea");
    std::os::unix::fs::symlink(env!("CARGO_BIN_EXE_rea"), &renamed).unwrap();
    let out = Command::new(&renamed)
        .args(["completions", "bash"])
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", stderr(&out));
    let script = stdout(&out);
    assert!(script.contains("complete -F _rea"), "{script}");
    let _ = fs::remove_dir_all(&dir);
}
