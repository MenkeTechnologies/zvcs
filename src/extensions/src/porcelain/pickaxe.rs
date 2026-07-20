use anyhow::Result;
use std::process::ExitCode;

/// `git pickaxe` — the hidden legacy name for `git blame`.
///
/// `pickaxe` was the original name of the blame implementation
/// (`builtin/pickaxe.c`, renamed to `builtin/blame.c` in 2006). The name is
/// still wired to the same `cmd_blame` entry in git's command table, but it is
/// registered without `CMD_HIDDEN` removal — so it is absent from
/// `git help -a` while still present in `git --list-cmds=builtins`, and still
/// dispatchable. Verified on the installed git rather than from memory:
///
/// ```text
/// $ git version
/// git version 2.55.0
/// $ git --list-cmds=builtins | grep -x pickaxe
/// pickaxe
/// $ git help -a | tr ' ' '\n' | grep -x pickaxe    # (no output)
/// ```
///
/// Equivalence to `blame` was checked empirically, not assumed:
///   * `git pickaxe <file>` and `git blame <file>` produce identical stdout.
///   * `git pickaxe` with no operand prints git's usage block and exits 129,
///     byte-identical to `git blame` with no operand.
///   * Neither the usage block nor any error message contains the string
///     "pickaxe" — every diagnostic says `git blame`, because the shared
///     `cmd_blame` hardcodes its own usage strings.
///   * `git pickaxe --zzz <file>` prints ``error: unknown option `--zzz'``
///     followed by the same `usage: git blame ...` block.
///
/// It is *not* a synonym for `git annotate`: `annotate` forces
/// `OUTPUT_ANNOTATE_COMPAT`, and its output differs from `pickaxe`'s (confirmed
/// by diffing all three against each other on the same file).
///
/// Because there is no behavioral difference at all, this port delegates
/// verbatim to [`super::blame::blame`] rather than duplicating the blame
/// machinery. Stated plainly so this doc claims nothing the code does not do:
///   * Every flag `blame` ports is ported here, byte-for-byte identical.
///   * Every flag `blame` refuses is refused here, with `blame`'s own message.
///   * Every divergence documented on [`super::blame::blame`] applies here
///     unchanged — notably that unsupported flags (`-p`/`--porcelain`,
///     `--incremental`, `-M`/`-C`, `--reverse`, `-w`, regex/`:funcname` `-L`
///     forms, …) exit via the anyhow error path instead of git's usage dump
///     with exit code 129, and that a missing path operand bails
///     `no path given` rather than printing the usage block.
///
/// No leading-subcommand strip is performed, matching
/// [`super::init_db::init_db`]'s reasoning: `dispatch::run` passes only the
/// argument tail, so stripping a leading `"pickaxe"` would silently eat the
/// file operand of `git pickaxe pickaxe`.
pub fn pickaxe(args: &[String]) -> Result<ExitCode> {
    super::blame::blame(args)
}
