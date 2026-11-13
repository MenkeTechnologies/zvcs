use std::process::Stdio;

use bstr::{BStr, BString};

use crate::{
    Driver, driver,
    driver::{Operation, Process, State, process, substitute_f_parameter},
};

/// The error returned by [State::maybe_launch_process()][super::State::maybe_launch_process()].
#[derive(Debug, thiserror::Error)]
#[expect(missing_docs)]
pub enum Error {
    #[error("Failed to spawn driver: {command:?}")]
    SpawnCommand {
        source: std::io::Error,
        command: std::process::Command,
    },
    #[error("Process handshake with command {command:?} failed")]
    ProcessHandshake {
        source: process::client::handshake::Error,
        command: std::process::Command,
    },
}

/// Lifecycle
impl State {
    /// Obtain a process as defined in `driver` suitable for a given `operation. `rela_path` may be used to substitute the current
    /// file for use in the invoked `SingleFile` process.
    ///
    /// Note that if a long-running process is defined, the `operation` isn't relevant and capabilities are to be checked by the caller.
    pub fn maybe_launch_process(
        &mut self,
        driver: &Driver,
        operation: Operation,
        rela_path: &BStr,
    ) -> Result<Option<Process<'_>>, Error> {
        match driver.process.as_ref() {
            Some(process) => {
                let client = match self.running.remove(process) {
                    Some(c) => c,
                    None => {
                        // `subprocess_start()` (sub-process.c:75-110): a command that will not start
                        // and a handshake that fails are each reported, and the filter is skipped.
                        let (child, cmd) = spawn_driver(process.clone(), &self.context).inspect_err(|_| {
                            eprintln!("error: cannot fork to run subprocess '{process}'");
                        })?;
                        process::Client::try_handshake(child, "git-filter", &[2], &["clean", "smudge", "delay"])
                            .map_err(|failed| {
                                let process::client::handshake::Failed {
                                    error,
                                    process: mut child,
                                    input,
                                    out,
                                } = *failed;
                                if !matches!(error, process::client::handshake::Error::UnsupportedCapability { .. }) {
                                    eprintln!("error: {}", error.git_message(&process.to_string()));
                                    eprintln!("error: initialization for subprocess '{process}' failed");
                                }
                                subprocess_stop(&mut child, process.as_ref());
                                drop((input, out));
                                Error::ProcessHandshake {
                                    source: error,
                                    command: cmd,
                                }
                            })?
                    }
                };

                // TODO: find a way to not have to do this 'borrow-dance'.
                // this strangeness is to workaround the borrowchecker, who otherwise won't let us return a reader. Quite sad :/.
                // One would want to `get_mut()` or insert essentially, but it won't work.
                self.running.insert(process.clone(), client);
                let client = self.running.get_mut(process).expect("just inserted");

                Ok(Some(Process::MultiFile {
                    client,
                    key: driver::Key(process.to_owned()),
                }))
            }
            None => {
                let cmd = match operation {
                    Operation::Clean => driver
                        .clean
                        .as_ref()
                        .map(|cmd| substitute_f_parameter(cmd.as_ref(), rela_path)),

                    Operation::Smudge => driver
                        .smudge
                        .as_ref()
                        .map(|cmd| substitute_f_parameter(cmd.as_ref(), rela_path)),
                };

                let cmd = match cmd {
                    Some(cmd) => cmd,
                    None => return Ok(None),
                };

                // `filter_buffer_or_fd()` names the command as configured (`params->cmd`), and
                // `apply_single_file_filter()` reports the failed async after it (convert.c:665-668,
                // 734-736).
                let (child, command) = spawn_driver(cmd, &self.context).inspect_err(|_| {
                    let configured = match operation {
                        Operation::Clean => driver.clean.as_ref(),
                        Operation::Smudge => driver.smudge.as_ref(),
                    }
                    .expect("a command was substituted from it");
                    eprintln!("error: cannot fork to run external filter '{configured}'");
                    eprintln!("error: external filter '{configured}' failed");
                })?;
                Ok(Some(Process::SingleFile { child, command }))
            }
        }
    }
}

fn spawn_driver(
    cmd: BString,
    context: &gix_command::Context,
) -> Result<(std::process::Child, std::process::Command), Error> {
    let program = cmd.clone();
    let mut cmd: std::process::Command = gix_command::prepare(gix_path::from_bstr(cmd).into_owned())
        .command_may_be_shell_script()
        .with_context(context.clone())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .into();
    gix_trace::debug!(cmd = ?cmd, "launching filter driver");
    let child = match cmd.spawn() {
        Ok(child) => child,
        Err(err) => {
            report_start_failure(program.as_ref(), &err);
            return Err(Error::SpawnCommand {
                source: err,
                command: cmd,
            });
        }
    };
    Ok((child, cmd))
}

/// What `start_command()` reports for a `use_shell` command that would not start. Only a
/// command `prepare_shell_cmd()` runs without a shell can fail here (run-command.c:290-293);
/// one without a `/` is looked up in `PATH` first, and not finding it is `cannot run`
/// (run-command.c:435-444, 757-761), while an `execve()` that fails is the child's `die()`,
/// relayed as `fatal: cannot exec` (run-command.c:378-405, 870-898).
fn report_start_failure(program: &BStr, err: &std::io::Error) {
    // `strerror()`: the description Rust puts in front of its ` (os error N)`.
    let full = err.to_string();
    let reason = full.rsplit_once(" (os error ").map_or(full.as_str(), |(reason, _)| reason);
    if program.contains(&b'/') {
        eprintln!("fatal: cannot exec '{program}': {reason}");
    } else {
        eprintln!("error: cannot run {program}: {reason}");
    }
}

/// `subprocess_stop()` (sub-process.c:52-62): `SIGTERM`, then `finish_command()`, whose
/// `wait_or_whine()` reports a child that a signal other than `SIGINT`, `SIGQUIT` or `SIGPIPE`
/// ended (run-command.c:574-577). The pipes are still open while it is stopped.
fn subprocess_stop(child: &mut std::process::Child, cmd: &BStr) {
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        // SAFETY: `kill()` takes plain integers; the pid is that of a child this process has not
        // reaped yet, so it cannot name another process.
        #[expect(unsafe_code)]
        unsafe {
            libc::kill(child.id() as libc::pid_t, libc::SIGTERM);
        }
        if let Some(signal) = child.wait().ok().and_then(|status| status.signal()) {
            if ![libc::SIGINT, libc::SIGQUIT, libc::SIGPIPE].contains(&signal) {
                eprintln!("error: {cmd} died of signal {signal}");
            }
        }
    }
    #[cfg(not(unix))]
    {
        child.kill().ok();
        child.wait().ok();
    }
}
