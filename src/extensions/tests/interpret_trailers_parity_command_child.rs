//! `trailer.<key>.cmd` and `trailer.<key>.command` run as `use_shell` children.
//!
//! ```c
//! strvec_pushv(&cp.env, (const char **)local_repo_env);
//! cp.no_stdin = 1;
//! cp.use_shell = 1;
//! if (capture_command(&cp, &buf, 1024)) {
//!         error(_("running trailer command '%s' failed"), cmd.buf);
//! ```
//!
//! (`apply_command()`, trailer.c:218-251.) `prepare_shell_cmd()` (run-command.c:
//! 288-309) execs a word with no shell metacharacter directly — so a missing
//! program is `error: cannot run <cmd>: No such file or directory`, or the
//! child's `fatal: cannot exec '<cmd>': ...` for a path — and otherwise runs
//! `sh -c` with the command text itself as `$0`. The child loses the repository
//! environment (`GIT_DIR`, `GIT_CONFIG_PARAMETERS`, ...). zvcs always went
//! through `sh`, named it `sh` in `$0` and in the shell's own diagnostics, and
//! passed the repository environment on.
//!
//! Expectations come from stock git (`support/stock_git.rs`) in an identical
//! repository.
#![cfg(unix)]

#[path = "support/stock_git.rs"]
mod stock_git;
#[path = "support/twin_repo.rs"]
mod twin_repo;

const MSG: &str = "subject\n\nbody\n\nSigned-off-by: A <a@x>\nAcked-by: B <b@x>\n";

#[test]
fn trailer_commands_run_as_git_runs_them() {
    let Some(stock) = stock_git::stock_git() else { return };
    let (s, z) = twin_repo::pair("trailer-cmd-child", stock);
    for side in [&s, &z] {
        side.write("msg", MSG);
    }
    let cases: &[(&[(&str, &str)], &[&str])] = &[
        (&[], &["-c", "trailer.a.cmd=nosuchprog", "interpret-trailers", "--trailer", "a=q", "msg"]),
        (&[], &["-c", "trailer.a.command=nosuchprog", "interpret-trailers", "--trailer", "a=q", "msg"]),
        (&[], &["-c", "trailer.a.cmd=./nosuchprog", "interpret-trailers", "--trailer", "a=q", "msg"]),
        (&[], &["-c", "trailer.a.command=echo $0", "interpret-trailers", "--trailer", "a=q", "msg"]),
        (&[], &["-c", "trailer.a.cmd=echo $0 \"$1\"", "interpret-trailers", "--trailer", "a=q", "msg"]),
        (&[], &["-c", "trailer.a.command=echo $ARG-x", "interpret-trailers", "--trailer", "a=q", "msg"]),
        (&[], &["-c", "trailer.a.cmd=printf", "interpret-trailers", "--trailer", "a=q", "msg"]),
        (
            &[("GIT_DIR", ".git")],
            &["-c", "trailer.a.command=echo \"${GIT_DIR-unset} ${GIT_CONFIG_PARAMETERS-unset}\"", "interpret-trailers", "--trailer", "a=q", "msg"],
        ),
    ];
    for (env, args) in cases {
        assert_eq!(z.git_env(env, args), s.git_env(env, args), "{env:?} {args:?}");
    }
}

#[test]
fn a_commit_trailer_command_sees_no_repository_environment() {
    let Some(stock) = stock_git::stock_git() else { return };
    let (s, z) = twin_repo::pair("trailer-cmd-commit-env", stock);
    let env = [("GIT_DIR", ".git")];
    let args = [
        "-c",
        "trailer.a.command=echo \"${GIT_DIR-unset} ${GIT_INDEX_FILE-unset}\"",
        "commit",
        "--allow-empty",
        "-m",
        "x",
        "--trailer",
        "a=q",
    ];
    assert_eq!(z.git_env(&env, &args), s.git_env(&env, &args));
    let show = ["log", "-1", "--format=%B"];
    assert_eq!(z.git(&show), s.git(&show));
}
