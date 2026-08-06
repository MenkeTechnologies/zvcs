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
///     `--initial-branch[=<name>]`, `--template=<dir>`,
///     `--separate-git-dir <dir>`, `--shared[=<perms>]`,
///     `--object-format=<hash>`, `--ref-format=<format>`, each flag's
///     auto-generated `--no-` negation, `--`, one optional `<directory>`) is
///     ported here, byte-for-byte identical on stdout and in post-command repo
///     state.
///   * The defaults `--object-format=sha1` and `--ref-format=files` are honored
///     as no-ops; `--object-format=sha256` and `--ref-format=reftable` are
///     rejected with an honest "not supported" error (unverified sha256 write
///     path / no vendored reftable backend) rather than being silently ignored,
///     so no run ever produces a repo that differs from what the flag asked for.
///     An otherwise unrecognized value reproduces git's exact error text.
///   * Every configuration source `init` consults is consulted here too:
///     `init.defaultBranch`, `init.templateDir`, `init.defaultObjectFormat`,
///     `init.defaultRefFormat` (each behind its `GIT_DEFAULT_HASH` /
///     `GIT_DEFAULT_REF_FORMAT` / `GIT_TEMPLATE_DIR` environment override) and
///     `init.defaultSubmodulePathConfig`.
///   * The divergences documented on [`super::init::init`] apply here unchanged:
///     reinitialization does not re-copy missing template hooks or
///     `info/exclude`, and a configured `sha256` / `reftable` format is rejected
///     rather than silently laid down in the other format.
///
/// One inherited divergence, on stderr and belonging to `init` rather than to this
/// synonym: stock git prints the `advice.defaultBranchName` hint block when
/// `init.defaultBranch` is unset; this port prints no hint. The refusals themselves are
/// git's — `-h` and an unknown option print the usage block and exit 129, an unknown
/// hash or ref format is `fatal:` and exit 128.
///
/// Unlike [`super::fsck_objects::fsck_objects`], no leading-subcommand strip is
/// performed: `dispatch::run` takes the subcommand as a separate
/// parameter and passes only the argument tail, so stripping a leading
/// `"init-db"` would silently eat the directory operand of
/// `git init-db init-db`.
pub fn init_db(args: &[String]) -> Result<ExitCode> {
    super::init::init(args)
}
