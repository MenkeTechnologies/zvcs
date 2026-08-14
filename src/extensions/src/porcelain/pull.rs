//! `git pull` — fetch the configured (or named) remote, then integrate the
//! fetched upstream into the current branch.
//!
//! `pull` is `fetch` followed by an integration step. The fetch is delegated to
//! the ported [`fetch`](super::fetch), so its option surface is available to
//! `pull` verbatim: `--all`, `-f`/`--force`, `-t`/`--tags`, `-p`/`--prune`,
//! `--depth`/`--deepen`/`--unshallow`, `--shallow-since`/`--shallow-exclude`,
//! `-q`/`-v`, `--progress`/`--no-progress`, `--dry-run`, `-a`/`--append`,
//! `--show-forced-updates`/`--no-show-forced-updates`,
//! `-k`/`--keep`, `--recurse-submodules`, `-j`/`--jobs`, `--upload-pack`,
//! `-o`/`--server-option`, `--refmap`, `-4`/`--ipv4`, `-6`/`--ipv6`,
//! `--negotiation-restrict`/`--negotiation-tip`/`--negotiation-include`, and the
//! `From …` per-ref summary git prints to stderr.
//!
//! The integration step is the ported [`merge`](super::merge) by default, or the
//! ported [`rebase`](super::rebase) when a rebase is requested. The rebase is
//! selected the way git's `config_get_rebase()` selects it: a CLI
//! `--rebase[=<value>]`/`-r`/`--no-rebase` wins, else `branch.<name>.rebase`,
//! else `pull.rebase`; `<value>` is `true`/`false`/`merges`/`interactive`
//! (`preserve` is refused as git refuses it). A rebase forwards its compatible
//! knobs to `rebase` — `-s`/`--strategy`, `-X`/`--strategy-option`, `--signoff`,
//! `--autostash` (and `pull.autoStash`/`rebase.autoStash`), `--rebase-merges` (from
//! `--rebase=merges`), `--stat`/`--no-stat`/`-n` — and rebases the current branch
//! onto the fetched upstream. A rebase whose upstream is already a descendant of
//! `HEAD` would replay nothing, so — as git's `can_ff`/`ran_ff` do — the merge runs
//! with a forced `--ff-only` instead and the rebase never starts.
//!
//! Supported invocation forms:
//!   * `git pull`                  — use the current branch's configured upstream.
//!   * `git pull <remote>`         — fetch `<remote>`, merge the configured upstream branch.
//!   * `git pull <remote> <ref>…`  — fetch `<remote>`, integrate what the fetch
//!     marked for-merge in `FETCH_HEAD` (`get_merge_heads()`). A `<ref>` that
//!     lands nowhere under `refs/remotes/` — a tag, a `refs/pull/…` head,
//!     anything outside the remote's refspec — integrates just the same, and
//!     several of them become an octopus.
//!
//! Every `--no-` spelling of a forwarded fetch option (`--no-tags`,
//! `--no-prune`, `--no-force`, `--no-all`, `--no-keep`, …) is accepted and
//! handed to the fetch: git declares them `OPT_PASSTHRU`/`OPT_BOOL` and
//! `recreate_opt()` passes on whichever spelling it saw.
//!
//! The integration step is handed a literal `FETCH_HEAD`, as `run_merge()` does,
//! so the merge re-reads the file and sees every for-merge head: one is an
//! ordinary merge, several are an octopus, and each is named the way the fetch
//! described it (`Merge branch 'main' of <url>`).
//!
//! Gates that sit outside the integration step, all from `cmd_pull()`:
//!   * An unmerged index or a `MERGE_HEAD` left over from an unfinished merge
//!     stops the pull before the fetch (`die_resolve_conflict()` /
//!     `die_conclude_merge()`), exit 128.
//!   * Diverged branches with neither an `--ff…` flag nor `pull.ff`, and no
//!     rebase policy from the command line or config, are refused with the
//!     `advice.diverging` block and `fatal: Need to specify how to reconcile
//!     divergent branches.` — git does not pick an integration strategy on its
//!     own, and neither does this.
//!   * Several merge heads cannot be rebased onto or fast-forwarded to
//!     (`Cannot rebase onto multiple branches.` / `Cannot fast-forward to
//!     multiple branches.`), and cannot start a branch (`Cannot merge multiple
//!     branches into empty head.`).
//!   * When rebasing without `--autostash`, `require_clean_work_tree()` runs
//!     *before* the fetch, so a dirty tree ends the pull with
//!     `error: cannot pull with rebase: You have unstaged changes.` and exit
//!     128 — no network traffic, no tracking refs moved.
//!   * An unborn `HEAD` is `pull_into_void()`: the fetched head is checked out
//!     over the empty tree and becomes the branch's first commit, with an
//!     `initial pull` reflog entry. This is the
//!     `git init && git remote add && git pull` path.
//!
//! Fast-forward policy (merge path only): `pull.ff` (`true`/`false`/`only`) is
//! the default, overriding `merge.ff` for `pull` as git's `config_get_ff()`
//! does; a CLI `--ff`/`--no-ff`/`--ff-only` overrides both, and when neither is
//! set the decision falls through to `merge.ff` inside [`merge`](super::merge).
//!
//! `run_merge()`/`run_rebase()` also forward the accumulated `-q`/`-v`
//! verbosity (git's `argv_push_verbosity()`), so `pull --quiet` silences the
//! integration step's `Updating …`/`Fast-forward`/diffstat block and not just
//! the fetch. `--squash`/`--no-squash` and `--allow-unrelated-histories` are
//! `run_merge()` passthroughs the merge port implements, so they are forwarded
//! on the merge path and — as git does — rejected on the rebase path.
//!
//! `--[no-]commit`, `--[no-]edit`, `--[no-]log[=<n>]` and `--cleanup=<mode>` are
//! `run_merge()` passthroughs too, forwarded as given; the merge port implements
//! all four. git pushes none of them onto the rebase command line and does not
//! reject them there either, so neither does this.
//!
//! What is refused rather than faked, because the underlying substrate is
//! absent: `-s`/`-X`/`--signoff` on the *merge* path (they are honored on the
//! *rebase* path), `--rebase=interactive` (interactive todo editing needs a TTY
//! editor loop), `--set-upstream`, and `--gpg-sign`/`-S`/`--verify-signatures`
//! (GPG is not vendored).
//!
//! Option *errors* are `parse-options`': an unknown option prints
//! `error: unknown option \`<name>'` and the whole usage block on stderr and
//! exits 129, and a value-taking option with nothing after it prints
//! `error: option \`<name>' requires a value` alone, also 129. Only `-h` writes
//! the usage to stdout.
//!
//! `--autostash` is forwarded to whichever integration step runs — `run_merge()`
//! and `run_rebase()` both push it — so a dirty tree is stashed and restored on
//! either path.
//!
//! The diffstat selectors `-n`, `--stat`/`--no-stat`, `--summary`/`--no-summary`
//! and `--compact-summary`/`--no-compact-summary` are git's `OPT_PASSTHRU`
//! `opt_diffstat` — a single slot, so the last one wins, and the flag git
//! reconstructed from it is handed verbatim to the integration step
//! (`run_merge()`/`run_rebase()` both push `opt_diffstat`). `git rebase` has no
//! `--summary` or `--compact-summary`, so those combined with `--rebase` fail in
//! the rebase step exactly as they do for git.

use anyhow::{bail, Result};
use std::process::ExitCode;

use gix::remote::Direction;

use super::{Arg, LongOpt};

/// `cmd_pull()`'s `struct option pull_options[]` (builtin/pull.c), in table order,
/// as [`super::resolve_long_aliased`] reads it.
///
/// `--ff-only`, `--unshallow` and `--refmap` carry `PARSE_OPT_NONEG` and so have no
/// `--no-` spelling; `-4`/`-6` are `OPT_PASSTHRU`s with only `PARSE_OPT_NOARG`, so
/// they do.
const LONG_OPTS: &[LongOpt] = &[
    LongOpt { name: "verbose",                     neg: true,  arg: Arg::None },
    LongOpt { name: "quiet",                       neg: true,  arg: Arg::None },
    LongOpt { name: "progress",                    neg: true,  arg: Arg::None },
    LongOpt { name: "recurse-submodules",          neg: true,  arg: Arg::Optional },
    LongOpt { name: "rebase",                      neg: true,  arg: Arg::Optional },
    LongOpt { name: "stat",                        neg: true,  arg: Arg::None },
    LongOpt { name: "summary",                     neg: true,  arg: Arg::None },
    LongOpt { name: "compact-summary",             neg: true,  arg: Arg::None },
    LongOpt { name: "log",                         neg: true,  arg: Arg::Optional },
    LongOpt { name: "signoff",                     neg: true,  arg: Arg::Optional },
    LongOpt { name: "squash",                      neg: true,  arg: Arg::None },
    LongOpt { name: "commit",                      neg: true,  arg: Arg::None },
    LongOpt { name: "edit",                        neg: true,  arg: Arg::None },
    LongOpt { name: "cleanup",                     neg: true,  arg: Arg::Required },
    LongOpt { name: "ff",                          neg: true,  arg: Arg::None },
    LongOpt { name: "ff-only",                     neg: false, arg: Arg::None },
    LongOpt { name: "verify",                      neg: true,  arg: Arg::None },
    LongOpt { name: "verify-signatures",           neg: true,  arg: Arg::None },
    LongOpt { name: "autostash",                   neg: true,  arg: Arg::None },
    LongOpt { name: "strategy",                    neg: true,  arg: Arg::Required },
    LongOpt { name: "strategy-option",             neg: true,  arg: Arg::Required },
    LongOpt { name: "gpg-sign",                    neg: true,  arg: Arg::Optional },
    LongOpt { name: "allow-unrelated-histories",   neg: true,  arg: Arg::None },
    LongOpt { name: "all",                         neg: true,  arg: Arg::None },
    LongOpt { name: "append",                      neg: true,  arg: Arg::None },
    LongOpt { name: "upload-pack",                 neg: true,  arg: Arg::Required },
    LongOpt { name: "force",                       neg: true,  arg: Arg::None },
    LongOpt { name: "tags",                        neg: true,  arg: Arg::None },
    LongOpt { name: "prune",                       neg: true,  arg: Arg::None },
    LongOpt { name: "jobs",                        neg: true,  arg: Arg::Optional },
    LongOpt { name: "dry-run",                     neg: true,  arg: Arg::None },
    LongOpt { name: "keep",                        neg: true,  arg: Arg::None },
    LongOpt { name: "depth",                       neg: true,  arg: Arg::Required },
    LongOpt { name: "shallow-since",               neg: true,  arg: Arg::Required },
    LongOpt { name: "shallow-exclude",             neg: true,  arg: Arg::Required },
    LongOpt { name: "deepen",                      neg: true,  arg: Arg::Required },
    LongOpt { name: "unshallow",                   neg: false, arg: Arg::None },
    LongOpt { name: "update-shallow",              neg: true,  arg: Arg::None },
    LongOpt { name: "refmap",                      neg: false, arg: Arg::Required },
    LongOpt { name: "server-option",               neg: true,  arg: Arg::Required },
    LongOpt { name: "ipv4",                        neg: true,  arg: Arg::None },
    LongOpt { name: "ipv6",                        neg: true,  arg: Arg::None },
    LongOpt { name: "negotiation-restrict",        neg: true,  arg: Arg::Required },
    // `OPT_ALIAS(0, "negotiation-tip", "negotiation-restrict")`: `preprocess_options()`
    // copies the source entry over the alias, keeping only the alias's own name, so it
    // resolves with the source's negatability and value handling.
    LongOpt { name: "negotiation-tip",            neg: true,  arg: Arg::Required },
    LongOpt { name: "negotiation-include",         neg: true,  arg: Arg::Required },
    LongOpt { name: "show-forced-updates",         neg: true,  arg: Arg::None },
    LongOpt { name: "set-upstream",                neg: true,  arg: Arg::None },
];

/// The one `OPT_ALIAS()` group in `pull_options[]`. `is_alias()` (parse-options.c:471)
/// reads it so `--negotiation-` does not report itself as ambiguous between an alias
/// and the option it aliases.
const ALIAS_GROUPS: &[&[&str]] = &[&["negotiation-tip", "negotiation-restrict"]];

/// `usage_with_options()` rendering of `builtin/pull.c`'s option table, verbatim
/// (git's `-h` writes it to stdout and exits 129).
const USAGE: &str = concat!(
    "usage: git pull [<options>] [<repository> [<refspec>...]]\n",
    "\n",
    "    -v, --[no-]verbose    be more verbose\n",
    "    -q, --[no-]quiet      be more quiet\n",
    "    --[no-]progress       force progress reporting\n",
    "    --[no-]recurse-submodules[=<on-demand>]\n",
    "                          control for recursive fetching of submodules\n",
    "\n",
    "Options related to merging\n",
    "    -r, --[no-]rebase[=(false|true|merges|interactive)]\n",
    "                          incorporate changes by rebasing rather than merging\n",
    "    -n                    do not show a diffstat at the end of the merge\n",
    "    --[no-]stat           show a diffstat at the end of the merge\n",
    "    --[no-]compact-summary\n",
    "                          show a compact-summary at the end of the merge\n",
    "    --[no-]log[=<n>]      add (at most <n>) entries from shortlog to merge commit message\n",
    "    --[no-]signoff[=...]  add a Signed-off-by trailer\n",
    "    --[no-]squash         create a single commit instead of doing a merge\n",
    "    --[no-]commit         perform a commit if the merge succeeds (default)\n",
    "    --[no-]edit           edit message before committing\n",
    "    --[no-]cleanup <mode> how to strip spaces and #comments from message\n",
    "    --[no-]ff             allow fast-forward\n",
    "    --ff-only             abort if fast-forward is not possible\n",
    "    --[no-]verify         control use of pre-merge-commit and commit-msg hooks\n",
    "    --[no-]verify-signatures\n",
    "                          verify that the named commit has a valid GPG signature\n",
    "    --[no-]autostash      automatically stash/stash pop before and after\n",
    "    -s, --[no-]strategy <strategy>\n",
    "                          merge strategy to use\n",
    "    -X, --[no-]strategy-option <option=value>\n",
    "                          option for selected merge strategy\n",
    "    -S, --[no-]gpg-sign[=<key-id>]\n",
    "                          GPG sign commit\n",
    "    --[no-]allow-unrelated-histories\n",
    "                          allow merging unrelated histories\n",
    "\n",
    "Options related to fetching\n",
    "    --[no-]all            fetch from all remotes\n",
    "    -a, --[no-]append     append to .git/FETCH_HEAD instead of overwriting\n",
    "    --[no-]upload-pack <path>\n",
    "                          path to upload pack on remote end\n",
    "    -f, --[no-]force      force overwrite of local branch\n",
    "    -t, --[no-]tags       fetch all tags and associated objects\n",
    "    -p, --[no-]prune      prune remote-tracking branches no longer on remote\n",
    "    -j, --[no-]jobs[=<n>] number of submodules pulled in parallel\n",
    "    --[no-]dry-run        dry run\n",
    "    -k, --[no-]keep       keep downloaded pack\n",
    "    --[no-]depth <depth>  deepen history of shallow clone\n",
    "    --[no-]shallow-since <time>\n",
    "                          deepen history of shallow repository based on time\n",
    "    --[no-]shallow-exclude <ref>\n",
    "                          deepen history of shallow clone, excluding ref\n",
    "    --[no-]deepen <n>     deepen history of shallow clone\n",
    "    --unshallow           convert to a complete repository\n",
    "    --[no-]update-shallow accept refs that update .git/shallow\n",
    "    --refmap <refmap>     specify fetch refmap\n",
    "    -o, --[no-]server-option <server-specific>\n",
    "                          option to transmit\n",
    "    -4, --[no-]ipv4       use IPv4 addresses only\n",
    "    -6, --[no-]ipv6       use IPv6 addresses only\n",
    "    --[no-]negotiation-restrict <revision>\n",
    "                          report that we have only objects reachable from this object\n",
    "    --[no-]negotiation-tip <revision>\n",
    "                          alias of --negotiation-restrict\n",
    "    --[no-]negotiation-include <revision>\n",
    "                          ensure this ref is always sent as a negotiation have\n",
    "    --[no-]show-forced-updates\n",
    "                          check for forced-updates on all updated branches\n",
    "    --[no-]set-upstream   set upstream for git pull/fetch\n",
    "\n",
);

/// Which integration step `pull` runs after the fetch, mirroring git's
/// `enum rebase_type` as selected by `config_get_rebase()`.
#[derive(Clone, Copy, PartialEq, Eq)]
enum RebaseMode {
    Disabled,
    Plain,
    Merges,
    Interactive,
}

/// `set_reflog_message()` (builtin/pull.c:149), called from `cmd_pull` before
/// `parse_options` runs, so the message is the argv exactly as typed:
///
/// ```c
/// if (!getenv("GIT_REFLOG_ACTION"))
///     set_reflog_message(argc, argv);
/// ```
///
/// `setenv(..., 0)` does not overwrite, which is what makes `pull` win over the
/// integration step: `cmd_merge` runs its own
/// `setenv("GIT_REFLOG_ACTION", "merge <heads>", 0)` (builtin/merge.c:1586) and
/// finds the variable already set, then writes
/// `"<GIT_REFLOG_ACTION>: <msg>"` into the reflog (builtin/merge.c:492). A pull
/// therefore leaves `pull . refs/heads/feature: Fast-forward` where a bare merge
/// would leave `merge FETCH_HEAD: Fast-forward`.
///
/// This sets the variable; the integration step still has to read it. The ported
/// [`merge`](super::merge) builds its reflog message as a hardcoded
/// `merge <spec>: <msg>` and does not consult `GIT_REFLOG_ACTION`, so every
/// `git pull` that reaches it still writes the merge-shaped message — the one
/// remaining half of this port, and the reason the pull reflog does not match
/// stock yet. `am.rs` and `tag.rs` already read the variable, so the convention
/// it belongs to is live.
fn set_reflog_message(args: &[String]) {
    if std::env::var_os("GIT_REFLOG_ACTION").is_some() {
        return;
    }
    let mut msg = String::from("pull");
    for a in args {
        msg.push(' ');
        msg.push_str(a);
    }
    std::env::set_var("GIT_REFLOG_ACTION", msg);
}

/// Parse a `--rebase=<value>` / `pull.rebase` / `branch.<name>.rebase` value the
/// way git's `rebase_parse_value()` does.
fn parse_rebase_value(v: &str) -> Result<RebaseMode> {
    match v.to_ascii_lowercase().as_str() {
        "" | "true" | "yes" | "on" | "1" => Ok(RebaseMode::Plain),
        "false" | "no" | "off" | "0" => Ok(RebaseMode::Disabled),
        "merges" => Ok(RebaseMode::Merges),
        "interactive" => Ok(RebaseMode::Interactive),
        "preserve" => crate::git_fatal!(
            "preserve is no longer supported (--rebase=preserve / pull.rebase=preserve); use 'merges' instead"
        ),
        other => crate::git_fatal!("Invalid value for rebase: '{other}'"),
    }
}

/// Resolve the configured rebase policy: `branch.<name>.rebase` overrides
/// `pull.rebase`, and an unset key means a merge.
///
/// The second half of the answer is git's `rebase_unspecified` out-parameter:
/// neither key was set at all, which is what lets `cmd_pull()` tell "merge,
/// because that is configured" apart from "no policy was ever chosen" — only
/// the latter refuses to integrate a diverged branch.
fn config_rebase(repo: &gix::Repository, branch: Option<&str>) -> Result<(RebaseMode, bool)> {
    let snap = repo.config_snapshot();
    let raw = branch
        .and_then(|b| snap.string(&format!("branch.{b}.rebase")))
        .or_else(|| snap.string("pull.rebase"));
    match raw.map(|v| v.to_string()) {
        None => Ok((RebaseMode::Disabled, true)),
        Some(v) => Ok((parse_rebase_value(&v)?, false)),
    }
}

pub fn pull(args: &[String]) -> Result<ExitCode> {
    set_reflog_message(args);

    // ---- parse -----------------------------------------------------------
    let mut positionals: Vec<&str> = Vec::new();

    // Fast-forward flag a CLI option selects (merge path); forwarded to `merge`
    // to override both pull.ff and merge.ff. `None` until seen.
    let mut ff_cli: Option<&'static str> = None;

    // Integration selection (CLI). `None` falls through to config.
    let mut rebase_cli: Option<RebaseMode> = None;

    // Integration knobs forwarded to `merge`/`rebase`.
    // git's `opt_diffstat` is a single `OPT_PASSTHRU` slot shared by `-n`,
    // `--stat`/`--no-stat`, `--summary`/`--no-summary` and
    // `--compact-summary`/`--no-compact-summary`: the last occurrence wins and
    // `recreate_opt()` re-renders it, so the literal below is what reaches the
    // integration step.
    let mut diffstat: Option<&'static str> = None;
    let mut strategy: Option<String> = None; // -s / --strategy
    let mut strategy_opts: Vec<String> = Vec::new(); // -X / --strategy-option
    let mut signoff = false;
    let mut autostash: Option<bool> = None;
    // `--squash`/`--no-squash` and `--allow-unrelated-histories`, both
    // `run_merge()` passthroughs the merge port implements.
    let mut squash: Option<bool> = None;
    // `run_merge()`'s verbatim passthroughs — `--[no-]commit`, `--[no-]edit`,
    // `--[no-]log[=<n>]`, `--cleanup=<mode>` — which the merge port implements.
    let mut merge_passthru: Vec<String> = Vec::new();
    let mut allow_unrelated = false;
    // `--verify` (git's default) / `--no-verify`: forwarded to `merge`, which
    // runs the `pre-merge-commit` and `commit-msg` hooks. git's pull passes it
    // only to the merge, never to the rebase.
    let mut no_verify = false;

    // Knobs forwarded to `fetch`.
    let mut f_all = false;
    let mut f_force = false;
    // `None` where git would push nothing at all to the fetch; `Some(false)` is
    // the `--no-` spelling, which git recreates and forwards just like the
    // positive one.
    let mut f_tags: Option<bool> = None;
    let mut f_prune: Option<bool> = None;
    let mut f_unshallow = false;
    let mut f_update_shallow = false;
    let mut f_depth: Option<String> = None;
    let mut f_deepen: Option<String> = None;
    let mut f_shallow_since: Option<String> = None;
    let mut f_shallow_exclude: Vec<String> = Vec::new();
    let mut f_quiet = false;
    let mut f_verbose = false;
    // Fetch knobs with no merge/rebase meaning, forwarded verbatim.
    let mut f_progress: Option<bool> = None;
    let mut f_dry_run = false;
    let mut f_append = false;
    let mut f_keep: Option<bool> = None;
    let mut f_show_forced: Option<bool> = None;
    let mut f_recurse: Option<String> = None;
    // `Some(None)` is a bare `--jobs`, forwarded as such; `Some(Some(n))` carried
    // its value attached.
    let mut f_jobs: Option<Option<String>> = None;
    let mut f_upload_pack: Option<String> = None;
    let mut f_server_options: Vec<String> = Vec::new();
    let mut f_refmap: Vec<String> = Vec::new();
    // `--negotiation-restrict`/`--negotiation-tip`/`--negotiation-include`, forwarded verbatim so the
    // fetch resolves them against the same repository.
    let mut f_negotiation_restrict: Vec<String> = Vec::new();
    let mut f_negotiation_include: Vec<String> = Vec::new();
    // `-4`/`--ipv4` and `-6`/`--ipv6`, forwarded to the fetch as git's pull does.
    let mut f_address_family: Option<&'static str> = None;

    let mut i = 0;
    while i < args.len() {
        let typed = args[i].as_str();
        i += 1;

        // Respell a unique abbreviation as the name it resolves to, so `--autost`
        // reaches the same arm as `--autostash`. The aliased form is needed because
        // `pull_options[]` has an `OPT_ALIAS()`: without the group, `--negotiation-`
        // would report itself ambiguous between the alias and its source.
        let canonical;
        let a = match super::canonical_long_aliased(typed, LONG_OPTS, ALIAS_GROUPS) {
            super::Long::Name(name) => {
                canonical = name;
                canonical.as_ref()
            }
            super::Long::Ambiguous(first, second) => {
                return Ok(super::ambiguous_option(typed, &first, &second, USAGE))
            }
        };

        // Split `--opt=value` for the value-taking long options.
        let (key, inline) = match (a.starts_with("--"), a.split_once('=')) {
            (true, Some((k, v))) => (k, Some(v.to_string())),
            _ => (a, None),
        };

        // Value for a value-taking option: inline `=v` or the next argv entry.
        // A missing one is `parse-options`' own error — one line, no usage block,
        // exit 129.
        macro_rules! take_value {
            ($name:literal) => {
                match inline.clone() {
                    Some(v) => v,
                    None => match args.get(i).cloned() {
                        Some(v) => {
                            i += 1;
                            v
                        }
                        None => return Ok(super::missing_option_value(key)),
                    },
                }
            };
        }

        match key {
            // Fast-forward policy (merge path).
            "--ff" => ff_cli = Some("--ff"),
            "--ff-only" => ff_cli = Some("--ff-only"),
            "--no-ff" => ff_cli = Some("--no-ff"),

            // Rebase selection.
            "--rebase" => {
                rebase_cli = Some(match inline.as_deref() {
                    None => RebaseMode::Plain,
                    Some(v) => parse_rebase_value(v)?,
                });
            }
            "--no-rebase" => rebase_cli = Some(RebaseMode::Disabled),
            "-r" => rebase_cli = Some(RebaseMode::Plain),

            // Integration knobs forwarded to merge/rebase.
            "--stat" => diffstat = Some("--stat"),
            "--no-stat" => diffstat = Some("--no-stat"),
            "--summary" => diffstat = Some("--summary"),
            "--no-summary" => diffstat = Some("--no-summary"),
            "-n" => diffstat = Some("-n"),
            "--compact-summary" => diffstat = Some("--compact-summary"),
            "--no-compact-summary" => diffstat = Some("--no-compact-summary"),
            "-s" | "--strategy" => strategy = Some(take_value!("strategy")),
            "--no-strategy" => strategy = None,
            "-X" | "--strategy-option" => strategy_opts.push(take_value!("strategy-option")),
            "--no-strategy-option" => strategy_opts.clear(),
            "--signoff" => signoff = true,
            "--no-signoff" => signoff = false,
            "--autostash" => autostash = Some(true),
            "--no-autostash" => autostash = Some(false),

            // Fetch knobs forwarded to super::fetch. git declares each of these
            // `OPT_PASSTHRU`/`OPT_BOOL`, so every one has a `--no-` spelling that
            // `recreate_opt()` hands to the fetch verbatim — `git pull --no-tags`
            // has to reach `git fetch --no-tags` rather than be rejected here.
            "--all" => f_all = true,
            "--no-all" => f_all = false,
            "-f" | "--force" => f_force = true,
            "--no-force" => f_force = false,
            "-t" | "--tags" => f_tags = Some(true),
            "--no-tags" => f_tags = Some(false),
            "-p" | "--prune" => f_prune = Some(true),
            "--no-prune" => f_prune = Some(false),
            "--unshallow" => f_unshallow = true,
            "--update-shallow" => f_update_shallow = true,
            "--no-update-shallow" => f_update_shallow = false,
            "--depth" => f_depth = Some(take_value!("depth")),
            "--no-depth" => f_depth = None,
            "--deepen" => f_deepen = Some(take_value!("deepen")),
            "--no-deepen" => f_deepen = None,
            "--shallow-since" => f_shallow_since = Some(take_value!("shallow-since")),
            "--no-shallow-since" => f_shallow_since = None,
            "--shallow-exclude" => f_shallow_exclude.push(take_value!("shallow-exclude")),
            "--no-shallow-exclude" => f_shallow_exclude.clear(),
            // `OPT__VERBOSITY` counts up and down; the `--no-` spellings zero
            // their own side rather than being unknown options.
            "-q" | "--quiet" => f_quiet = true,
            "--no-quiet" => f_quiet = false,
            "-v" | "--verbose" => f_verbose = true,
            "--no-verbose" => f_verbose = false,
            "--progress" => f_progress = Some(true),
            "--no-progress" => f_progress = Some(false),
            // git's pull runs the fetch with `--dry-run` and then returns
            // without integrating anything (`builtin/pull.c` returns right after
            // `run_fetch`), so nothing is merged or rebased.
            "--dry-run" => f_dry_run = true,
            "--no-dry-run" => f_dry_run = false,
            "-a" | "--append" => f_append = true,
            "--no-append" => f_append = false,
            "-k" | "--keep" => f_keep = Some(true),
            // Forwarded rather than swallowed: the fetch port is where the
            // "no unpack-objects path" refusal lives, and git's pull passes the
            // flag straight through too.
            "--no-keep" => f_keep = Some(false),
            "--show-forced-updates" => f_show_forced = Some(true),
            "--no-show-forced-updates" => f_show_forced = Some(false),
            "--recurse-submodules" => {
                f_recurse = Some(inline.clone().unwrap_or_else(|| "yes".into()))
            }
            "--no-recurse-submodules" => f_recurse = Some("no".into()),
            // `OPT_PASSTHRU('j', "jobs", …, PARSE_OPT_OPTARG)`: an attached value
            // is recreated as `--jobs=<n>`, and a bare `--jobs` is forwarded bare —
            // where `git fetch`'s own `--jobs` (which does require a value) picks
            // up whatever positional followed it. That is why `git pull --jobs 2`
            // fetches from `origin` rather than from a remote called `2`.
            "-j" | "--jobs" => {
                f_jobs = Some(inline.clone());
            }
            "--no-jobs" => f_jobs = None,
            "--upload-pack" => f_upload_pack = Some(take_value!("upload-pack")),
            "--no-upload-pack" => f_upload_pack = None,
            "-o" | "--server-option" => f_server_options.push(take_value!("server-option")),
            "--no-server-option" => f_server_options.clear(),
            "--refmap" => f_refmap.push(take_value!("refmap")),
            "--negotiation-restrict" | "--negotiation-tip" => {
                f_negotiation_restrict.push(take_value!("negotiation-restrict"));
            }
            "--no-negotiation-restrict" | "--no-negotiation-tip" => f_negotiation_restrict.clear(),
            "--negotiation-include" => f_negotiation_include.push(take_value!("negotiation-include")),
            "--no-negotiation-include" => f_negotiation_include.clear(),
            "-4" | "--ipv4" => f_address_family = Some("--ipv4"),
            "-6" | "--ipv6" => f_address_family = Some("--ipv6"),
            // git's pull knows `--[no-]ipv4`/`--[no-]ipv6` and recreates whichever
            // spelling it saw for the fetch, where the negation is an unknown
            // option — so the negation is forwarded rather than swallowed here.
            "--no-ipv4" => f_address_family = Some("--no-ipv4"),
            "--no-ipv6" => f_address_family = Some("--no-ipv6"),

            // `--verify` (default) / `--no-verify` reach the merge, which runs
            // the `pre-merge-commit` and `commit-msg` hooks.
            "--verify" => no_verify = false,
            "--no-verify" => no_verify = true,

            // `--squash`/`--allow-unrelated-histories` are `run_merge()`
            // passthroughs, and the merge port implements both, so they are
            // forwarded rather than refused. Neither has a rebase equivalent;
            // git rejects them there and so does the merge-only check below.
            _ if a.starts_with("--log=") || a.starts_with("--cleanup=") => {
                merge_passthru.push(a.to_string());
            }
            "--squash" => squash = Some(true),
            "--no-squash" => squash = Some(false),
            "--allow-unrelated-histories" => allow_unrelated = true,
            "--no-allow-unrelated-histories" => allow_unrelated = false,

            // `run_merge()` passthroughs, forwarded as given. None of them has a
            // rebase equivalent, and git does not reject them there either — it
            // simply does not push them onto the rebase command line.
            "--commit" | "--no-commit" | "-e" | "--edit" | "--no-edit" | "--log" | "--no-log" => {
                merge_passthru.push(a.to_string());
            }
            "--cleanup" => {
                let mode = take_value!("cleanup");
                merge_passthru.push(format!("--cleanup={mode}"));
            }
            // `OPT_CLEANUP` is an `OPT_STRING`, so its unset writes NULL over
            // `cleanup_arg` (parse-options.c:200-202) — and `cmd_pull()` only
            // forwards `--cleanup=<mode>` to the merge `if (cleanup_arg)`
            // (builtin/pull.c:549). A `--no-cleanup` therefore discards an earlier
            // value and sends nothing, rather than being an unknown option.
            "--no-cleanup" => merge_passthru.retain(|o| !o.starts_with("--cleanup=")),

            // Absent substrate. `--no-set-upstream`/`--no-gpg-sign` ask for the
            // default (do neither), so they are accepted where the positive form
            // is refused, exactly as git treats them as ordinary negations.
            "--no-set-upstream" | "--no-gpg-sign" => {}
            "--set-upstream" => {
                bail!("--set-upstream is not supported (not exposed by the high-level fetch)")
            }
            "-S" | "--gpg-sign" => bail!("--gpg-sign is not supported (GPG is not vendored)"),
            "--verify-signatures" | "--no-verify-signatures" => {
                bail!("--verify-signatures is not supported (GPG is not vendored)")
            }

            // `parse_options`' built-in `-h`: the option table on stdout,
            // exit 129. It fires wherever it appears, ahead of everything else.
            "-h" => {
                print!("{USAGE}");
                return Ok(ExitCode::from(129));
            }

            "--" => {
                positionals.extend(args[i..].iter().map(String::as_str));
                break;
            }
            // Attached short-option values git's parse-options accepts, e.g.
            // `-Xtheirs` / `-sort`.
            other if other.starts_with("-X") && other.len() > 2 => {
                strategy_opts.push(other[2..].to_string())
            }
            other if other.starts_with("-s") && other.len() > 2 => {
                strategy = Some(other[2..].to_string())
            }
            // `parse-options`' own rejection: the offending name without its
            // dashes, then the whole option table, exit 129.
            other if other.starts_with("--") => {
                eprintln!("error: unknown option `{}'", key.trim_start_matches('-'));
                eprint!("{USAGE}");
                return Ok(ExitCode::from(129));
            }
            other if other.starts_with('-') && other != "-" => {
                eprintln!("error: unknown switch `{}'", &other[1..2]);
                eprint!("{USAGE}");
                return Ok(ExitCode::from(129));
            }
            // A non-option argument: the resolver hands those back untouched, so
            // the argv slice itself is pushed and the operand keeps `args`' lifetime.
            _ => positionals.push(typed),
        }
    }

    let mut repo = gix::discover(".")?;
    // A pull moves `HEAD` and the remote-tracking refs, and both write reflogs.
    // Without a configured identity the write fails, and by then the fast-forward
    // has already updated the worktree — a half-applied pull. git synthesizes an
    // identity for reflog purposes rather than failing, which is what `fetch` and
    // `receive-pack` already do here.
    crate::ensure_reflog_identity(&mut repo);
    let repo = repo;
    let head_name = repo.head_name()?;
    let branch_short = head_name.as_ref().map(|h| h.shorten().to_string());

    // Resolve the integration policy git's `config_get_rebase()` computes: a CLI
    // flag wins, else branch.<name>.rebase / pull.rebase.
    let (rebase_mode, rebase_unspecified) = match rebase_cli {
        Some(m) => (m, false),
        None => config_rebase(&repo, branch_short.as_deref())?,
    };
    let rebasing = rebase_mode != RebaseMode::Disabled;

    // `repo_read_index_unmerged()` / `file_exists(MERGE_HEAD)` in `cmd_pull()`:
    // both sit above `run_fetch()`, so an unfinished merge stops the pull before
    // a single byte moves.
    if let Some(code) = refuse_unfinished_merge(&repo)? {
        return Ok(code);
    }

    // `cmd_pull()` checks the worktree BEFORE `run_fetch()` when it is going to
    // rebase (`builtin/pull.c`: `require_clean_work_tree()` sits above the fetch,
    // and above the merge-head resolution that follows it), so a dirty tree ends
    // the pull without a single byte of network traffic and without the tracking
    // refs moving. `--autostash` skips the check, since the rebase stashes the
    // changes itself.
    if rebasing && !autostash_wanted(&repo, autostash)? {
        if let Some(code) = require_clean_work_tree(&repo, "pull with rebase")? {
            return Ok(code);
        }
    }

    // Resolve which remote-tracking ref the fetched upstream lands at.
    // `<repository>` may just as well be a URL, and then there is no `remote.<name>` section and
    // nothing under `refs/remotes/` for the fetch to have updated. git never depends on one:
    // `cmd_pull()` collects its merge heads from `FETCH_HEAD`, which the fetch has just written for
    // exactly the refs that were asked for. Only a configured remote gets the tracking-ref
    // treatment, because that is the one whose name a tracking ref can be built from.
    let named_remote = positionals
        .first()
        .is_some_and(|name| repo.remote_names().iter().any(|known| known == name));
    let target_ref = if positionals.len() >= 2 && named_remote {
        // Explicit `<remote> <branch>`: after a default-refspec fetch the branch
        // lands at refs/remotes/<remote>/<branch>.
        format!("refs/remotes/{}/{}", positionals[0], positionals[1])
    } else if positionals.len() >= 2 {
        "FETCH_HEAD".to_string()
    } else {
        // No explicit branch: derive the tracking ref from the current branch's
        // upstream configuration (branch.<name>.remote / .merge).
        let head = match head_name.as_ref() {
            Some(head) => head,
            None => return Ok(no_merge_candidates(&repo, None, rebasing)),
        };
        match repo.branch_remote_tracking_ref_name(head.as_ref(), Direction::Fetch) {
            Some(Ok(name)) => name.as_bstr().to_string(),
            Some(Err(err)) => return Err(err.into()),
            None => {
                return Ok(no_merge_candidates(
                    &repo,
                    Some(head.shorten().to_string()).as_deref(),
                    rebasing,
                ))
            }
        }
    };

    // ---- phase 1: fetch --------------------------------------------------
    // Delegate to the ported fetch, which acquires the repo lock itself, prints
    // the git-style `From …` per-ref summary, and honors the forwarded knobs.
    let mut fetch_args: Vec<String> = Vec::new();
    if f_all {
        fetch_args.push("--all".into());
    }
    if f_force {
        fetch_args.push("--force".into());
    }
    match f_tags {
        Some(true) => fetch_args.push("--tags".into()),
        Some(false) => fetch_args.push("--no-tags".into()),
        None => {}
    }
    match f_prune {
        Some(true) => fetch_args.push("--prune".into()),
        Some(false) => fetch_args.push("--no-prune".into()),
        None => {}
    }
    if f_update_shallow {
        fetch_args.push("--update-shallow".into());
    }
    if f_unshallow {
        fetch_args.push("--unshallow".into());
    }
    if let Some(d) = &f_depth {
        fetch_args.push("--depth".into());
        fetch_args.push(d.clone());
    }
    if let Some(d) = &f_deepen {
        fetch_args.push("--deepen".into());
        fetch_args.push(d.clone());
    }
    if let Some(t) = &f_shallow_since {
        fetch_args.push("--shallow-since".into());
        fetch_args.push(t.clone());
    }
    for r in &f_shallow_exclude {
        fetch_args.push("--shallow-exclude".into());
        fetch_args.push(r.clone());
    }
    if f_quiet {
        fetch_args.push("--quiet".into());
    }
    if f_verbose {
        fetch_args.push("--verbose".into());
    }
    match f_progress {
        Some(true) => fetch_args.push("--progress".into()),
        Some(false) => fetch_args.push("--no-progress".into()),
        None => {}
    }
    if f_dry_run {
        fetch_args.push("--dry-run".into());
    }
    if f_append {
        fetch_args.push("--append".into());
    }
    match f_keep {
        Some(true) => fetch_args.push("--keep".into()),
        Some(false) => fetch_args.push("--no-keep".into()),
        None => {}
    }
    match f_show_forced {
        Some(true) => fetch_args.push("--show-forced-updates".into()),
        Some(false) => fetch_args.push("--no-show-forced-updates".into()),
        None => {}
    }
    if let Some(r) = &f_recurse {
        fetch_args.push(format!("--recurse-submodules={r}"));
    }
    if let Some(j) = &f_jobs {
        match j {
            Some(n) => fetch_args.push(format!("--jobs={n}")),
            None => fetch_args.push("--jobs".into()),
        }
    }
    if let Some(p) = &f_upload_pack {
        fetch_args.push(format!("--upload-pack={p}"));
    }
    for o in &f_server_options {
        fetch_args.push(format!("--server-option={o}"));
    }
    for r in &f_refmap {
        fetch_args.push(format!("--refmap={r}"));
    }
    for t in &f_negotiation_restrict {
        fetch_args.push(format!("--negotiation-restrict={t}"));
    }
    for t in &f_negotiation_include {
        fetch_args.push(format!("--negotiation-include={t}"));
    }
    if let Some(f) = f_address_family {
        fetch_args.push(f.into());
    }
    // `--all` fans out over every configured remote and takes no repository
    // argument; otherwise git hands the whole `<remote> [<refspec>…]` tail to the
    // fetch (`run_fetch()` in `builtin/pull.c`). The refspecs select what is
    // downloaded, and the remote's configured refspecs still update the tracking
    // ref that the merge or rebase below reads, via the opportunistic second stage.
    if !f_all {
        fetch_args.extend(positionals.iter().map(|p| (*p).to_string()));
    }
    // Network / bad-remote failures surface as `Err`; a ref-rejection returns a
    // non-success code with the summary already printed. The tracking-ref check
    // below then reports the missing upstream, as git's pull does.
    let fetch_code = super::fetch(&fetch_args)?;

    // `--dry-run` stops here: git's pull returns right after `run_fetch` without
    // touching the worktree, so no merge or rebase is attempted and the tracking
    // refs the integration step would need were never written.
    if f_dry_run {
        return Ok(fetch_code);
    }

    // `cmd_pull()` is `if (run_fetch(...)) return 1;` - a fetch that failed ends the pull right
    // there, with 1 whatever the fetch itself exited with, and no integration step is attempted.
    if fetch_code != ExitCode::SUCCESS {
        return Ok(ExitCode::FAILURE);
    }

    // `get_merge_heads()`: everything the fetch marked for-merge in `FETCH_HEAD`,
    // which is what `cmd_pull()` integrates — not a remote-tracking ref. A
    // `<remote> <ref>` pair that lands nowhere under `refs/remotes/` (a tag, a
    // `refs/pull/…` head) is merged just the same, and several refspecs give
    // several heads.
    let merge_heads = super::merge::fetch_head_for_merge(&repo)?;
    if merge_heads.is_empty() {
        // `die_no_merge_candidates()`: nothing came back that could be merged.
        crate::git_fatal!("couldn't find remote ref {target_ref}");
    }

    // ---- phase 2: integrate ----------------------------------------------

    // `cmd_pull()`: an unborn `HEAD` has no history to merge into, so git treats
    // the fetched head as the initial state — `pull_into_void()` fast-forwards the
    // empty tree to it and points `HEAD` there with an `initial pull` reflog entry.
    // This is the `git init && git remote add && git pull origin main` flow.
    if repo.head()?.is_unborn() {
        if merge_heads.len() > 1 {
            eprintln!("fatal: Cannot merge multiple branches into empty head.");
            return Ok(ExitCode::from(128));
        }
        return pull_into_void(&repo, merge_heads[0].0);
    }

    let head_id = repo.head_id()?.detach();

    // `already_up_to_date()`: every fetched head is already reachable from `HEAD`.
    // git does not report this itself — the integration step does, and *which*
    // line it prints is the difference between the two: the merge says
    // `Already up to date.` while the rebase, which still runs, says
    // `Current branch <b> is up to date.`. Both are exercised against stock git
    // in tests/pull_up_to_date.rs, so nothing is short-circuited here.
    let already_up_to_date = merge_heads.iter().all(|(id, _)| {
        repo.merge_base(*id, head_id).map(|base| base.detach() == *id).unwrap_or(false)
    });

    // `builtin/pull.c`'s `can_ff`: `get_can_ff()` asks whether the *first* fetched
    // head is a descendant of `HEAD`. When it is, a rebase would replay nothing, so
    // git forces `opt_ff = "--ff-only"` and runs the *merge* (`ran_ff`), never
    // starting the rebase.
    let can_ff = repo
        .merge_base(merge_heads[0].0, head_id)
        .map(|base| base.detach() == head_id)
        .unwrap_or(false);

    // `cmd_pull()`'s divergence gate. With neither an `--ff…` flag nor `pull.ff`,
    // and with the rebase policy left unspecified by both the command line and
    // config, git refuses to pick an integration strategy for a diverged branch —
    // it does not quietly merge.
    // `divergent = !can_ff && !already_up_to_date`: a branch that is merely
    // *ahead* of the fetched head has not diverged, and needs no policy.
    let divergent = !can_ff && !already_up_to_date;
    let ff_configured = ff_cli.is_some() || repo.config_snapshot().string("pull.ff").is_some();
    if !ff_configured && rebase_unspecified && divergent {
        show_advice_pull_non_ff(&repo);
        eprintln!("fatal: Need to specify how to reconcile divergent branches.");
        return Ok(ExitCode::from(128));
    }

    // Several merge heads have no meaning for a rebase, and cannot all be
    // fast-forwarded to.
    if merge_heads.len() > 1 {
        if rebasing {
            eprintln!("fatal: Cannot rebase onto multiple branches.");
            return Ok(ExitCode::from(128));
        }
        if ff_cli == Some("--ff-only") {
            eprintln!("fatal: Cannot fast-forward to multiple branches.");
            return Ok(ExitCode::from(128));
        }
    }

    if rebasing && !can_ff {
        if rebase_mode == RebaseMode::Interactive {
            bail!(
                "--rebase=interactive is not supported (interactive todo editing needs a TTY editor loop)"
            );
        }

        // Rebase the current branch onto the fetched upstream, forwarding the
        // knobs the ported rebase accepts.
        let mut rebase_args: Vec<String> = Vec::new();
        if rebase_mode == RebaseMode::Merges {
            rebase_args.push("--rebase-merges".into());
        }
        if let Some(s) = &strategy {
            rebase_args.push("--strategy".into());
            rebase_args.push(s.clone());
        }
        for x in &strategy_opts {
            rebase_args.push("--strategy-option".into());
            rebase_args.push(x.clone());
        }
        if signoff {
            rebase_args.push("--signoff".into());
        }
        // A clean tree makes autostash a no-op; a dirty tree was already gated by
        // the pre-fetch `require_clean_work_tree()` above unless autostash is on,
        // in which case the rebase port stashes it itself.
        if autostash_wanted(&repo, autostash)? {
            rebase_args.push("--autostash".into());
        }
        // `run_rebase()` pushes `opt_diffstat` verbatim. `git rebase` knows only
        // `-n`/`--stat`/`--no-stat`, so `--summary`/`--compact-summary` reach it
        // as unknown options and fail there, as they do for git.
        if let Some(d) = diffstat {
            rebase_args.push(d.to_string());
        }
        // `argv_push_verbosity(&args)` in `run_rebase()`: the accumulated `-q`/
        // `-v` count reaches the integration step, which is what makes
        // `pull --quiet` quiet rather than only quieting the fetch.
        if f_quiet {
            rebase_args.push("--quiet".into());
        }
        if f_verbose {
            rebase_args.push("--verbose".into());
        }
        // git's `cmd_pull()` rejects both of these ahead of the rebase, since a
        // rebase has nowhere to put them.
        if squash.is_some() {
            crate::git_fatal!("--squash is incompatible with --rebase");
        }
        if allow_unrelated {
            crate::git_fatal!("--allow-unrelated-histories is incompatible with --rebase");
        }
        // `get_rebase_newbase_and_upstream()`: without a fork point both the new
        // base and the upstream are the fetched head itself, and git passes the
        // object id — which is what its `rebase (start): checkout <oid>` reflog
        // entry records.
        rebase_args.push(merge_heads[0].0.to_string());
        return super::rebase(&rebase_args);
    }

    // Merge path. Integration knobs the merge port does not implement cannot be
    // forwarded; refuse rather than silently drop them.
    if strategy.is_some() || !strategy_opts.is_empty() || signoff {
        bail!(
            "-s/--strategy, -X/--strategy-option and --signoff are not supported on the merge path \
             (the merge port implements only the 'ort' strategy with no strategy options or sign-off)"
        );
    }

    // Resolve the fast-forward policy git's `config_get_ff()` computes for pull:
    // a CLI flag wins; else pull.ff (which overrides merge.ff) is forwarded to
    // `merge`; else nothing is forwarded and `merge` reads merge.ff itself.
    let ff_flag: Option<&str> = match ff_cli {
        // The `can_ff` short-circuit above overrides everything, as git's assignment to
        // `opt_ff` right before `run_merge()` does.
        _ if rebasing => Some("--ff-only"),
        Some(f) => Some(f),
        None => match repo
            .config_snapshot()
            .string("pull.ff")
            .map(|v| v.to_string().to_ascii_lowercase())
            .as_deref()
        {
            Some("only") => Some("--ff-only"),
            Some("false" | "no" | "off" | "0") => Some("--no-ff"),
            Some(_) => Some("--ff"), // true/yes/on/1/valueless → allow
            None => None,
        },
    };

    // Delegate the fast-forward, --no-ff/diverged merge, dirty check,
    // worktree/index update and git-identical stdout to the ported `merge`,
    // forwarding the resolved ff policy and any diffstat preference.
    let mut merge_args: Vec<String> = Vec::new();
    if let Some(f) = ff_flag {
        merge_args.push(f.to_string());
    }
    // git's pull hands `--no-verify` to the merge only; the rebase path never
    // sees it (`builtin/pull.c` pushes it in `run_merge`).
    if no_verify {
        merge_args.push("--no-verify".into());
    }
    // `run_merge()` pushes `opt_diffstat` verbatim.
    if let Some(d) = diffstat {
        merge_args.push(d.to_string());
    }
    // `argv_push_verbosity(&args)` in `run_merge()`. Without this the merge
    // printed its `Updating …`/`Fast-forward`/diffstat block even under
    // `pull --quiet`, where only the fetch had been silenced.
    if f_quiet {
        merge_args.push("--quiet".into());
    }
    if f_verbose {
        merge_args.push("--verbose".into());
    }
    match squash {
        Some(true) => merge_args.push("--squash".into()),
        Some(false) => merge_args.push("--no-squash".into()),
        None => {}
    }
    merge_args.extend(merge_passthru.iter().cloned());
    if allow_unrelated {
        merge_args.push("--allow-unrelated-histories".into());
    }
    // `run_merge()` forwards the autostash choice and lets `git merge` do the
    // stashing, which is why a dirty tree is fine on the merge path.
    match autostash {
        Some(true) => merge_args.push("--autostash".into()),
        Some(false) => merge_args.push("--no-autostash".into()),
        None if autostash_wanted(&repo, None)? => merge_args.push("--autostash".into()),
        None => {}
    }
    // `run_merge()` ends with a literal `FETCH_HEAD` (builtin/pull.c): the merge
    // re-reads the file, so it sees every for-merge head — an octopus when the
    // pull asked for several — and names them the way the fetch described them.
    merge_args.push("FETCH_HEAD".into());
    super::merge(&merge_args)
}

/// `die_resolve_conflict("pull")` / `die_conclude_merge()` (advice.c), which
/// `cmd_pull()` reaches before the fetch.
///
/// An unmerged index is the first test and wins over the second, so a conflicted
/// merge in progress is reported as unresolved conflicts rather than as an
/// unfinished merge. Both exit 128.
fn refuse_unfinished_merge(repo: &gix::Repository) -> Result<Option<ExitCode>> {
    let snap = repo.config_snapshot();
    let unmerged = repo.open_index().map(|index| {
        index.entries().iter().any(|e| e.stage_raw() != 0)
    });
    if unmerged.unwrap_or(false) {
        eprintln!("error: Pulling is not possible because you have unmerged files.");
        if snap.boolean("advice.resolveConflict") != Some(false) {
            eprintln!("hint: Fix them up in the work tree, and then use 'git add/rm <file>'");
            eprintln!("hint: as appropriate to mark resolution and make a commit.");
        }
        eprintln!("fatal: Exiting because of an unresolved conflict.");
        return Ok(Some(ExitCode::from(128)));
    }
    if repo.git_dir().join("MERGE_HEAD").exists() {
        eprintln!("error: You have not concluded your merge (MERGE_HEAD exists).");
        if snap.boolean("advice.resolveConflict") != Some(false) {
            eprintln!("hint: Please, commit your changes before merging.");
        }
        eprintln!("fatal: Exiting because of unfinished merge.");
        return Ok(Some(ExitCode::from(128)));
    }
    Ok(None)
}

/// `show_advice_pull_non_ff()` (builtin/pull.c): what to do about a diverged
/// branch, printed before the refusal unless `advice.diverging` is off.
fn show_advice_pull_non_ff(repo: &gix::Repository) {
    if repo.config_snapshot().boolean("advice.diverging") == Some(false) {
        return;
    }
    for line in [
        "You have divergent branches and need to specify how to reconcile them.",
        "You can do so by running one of the following commands sometime before",
        "your next pull:",
        "",
        "  git config pull.rebase false  # merge",
        "  git config pull.rebase true   # rebase",
        "  git config pull.ff only       # fast-forward only",
        "",
        "You can replace \"git config\" with \"git config --global\" to set a default",
        "preference for all repositories. You can also pass --rebase, --no-rebase,",
        "or --ff-only on the command line to override the configured default per",
        "invocation.",
    ] {
        if line.is_empty() {
            eprintln!("hint:");
        } else {
            eprintln!("hint: {line}");
        }
    }
}

/// `config_autostash()`: the CLI flag wins, else `pull.autoStash` — which
/// overrides `rebase.autoStash` for a pull — else `rebase.autoStash`, else off.
fn autostash_wanted(repo: &gix::Repository, cli: Option<bool>) -> Result<bool> {
    Ok(match cli {
        Some(v) => v,
        None => {
            let snap = repo.config_snapshot();
            snap.boolean("pull.autoStash")
                .or_else(|| snap.boolean("rebase.autoStash"))
                == Some(true)
        }
    })
}

/// `require_clean_work_tree()` (wt-status.c), as `cmd_pull()` calls it before the
/// fetch when it is going to rebase:
///
/// ```text
/// error: cannot pull with rebase: You have unstaged changes.
/// error: Please commit or stash them.
/// ```
///
/// `Some(exit)` when the tree is dirty — git's `gently == 0`, so it exits 128 —
/// and `None` when the pull may proceed. The unmerged paths `refresh_index()`
/// announces are printed first, on stdout, as they are for the rebase.
fn require_clean_work_tree(repo: &gix::Repository, action: &str) -> Result<Option<ExitCode>> {
    let (unstaged, staged, conflicts) = super::rebase::dirty_state(repo)?;
    for path in &conflicts {
        println!("{path}: needs merge");
    }
    if !unstaged && !staged {
        return Ok(None);
    }
    if unstaged {
        eprintln!("error: cannot {action}: You have unstaged changes.");
        if staged {
            eprintln!("error: additionally, your index contains uncommitted changes.");
        }
    } else {
        eprintln!("error: cannot {action}: Your index contains uncommitted changes.");
    }
    eprintln!("error: Please commit or stash them.");
    Ok(Some(ExitCode::from(128)))
}

/// `die_no_merge_candidates()` (builtin/pull.c): the block git prints when the
/// current branch has no upstream to pull from, exit 1.
///
/// `branch` is the current branch's short name, or `None` on a detached `HEAD`,
/// which git reports as not being on a branch at all. The remote is named only
/// when the repository has exactly one — `get_only_remote()` — and is the
/// placeholder `<remote>` otherwise.
fn no_merge_candidates(repo: &gix::Repository, branch: Option<&str>, rebasing: bool) -> ExitCode {
    let integration = if rebasing {
        "Please specify which branch you want to rebase against."
    } else {
        "Please specify which branch you want to merge with."
    };
    match branch {
        None => {
            eprintln!("You are not currently on a branch.");
            eprintln!("{integration}");
            eprintln!("See git-pull(1) for details.");
            eprintln!();
            eprintln!("    git pull <remote> <branch>");
            eprintln!();
        }
        Some(branch) => {
            let remotes = repo.remote_names();
            let remote = match remotes.len() {
                1 => remotes.iter().next().map(ToString::to_string),
                _ => None,
            };
            let remote = remote.unwrap_or_else(|| "<remote>".to_string());
            eprintln!("There is no tracking information for the current branch.");
            eprintln!("{integration}");
            eprintln!("See git-pull(1) for details.");
            eprintln!();
            eprintln!("    git pull <remote> <branch>");
            eprintln!();
            eprintln!("If you wish to set tracking information for this branch you can do so with:");
            eprintln!();
            eprintln!("    git branch --set-upstream-to={remote}/<branch> {branch}");
            eprintln!();
        }
    }
    ExitCode::FAILURE
}

/// `pull_into_void()` (builtin/pull.c): integrate `merge_head` into an unborn
/// branch.
///
/// git treats the index as based on the empty tree and fast-forwards to the
/// fetched head, so work already added on the unborn branch is not lost and an
/// untracked file in the way still refuses, then points `HEAD` at it with an
/// `initial pull` reflog entry.
fn pull_into_void(repo: &gix::Repository, merge_head: gix::ObjectId) -> Result<ExitCode> {
    use gix::bstr::BStr;
    use gix::refs::transaction::{Change, LogChange, PreviousValue, RefEdit, RefLog};

    let empty_tree = gix::ObjectId::empty_tree(repo.object_hash());
    let target_tree = repo.find_object(merge_head)?.peel_to_tree()?.id;
    let index = repo.index_or_load_from_head_or_empty()?.into_owned();
    let should_interrupt = std::sync::atomic::AtomicBool::new(false);
    let labels = gix::merge::blob::builtin_driver::text::Labels {
        ancestor: Some(BStr::new(b"empty tree")),
        current: Some(BStr::new(b"HEAD")),
        other: Some(BStr::new(b"FETCH_HEAD")),
    };
    let merged = crate::merge_apply::three_way_merge_guarded(
        repo,
        empty_tree,
        empty_tree,
        target_tree,
        &index,
        labels,
        &should_interrupt,
        false,
        &crate::merge_apply::StrategyOptions::default(),
        empty_tree,
    )?;
    let applied = match merged {
        crate::merge_apply::Merged::Applied(applied) => applied,
        // `checkout_fast_forward()` refused: nothing on disk moved and `HEAD`
        // stays unborn.
        crate::merge_apply::Merged::Refused(clobber) => {
            clobber.report("merge");
            return Ok(ExitCode::FAILURE);
        }
    };
    let mut new_index = applied.index;
    new_index.write(Default::default())?;

    repo.edit_reference(RefEdit {
        change: Change::Update {
            log: LogChange {
                mode: RefLog::AndReference,
                force_create_reflog: false,
                message: "initial pull".into(),
            },
            // The branch does not exist yet, which is what makes this the unborn case.
            expected: PreviousValue::MustNotExist,
            new: gix::refs::Target::Object(merge_head),
        },
        name: "HEAD".try_into().map_err(|e| anyhow::anyhow!("invalid ref name HEAD: {e}"))?,
        deref: true,
    })?;
    Ok(ExitCode::SUCCESS)
}
