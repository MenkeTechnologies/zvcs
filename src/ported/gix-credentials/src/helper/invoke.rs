use std::io::Read;

use crate::helper::{Action, Context, Error, NextAction, Outcome, Result};

impl Action {
    /// Send ourselves to the given `write` which is expected to be credentials-helper compatible
    pub fn send(&self, write: &mut dyn std::io::Write) -> std::io::Result<()> {
        match self {
            Action::Get(ctx) => ctx.write_to(write),
            Action::Store(last) | Action::Erase(last) => {
                // `run_credential_helper()` (credential.c:462-466) hands the helper
                // exactly what `credential_write()` produced — one `key=value\n` per
                // field — and then `fclose(fp)`, so the helper reads EOF rather than a
                // blank line. A helper that logs its stdin verbatim shows the
                // difference as a trailing empty line. The newline is still added when
                // the payload does not end in one, because gix's `store`/`erase`
                // payload is a previous helper's stdout and may stop mid-line.
                write.write_all(last).ok();
                if !last.is_empty() && !last.ends_with(b"\n") {
                    write.write_all(b"\n").ok();
                }
                Ok(())
            }
        }
    }
}

/// Invoke the given `helper` with `action` in `context`.
///
/// Usually the first call is performed with [`Action::Get`] to obtain `Some` identity, which subsequently can be used if it is complete.
/// Note that it may also only contain the username _or_ password, and should start out with everything the helper needs.
/// On successful usage, use [`NextAction::store()`], otherwise [`NextAction::erase()`], which is when this function
/// returns `Ok(None)` as no outcome is expected.
pub fn invoke(helper: &mut crate::Program, action: &Action) -> Result {
    let options = action.context().map(|ctx| ctx.options).unwrap_or_default();
    match raw(helper, action)? {
        None => Ok(None),
        Some(stdout) => {
            let ctx = Context::from_bytes(stdout.as_slice(), options)?;
            Ok(Some(Outcome {
                username: ctx.username,
                password: ctx.password,
                oauth_refresh_token: ctx.oauth_refresh_token,
                quit: ctx.quit.unwrap_or(false),
                next: NextAction {
                    previous_output: stdout.into(),
                    options,
                },
            }))
        }
    }
}

pub(crate) fn raw(helper: &mut crate::Program, action: &Action) -> std::result::Result<Option<Vec<u8>>, Error> {
    // `if (start_command(&helper) < 0) return -1;` (credential.c:459-460): a helper that cannot be started fails
    // on its own, and `credential_fill()` goes on to the next one (credential.c:519-520). A shell that will not
    // execute is `child_err_spew()`'s report, through the die-message routine (run-command.c:378-406).
    let (mut stdin, stdout) = match helper.start(action) {
        Ok(streams) => streams,
        Err(err) => {
            let cmd = helper.shell_command_text(action);
            let text = err.to_string();
            let text = match err.raw_os_error() {
                Some(code) => text
                    .strip_suffix(&format!(" (os error {code})"))
                    .unwrap_or(&text)
                    .to_owned(),
                None => text,
            };
            eprintln!("fatal: cannot exec '{cmd}': {text}");
            return Err(Error::CredentialsHelperFailed { source: err });
        }
    };
    if let (Action::Get(_), None) = (&action, &stdout) {
        panic!("BUG: `Helper` impls must return an output handle to read output from if Action::Get is provided")
    }
    // `run_credential_helper()` writes the request with `SIGPIPE` ignored and checks neither the writes nor the
    // `fclose()` (credential.c:462-466), so a helper that exits without reading its input, `!true` say, is read
    // from like any other rather than failing the fill on `EPIPE`:
    //
    // ```c
    // fp = xfdopen(helper.in, "w");
    // sigchain_push(SIGPIPE, SIG_IGN);
    // credential_write(c, fp, want_output ? CREDENTIAL_OP_HELPER : CREDENTIAL_OP_RESPONSE);
    // fclose(fp);
    // sigchain_pop(SIGPIPE);
    // ```
    action.send(&mut stdin).ok();
    drop(stdin);
    let stdout = stdout
        .map(|mut stdout| {
            let mut buf = Vec::new();
            stdout.read_to_end(&mut buf).map(|_| buf)
        })
        .transpose()
        .map_err(|err| Error::CredentialsHelperFailed { source: err })?;
    helper.finish().map_err(|err| {
        if err.kind() == std::io::ErrorKind::Other {
            Error::CredentialsHelperFailed { source: err }
        } else {
            err.into()
        }
    })?;

    match matches!(action, Action::Get(_)).then(|| stdout).flatten() {
        None => Ok(None),
        Some(stdout) => Ok(Some(stdout)),
    }
}
