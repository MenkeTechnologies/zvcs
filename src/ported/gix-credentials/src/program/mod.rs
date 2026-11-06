use std::process::{Command, Stdio};

use bstr::{BString, ByteSlice, ByteVec};

use crate::{Program, helper};

/// The kind of helper program to use.
#[derive(Debug, Clone, Eq, PartialEq)]
pub enum Kind {
    /// The built-in `git credential` helper program, part of any `git` distribution.
    Builtin,
    /// A custom credentials helper, as identified just by the name with optional arguments
    ExternalName {
        /// The name like `foo` along with optional args, like `foo --arg --bar="a b"`, with arguments using `sh` shell quoting rules.
        /// The program executed will be `git-credential-foo [args]` if `name_and_args` starts with `foo [args]`.
        /// Note that a shell is only used if it's needed.
        name_and_args: BString,
    },
    /// A custom credentials helper, as identified just by the absolute path to the program and optional arguments. The program is executed through a shell.
    ExternalPath {
        /// The absolute path to the executable, like `/path/to/exe` along with optional args, like `/path/to/exe --arg --bar="a b"`, with arguments using `sh`
        /// shell quoting rules.
        path_and_args: BString,
    },
    /// A script to execute with `sh`.
    ExternalShellScript(BString),
}

/// Initialization
impl Program {
    /// Create a new program of the given `kind`.
    pub fn from_kind(kind: Kind) -> Self {
        Program {
            kind,
            child: None,
            stderr: true,
        }
    }

    /// Parse the given input as per the custom helper definition, supporting `!<script>`, `name` and `/absolute/name`, the latter two
    /// also support arguments which are ignored here.
    pub fn from_custom_definition(input: impl Into<BString>) -> Self {
        fn from_custom_definition_inner(mut input: BString) -> Program {
            let kind = if input.starts_with(b"!") {
                input.remove(0);
                Kind::ExternalShellScript(input)
            } else {
                let path = gix_path::from_bstr(
                    input
                        .find_byte(b' ')
                        .map_or(input.as_slice(), |pos| &input[..pos])
                        .as_bstr(),
                );
                if gix_path::is_absolute(path) {
                    Kind::ExternalPath { path_and_args: input }
                } else {
                    Kind::ExternalName { name_and_args: input }
                }
            };
            Program {
                kind,
                child: None,
                stderr: true,
            }
        }
        from_custom_definition_inner(input.into())
    }

    /// Convert the program into the respective command, suitable to invoke `action`.
    ///
    /// A helper given as an absolute path or as a `!` script is not spawned from this command: invoking it runs it
    /// the way `run_credential_helper()` does, as `sh -c "<helper> <operation>"`.
    pub fn to_command(&self, action: &helper::Action) -> std::process::Command {
        let git_program = gix_path::env::exe_invocation();
        let cmd = match &self.kind {
            Kind::Builtin => {
                let mut cmd = Command::from(gix_command::prepare(git_program));
                cmd.arg("credential").arg(action.as_arg(false));
                cmd
            }
            Kind::ExternalName { name_and_args } => {
                let mut args = name_and_args.clone();
                args.insert_str(0, "credential-");
                args.insert_str(0, " ");
                args.insert_str(0, git_program.to_string_lossy().as_ref());
                gix_command::prepare(gix_path::from_bstr(args.as_bstr()).into_owned())
                    .arg(action.as_arg(true))
                    .command_may_be_shell_script_allow_manual_argument_splitting()
                    .into()
            }
            Kind::ExternalShellScript(for_shell)
            | Kind::ExternalPath {
                path_and_args: for_shell,
            } => gix_command::prepare(gix_path::from_bstr(for_shell.as_bstr()).as_ref())
                .command_may_be_shell_script()
                .arg(action.as_arg(true))
                .into(),
        };
        self.with_stdio(cmd, action)
    }

    /// `cmd` with the standard streams a helper invoked for `action` is given.
    fn with_stdio(&self, mut cmd: Command, action: &helper::Action) -> Command {
        cmd.stdin(Stdio::piped())
            .stdout(if action.expects_output() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stderr(if self.stderr { Stdio::inherit() } else { Stdio::null() });
        cmd
    }
}

/// Builder
impl Program {
    /// By default `stderr` of programs is inherited and typically displayed in the terminal.
    pub fn suppress_stderr(mut self) -> Self {
        self.stderr = false;
        self
    }
}

impl Program {
    pub(crate) fn start(
        &mut self,
        action: &helper::Action,
    ) -> std::io::Result<(std::process::ChildStdin, Option<std::process::ChildStdout>)> {
        assert!(self.child.is_none(), "BUG: must not call `start()` twice");
        let mut cmd = match &self.kind {
            // `credential_do()` appends the operation to the helper's text and `run_credential_helper()` runs
            // the result with `use_shell` (credential.c:446-459, :487-500):
            //
            // ```c
            // strbuf_addf(&cmd, " %s", operation);
            // ```
            //
            // That space is one of `prepare_shell_cmd()`'s metacharacters and there are no further arguments,
            // so the helper always runs as `SHELL_PATH -c <cmd> <cmd>` (run-command.c:288-309), the text
            // standing in as `$0`. A helper that is missing or cannot be executed is the shell's to report,
            // `<cmd>: <path>: No such file or directory`, and its exit status fails only this helper.
            Kind::ExternalShellScript(_) | Kind::ExternalPath { .. } => {
                let script = gix_path::from_bstring(self.shell_command_text(action));
                let mut cmd = Command::new(gix_path::env::shell());
                cmd.arg("-c").arg(&script).arg(&script);
                self.with_stdio(cmd, action)
            }
            Kind::Builtin | Kind::ExternalName { .. } => self.to_command(action),
        };
        gix_trace::debug!(cmd = ?cmd, "launching credential helper");
        let mut child = cmd.spawn()?;
        let stdin = child.stdin.take().expect("stdin to be configured");
        let stdout = child.stdout.take();

        self.child = child.into();
        Ok((stdin, stdout))
    }

    /// The command `credential_do()` builds for `action` (credential.c:487-500): the helper's text without its
    /// `!`, an absolute path as it is, a name as `git credential-<name>`, each followed by the operation.
    pub(crate) fn shell_command_text(&self, action: &helper::Action) -> BString {
        let mut cmd = match &self.kind {
            Kind::Builtin => BString::from("git credential"),
            Kind::ExternalName { name_and_args } => {
                let mut cmd = BString::from("git credential-");
                cmd.push_str(name_and_args);
                cmd
            }
            Kind::ExternalShellScript(text) | Kind::ExternalPath { path_and_args: text } => text.clone(),
        };
        cmd.push(b' ');
        cmd.push_str(action.as_arg(!matches!(self.kind, Kind::Builtin)));
        cmd
    }

    pub(crate) fn finish(&mut self) -> std::io::Result<()> {
        let mut child = self.child.take().expect("Call `start()` before calling finish()");
        let status = child.wait()?;
        if status.success() {
            Ok(())
        } else {
            Err(std::io::Error::other(format!(
                "Credentials helper program failed with status code {:?}",
                status.code()
            )))
        }
    }
}

///
pub mod main;
pub use main::function::main;
