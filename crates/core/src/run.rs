//! Process execution: a direct `exec` for single-command aliases, and a small
//! supervisor for pipelines and `&&` / `||` / `;` lists.
//!
//! The supervisor follows bash's observable behavior where it matters to
//! callers (exit codes 126/127, signal deaths propagated, Ctrl-C stops a list)
//! with two deliberate deviations, both documented at `pipeline_outcome`.

use std::ffi::CString;
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::process::ExitStatusExt;
use std::process::{Child, ChildStdout, Command, ExitStatus, Stdio};

use crate::template::{Argv, Op, Plan};

/// How a process (or a whole pipeline) ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// Normal exit with this status code.
    Exited(i32),
    /// Killed by this signal number.
    Signaled(i32),
}

impl Outcome {
    /// Convert a wait status from `Child::wait`.
    fn from_status(status: ExitStatus) -> Self {
        match (status.code(), status.signal()) {
            (Some(code), _) => Outcome::Exited(code),
            (None, Some(signal)) => Outcome::Signaled(signal),
            // Only stopped/continued states have neither, and `wait` never
            // reports those (it doesn't pass WUNTRACED). Generic failure.
            (None, None) => Outcome::Exited(1),
        }
    }

    /// Whether this counts as success for `&&` / `||`: exit status 0 only.
    pub fn success(self) -> bool {
        self == Outcome::Exited(0)
    }

    /// Whether the user interrupted it from the terminal: death by SIGINT
    /// (Ctrl-C) or SIGQUIT (Ctrl-\).
    fn interrupted(self) -> bool {
        matches!(self, Outcome::Signaled(libc::SIGINT | libc::SIGQUIT))
    }
}

/// Exit status for a program that could not be started, per shell convention:
/// 127 for "not found", 126 for anything else (permissions, bad format).
fn spawn_failure_code(err: &io::Error) -> i32 {
    if err.kind() == io::ErrorKind::NotFound {
        127
    } else {
        126
    }
}

/// Print a spawn or exec failure to stderr, prefixed with the alias name so
/// the user knows which alias failed.
fn report_spawn_failure(alias: &str, program: &std::ffi::OsStr, err: &io::Error) {
    let program = program.to_string_lossy();
    // Mirror the shell's wording for the common case; it's what users grep for.
    if err.kind() == io::ErrorKind::NotFound {
        eprintln!("{alias}: {program}: command not found");
    } else {
        eprintln!("{alias}: {program}: {err}");
    }
}

/// Build a `Command` from an argv, with std's PATH lookup applying to
/// `argv[0]`.
///
/// `argv` is non-empty; the expander guarantees this.
fn command_for(argv: &Argv) -> Command {
    let mut command = Command::new(&argv[0]);
    command.args(&argv[1..]);
    command
}

/// Replace this process with the single command.
///
/// On success this process *becomes* the target: same PID, signals delivered
/// straight to it, and its exit status is ours. That is why single-command
/// aliases skip the supervisor entirely. Never returns: if the exec fails, it
/// exits with 126 or 127.
///
/// The target gets everything exactly as the alias received it: file
/// descriptors (even closed ones), signal dispositions, environment. Running
/// an alias is then indistinguishable from running the command directly.
///
/// Constraint on change: this calls `execvp` itself rather than going through
/// std's `Command::exec`, which always resets SIGPIPE to the default first.
/// That reset exists because std's runtime ignores SIGPIPE at startup; the
/// stub skips that runtime, so SIGPIPE is already whatever the alias
/// inherited. If it was inherited *ignored*, it stays ignored for the target,
/// which is what POSIX shells do with signals ignored on entry, and what the
/// supervisor does for SIGINT (see `shield_from_terminal_signals`). The
/// `aliasmux eval` path restores the default at startup, so it gets the
/// default here too.
pub fn exec_single(alias: &str, argv: &Argv) -> ! {
    // A NUL can't reach an exec, so a word containing one (only possible from
    // the template text itself) is reported like any other start failure.
    let cstrings: Result<Vec<CString>, _> = argv
        .iter()
        .map(|word| CString::new(word.as_bytes()))
        .collect();
    let err = match cstrings {
        Ok(cstrings) => {
            let mut pointers: Vec<*const libc::c_char> =
                cstrings.iter().map(|c| c.as_ptr()).collect();
            pointers.push(std::ptr::null());
            // SAFETY: `pointers` is a NULL-terminated array of pointers to
            // NUL-terminated strings that outlive the call. `execvp` searches
            // PATH like the shell and only returns on failure.
            unsafe { libc::execvp(pointers[0], pointers.as_ptr()) };
            io::Error::last_os_error()
        }
        Err(_) => io::Error::new(io::ErrorKind::InvalidInput, "argument contains a NUL byte"),
    };
    report_spawn_failure(alias, &argv[0], &err);
    std::process::exit(spawn_failure_code(&err));
}

/// A pipeline stage after the spawn attempt.
enum Stage {
    /// Started; must be waited for.
    Running(Child),
    /// Never started; its outcome is already known.
    Failed(Outcome),
}

/// Combine stage outcomes, left to right and non-empty, into the pipeline's
/// outcome.
///
/// Deviation from sh: the result is the rightmost *failing* stage (like bash's
/// `set -o pipefail`), not simply the last stage. Without this, in
/// `flif -d {1} - | ... | flif ... {1}.rot.flif && mv {1}.rot.flif {1}` a
/// failed decode could still let the `mv` replace the original with garbage.
///
/// Deviation from bash's pipefail: a non-final stage killed by SIGPIPE counts
/// as success. It means a later stage stopped reading on purpose (`yes | head`),
/// which plain pipefail misreports as failure.
fn pipeline_outcome(outcomes: &[Outcome]) -> Outcome {
    let last = outcomes.len() - 1;
    // Scan right to left so the first failure found is the rightmost one.
    for (index, outcome) in outcomes.iter().enumerate().rev() {
        let benign_sigpipe = index != last && *outcome == Outcome::Signaled(libc::SIGPIPE);
        if !outcome.success() && !benign_sigpipe {
            return *outcome;
        }
    }
    Outcome::Exited(0)
}

/// Run one pipeline, given as argv per stage (left to right, non-empty), to
/// completion, and combine the outcomes as [`pipeline_outcome`] does.
fn run_pipeline(alias: &str, stages: &[Argv]) -> Outcome {
    let last = stages.len() - 1;
    let mut spawned = Vec::with_capacity(stages.len());
    // Read end of the pipe from the previous stage, waiting to become the next
    // stage's stdin. `None` before the first stage, or if the previous stage
    // failed to spawn.
    let mut upstream: Option<ChildStdout> = None;

    for (index, argv) in stages.iter().enumerate() {
        let mut command = command_for(argv);
        // Stage 0 inherits our stdin. Later stages read the previous stage's
        // output; if that stage never started, they get EOF (/dev/null) like in
        // a shell, rather than stealing our stdin from stage 0.
        if index > 0 {
            command.stdin(upstream.take().map_or_else(Stdio::null, Stdio::from));
        }
        // Every stage but the last writes into a fresh pipe; the last inherits
        // our stdout. std creates pipes close-on-exec, so no other child
        // inherits stray ends.
        if index < last {
            command.stdout(Stdio::piped());
        }
        // A failed spawn is recorded, not fatal: the other stages still run and
        // see EOF / SIGPIPE, the same as when a shell can't find a command.
        let stage = match command.spawn() {
            Ok(mut child) => {
                upstream = child.stdout.take();
                Stage::Running(child)
            }
            Err(err) => {
                report_spawn_failure(alias, &argv[0], &err);
                Stage::Failed(Outcome::Exited(spawn_failure_code(&err)))
            }
        };
        spawned.push(stage);
        // `command` drops here, closing our copy of the pipe read end we just
        // handed to the child. Constraint on change: that copy must not outlive
        // this iteration. While we hold a read end, the upstream writer never
        // gets SIGPIPE, so `yes | head` would hang forever.
    }

    // Wait for every stage, in order. Order doesn't matter for correctness:
    // the children run concurrently and each wait just blocks until that one
    // is done. `wait` retries on EINTR internally, which our no-op signal
    // handler (see `shield_from_terminal_signals`) relies on.
    let outcomes: Vec<Outcome> = spawned
        .into_iter()
        .map(|stage| match stage {
            // `wait` can only fail with ECHILD, which means the child was reaped
            // elsewhere; nothing in this process does that, so it's a generic failure.
            Stage::Running(mut child) => child
                .wait()
                .map_or(Outcome::Exited(1), Outcome::from_status),
            Stage::Failed(outcome) => outcome,
        })
        .collect();
    pipeline_outcome(&outcomes)
}

/// Run a whole plan: the first pipeline, then each operator-chained one.
///
/// Returns the outcome of the last pipeline that ran.
pub fn run_plan(alias: &str, plan: &Plan) -> Outcome {
    let mut outcome = run_pipeline(alias, &plan.head);
    for (op, stages) in &plan.tail {
        // After Ctrl-C the user wants everything to stop, not just the current
        // step, so even `;` and `||` are skipped (bash does the same).
        if outcome.interrupted() {
            break;
        }
        // A skipped pipeline leaves `outcome` unchanged, which gives the shell's
        // `a && b || c` meaning: if `a` fails, `b` is skipped and `c` runs.
        let should_run = match op {
            Op::And => outcome.success(),
            Op::Or => !outcome.success(),
            Op::Then => true,
        };
        if should_run {
            outcome = run_pipeline(alias, stages);
        }
    }
    outcome
}

/// Let SIGPIPE end this process quietly, as it does other Unix tools.
///
/// Rust's std sets SIGPIPE to "ignored" before `main`, so writing to a closed
/// pipe returns an error instead, and `println!` turns that error into a
/// panic: `aliasmux list | head` would print "failed printing to stdout".
/// Restoring the default makes the process simply end, as `ls | head` does.
///
/// Only the `aliasmux` tool needs this. The stub skips std's runtime setup
/// (it has no `main` of std's), so its SIGPIPE starts at whatever it
/// inherited, and the commands it starts get SIGPIPE back at its default from
/// `Command` either way (see `exec_single`).
pub fn restore_default_sigpipe() {
    // SAFETY: a plain disposition change with no handler; we are still
    // single-threaded at this point (called first thing in `main`).
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
    }
}

/// Ignore SIGPIPE in the supervisor, so a write to a closed pipe fails with
/// EPIPE instead of killing it.
///
/// The supervisor writes only error messages, to stderr. If stderr is a pipe
/// whose reader has gone, dying there would abandon children that are still
/// running and lose their exit status. std's runtime used to set this up for
/// the stub before `main`; the stub now skips that setup, so it is done here,
/// only when there is a supervisor. Children are unaffected: `Command` resets
/// SIGPIPE to the default in each one.
pub fn ignore_sigpipe() {
    // SAFETY: a plain disposition change with no handler.
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_IGN);
    }
}

/// Make sure file descriptors 0, 1 and 2 are open, opening `/dev/null` on any
/// that are closed.
///
/// Only the supervisor needs this, since it is the only path that opens
/// descriptors of its own (the pipes between stages). Without it, a closed
/// fd 2 would be the first one a pipe received, and the supervisor's own
/// error messages ("command not found") would be written into a stage's
/// input. A single command is exec'd with its descriptors untouched instead,
/// closed ones included, exactly as if run directly (see `exec_single`).
///
/// Deviation from sh: a pipeline started with, say, stdout closed gives its
/// last stage `/dev/null` there, where sh would leave it closed. std's
/// runtime made the same choice for every Rust program.
///
/// One `poll` call when all three are open, which is almost always.
pub fn ensure_standard_fds() {
    let mut fds = [0, 1, 2].map(|fd| libc::pollfd {
        fd,
        events: 0,
        revents: 0,
    });
    // SAFETY: `fds` is a valid array of three pollfd; a zero timeout returns
    // at once. Closed descriptors are reported as POLLNVAL.
    let ready = unsafe { libc::poll(fds.as_mut_ptr(), 3, 0) };
    if ready <= 0 {
        return;
    }
    for pfd in fds.iter().filter(|p| p.revents & libc::POLLNVAL != 0) {
        // `open` returns the lowest free descriptor, which is this one: the
        // ones below it are open, or were reopened by earlier iterations.
        // SAFETY: a constant NUL-terminated path and plain flags.
        let fd = unsafe { libc::open(c"/dev/null".as_ptr(), libc::O_RDWR) };
        // Assumption: /dev/null exists. If it doesn't, carry on; the alias
        // still runs, just without this protection.
        if fd >= 0 && fd != pfd.fd {
            // SAFETY: closing a descriptor this function just opened.
            unsafe { libc::close(fd) };
        }
    }
}

/// Signal handler that does nothing.
extern "C" fn do_nothing(_signal: libc::c_int) {}

/// Keep the supervisor alive through Ctrl-C / Ctrl-\ while children run.
///
/// The terminal sends SIGINT to the whole foreground process group, so the
/// children receive it directly. The supervisor must survive it so that it can
/// wait for children that handle SIGINT gracefully (ffmpeg finalizing a file)
/// and then report their outcome, instead of returning the prompt early.
///
/// Constraint on change: this installs a *no-op handler*, not `SIG_IGN`. An
/// ignored signal stays ignored across `exec`, so every child would become
/// immune to Ctrl-C. A caught signal is reset to the default on `exec`, so
/// children are unaffected. SIGTERM is deliberately not caught: it targets the
/// supervisor alone, and dying is the right response (bash -c does the same).
///
/// A signal that is already ignored when we start is left ignored. That is how
/// a background job (`alias &` in a script, or under nohup) is protected from
/// Ctrl-C, and the children must inherit the protection. Installing the
/// handler anyway would undo it: the handler resets to the *default* on exec,
/// not to the disposition we started with. (The approach of alias_runtime.hpp,
/// which checks the old disposition the same way.)
pub fn shield_from_terminal_signals() {
    for signal in [libc::SIGINT, libc::SIGQUIT] {
        // SAFETY: a zeroed `sigaction` is a valid "no flags, empty mask" value
        // that we then fill in, or that `sigaction` fills in when querying.
        // The handler is trivially async-signal-safe. Assumption: we are
        // single-threaded here, so the query and the install can't race with
        // other code changing dispositions.
        unsafe {
            // Query only (null new action) to see what we inherited.
            let mut current: libc::sigaction = std::mem::zeroed();
            libc::sigaction(signal, std::ptr::null(), &mut current);
            // Inherited SIG_IGN: keep it, for us and (via exec) the children.
            if current.sa_sigaction == libc::SIG_IGN {
                continue;
            }
            let mut action: libc::sigaction = std::mem::zeroed();
            action.sa_sigaction = do_nothing as extern "C" fn(libc::c_int) as libc::sighandler_t;
            // SA_RESTART lets blocking calls resume instead of failing with EINTR.
            action.sa_flags = libc::SA_RESTART;
            libc::sigemptyset(&mut action.sa_mask);
            libc::sigaction(signal, &action, std::ptr::null_mut());
        }
    }
}

/// Exit with the plan's outcome, as transparently as possible.
pub fn exit_with(outcome: Outcome) -> ! {
    match outcome {
        Outcome::Exited(code) => std::process::exit(code),
        Outcome::Signaled(signal) => {
            // Die by the same signal so our caller sees what really happened.
            // This matters most for SIGINT: bash only aborts a running script or
            // loop on Ctrl-C if the child died *by SIGINT*, not if it exited 130.
            // Arbitrary choice: limited to signals that don't dump core, so the
            // supervisor (which didn't crash) never leaves a core file behind.
            if matches!(
                signal,
                libc::SIGINT | libc::SIGTERM | libc::SIGHUP | libc::SIGPIPE
            ) {
                // SAFETY: plain libc calls; restoring the default disposition
                // first makes `raise` terminate us instead of hitting our
                // no-op handler (SIGINT) or std's ignore (SIGPIPE).
                unsafe {
                    libc::signal(signal, libc::SIG_DFL);
                    libc::raise(signal);
                }
            }
            // Other signals (or if raise somehow returned): shell convention.
            std::process::exit(128 + signal)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pipeline_outcome_rules() {
        use Outcome::{Exited, Signaled};
        // All success, rightmost failure wins, and SIGPIPE upstream is benign.
        assert_eq!(pipeline_outcome(&[Exited(0), Exited(0)]), Exited(0));
        assert_eq!(
            pipeline_outcome(&[Exited(2), Exited(3), Exited(0)]),
            Exited(3)
        );
        assert_eq!(
            pipeline_outcome(&[Signaled(libc::SIGPIPE), Exited(0)]),
            Exited(0)
        );
        // ...but SIGPIPE in the last stage is our own output failing: report it.
        assert_eq!(
            pipeline_outcome(&[Exited(0), Signaled(libc::SIGPIPE)]),
            Signaled(libc::SIGPIPE)
        );
    }
}
