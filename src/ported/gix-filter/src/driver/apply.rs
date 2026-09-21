use std::{collections::HashMap, io::Read, sync::Arc};

use bstr::{BStr, BString, ByteSlice};

use crate::{
    Driver, driver,
    driver::{Operation, Process, State, process, process::client::invoke},
};

/// What to do if delay is supported by a process filter.
#[derive(Default, Debug, Copy, Clone)]
pub enum Delay {
    /// Use delayed processing for this entry.
    ///
    /// Note that it's up to the filter to determine whether or not the processing should be delayed.
    #[default]
    Allow,
    /// Do not delay the processing, and force it to happen immediately. In this case, no delayed processing will occur
    /// even if the filter supports it.
    ///
    /// This is the default as it requires no special precautions to be taken by the caller as
    /// outputs will be produced immediately.
    Forbid,
}

/// The error returned by [State::apply()][super::State::apply()].
#[derive(Debug, thiserror::Error)]
#[expect(missing_docs)]
pub enum Error {
    #[error(transparent)]
    Init(#[from] driver::init::Error),
    #[error("Could not write entire object to driver")]
    WriteSource(#[from] std::io::Error),
    #[error("Filter process delayed an entry even though that was not requested")]
    DelayNotAllowed,
    #[error("Failed to invoke '{command}' command")]
    ProcessInvoke {
        source: process::client::invoke::Error,
        command: String,
    },
    #[error("The invoked command '{command}' in process indicated an error: {status:?}")]
    ProcessStatus {
        status: driver::process::Status,
        command: String,
    },
}

/// Additional information for use in the [`State::apply()`] method.
#[derive(Debug, Copy, Clone)]
pub struct Context<'a, 'b> {
    /// The repo-relative using slashes as separator of the entry currently being processed.
    pub rela_path: &'a BStr,
    /// The name of the reference that `HEAD` is pointing to. It's passed to `process` filters if present.
    pub ref_name: Option<&'b BStr>,
    /// The root-level tree that contains the current entry directly or indirectly, or the commit owning the tree (if available).
    ///
    /// This is passed to `process` filters if present.
    pub treeish: Option<gix_hash::ObjectId>,
    /// The actual blob-hash of the data we are processing. It's passed to `process` filters if present.
    ///
    /// Note that this hash might be different from the `$Id$` of the respective `ident` filter, as the latter generates the hash itself.
    pub blob: Option<gix_hash::ObjectId>,
}

/// Apply operations to filter programs.
impl State {
    /// Apply `operation` of `driver` to the bytes read from `src` and return a reader to immediately consume the output
    /// produced by the filter. `rela_path` is the repo-relative path of the entry to handle.
    /// It's possible that the filter stays inactive, in which case the `src` isn't consumed and has to be used by the caller.
    ///
    /// Each call to this method will cause the corresponding filter to be invoked unless `driver` indicates a `process` filter,
    /// which is only launched once and maintained using this state.
    ///
    /// Note that it's not an error if there is no filter process for `operation` or if a long-running process doesn't support
    /// the desired capability.
    ///
    /// ### Deviation
    ///
    /// If a long-running process returns the 'abort' status after receiving the data, it will be removed similarly to how `git` does it.
    /// However, if it returns an unsuccessful error status later, it will not be removed, but reports the error only.
    /// If any other non-'error' status is received, the process will be stopped. But that doesn't happen if such a status is received
    /// after reading the filtered result.
    pub fn apply<'a>(
        &'a mut self,
        driver: &Driver,
        src: &mut impl std::io::Read,
        operation: Operation,
        ctx: Context<'_, '_>,
    ) -> Result<Option<Box<dyn std::io::Read + 'a>>, Error> {
        match self.apply_delayed(driver, src, operation, Delay::Forbid, ctx)? {
            Some(MaybeDelayed::Delayed(_)) => {
                unreachable!("we forbid delaying the entry")
            }
            Some(MaybeDelayed::Immediate(read)) => Ok(Some(read)),
            None => Ok(None),
        }
    }

    /// Like [`apply()`](Self::apply()), but use `delay` to determine if the filter result may be delayed or not.
    ///
    /// Poll [`list_delayed_paths()`](Self::list_delayed_paths()) until it is empty and query the available paths again.
    /// Note that even though it's possible, the API assumes that commands aren't mixed when delays are allowed.
    pub fn apply_delayed<'a>(
        &'a mut self,
        driver: &Driver,
        src: &mut impl std::io::Read,
        operation: Operation,
        delay: Delay,
        ctx: Context<'_, '_>,
    ) -> Result<Option<MaybeDelayed<'a>>, Error> {
        match self.maybe_launch_process(driver, operation, ctx.rela_path)? {
            Some(Process::SingleFile { mut child, command }) => {
                // To avoid deadlock when the filter immediately echoes input to output (like `cat`),
                // we need to write to stdin and read from stdout concurrently. If we write all data
                // to stdin before reading from stdout, and the pipe buffer fills up, both processes
                // will block: the filter blocks writing to stdout (buffer full), and we block writing
                // to stdin (waiting for the filter to consume data).
                //
                // Solution: Read all data into a buffer, then spawn a thread to write it to stdin
                // while we can immediately read from stdout.
                //
                // TODO(perf): This keeps the entire input in memory until the writer is done. The
                // input is retained as a fallback while the entire output is buffered, making peak
                // storage approximately input plus output (the `Arc` clone does not copy the input).
                // git pays the same price — `apply_single_file_filter()` collects the whole result
                // into a `strbuf` before it can tell whether the filter succeeded. Git's
                // `apply_single_file_filter()` can instead have its async worker copy directly from
                // an input file descriptor while the main thread reads output
                // (`convert.c::filter_buffer_or_fd()`), avoiding an input-sized allocation in that
                // case. Find a way to similarly pump the borrowed reader concurrently.
                let mut input_data = Vec::new();
                std::io::copy(src, &mut input_data)?;

                let stdin = child.stdin.take().expect("configured");
                let input_data: Arc<[u8]> = input_data.into();
                let fallback = Arc::clone(&input_data);
                let write_thread = WriterThread::write_all_in_background(input_data, stdin)?;

                Ok(Some(MaybeDelayed::Immediate(Box::new(ReadFilterOutput {
                    inner: child.stdout.take(),
                    child: Some((child, command)),
                    write_thread: Some(write_thread),
                    fallback: Some(fallback),
                    // `error()` in `filter_buffer_or_fd()`/`apply_single_file_filter()` names the
                    // command as configured, before `%f` substitution (convert.c:694, 729-735, where
                    // `params->cmd` is `drv->clean`/`drv->smudge` itself).
                    cmd: match operation {
                        Operation::Clean => driver.clean.clone(),
                        Operation::Smudge => driver.smudge.clone(),
                    }
                    .unwrap_or_default(),
                    required: driver.required,
                    buffered: None,
                }))))
            }
            Some(Process::MultiFile { client, key }) => {
                let command = operation.as_str();
                if !client.capabilities().contains(command) {
                    return Ok(None);
                }

                let invoke_result = client.invoke(
                    command,
                    &mut [
                        ("pathname", Some(ctx.rela_path.to_owned())),
                        ("ref", ctx.ref_name.map(ToOwned::to_owned)),
                        ("treeish", ctx.treeish.map(|id| id.to_hex().to_string().into())),
                        ("blob", ctx.blob.map(|id| id.to_hex().to_string().into())),
                        (
                            "can-delay",
                            match delay {
                                Delay::Allow if client.capabilities().contains("delay") => Some("1".into()),
                                Delay::Forbid | Delay::Allow => None,
                            },
                        ),
                    ]
                    .into_iter()
                    .filter_map(|(key, value)| value.map(|v| (key, v))),
                    src,
                );
                let status = match invoke_result {
                    Ok(status) => status,
                    Err(err) => {
                        let invoke::Error::Io(io_err) = &err;
                        handle_io_err(io_err, &mut self.running, key.0.as_ref());
                        return Err(Error::ProcessInvoke {
                            command: command.into(),
                            source: err,
                        });
                    }
                };

                if status.is_delayed() {
                    if matches!(delay, Delay::Forbid) {
                        return Err(Error::DelayNotAllowed);
                    }
                    Ok(Some(MaybeDelayed::Delayed(key)))
                } else if status.is_success() {
                    // TODO: find a way to not have to do the 'borrow-dance'.
                    let client = self.running.remove(&key.0).expect("present for borrowcheck dance");
                    self.running.insert(key.0.clone(), client);
                    let client = self.running.get_mut(&key.0).expect("just inserted");

                    Ok(Some(MaybeDelayed::Immediate(Box::new(client.as_read()))))
                } else {
                    let message = status.message().unwrap_or_default();
                    match message {
                        "abort" => {
                            client.capabilities_mut().remove(command);
                        }
                        "error" => {}
                        _strange => {
                            // `handle_filter_error()` reports only this branch — a broken protocol —
                            // on stderr; `error` and `abort` are the filter's own, documented answers
                            // and stay silent (convert.c:776-793).
                            eprintln!(
                                "error: external filter '{}' failed",
                                driver.process.as_ref().map_or("".into(), |cmd| cmd.as_bstr())
                            );
                            let client = self.running.remove(&key.0).expect("we definitely have it");
                            client.into_child().kill().ok();
                        }
                    }
                    Err(Error::ProcessStatus {
                        command: command.into(),
                        status,
                    })
                }
            }
            None => Ok(None),
        }
    }
}

/// A type to represent delayed or immediate apply-filter results.
pub enum MaybeDelayed<'a> {
    /// Using the delayed protocol, this entry has been sent to a long-running process and needs to be
    /// checked for again, later, using the [`driver::Key`] to refer to the filter who owes a response.
    ///
    /// Note that the path to the entry is also needed to obtain the filtered result later.
    Delayed(driver::Key),
    /// The filtered result can be read from the contained reader right away.
    ///
    /// Note that it must be consumed in full or till a read error occurs.
    Immediate(Box<dyn std::io::Read + 'a>),
}

/// A helper to manage writing to stdin on a separate thread to avoid deadlock.
struct WriterThread {
    handle: Option<std::thread::JoinHandle<std::io::Result<()>>>,
}

impl WriterThread {
    /// Spawn a thread that will write all data from `data` to `stdin`.
    fn write_all_in_background(data: Arc<[u8]>, mut stdin: std::process::ChildStdin) -> std::io::Result<Self> {
        let handle = std::thread::Builder::new()
            .name("gix-filter-stdin-writer".into())
            .stack_size(128 * 1024)
            .spawn(move || {
                use std::io::Write;
                stdin.write_all(&data)?;
                // Explicitly drop stdin to close the pipe and signal EOF to the child
                drop(stdin);
                Ok(())
            })?;

        Ok(Self { handle: Some(handle) })
    }

    /// Wait for the writer thread to complete and return any error that occurred.
    fn join(&mut self) -> std::io::Result<()> {
        let Some(handle) = self.handle.take() else {
            return Ok(());
        };
        handle.join().map_err(|panic_info| {
            let msg = if let Some(s) = panic_info.downcast_ref::<String>() {
                format!("Writer thread panicked: {s}")
            } else if let Some(s) = panic_info.downcast_ref::<&str>() {
                format!("Writer thread panicked: {s}")
            } else {
                "Writer thread panicked while writing to filter stdin".to_string()
            };
            std::io::Error::other(msg)
        })?
    }
}

impl Drop for WriterThread {
    fn drop(&mut self) {
        // Best effort join on drop.
        if let Err(_err) = self.join() {
            gix_trace::debug!(err = %_err, "Failed to join writer thread during drop");
        }
    }
}

/// A utility type to facilitate streaming the output of a filter process.
struct ReadFilterOutput {
    inner: Option<std::process::ChildStdout>,
    /// Present until the process is waited on, then taken so subsequent reads don't wait again.
    child: Option<(std::process::Child, std::process::Command)>,
    /// The thread writing to stdin, if any. Must be joined when reading is done.
    write_thread: Option<WriterThread>,
    /// Original input to return if a non-required driver fails. Taken once the run is finished.
    fallback: Option<Arc<[u8]>>,
    /// The driver command as configured, for the `error()` messages git prints (convert.c:694, 735).
    cmd: BString,
    /// Whether a failure is fatal to the caller rather than a silent fallback to `fallback`.
    required: bool,
    /// Fully buffered output of the driver, or its original input after failure.
    buffered: Option<std::io::Cursor<BufferedOutput>>,
}

enum BufferedOutput {
    Filtered(Vec<u8>),
    Original(Arc<[u8]>),
}

impl AsRef<[u8]> for BufferedOutput {
    fn as_ref(&self) -> &[u8] {
        match self {
            BufferedOutput::Filtered(data) => data,
            BufferedOutput::Original(data) => data,
        }
    }
}

impl ReadFilterOutput {
    /// Buffer all output to verify that the driver succeeded before exposing it, exactly as
    /// `apply_single_file_filter()` collects the whole result with `strbuf_read()` and only then
    /// decides whether to hand it to the caller (convert.c:728-742).
    ///
    /// On failure the original input takes the place of the output, which is what git's caller sees:
    /// a filter that returns 0 leaves `dst` untouched and `convert_to_git()` keeps using `src`
    /// (convert.c:1440). A `required` driver has no such fallback — the failure is returned so the
    /// caller can `die()` with the path and driver name (convert.c:1441).
    fn buffer_driver_output(&mut self, fallback: Arc<[u8]>) -> std::io::Result<&mut std::io::Cursor<BufferedOutput>> {
        let mut output = Vec::new();
        let read_result = self.inner.take().expect("configured").read_to_end(&mut output);
        let write_result = self.write_thread.take().map_or(Ok(()), |mut thread| thread.join());
        let (mut child, _command) = self.child.take().expect("configured");
        let status = child.wait();
        // A filter may deliberately close stdin before consuming all input, which makes the writer
        // see a broken pipe even though the filter produced valid output and exited successfully.
        // Other write failures invalidate the filtered output; read and exit status are checked below.
        let write_succeeded = match &write_result {
            Ok(()) => true,
            Err(err) => err.kind() == std::io::ErrorKind::BrokenPipe,
        };
        let exited_well = status.as_ref().is_ok_and(std::process::ExitStatus::success);
        let succeeded = read_result.is_ok() && write_succeeded && exited_well;

        if !succeeded {
            gix_trace::debug!(
                ?_command,
                ?read_result,
                ?write_result,
                ?status,
                "Filter driver failed; using original input"
            );
            let cmd = &self.cmd;
            // The order and the wording are git's: the async half reports the child first
            // (convert.c:687, 694), the parent half second (convert.c:729-735).
            if !write_succeeded {
                eprintln!("error: cannot feed the input to external filter '{cmd}'");
            }
            if let Ok(status) = &status {
                if !status.success() {
                    eprintln!(
                        "error: external filter '{cmd}' failed {}",
                        status.code().unwrap_or(-1)
                    );
                }
            }
            if read_result.is_err() {
                eprintln!("error: read from external filter '{cmd}' failed");
            }
            eprintln!("error: external filter '{cmd}' failed");
        }
        self.buffered = Some(std::io::Cursor::new(if succeeded {
            BufferedOutput::Filtered(output)
        } else {
            BufferedOutput::Original(fallback)
        }));
        if !succeeded && self.required {
            return Err(std::io::Error::other(format!(
                "external filter '{}' failed",
                self.cmd
            )));
        }
        Ok(self.buffered.as_mut().expect("just initialized"))
    }
}

pub(crate) fn handle_io_err(err: &std::io::Error, running: &mut HashMap<BString, process::Client>, process: &BStr) {
    if matches!(
        err.kind(),
        std::io::ErrorKind::BrokenPipe | std::io::ErrorKind::UnexpectedEof
    ) {
        running.remove(process).expect("present or we wouldn't be here");
    }
}

impl std::io::Read for ReadFilterOutput {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if let Some(buffered) = self.buffered.as_mut() {
            return buffered.read(buf);
        }
        let fallback = self.fallback.take().expect("set when the child was spawned");
        self.buffer_driver_output(fallback)?.read(buf)
    }
}
