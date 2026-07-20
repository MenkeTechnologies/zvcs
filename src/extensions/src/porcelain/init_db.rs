use anyhow::Result;
use std::process::ExitCode;

/// `git init-db` — a synonym for `git init`.
///
/// `git-init-db(1)` states this outright: "This is a synonym for git-init(1).
/// Please refer to the documentation of that command." Stock git resolves both
/// names to the same `cmd_init_db` entry in `builtin.c`, which is why even the
/// usage text printed on a bad flag reads `usage: git init ...` rather than
/// naming `init-db`. There is no behavioral difference — same flags, same
/// output, same exit codes — so this port delegates verbatim to
/// [`super::init::init`] instead of duplicating the repository creation.
///
/// Consequences of that delegation, stated plainly so this doc claims nothing
/// the code does not do:
///   * Every flag `init` ports (`--bare`, `-q`/`--quiet`, `-b`/
///     `--initial-branch[=<name>]`, `--`, one optional `<directory>`) is ported
///     here, byte-for-byte identical on stdout and in post-command repo state.
///   * Every flag `init` refuses is refused here with `init`'s own message.
///     That covers the remaining flags listed in the `init-db` synopsis —
///     `--template=<dir>`, `--separate-git-dir <dir>`, `--shared[=<perms>]` —
///     plus `--object-format` and `--ref-format`: they `bail!` rather than
///     being silently ignored, so no run ever produces a repo that differs from
///     what the flag asked for.
///   * The divergences documented on [`super::init::init`] apply here unchanged:
///     reinitialization does not re-copy missing template hooks or
///     `info/exclude`, and `--bare` into a non-empty directory surfaces gix's
///     `DirectoryNotEmpty` where stock git permits it.
///
/// Two further inherited divergences, both on stderr and both belonging to
/// `init` rather than to this synonym:
///   * Stock git prints the `advice.defaultBranchName` hint block when
///     `init.defaultBranch` is unset; this port prints no hint.
///   * A rejected flag exits with the anyhow error path rather than git's
///     usage dump and exit code 129.
///
/// Unlike [`super::fsck_objects::fsck_objects`], no leading-subcommand strip is
/// performed: `dispatch::run` takes the subcommand as a separate
/// parameter and passes only the argument tail, so stripping a leading
/// `"init-db"` would silently eat the directory operand of
/// `git init-db init-db`.
pub fn init_db(args: &[String]) -> Result<ExitCode> {
    super::init::init(args)
}
