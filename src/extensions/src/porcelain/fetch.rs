use anyhow::Result;
use prodash::Root as _;
use std::collections::HashSet;
use std::io::{IsTerminal, Read, Write};
use std::num::NonZeroU32;
use std::process::ExitCode;
use std::sync::atomic::AtomicBool;

use gix::bstr::{BStr, BString, ByteSlice};
use gix::refs::transaction::{Change, LogChange, PreviousValue, RefEdit, RefLog};
use gix::refs::{Category, FullName, Target, TargetRef};

use gix::remote::fetch::refs::update::Mode;
use gix::remote::fetch::{RefLogMessage, Shallow, Status, Tags};

/// `git fetch [<options>] [<remote> [<refspec>...]]` — download objects and
/// update the remote-tracking refs, backed by gitoxide's blocking fetch.
///
/// Supported forms:
///   * `git fetch`                    → fetch the branch's remote, else the default remote
///   * `git fetch <remote>`           → fetch a named remote (or a bare URL)
///   * `git fetch <remote> <refspec>…`→ fetch explicit refspecs (override configured)
///   * `--all`                        → fetch every configured remote
///   * `-m`/`--multiple`              → treat all positionals as remotes and fetch each
///   * `-t`/`--tags`                  → also fetch all tags (`refs/tags/*:refs/tags/*`)
///   * `-n`/`--no-tags`               → disable automatic tag following
///   * `-p`/`--prune`                 → delete tracking refs no longer on the remote
///   * `-P`/`--prune-tags`            → add the tags refspec and (with `-p`) prune stale tags
///   * `-f`/`--force`                 → force updates (treat every refspec as `+`)
///   * `--depth <n>`/`--deepen <n>`/`--unshallow` → shallow-clone history controls
///   * `--shallow-since <time>`       → set the shallow boundary at a cutoff date
///   * `--shallow-exclude <ref>`      → exclude history reachable from a ref (repeatable)
///   * `-v`/`--verbose`, `-q`/`--quiet`, `--dry-run` (and their `--no-…` negations)
///   * `--porcelain`                  → machine-readable `<flag> <old> <new> <ref>` on stdout
///   * `--write-fetch-head`/`--no-write-fetch-head`, `-a`/`--append` → `.git/FETCH_HEAD`
///   * `--progress`/`--no-progress`   → force/suppress the stderr progress meter
///   * `--show-forced-updates`/`--no-show-forced-updates` → the `(forced update)` note
///   * `--prefetch`                   → rewrite every refspec into `refs/prefetch/…`
///   * `--stdin`                      → read additional refspecs from standard input
///   * `-u`/`--update-head-ok`        → allow updating the ref `HEAD` points at
///   * `-k`/`--keep`                  → keep the downloaded pack (always the case here)
///   * `--write-commit-graph`         → write the commit-graph after fetching
///   * `--recurse-submodules[=yes|no]`, `-j`/`--jobs <n>` → fetch in populated submodules
///   * `--upload-pack <path>`         → run `<path>` instead of `git-upload-pack` on the other end
///   * `-o`/`--server-option <opt>`   → protocol-v2 `server-option=<opt>` line (repeatable)
///   * `--refmap <refspec>`           → map the command-line refspecs' results with `<refspec>`
///     instead of the remote's configured ones (repeatable; `--refmap=''` stores nowhere)
///   * `--negotiation-restrict <rev>` (alias `--negotiation-tip`) → seed the `have` walk with only
///     these commits; the argument may be a rev or a glob on ref names (repeatable)
///   * `--negotiation-include <rev>`  → send these commits as `have` whatever the algorithm picks
///     (repeatable); `remote.<name>.negotiationInclude` supplies the default
///   * `--negotiate-only`             → print the common commits and fetch nothing
///   * `--atomic`                     → apply every ref update in one transaction, or none of them
///   * `--refetch`                    → send no `have` at all and refetch as a fresh clone would
///   * `--auto-maintenance`/`--auto-gc` (on by default) → `maintenance run --auto` on the way out
///
/// Config-supplied defaults (overridden by the matching flag, git precedence
/// CLI > config > built-in default):
///   * `fetch.prune`              → behave as `--prune`
///   * `fetch.pruneTags`          → behave as `--prune-tags`
///   * `fetch.all`                → behave as `--all` when no remote is named
///   * `fetch.showForcedUpdates`  → default for `--show-forced-updates`
///   * `fetch.writeCommitGraph`   → default for `--write-commit-graph`
///   * `fetch.recurseSubmodules`  → default for `--recurse-submodules`
///   * `fetch.parallel`           → default for `-j`/`--jobs`
///   * `fetch.output`             → `compact` abbreviates the `<from> -> <to>` columns
///   * `remote.<name>.uploadpack` → default for `--upload-pack`
///   * `remote.<name>.serverOption` → default set of `-o`/`--server-option` values
///   * `remote.<name>.negotiationInclude` → default set of `--negotiation-include` tips
///
/// The transfer-side object check, which has no flag at all
/// (`fetch_pack_fsck_config()`, `fetch-pack.c:1954`, and [`fsck_fetched`]):
///   * `fetch.fsckObjects`, falling back to `transfer.fsckObjects` → lint every
///     object the pack delivered before a ref moves, killing the fetch on the
///     first error with `index-pack`'s own two `fatal:` lines
///   * `fetch.fsck.<msg-id>`      → that message's severity for this fetch only;
///     it never falls back on `fsck.<msg-id>`
///   * `fetch.fsck.skipList`      → object ids whose messages are dropped
///   * `core.bigFileThreshold`    → which blobs the check sees as streamed, and
///     so whether `gitmodulesLarge` can fire
///
/// Command-line refspecs go through git's two-stage match (`get_ref_map` in
/// `builtin/fetch.c`): the refspecs on the command line select the refs, and
/// the remote's configured refspecs — or `--refmap` — then map *only those*
/// onto local tracking refs. That second stage is why `git fetch origin main`
/// still updates `refs/remotes/origin/main`; those opportunistic updates are
/// reported in the summary but contribute no `FETCH_HEAD` row, exactly as
/// git's `FETCH_HEAD_IGNORE` does.
///
/// The per-ref summary is written to stderr in `git fetch` layout (`From <url>`
/// header plus one aligned line per changed or pruned ref), or to stdout in the
/// machine-readable layout under `--porcelain`. Options that require substrate
/// gitoxide's high-level fetch does not expose are rejected rather than silently
/// ignored: `--filter` and `--set-upstream`.
///
/// When the remote is itself shallow it offers refs that reach shallow roots we don't have.
/// Adopting those roots would rewrite `.git/shallow`, so by default each such ref is left
/// alone and warned about, exactly as git's `update_shallow()` decides; `--update-shallow`
/// takes the roots the fetched refs actually need and adds them to the boundary instead.
///
/// Known divergence, stated rather than hidden: under `--dry-run` git reports an
/// auto-followed tag *twice* — nothing is written, so its `backfill_tags()`
/// round proposes the same tag the first round already listed. Reproducing that
/// would mean running a second transport round for the sole purpose of printing a
/// duplicate line, so this prints the tag once. A real (non-dry-run) fetch is
/// byte-identical.
///
/// `-4`/`--ipv4` and `-6`/`--ipv6` are git's `transport_family`: they restrict address
/// resolution for `git://` and `http(s)://` and become `ssh`'s `-4`/`-6`, and are forwarded
/// into a submodule fetch the way `add_options_to_argv()` forwards them. A `file://` remote
/// opens no socket and ignores them, as git does.
// The final `take_value!` expansion bumps the `i` cursor that no later arm reads;
// the write is needed by every other expansion, so it can't be removed.
#[allow(unused_assignments)]
/// `git fetch`'s usage block, byte-for-byte from stock git 2.55.0, printed on stdout
/// for `-h` with exit 129 — `parse-options` answers it before anything else.
pub(super) const USAGE: &str = "usage: git fetch [<options>] [<repository> [<refspec>...]]\n   or: git fetch [<options>] <group>\n   or: git fetch --multiple [<options>] [(<repository>|<group>)...]\n   or: git fetch --all [<options>]\n\n    -v, --[no-]verbose    be more verbose\n    -q, --[no-]quiet      be more quiet\n    --[no-]all            fetch from all remotes\n    --[no-]set-upstream   set upstream for git pull/fetch\n    -a, --[no-]append     append to .git/FETCH_HEAD instead of overwriting\n    --[no-]atomic         use atomic transaction to update references\n    --[no-]upload-pack <path>\n                          path to upload pack on remote end\n    -f, --[no-]force      force overwrite of local reference\n    -m, --[no-]multiple   fetch from multiple remotes\n    -t, --[no-]tags       fetch all tags and associated objects\n    -n                    do not fetch all tags (--no-tags)\n    -j, --[no-]jobs <n>   number of submodules fetched in parallel\n    --[no-]prefetch       modify the refspec to place all refs within refs/prefetch/\n    -p, --[no-]prune      prune remote-tracking branches no longer on remote\n    -P, --[no-]prune-tags prune local tags no longer on remote and clobber changed tags\n    --[no-]recurse-submodules[=<on-demand>]\n                          control recursive fetching of submodules\n    --[no-]dry-run        dry run\n    --[no-]porcelain      machine-readable output\n    --[no-]write-fetch-head\n                          write fetched references to the FETCH_HEAD file\n    -k, --[no-]keep       keep downloaded pack\n    -u, --[no-]update-head-ok\n                          allow updating of HEAD ref\n    --[no-]progress       force progress reporting\n    --[no-]depth <depth>  deepen history of shallow clone\n    --[no-]shallow-since <time>\n                          deepen history of shallow repository based on time\n    --[no-]shallow-exclude <ref>\n                          deepen history of shallow clone, excluding ref\n    --[no-]deepen <n>     deepen history of shallow clone\n    --unshallow           convert to a complete repository\n    --refetch             re-fetch without negotiating common commits\n    --[no-]update-shallow accept refs that update .git/shallow\n    --refmap <refmap>     specify fetch refmap\n    -o, --[no-]server-option <server-specific>\n                          option to transmit\n    -4, --ipv4            use IPv4 addresses only\n    -6, --ipv6            use IPv6 addresses only\n    --[no-]negotiation-restrict <revision>\n                          report that we have only objects reachable from this object\n    --[no-]negotiation-tip <revision>\n                          alias of --negotiation-restrict\n    --[no-]negotiation-include <revision>\n                          ensure this ref is always sent as a negotiation have\n    --[no-]negotiate-only do not fetch a packfile; instead, print ancestors of negotiation tips\n    --[no-]filter <args>  object filtering\n    --[no-]auto-maintenance\n                          run 'maintenance --auto' after fetching\n    --[no-]auto-gc        run 'maintenance --auto' after fetching\n    --[no-]show-forced-updates\n                          check for forced-updates on all updated branches\n    --[no-]write-commit-graph\n                          write the commit-graph after fetching\n    --[no-]stdin          accept refspecs from stdin\n\n";

/// `usage_with_options_internal()`'s `USAGE_FULL` rendering — what `--help-all`
/// prints. It is [`USAGE`] with the `PARSE_OPT_HIDDEN` entries left in:
/// `--[no-]submodule-prefix`, `--[no-]recurse-submodules-default`.
/// Captured byte-for-byte from stock git 2.55.0's `git fetch --help-all`.
pub(super) const USAGE_ALL: &str = r#"usage: git fetch [<options>] [<repository> [<refspec>...]]
   or: git fetch [<options>] <group>
   or: git fetch --multiple [<options>] [(<repository>|<group>)...]
   or: git fetch --all [<options>]

    -v, --[no-]verbose    be more verbose
    -q, --[no-]quiet      be more quiet
    --[no-]all            fetch from all remotes
    --[no-]set-upstream   set upstream for git pull/fetch
    -a, --[no-]append     append to .git/FETCH_HEAD instead of overwriting
    --[no-]atomic         use atomic transaction to update references
    --[no-]upload-pack <path>
                          path to upload pack on remote end
    -f, --[no-]force      force overwrite of local reference
    -m, --[no-]multiple   fetch from multiple remotes
    -t, --[no-]tags       fetch all tags and associated objects
    -n                    do not fetch all tags (--no-tags)
    -j, --[no-]jobs <n>   number of submodules fetched in parallel
    --[no-]prefetch       modify the refspec to place all refs within refs/prefetch/
    -p, --[no-]prune      prune remote-tracking branches no longer on remote
    -P, --[no-]prune-tags prune local tags no longer on remote and clobber changed tags
    --[no-]recurse-submodules[=<on-demand>]
                          control recursive fetching of submodules
    --[no-]dry-run        dry run
    --[no-]porcelain      machine-readable output
    --[no-]write-fetch-head
                          write fetched references to the FETCH_HEAD file
    -k, --[no-]keep       keep downloaded pack
    -u, --[no-]update-head-ok
                          allow updating of HEAD ref
    --[no-]progress       force progress reporting
    --[no-]depth <depth>  deepen history of shallow clone
    --[no-]shallow-since <time>
                          deepen history of shallow repository based on time
    --[no-]shallow-exclude <ref>
                          deepen history of shallow clone, excluding ref
    --[no-]deepen <n>     deepen history of shallow clone
    --unshallow           convert to a complete repository
    --refetch             re-fetch without negotiating common commits
    --[no-]submodule-prefix <dir>
                          prepend this to submodule path output
    --[no-]recurse-submodules-default <on-demand>
                          default for recursive fetching of submodules (lower priority than config files)
    --[no-]update-shallow accept refs that update .git/shallow
    --refmap <refmap>     specify fetch refmap
    -o, --[no-]server-option <server-specific>
                          option to transmit
    -4, --ipv4            use IPv4 addresses only
    -6, --ipv6            use IPv6 addresses only
    --[no-]negotiation-restrict <revision>
                          report that we have only objects reachable from this object
    --[no-]negotiation-tip <revision>
                          alias of --negotiation-restrict
    --[no-]negotiation-include <revision>
                          ensure this ref is always sent as a negotiation have
    --[no-]negotiate-only do not fetch a packfile; instead, print ancestors of negotiation tips
    --[no-]filter <args>  object filtering
    --[no-]auto-maintenance
                          run 'maintenance --auto' after fetching
    --[no-]auto-gc        run 'maintenance --auto' after fetching
    --[no-]show-forced-updates
                          check for forced-updates on all updated branches
    --[no-]write-commit-graph
                          write the commit-graph after fetching
    --[no-]stdin          accept refspecs from stdin

"#;

/// `cmd_fetch()`'s `struct option builtin_fetch_options[]` (builtin/fetch.c), in
/// table order, as [`super::resolve_long_aliased`] reads it.
///
/// `--unshallow`, `--refetch`, `--refmap` and `-4`/`-6` carry `PARSE_OPT_NONEG`,
/// so none of those has a `--no-` spelling.
pub(super) const LONG_OPTS: &[super::LongOpt] = &[
    super::LongOpt { name: "verbose",                     neg: true,  arg: super::Arg::None },
    super::LongOpt { name: "quiet",                       neg: true,  arg: super::Arg::None },
    super::LongOpt { name: "all",                         neg: true,  arg: super::Arg::None },
    super::LongOpt { name: "set-upstream",                neg: true,  arg: super::Arg::None },
    super::LongOpt { name: "append",                      neg: true,  arg: super::Arg::None },
    super::LongOpt { name: "atomic",                      neg: true,  arg: super::Arg::None },
    super::LongOpt { name: "upload-pack",                 neg: true,  arg: super::Arg::Required },
    super::LongOpt { name: "force",                       neg: true,  arg: super::Arg::None },
    super::LongOpt { name: "multiple",                    neg: true,  arg: super::Arg::None },
    super::LongOpt { name: "tags",                        neg: true,  arg: super::Arg::None },
    super::LongOpt { name: "jobs",                        neg: true,  arg: super::Arg::Required },
    super::LongOpt { name: "prefetch",                    neg: true,  arg: super::Arg::None },
    super::LongOpt { name: "prune",                       neg: true,  arg: super::Arg::None },
    super::LongOpt { name: "prune-tags",                  neg: true,  arg: super::Arg::None },
    super::LongOpt { name: "recurse-submodules",          neg: true,  arg: super::Arg::Optional },
    super::LongOpt { name: "dry-run",                     neg: true,  arg: super::Arg::None },
    super::LongOpt { name: "porcelain",                   neg: true,  arg: super::Arg::None },
    super::LongOpt { name: "write-fetch-head",            neg: true,  arg: super::Arg::None },
    super::LongOpt { name: "keep",                        neg: true,  arg: super::Arg::None },
    super::LongOpt { name: "update-head-ok",              neg: true,  arg: super::Arg::None },
    super::LongOpt { name: "progress",                    neg: true,  arg: super::Arg::None },
    super::LongOpt { name: "depth",                       neg: true,  arg: super::Arg::Required },
    super::LongOpt { name: "shallow-since",               neg: true,  arg: super::Arg::Required },
    super::LongOpt { name: "shallow-exclude",             neg: true,  arg: super::Arg::Required },
    super::LongOpt { name: "deepen",                      neg: true,  arg: super::Arg::Required },
    super::LongOpt { name: "unshallow",                   neg: false, arg: super::Arg::None },
    super::LongOpt { name: "refetch",                     neg: false, arg: super::Arg::None },
    super::LongOpt { name: "submodule-prefix",            neg: true,  arg: super::Arg::Required },
    super::LongOpt { name: "recurse-submodules-default",  neg: true,  arg: super::Arg::Required },
    super::LongOpt { name: "update-shallow",              neg: true,  arg: super::Arg::None },
    super::LongOpt { name: "refmap",                      neg: false, arg: super::Arg::Required },
    super::LongOpt { name: "server-option",               neg: true,  arg: super::Arg::Required },
    super::LongOpt { name: "ipv4",                        neg: false, arg: super::Arg::None },
    super::LongOpt { name: "ipv6",                        neg: false, arg: super::Arg::None },
    super::LongOpt { name: "negotiation-restrict",        neg: true,  arg: super::Arg::Required },
    // `OPT_ALIAS(0, "negotiation-tip", "negotiation-restrict")`: `preprocess_options()`
    // copies the source entry over the alias, keeping only the alias's own name.
    super::LongOpt { name: "negotiation-tip",            neg: true,  arg: super::Arg::Required },
    super::LongOpt { name: "negotiation-include",         neg: true,  arg: super::Arg::Required },
    super::LongOpt { name: "negotiate-only",              neg: true,  arg: super::Arg::None },
    super::LongOpt { name: "filter",                      neg: true,  arg: super::Arg::Required },
    super::LongOpt { name: "auto-maintenance",            neg: true,  arg: super::Arg::None },
    super::LongOpt { name: "auto-gc",                     neg: true,  arg: super::Arg::None },
    super::LongOpt { name: "show-forced-updates",         neg: true,  arg: super::Arg::None },
    super::LongOpt { name: "write-commit-graph",          neg: true,  arg: super::Arg::None },
    super::LongOpt { name: "stdin",                       neg: true,  arg: super::Arg::None },
];

/// The one `OPT_ALIAS()` group in `builtin_fetch_options[]`. `is_alias()`
/// (parse-options.c:471) reads it so `--negotiation-` does not report itself as
/// ambiguous between an alias and the option it aliases.
const ALIAS_GROUPS: &[&[&str]] = &[&["negotiation-tip", "negotiation-restrict"]];

pub fn fetch(args: &[String]) -> Result<ExitCode> {
    if args.iter().any(|a| a == "-h") {
        print!("{USAGE}");
        return Ok(ExitCode::from(129));
    }
    // `--help-all` renders `USAGE_FULL` — `USAGE` plus the hidden
    // `--submodule-prefix` and `--recurse-submodules-default`.
    // `parse_options_step()` *breaks* on `--` and `--end-of-options` one line
    // before it tests the name (parse-options.c:1112-1122), so the search stops
    // at the terminator; and because the test is a `strcmp`, no abbreviation
    // and no `=<value>` spelling reaches it.
    if args
        .iter()
        .take_while(|a| a.as_str() != "--" && a.as_str() != "--end-of-options")
        .any(|a| a == "--help-all")
    {
        print!("{USAGE_ALL}");
        return Ok(ExitCode::from(129));
    }
    let mut repo = gix::discover(".")?;

    // Remote-tracking ref updates write reflogs; without a configured identity, seed
    // a synthesized system default so the reflog write can't fail (git does the same).
    crate::ensure_reflog_identity(&mut repo);

    // --- argument parsing -------------------------------------------------
    let mut opts = FetchOpts::default();
    // `cmd_fetch()` builds the reflog action from the command line itself — `fetch`
    // followed by every argument — unless `GIT_REFLOG_ACTION` already named one.
    opts.reflog_action = std::env::var("GIT_REFLOG_ACTION").unwrap_or_else(|_| {
        let mut msg = String::from("fetch");
        for a in args {
            msg.push(' ');
            msg.push_str(a);
        }
        msg
    });
    // Tri-state so `fetch.all` can supply the default: `Some(true/false)` is an
    // explicit `--all`/`--no-all`, `None` defers to config (git precedence:
    // CLI > config > built-in default).
    let mut all_flag: Option<bool> = None;
    let mut multiple = false;
    let mut positionals: Vec<&str> = Vec::new();

    // Shallow-boundary selectors that combine (git's `--shallow-exclude` is a
    // repeatable OPT_STRING_LIST → `deepen_not`, `--shallow-since` → `deepen_since`,
    // and the two may be given together). Accumulated here and resolved into a
    // single `Shallow` value after parsing.
    let mut shallow_exclude: Vec<gix::refs::PartialName> = Vec::new();
    let mut shallow_since: Option<gix::date::Time> = None;

    // `--stdin`: git appends the refspecs read from standard input to the ones
    // named on the command line, so the read is deferred until parsing is done.
    let mut read_stdin = false;
    // Tri-states resolved against config after parsing.
    let mut show_forced_updates: Option<bool> = None;
    let mut write_commit_graph: Option<bool> = None;
    let mut recurse_submodules: Option<Recurse> = None;
    let mut jobs: Option<usize> = None;
    // `--refmap` (repeatable). Kept as raw strings because an empty one is legal and doesn't parse.
    let mut refmap: Vec<String> = Vec::new();

    let mut i = 0;
    while i < args.len() {
        let typed = args[i].as_str();
        i += 1;

        // Respell a unique abbreviation as the name it resolves to, so `--negotiate-o`
        // reaches the same arm as `--negotiate-only`. The aliased form is needed
        // because `builtin_fetch_options[]` has an `OPT_ALIAS()`: without the group,
        // `--negotiation-` would report itself ambiguous between the alias and its
        // source.
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
        let (key, inline_val) = match (a.starts_with("--"), a.split_once('=')) {
            (true, Some((k, v))) => (k, Some(v.to_string())),
            _ => (a, None),
        };

        // Fetch the value for a value-taking option (inline `=v` or next arg).
        // Kept as a plain expression (not a closure) so the `i` cursor stays
        // freely borrowable in the other match arms.
        macro_rules! take_value {
            ($name:literal) => {
                match inline_val.clone() {
                    Some(v) => v,
                    None => match args.get(i).cloned() {
                        Some(v) => {
                            i += 1;
                            v
                        }
                        // `get_arg()`: named as typed, one line, exit 129.
                        None => return Ok(super::missing_option_value(key)),
                    },
                }
            };
        }

        match key {
            "-v" | "--verbose" => opts.verbose = true,
            "-q" | "--quiet" => opts.quiet = true,
            "--dry-run" => opts.dry_run = true,
            "--all" => all_flag = Some(true),
            "-m" | "--multiple" => multiple = true,
            "-t" | "--tags" => opts.tags = Some(Tags::All),
            // git: `-n` is the short form of `--no-tags`, not `--dry-run`.
            "-n" | "--no-tags" => opts.tags = Some(Tags::None),
            "-p" | "--prune" => {
                opts.prune = Some(true);
                opts.prune_from_cli = true;
            }
            "-P" | "--prune-tags" => {
                opts.prune_tags = Some(true);
                opts.prune_tags_from_cli = true;
            }
            "-f" | "--force" => opts.force = true,
            // Negations git's parse-options accepts for the `--[no-]…` booleans:
            // resetting each flag to its default (git clears the corresponding bit).
            "--no-verbose" => opts.verbose = false,
            "--no-quiet" => opts.quiet = false,
            "--no-dry-run" => opts.dry_run = false,
            "--no-all" => all_flag = Some(false),
            "--no-multiple" => multiple = false,
            "--no-prune" => {
                opts.prune = Some(false);
                opts.prune_from_cli = true;
            }
            "--no-prune-tags" => {
                opts.prune_tags = Some(false);
                opts.prune_tags_from_cli = true;
            }
            "--no-force" => opts.force = false,
            "--unshallow" => opts.shallow = Some(Shallow::undo()),

            // Accept the new shallow roots a shallow remote asks us to adopt instead of
            // rejecting the refs that need them - git's `--update-shallow`.
            "--update-shallow" => opts.update_shallow = true,
            "--no-update-shallow" => opts.update_shallow = false,

            // Machine-readable output: the per-ref rows go to stdout as
            // `<flag> <old-object-id> <new-object-id> <local-reference>` and the
            // `From <url>` header is not printed.
            "--porcelain" => opts.porcelain = true,
            "--no-porcelain" => opts.porcelain = false,

            // FETCH_HEAD control. `--write-fetch-head` is git's default; `-a`
            // appends to the existing file instead of truncating it.
            "--write-fetch-head" => opts.write_fetch_head = true,
            "--no-write-fetch-head" => opts.write_fetch_head = false,
            "-a" | "--append" => opts.append = true,
            "--no-append" => opts.append = false,

            // Progress meter: forced on, forced off, or (unset) shown when stderr
            // is a terminal, exactly as git decides it.
            "--progress" => opts.progress = Some(true),
            "--no-progress" => opts.progress = Some(false),

            // Whether to annotate non-fast-forward updates with `(forced update)`.
            "--show-forced-updates" => show_forced_updates = Some(true),
            "--no-show-forced-updates" => show_forced_updates = Some(false),

            // Place every fetched ref under `refs/prefetch/` instead of its
            // configured destination (git's `filter_prefetch_refspec`).
            "--prefetch" => opts.prefetch = true,
            "--no-prefetch" => opts.prefetch = false,

            // Additional refspecs from standard input, appended to the ones on
            // the command line.
            "--stdin" => read_stdin = true,
            "--no-stdin" => read_stdin = false,

            // Permit updating the ref `HEAD` resolves to in a worktree, which is
            // otherwise refused to keep the index and worktree consistent.
            "-u" | "--update-head-ok" => opts.update_head_ok = true,
            "--no-update-head-ok" => opts.update_head_ok = false,

            // `-k`/`--keep` asks for the received pack to be kept rather than
            // exploded into loose objects. This build never runs the equivalent
            // of `unpack-objects` — gitoxide always writes the pack and its index
            // into `objects/pack` — so the flag names the behaviour that is
            // already in force. `--no-keep` would have to explode the pack, which
            // has no implementation here, so it is refused instead of ignored.
            "-k" | "--keep" => {}
            "--no-keep" => anyhow::bail!(
                "unsupported option \"--no-keep\" (the received pack is always kept; \
                 there is no unpack-objects path)"
            ),

            // Post-fetch commit-graph write (git's `--write-commit-graph`).
            "--write-commit-graph" => write_commit_graph = Some(true),
            "--no-write-commit-graph" => write_commit_graph = Some(false),

            // Submodule recursion and its parallelism.
            "--recurse-submodules" => {
                recurse_submodules = Some(match inline_val.as_deref() {
                    None | Some("yes") | Some("true") => Recurse::Yes,
                    Some("no") | Some("false") => Recurse::No,
                    Some("on-demand") => anyhow::bail!(
                        "unsupported option \"--recurse-submodules=on-demand\" (it needs the \
                         superproject's old/new submodule gitlinks to decide what to fetch)"
                    ),
                    Some(other) => {
                        crate::git_fatal!("--recurse-submodules expects yes/on-demand/no, got {other:?}")
                    }
                });
            }
            "--no-recurse-submodules" => recurse_submodules = Some(Recurse::No),
            "-j" | "--jobs" => {
                let v = take_value!("--jobs");
                // `OPT_INTEGER`, through the shared parse-options grammar so the
                // unit suffixes its own rejection advertises actually work.
                let n = match crate::optint::integer(&crate::optint::long_opt("jobs"), &v) {
                    Ok(n) => n,
                    Err(e) => {
                        eprintln!("error: {e}");
                        return Ok(ExitCode::from(129));
                    }
                };
                // `0` is git's "pick a reasonable number", resolved below.
                jobs = Some(n.max(0) as usize);
            }

            "--depth" => {
                let v = take_value!("--depth");
                // `--depth` is an `OPT_STRING` in git; the number is checked by
                // `cmd_fetch()` itself, which is why a bad one is a `fatal:` and
                // not parse-options' 129.
                let Some(n) = v.parse::<u32>().ok().and_then(NonZeroU32::new) else {
                    eprintln!("fatal: depth {v} is not a positive number");
                    return Ok(ExitCode::from(128));
                };
                opts.shallow = Some(Shallow::DepthAtRemote(n));
            }
            "--deepen" => {
                let v = take_value!("--deepen");
                // `OPT_INTEGER`, so a bad value is parse-options' rejection.
                let n = match crate::optint::integer(&crate::optint::long_opt("deepen"), &v) {
                    Ok(n) => n,
                    Err(e) => {
                        eprintln!("error: {e}");
                        return Ok(ExitCode::from(129));
                    }
                };
                opts.shallow = Some(Shallow::Deepen(n.max(0) as u32));
            }
            // Shallow boundary at a cutoff date (git's `deepen_since`). `fetch-pack.c:439` runs
            // the value through `approxidate()`, which never fails — an unreadable date is the
            // current time, not an error.
            "--shallow-since" => {
                let v = take_value!("--shallow-since");
                shallow_since = Some(gix::date::Time::new(crate::date::approxidate(&v), 0));
            }
            // Exclude history reachable from a ref (git's repeatable `deepen_not`).
            "--shallow-exclude" => {
                let v = take_value!("--shallow-exclude");
                let name = gix::refs::PartialName::try_from(v.as_str())
                    .map_err(|_| anyhow::anyhow!("--shallow-exclude expects a valid ref, got {v:?}"))?;
                shallow_exclude.push(name);
            }
            // The program to run instead of `git-upload-pack` on the other end. git passes it verbatim to
            // whatever spawns the service, so it can be a path or (over ssh) a whole command line.
            "--upload-pack" => opts.upload_pack = Some(take_value!("--upload-pack")),

            // Protocol-v2 server options, repeatable, transmitted as `server-option=<value>` lines.
            "-o" | "--server-option" => opts.server_options.push(take_value!("--server-option").into()),

            // git's `parse_refmap_arg`: repeatable, no negation, and an empty value is the documented way to
            // say "don't store anywhere" — it appends a refspec that matches nothing rather than clearing the
            // list, which still counts as "a refmap was given".
            "--refmap" => {
                let v = take_value!("--refmap");
                refmap.push(v);
            }

            // git's `mark_tips()`: with any of these given, only the named commits seed the `have`
            // walk. `--negotiation-tip` is the older spelling of `--negotiation-restrict` and lands
            // in the same list.
            "--negotiation-restrict" | "--negotiation-tip" => {
                let v = take_value!("--negotiation-restrict");
                opts.negotiation_restrict.get_or_insert_default().push(v);
            }
            // `--negotiation-include`: sent as `have` whatever the algorithm decides.
            "--negotiation-include" => {
                let v = take_value!("--negotiation-include");
                opts.negotiation_include.get_or_insert_default().push(v);
            }
            "--negotiate-only" => opts.negotiate_only = true,

            // All-or-nothing ref updates.
            "--atomic" => opts.atomic = true,
            "--no-atomic" => opts.atomic = false,

            // git's `OPT_SET_INT` pair on one `family` slot: the last of `-4`/`-6` wins, and neither
            // has a `--no-` form.
            "-4" | "--ipv4" => {
                opts.address_family = Some(gix::protocol::transport::AddressFamily::V4);
            }
            "-6" | "--ipv6" => {
                opts.address_family = Some(gix::protocol::transport::AddressFamily::V6);
            }

            // Ask for everything, negotiating nothing.
            "--refetch" => opts.refetch = true,
            // `--refetch` is `OPT_SET_INT_F(... PARSE_OPT_NONEG)`
            // (builtin/fetch.c), so `--no-refetch` is not a spelling parse-options
            // resolves; it falls through to the `unknown option` refusal below.

            // The `git maintenance run --auto` git runs on its way out. Two spellings of one flag,
            // enabled by default.
            "--auto-maintenance" | "--auto-gc" => opts.auto_maintenance = true,
            "--no-auto-maintenance" | "--no-auto-gc" => opts.auto_maintenance = false,

            // Options requiring substrate the high-level fetch does not expose.
            "--filter" => {
                let _ = take_value!("--filter");
                anyhow::bail!("--filter (partial clone) is not supported");
            }
            "--set-upstream" => {
                anyhow::bail!("--set-upstream is not supported");
            }
            "--" => {
                positionals.extend(args[i..].iter().map(String::as_str));
                break;
            }
            // A long name no table entry claims is `parse_options()`' own refusal —
            // the `error:` line and the block, both on stderr, exit 129 — not a gap
            // in this port. It has to be decided against the table rather than by
            // spelling, because `--unshallow`, `--refetch`, `--refmap`, `-4`/`--ipv4`
            // and `-6`/`--ipv6` carry `PARSE_OPT_NONEG` and so have no `--no-` form
            // for parse-options to resolve.
            s if s.starts_with("--")
                && matches!(
                    super::resolve_long(LONG_OPTS, &s[2..]),
                    super::Resolved::Unknown
                ) =>
            {
                // `error(_("unknown option `%s'"), ctx.argv[0] + 2)`
                // (parse-options.c:1215-1216) echoes the argument as typed,
                // `=<value>` included; `s` here has already been split at the
                // `=`, so the message goes back to the token.
                return Ok(super::unknown_option(typed, USAGE));
            }
            s if s.starts_with('-') && s.len() > 1 => anyhow::bail!("unsupported option {s:?}"),
            // A non-option argument is handed back unchanged by the resolver, so the
            // argv slice itself is pushed and the operand keeps `args`' lifetime.
            _ => positionals.push(typed),
        }
    }

    // `--stdin` refspecs are appended after everything named on the command line,
    // as git's `add_refspec` on the stdin lines does.
    let stdin_specs: Vec<String> = if read_stdin {
        let mut buf = String::new();
        std::io::stdin().read_to_string(&mut buf)?;
        buf.lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .map(str::to_string)
            .collect()
    } else {
        Vec::new()
    };

    // Resolve the accumulated shallow-boundary selectors. `--shallow-exclude`
    // (repeatable) may be combined with `--shallow-since`, mirroring git's
    // `deepen_not` + `deepen_since`; a lone `--shallow-since` sets only the cutoff.
    // Either form supersedes an earlier `--depth`/`--deepen`/`--unshallow`, as git
    // treats the shallow selectors as one group.
    if !shallow_exclude.is_empty() {
        opts.shallow = Some(Shallow::Exclude {
            remote_refs: shallow_exclude,
            since_cutoff: shallow_since,
        });
    } else if let Some(cutoff) = shallow_since {
        opts.shallow = Some(Shallow::Since { cutoff });
    }

    // --- config-supplied defaults -----------------------------------------
    // git resolves each of these with CLI > config > built-in default (see
    // builtin/fetch.c `cmd_fetch`): a bare `git fetch` behaves as if the
    // corresponding flag were given when the config is set, but an explicit
    // flag always wins. `-c`/`--config` overrides land here via gix's snapshot
    // (they are injected as `GIT_CONFIG_*` before the repo is opened).
    //   * `fetch.prune`     → default for `--prune`
    //   * `fetch.pruneTags` → default for `--prune-tags`
    //   * `fetch.all`       → default for `--all` (only with no explicit remote,
    //                          matching git: a positional remote suppresses it)
    let recurse;
    {
        let snap = repo.config_snapshot();
        if opts.prune.is_none() {
            opts.prune = snap.boolean("fetch.prune");
        }
        if opts.prune_tags.is_none() {
            opts.prune_tags = snap.boolean("fetch.pruneTags");
        }
        if all_flag.is_none()
            && positionals.is_empty()
            && snap.boolean("fetch.all") == Some(true)
        {
            all_flag = Some(true);
        }
        // git's `fetch.showForcedUpdates` defaults to true.
        opts.show_forced_updates = show_forced_updates
            .or_else(|| snap.boolean("fetch.showForcedUpdates"))
            .unwrap_or(true);
        opts.write_commit_graph = write_commit_graph
            .or_else(|| snap.boolean("fetch.writeCommitGraph"))
            .unwrap_or(false);
        // `fetch.output` selects between the default `full` layout and `compact`,
        // which folds a `<from>`/`<to>` pair that contains the other into a `*`.
        opts.compact = snap
            .string("fetch.output")
            .is_some_and(|v| v == "compact");
        // `fetch.recurseSubmodules` supplies the default; `on-demand` (git's own
        // default) is not implementable here and is treated as "off" rather than
        // guessed at, which is what a bare `git fetch` does in this build today.
        recurse = match recurse_submodules {
            Some(r) => r,
            None => match snap
                .string("fetch.recurseSubmodules")
                .map(|v| v.to_string())
                .as_deref()
            {
                Some("yes" | "true" | "on" | "1") => Recurse::Yes,
                _ => Recurse::No,
            },
        };
        // `fetch.parallel` is git's default for `-j`, and is itself 1 when unset;
        // an explicit `0` on either means "pick a reasonable number", which here
        // is the machine's available parallelism.
        let parallel = jobs.or_else(|| {
            snap.integer("fetch.parallel")
                .and_then(|n: i64| usize::try_from(n).ok())
        });
        opts.jobs = match parallel {
            Some(0) => std::thread::available_parallelism().map_or(1, |n| n.get()),
            Some(n) => n,
            None => 1,
        };
    }

    // `fetch.negotiationAlgorithm` selects the negotiator gitoxide builds in
    // `receive_pack` (`gix/src/remote/connection/fetch/receive_pack.rs:140`),
    // which honors `noop`, `skipping` and `consecutive`/`default`. gitoxide
    // treats an unrecognized value leniently and silently falls back to
    // `consecutive`; git validates it up front in `prepare_repo_settings()`
    // (`repo-settings.c`) and dies, so validate it here to the same effect —
    // matching git's case-insensitive comparison, which gitoxide's own parser
    // does not do.
    if let Some(algo) = repo
        .config_snapshot()
        .string("fetch.negotiationAlgorithm")
        .map(|v| v.to_string())
    {
        if !["skipping", "noop", "consecutive", "default"]
            .iter()
            .any(|k| algo.eq_ignore_ascii_case(k))
        {
            eprintln!("fatal: unknown fetch negotiation algorithm '{algo}'");
            return Ok(ExitCode::from(128));
        }
    }

    // `fetch.bundleURI`: a bundle provider to bootstrap from before talking to
    // the remote. git reads it here, in `cmd_fetch`, and only warns when the
    // download fails — "the remote Git server is the ultimate source of truth,
    // not the bundle URI" (Documentation/technical/bundle-uri.adoc).
    //
    // `git clone --bundle-uri` writes this key when the list it fetched
    // advertised `bundle.heuristic`, which is what makes the incremental case
    // cheap: `fetch_bundle_uri` then reads `fetch.bundleCreationToken` and skips
    // every bundle the repository already has.
    let bundle_uri = repo
        .config_snapshot()
        .string("fetch.bundleURI")
        .map(|v| v.to_string());
    if let Some(uri) = bundle_uri.as_deref() {
        let (failed, _has_heuristic) = super::bundle::uri::fetch_bundle_uri(&repo, uri);
        if failed {
            eprintln!("warning: failed to fetch bundles from '{uri}'");
        }
        // The bundles landed as new packs and new `refs/bundles/*` tips. This
        // handle's object database was opened before they existed, so the
        // negotiation below would look up a `refs/bundles/*` tip and not find
        // it; re-open so the fetch sees exactly what is on disk.
        repo = gix::discover(".")?;
    }

    let all = all_flag.unwrap_or(false);

    // Every refspec git accepts on the command line, from `--stdin` or via `--refmap` goes through
    // `refspec_append()`, which dies on a malformed one before anything is fetched.
    // Under `--all`/`--multiple` every positional is a remote name, so there are no refspecs to expand
    // or check. Otherwise `tag <name>` is git's shorthand for `refs/tags/<name>:refs/tags/<name>`.
    let mut positional_specs: Vec<String> = Vec::new();
    if !all && !multiple {
        let mut rest = positionals.iter().skip(1);
        while let Some(arg) = rest.next() {
            if *arg == "tag" {
                let name = rest
                    .next()
                    .ok_or_else(|| anyhow::anyhow!("you need to specify a tag name"))?;
                positional_specs.push(format!("refs/tags/{name}:refs/tags/{name}"));
            } else {
                positional_specs.push((*arg).to_string());
            }
        }
    }
    for spec in positional_specs
        .iter()
        .map(String::as_str)
        .chain(stdin_specs.iter().map(String::as_str))
        .chain(refmap.iter().map(String::as_str))
        .filter(|s| !s.is_empty())
    {
        if !refspec_globs_agree(spec) {
            eprintln!("fatal: invalid refspec '{spec}'");
            return Ok(ExitCode::from(128));
        }
    }

    // `--refmap` is the second half of git's two-stage match, so it only means anything once the first stage
    // has command-line refspecs to select refs with. `--all`/`--multiple` read every positional as a remote,
    // leaving no refspecs at all.
    if !refmap.is_empty() {
        let has_refspecs = !positional_specs.is_empty() || (!all && !multiple && !stdin_specs.is_empty());
        if !has_refspecs {
            eprintln!("fatal: --refmap option is only meaningful with command-line refspec(s)");
            return Ok(ExitCode::from(128));
        }
        opts.refmap = Some(
            refmap
                .iter()
                // git's documented `--refmap=''`: it appends a refspec that matches nothing, which is how the
                // fetch is told to store nowhere while still counting as a refmap.
                .filter(|s| !s.is_empty())
                .map(|s| {
                    gix::refspec::parse(s.as_str().into(), gix::refspec::parse::Operation::Fetch)
                        .map(|s| s.to_owned())
                })
                .collect::<Result<_, _>>()?,
        );
    }

    // git validates `--negotiate-only` before it connects: without tips it would negotiate from every
    // local ref and print a set nobody asked for, and with submodule recursion there would be no fetch
    // for the submodules to recurse into.
    if opts.negotiate_only {
        // `cmd_fetch` resolves the remote before it looks at the tips, so a run with
        // no remote to negotiate with is refused for that reason first. `--all` holds
        // a remote only when it collected exactly one; otherwise, and with no
        // positional, the arm that runs is `remote_get(NULL)` — note that `argc == 0`
        // is tested ahead of `--multiple`, so a bare `--multiple` lands there too.
        let no_remote = if all {
            repo.remote_names().len() != 1
        } else {
            positionals.is_empty() && default_fetch_remote_missing(&repo)
        };
        if no_remote {
            eprintln!("fatal: must supply remote when using --negotiate-only");
            return Ok(ExitCode::from(128));
        }
        // The check is on the tips the transport ends up with, which `set_transport_options()` also
        // fills from `remote.<name>.negotiationRestrict`, so a configured remote can satisfy it alone.
        let configured_tips = {
            let name = repo
                .find_fetch_remote(positionals.first().map(|s| BStr::new(*s)))
                .ok()
                .and_then(|remote| remote.name().map(|n| n.as_bstr().to_string()));
            !negotiation_restrict_config(&repo, name.as_deref()).is_empty()
        };
        if opts.negotiation_restrict.is_none() && !configured_tips {
            eprintln!("fatal: --negotiate-only needs one or more --negotiation-restrict=*");
            return Ok(ExitCode::from(128));
        }
        if recurse == Recurse::Yes {
            eprintln!("fatal: options '--negotiate-only' and '--recurse-submodules' cannot be used together");
            return Ok(ExitCode::from(128));
        }
    }

    // Turning the forced-update check off makes the summary silently misreport
    // rewritten branches as fast-forwards, so git says so once per invocation —
    // before any fetching, and regardless of `-q` or of whether anything is
    // fetched at all.
    // `advice_enabled(ADVICE_FETCH_SHOW_FORCED_UPDATES)` wraps both halves of
    // this report in `store_updated_refs()` — the "check disabled" note here and
    // the "it took N seconds" one that is not ported.
    if !opts.show_forced_updates && crate::advice::Advice::FetchShowForcedUpdates.enabled_in(&repo) {
        eprintln!(
            "warning: fetch normally indicates which branches had a forced update,\n\
             but that check has been disabled; to re-enable, use '--show-forced-updates'\n\
             flag or run 'git config fetch.showForcedUpdates true'"
        );
    }

    // Serialize ref mutations through the repo coordinator, as the write
    // commands do; a no-op guard if no daemon is running.
    let _lock = crate::lock::RepoLock::acquire(repo.git_dir());

    // The upstream of the current branch decides which FETCH_HEAD row is the
    // merge candidate (git's `FETCH_HEAD_MERGE`) when the configured refspecs
    // are used; explicit command-line refspecs mark every row for merge instead.
    let head_name = repo.head_name()?;
    let upstream = head_name.as_ref().and_then(|h| {
        let short = h.shorten().to_string();
        let remote = repo
            .config_snapshot()
            .string(&format!("branch.{short}.remote"))
            .map(|v| v.to_string())?;
        let merge = repo
            .branch_remote_ref_name(h.as_ref(), gix::remote::Direction::Fetch)
            .and_then(Result::ok)?;
        Some((remote, merge.as_bstr().to_string()))
    });

    // The progress tree is always built; only the renderer is conditional, so
    // gitoxide's counters go nowhere when progress is suppressed (as under a
    // non-terminal stderr) and to the line renderer otherwise.
    let show_progress =
        opts.progress.unwrap_or_else(|| std::io::stderr().is_terminal()) && !opts.quiet;
    let root = prodash::tree::Root::new();
    let mut op = root.add_child("fetch");
    let render = show_progress.then(|| {
        let mut o = prodash::render::line::Options {
            throughput: true,
            ..Default::default()
        }
        .auto_configure(prodash::render::line::StreamKind::Stderr);
        // `--progress` forces the live display even when stderr is not a terminal,
        // matching git; auto_configure would otherwise disable it in that case.
        if opts.progress == Some(true) {
            o.output_is_terminal = true;
        }
        o.hide_cursor = false;
        // git colors progress only on a real terminal, so `--progress` into a
        // pipe stays plain even though the meter is forced on.
        o.colored = std::io::stderr().is_terminal() && std::env::var_os("NO_COLOR").is_none();
        prodash::render::line::render(std::io::stderr(), root.downgrade(), o)
    });

    // --- dispatch by mode -------------------------------------------------
    let mut failure = false;
    let mut fatal = false;
    let mut fetch_head = FetchHead {
        path: repo.git_dir().join("FETCH_HEAD"),
        enabled: opts.write_fetch_head && !opts.dry_run,
        truncate: !opts.append,
    };

    let result = (|| -> Result<()> {
        if all {
            if !positionals.is_empty() {
                crate::git_fatal!("fetch --all does not take a repository argument");
            }
            // git announces each remote on stdout while fanning out, but only on
            // the genuinely multi-remote path: `cmd_fetch` short-circuits
            // `--all` over a single remote into the ordinary one-remote fetch,
            // which prints nothing. `-q` silences the announcement either way.
            let names = repo.remote_names();
            let announce = names.len() > 1 && !opts.quiet;
            for name in names {
                let n = name.as_bstr();
                if announce {
                    println!("Fetching {n}");
                }
                match fetch_one(
                    &repo,
                    Some(n),
                    &stdin_specs.iter().map(String::as_str).collect::<Vec<_>>(),
                    &opts,
                    upstream.as_ref(),
                    &mut fetch_head,
                    &mut op,
                ) {
                    Ok(Verdict::Ok) => {}
                    Ok(Verdict::Rejected) => failure = true,
                    Ok(Verdict::Fatal) => {
                        fatal = true;
                        break;
                    }
                    Err(e) => {
                        eprintln!("error: could not fetch {n}: {e}");
                        failure = true;
                    }
                }
            }
        } else if multiple {
            // `--multiple` reads every positional as a remote *name* or group
            // (`add_remote_or_group()`), so a URL or path is refused before anything is
            // fetched — `git fetch --multiple .` never contacts `.` at all.
            for name in &positionals {
                let known = repo
                    .remote_names()
                    .iter()
                    .any(|n| n.as_bstr() == BStr::new(*name))
                    || repo
                        .config_snapshot()
                        .string(&format!("remotes.{name}"))
                        .is_some();
                if !known {
                    eprintln!("fatal: no such remote or remote group: {name}");
                    fatal = true;
                    break;
                }
            }
            if fatal {
                return Ok(());
            }
            for name in &positionals {
                if !opts.quiet {
                    println!("Fetching {name}");
                }
                match fetch_one(
                    &repo,
                    Some(BStr::new(*name)),
                    &stdin_specs.iter().map(String::as_str).collect::<Vec<_>>(),
                    &opts,
                    upstream.as_ref(),
                    &mut fetch_head,
                    &mut op,
                ) {
                    Ok(Verdict::Ok) => {}
                    Ok(Verdict::Rejected) => failure = true,
                    Ok(Verdict::Fatal) => {
                        fatal = true;
                        break;
                    }
                    Err(e) => {
                        eprintln!("error: could not fetch {name}: {e}");
                        failure = true;
                    }
                }
            }
        } else {
            let name = positionals.first().map(|s| BStr::new(*s));
            // `cmd_fetch`'s no-argument arm is `remote = remote_get(NULL)`, which
            // comes back NULL when the name it settles on — `branch.<current>.remote`,
            // else the sole configured remote, else `origin` — has no URL. git then
            // reaches neither `fetch_one()` nor an error but the `fetch_multiple()`
            // arm, over a list nothing was ever added to: the loop runs zero times
            // and the command succeeds without a word. Three options are refused
            // there first, because none of them means anything with no remote.
            if name.is_none() && default_fetch_remote_missing(&repo) {
                if opts.atomic {
                    crate::git_fatal!("--atomic can only be used when fetching from one remote");
                }
                if read_stdin {
                    crate::git_fatal!("--stdin can only be used when fetching from one remote");
                }
                return Ok(());
            }
            let mut refspecs: Vec<&str> = positional_specs.iter().map(String::as_str).collect();
            refspecs.extend(stdin_specs.iter().map(String::as_str));
            match fetch_one(
                &repo,
                name,
                &refspecs,
                &opts,
                upstream.as_ref(),
                &mut fetch_head,
                &mut op,
            )? {
                Verdict::Ok => {}
                Verdict::Rejected => failure = true,
                Verdict::Fatal => fatal = true,
            }
        }
        Ok(())
    })();

    if let Some(handle) = render {
        handle.shutdown_and_wait();
    }
    result?;

    // `transfer.credentialsInUrl=die` is git's `fatal:` exit, taken before any
    // post-fetch work runs.
    if fatal {
        return Ok(ExitCode::from(128));
    }

    // `--write-commit-graph` / `fetch.writeCommitGraph`: rebuild the commit-graph
    // over everything now reachable, which is what git does at the end of a
    // fetch. git writes it as an incremental split chain
    // (`objects/info/commit-graphs/`); the commit-graph port has no chain
    // protocol, so this is the single-file form at `objects/info/commit-graph`.
    if opts.write_commit_graph && !opts.dry_run {
        let code = super::commit_graph(&[
            "write".to_string(),
            "--reachable".to_string(),
            "--no-progress".to_string(),
        ])?;
        if code != ExitCode::SUCCESS {
            failure = true;
        }
    }

    // `--recurse-submodules[=yes]` / `fetch.recurseSubmodules=yes`: run the same
    // fetch inside every populated submodule, up to `--jobs` at a time.
    if recurse == Recurse::Yes && !opts.dry_run && fetch_submodules(&repo, &opts)? {
        failure = true;
    }

    // `--auto-maintenance`/`--auto-gc`, the last thing `cmd_fetch()` does. `run_auto_maintenance()`
    // decides for itself whether anything is due, from `maintenance.auto`/`gc.auto`.
    //
    // Deviation: under `--refetch` git first pushes `maintenance.incremental-repack.auto=-1` into the
    // child's config so the duplicate objects a refetch leaves behind are consolidated into one pack.
    // That mechanism is `GIT_CONFIG_PARAMETERS`, which nothing in this build reads, so the hint is
    // dropped and the child picks its tasks from the repository's own configuration.
    if opts.auto_maintenance && !opts.dry_run {
        super::maintenance::run_auto_maintenance(&repo, opts.quiet)?;
    }

    if failure {
        return Ok(ExitCode::FAILURE);
    }
    Ok(ExitCode::SUCCESS)
}

/// The source of the first refspec that had to match a remote ref and did not, as
/// `get_fetch_map()` reports it.
///
/// Only exact sources qualify: a pattern is allowed to match nothing, an object ID is not looked up
/// among the refs at all, and the opportunistic second-stage refspecs are the ones git passes
/// `missing_ok = 1`. An empty source is git's shorthand for `HEAD`.
fn missing_remote_ref(map: &gix::remote::fetch::RefMap) -> Option<String> {
    map.refspecs.iter().enumerate().find_map(|(index, spec)| {
        let src = match spec.to_ref().instruction() {
            gix::refspec::Instruction::Fetch(
                gix::refspec::instruction::Fetch::Only { src }
                | gix::refspec::instruction::Fetch::AndUpdate { src, .. },
            ) => src,
            _ => return None,
        };
        if src.find_byteset(b"*?[]\\").is_some() || gix::ObjectId::from_hex(src).is_ok() {
            return None;
        }
        let matched = map.mappings.iter().any(|mapping| {
            matches!(mapping.spec_index, gix::protocol::fetch::refmap::SpecIndex::ExplicitInRemote(i) if i == index)
        });
        (!matched).then(|| {
            if src.is_empty() {
                "HEAD".to_string()
            } else {
                src.to_string()
            }
        })
    })
}

/// What one remote's fetch produced, beyond the objects themselves.
#[derive(PartialEq, Eq)]
pub(super) enum Verdict {
    /// Everything the refspecs asked for was applied.
    Ok,
    /// At least one ref update was rejected; the command exits non-zero.
    Rejected,
    /// `transfer.credentialsInUrl=die` matched, which git reports as a `fatal:`
    /// and exit 128 before any network traffic.
    Fatal,
}

/// git's `transfer.credentialsInUrl`, applied before a connection is opened.
///
/// A fetch URL that carries a plaintext password is accepted silently under the
/// default `allow`, reported as `warning: URL '<url>' uses plaintext
/// credentials` under `warn`, and refused with the same sentence as a `fatal:`
/// under `die`. The password is replaced with `<redacted>` in the message, as
/// git's `transport_anonymize_url` does.
///
/// git emits the warning once per transport it constructs for the URL (three
/// times for a fetch, twice for `ls-remote`); this build reports it once.
pub(super) fn credentials_in_url(repo: &gix::Repository, url: Option<&gix::url::Url>) -> Verdict {
    let Some(url) = url.filter(|u| u.password().is_some()) else {
        return Verdict::Ok;
    };
    let policy = repo
        .config_snapshot()
        .string("transfer.credentialsInUrl")
        .map(|v| v.to_string());
    // gix percent-encodes whatever the password field holds, so the placeholder
    // is a plain token during serialization and swapped for git's literal
    // `<redacted>` afterwards.
    const TOKEN: &str = "zvcsRedactedPasswordPlaceholder";
    let mut redacted = url.clone();
    redacted.set_password(Some(TOKEN.into()));
    let redacted = redacted.to_bstring().to_string().replace(TOKEN, "<redacted>");
    match policy.as_deref() {
        Some("die") => {
            eprintln!("fatal: URL '{redacted}' uses plaintext credentials");
            Verdict::Fatal
        }
        Some("warn") => {
            eprintln!("warning: URL '{redacted}' uses plaintext credentials");
            Verdict::Ok
        }
        _ => Verdict::Ok,
    }
}

/// `--recurse-submodules`' tri-state, minus git's `on-demand` which needs the
/// superproject's old/new gitlinks to decide and is refused at parse time.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Recurse {
    Yes,
    No,
}

/// Parsed command-line options shared across every remote a single invocation
/// touches (`--all`/`--multiple` fan out but carry the same flags).
struct FetchOpts {
    /// `GIT_REFLOG_ACTION`, or the whole command line as git composes it: `fetch` plus
    /// every argument, which is the prefix each stored ref's reflog line carries.
    reflog_action: String,
    dry_run: bool,
    verbose: bool,
    quiet: bool,
    force: bool,
    // `None` = neither the flag nor the config set it (git's "unspecified");
    // resolved to a concrete value from `fetch.prune`/`fetch.pruneTags` before
    // dispatch, so `Some(true)` here means "prune" regardless of origin.
    prune: Option<bool>,
    prune_tags: Option<bool>,
    /// Whether `prune`/`prune_tags` above came from the command line rather than from
    /// `fetch.prune`/`fetch.pruneTags`.
    ///
    /// git keeps the distinction because `remote.<name>.prune`/`remote.<name>.pruneTags` sit
    /// *between* the two (`do_fetch()`: command line, then `remote->prune`, then
    /// `config->prune`), and the remote is only known once a fetch is under way.
    prune_from_cli: bool,
    prune_tags_from_cli: bool,
    tags: Option<Tags>,
    shallow: Option<Shallow>,
    /// `--update-shallow`: let a shallow remote add roots to `.git/shallow` rather than
    /// rejecting the refs that would need them.
    update_shallow: bool,
    /// `--porcelain`: rows go to stdout in the machine-readable layout.
    porcelain: bool,
    /// `--write-fetch-head` (git's default) / `--no-write-fetch-head`.
    write_fetch_head: bool,
    /// `-a`/`--append`: add to the existing FETCH_HEAD instead of truncating it.
    append: bool,
    /// `--progress` forced on / `--no-progress` forced off / unset = auto.
    progress: Option<bool>,
    /// Resolved `--show-forced-updates` / `fetch.showForcedUpdates`.
    show_forced_updates: bool,
    /// `--prefetch`: every destination moves under `refs/prefetch/`.
    prefetch: bool,
    /// `-u`/`--update-head-ok`.
    update_head_ok: bool,
    /// Resolved `--write-commit-graph` / `fetch.writeCommitGraph`.
    write_commit_graph: bool,
    /// `fetch.output=compact`.
    compact: bool,
    /// Resolved `-j`/`--jobs` / `fetch.parallel`, always at least 1.
    jobs: usize,
    /// `--upload-pack <path>`; `remote.<name>.uploadpack` supplies the per-remote default.
    upload_pack: Option<String>,
    /// `-o`/`--server-option`, repeatable; `remote.<name>.serverOption` supplies the default.
    server_options: Vec<BString>,
    /// The refspecs `--refmap` supplied, already parsed. `None` means no `--refmap` was given at all,
    /// which is what decides whether the configured refspecs act as the opportunistic ones.
    refmap: Option<Vec<gix::refspec::RefSpec>>,
    /// `--negotiation-restrict`/`--negotiation-tip`, still unresolved. `None` means the flag was never
    /// given, which is what tells the negotiator to start from every local ref instead.
    negotiation_restrict: Option<Vec<String>>,
    /// `--negotiation-include`, still unresolved. `None` defers to `remote.<name>.negotiationInclude`,
    /// which is per-remote and therefore only known once a remote has been picked.
    negotiation_include: Option<Vec<String>>,
    /// `--negotiate-only`: print the common commits the remote acknowledged and fetch nothing.
    negotiate_only: bool,
    /// `--atomic`: apply the ref updates as one transaction, or none of them.
    atomic: bool,
    /// `--refetch`: ask for everything, sending no `have` at all.
    refetch: bool,
    /// `--auto-maintenance`/`--auto-gc`, on by default: run `maintenance run --auto` on the way out.
    auto_maintenance: bool,
    /// `-4`/`--ipv4` and `-6`/`--ipv6`, git's `transport_family`. `None` is `TRANSPORT_FAMILY_ALL`.
    address_family: Option<gix::protocol::transport::AddressFamily>,
}

impl Default for FetchOpts {
    fn default() -> Self {
        FetchOpts {
            reflog_action: "fetch".to_string(),
            dry_run: false,
            verbose: false,
            quiet: false,
            force: false,
            prune: None,
            prune_tags: None,
            prune_from_cli: false,
            prune_tags_from_cli: false,
            tags: None,
            shallow: None,
            update_shallow: false,
            porcelain: false,
            // git writes FETCH_HEAD unless `--no-write-fetch-head` is given.
            write_fetch_head: true,
            append: false,
            progress: None,
            show_forced_updates: true,
            prefetch: false,
            update_head_ok: false,
            write_commit_graph: false,
            compact: false,
            jobs: 1,
            upload_pack: None,
            server_options: Vec::new(),
            refmap: None,
            negotiation_restrict: None,
            negotiation_include: None,
            negotiate_only: false,
            atomic: false,
            refetch: false,
            // "This is enabled by default."
            auto_maintenance: true,
            address_family: None,
        }
    }
}

/// One line of the git-style per-ref summary.
/// Move the row for the checked-out branch's own update to the front, which is
/// where git's ref map puts it. A detached HEAD has no such row and nothing moves.
fn hoist_current_branch(repo: &gix::Repository, lines: &mut [Line]) {
    let Some(head) = repo.head_name().ok().flatten() else {
        return;
    };
    let short = head.shorten().to_string();
    if let Some(pos) = lines.iter().position(|l| l.from == short) {
        lines[..=pos].rotate_right(1);
    }
}

struct Line {
    flag: char,
    summary: String,
    from: String,
    to: String,
    reason: &'static str,
    /// Value the ref held before the fetch, for `--porcelain`'s second column.
    old: gix::ObjectId,
    /// Value it holds afterwards, for `--porcelain`'s third column.
    new: gix::ObjectId,
    /// Full local ref name for `--porcelain`'s fourth column (`FETCH_HEAD` for
    /// the rows that have no tracking ref).
    full: String,
}

/// The `.git/FETCH_HEAD` sink for one `git fetch` invocation.
///
/// git opens the file once per command and appends for every remote after the
/// first, so `--all`/`--multiple` accumulate rather than overwrite; `-a` starts
/// in append mode from the outset. Under `--dry-run` or `--no-write-fetch-head`
/// nothing is opened at all.
struct FetchHead {
    path: std::path::PathBuf,
    enabled: bool,
    truncate: bool,
}

impl FetchHead {
    /// Write one remote's rows, merge candidates first, exactly as git's two
    /// passes over `store_updated_refs` do.
    fn write(&mut self, rows: &[(String, bool)]) -> Result<()> {
        if !self.enabled {
            return Ok(());
        }
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(self.truncate)
            .append(!self.truncate)
            .open(&self.path)?;
        self.truncate = false;
        for for_merge in [true, false] {
            for (note, _) in rows.iter().filter(|(_, m)| *m == for_merge) {
                writeln!(f, "{note}")?;
            }
        }
        Ok(())
    }
}

/// Prepend `+` (force) to a refspec string unless it is already forced or a
/// negative/exclude spec (`^`).
fn forced(spec: BString) -> BString {
    match spec.first() {
        Some(b'+') | Some(b'^') => spec,
        _ => {
            let mut out = BString::from("+");
            out.extend_from_slice(&spec);
            out
        }
    }
}

/// git's `filter_prefetch_refspec`: move every destination under
/// `refs/prefetch/`, drop the specs that target `refs/tags/` or have no
/// destination at all, and force the rest.
///
/// A destination that already starts with `refs/` keeps the remainder
/// (`refs/remotes/origin/*` → `refs/prefetch/remotes/origin/*`); anything else is
/// appended whole.
fn prefetch_spec(spec: &BStr) -> Option<BString> {
    let s = spec.to_str().ok()?;
    let s = s.strip_prefix('+').unwrap_or(s);
    if s.starts_with('^') {
        return Some(spec.to_owned());
    }
    let (src, dst) = s.split_once(':')?;
    if dst.is_empty() || dst.starts_with("refs/tags/") {
        return None;
    }
    let tail = dst.strip_prefix("refs/").unwrap_or(dst);
    Some(BString::from(format!("+{src}:refs/prefetch/{tail}")))
}

/// git's minimum width for the `<from>` column (`refcol_width` in
/// `builtin/fetch.c`, which starts at 10 and only grows).
const REFCOL_WIDTH: usize = 10;

/// The URL as `git fetch` shows it in the `From …` header and in every
/// FETCH_HEAD row: trailing slashes are dropped, and a trailing `.git` with it
/// (`store_updated_refs` computes `url_len` exactly this way).
fn display_url(url: &str) -> String {
    let trimmed = url.trim_end_matches('/');
    let trimmed = match trimmed.strip_suffix(".git") {
        // git requires more than four characters before the suffix, so a bare
        // `.git` (or `x.git`) keeps its name.
        Some(head) if head.len() > 1 => head,
        _ => trimmed,
    };
    strip_userinfo(trimmed)
}

/// Drop the `user@` / `user:password@` from a URL's authority, which is what
/// makes `git fetch` over ssh say `From github.com:owner/repo` rather than
/// `From git@github.com:owner/repo`.
///
/// Measured against stock git 2.55.0 rather than inferred, because the scp-like
/// spelling and the URL spelling delimit the authority differently:
///
/// ```text
/// git@github.com:owner/repo.git        -> github.com:owner/repo
/// ssh://git@github.com/owner/repo.git  -> ssh://github.com/owner/repo
/// https://github.com/owner/repo.git    -> https://github.com/owner/repo
/// ```
///
/// The scheme survives; only the userinfo goes. The authority ends at the first
/// `/` in the URL form and at the first `:` in the scp-like one, so the `@` is
/// only honoured before that — otherwise a path that happens to contain `@`
/// (`host:mail@archive`) would lose its leading component.
fn strip_userinfo(url: &str) -> String {
    let (scheme, rest) = match url.find("://") {
        Some(i) => url.split_at(i + 3),
        None => ("", url),
    };
    let authority_end = if scheme.is_empty() {
        rest.find(':').unwrap_or(rest.len())
    } else {
        rest.find('/').unwrap_or(rest.len())
    };
    match rest[..authority_end].rfind('@') {
        Some(at) => format!("{scheme}{}", &rest[at + 1..]),
        None => format!("{scheme}{rest}"),
    }
}

/// The number of hex characters `git fetch` abbreviates object ids to in its
/// summary, which also fixes the summary column width
/// (`TRANSPORT_SUMMARY_WIDTH` is `2 * DEFAULT_ABBREV + 3`).
///
/// `core.abbrev` overrides git's built-in 7; `auto` and out-of-range values fall
/// back to it. git additionally lengthens an abbreviation that would be
/// ambiguous in the local object database, which this port does not do.
fn abbrev_len(repo: &gix::Repository) -> usize {
    const FALLBACK: usize = 7;
    let max = repo.object_hash().len_in_hex();
    repo.config_snapshot()
        .string("core.abbrev")
        .map(|v| v.to_string())
        .and_then(|v| v.parse::<usize>().ok())
        .filter(|n| (4..=max).contains(n))
        .unwrap_or(FALLBACK)
}

/// git's compact `fetch.output`: when one of `<from>`/`<to>` contains the other
/// as a substring, the containing side shows `*` in place of it.
fn compact(from: &str, to: &str) -> (String, String) {
    if !from.is_empty() && to.contains(from) {
        (from.to_string(), to.replace(from, "*"))
    } else if !to.is_empty() && from.contains(to) {
        (from.replace(to, "*"), to.to_string())
    } else {
        (from.to_string(), to.to_string())
    }
}

/// The `<kind> '<what>' of <url>` tail of a FETCH_HEAD row, from the full remote
/// ref name (git's `store_updated_refs`).
fn fetch_head_note(id: gix::ObjectId, for_merge: bool, remote_ref: &str, url: &str) -> String {
    let (kind, what) = match remote_ref {
        "HEAD" => ("", ""),
        r if r.starts_with("refs/heads/") => ("branch", &r["refs/heads/".len()..]),
        r if r.starts_with("refs/tags/") => ("tag", &r["refs/tags/".len()..]),
        r if r.starts_with("refs/remotes/") => ("remote-tracking branch", &r["refs/remotes/".len()..]),
        r => ("", r),
    };
    let mut note = format!(
        "{}\t{}\t",
        id.to_hex(),
        if for_merge { "" } else { "not-for-merge" }
    );
    if !what.is_empty() {
        if !kind.is_empty() {
            note.push_str(kind);
            note.push(' ');
        }
        note.push_str(&format!("'{what}' of "));
    }
    note.push_str(url);
    note
}

/// git's glob rules for a *fetch* refspec (`parse_refspec()` in `refspec.c`).
///
/// A `*` on one side must be matched by a `*` on the other, and a pattern source with no destination at all
/// is refused — `refs/heads/*` alone would name a set of refs with nowhere to put them. Negative (`^`) specs
/// carry only a left-hand side and are exempt from the second rule.
fn refspec_globs_agree(spec: &str) -> bool {
    let (negative, body) = match spec.strip_prefix('^') {
        Some(rest) => (true, rest),
        None => (false, spec.strip_prefix('+').unwrap_or(spec)),
    };
    // git splits on the *last* colon.
    let (src, dst) = match body.rfind(':') {
        Some(at) => (&body[..at], Some(&body[at + 1..])),
        None => (body, None),
    };
    let dst_is_glob = dst.is_some_and(|d| d.contains('*'));
    if !src.is_empty() && src.contains('*') {
        if dst.is_some() && !dst_is_glob {
            return false;
        }
        if dst.is_none() && !negative {
            return false;
        }
    } else if dst_is_glob {
        return false;
    }
    true
}

/// `remote.<name>.vcs` — the foreign-SCM helper a remote is reached through.
///
/// `remote_get_1()` records it as `remote->foreign_vcs` (remote.c:571-573), and
/// `transport_get()` (transport.c:1239, :1251-1253) then hands the *whole*
/// connection to `git-remote-<vcs>` instead of looking at the URL's scheme:
///
/// ```c
/// helper = remote->foreign_vcs;
/// …
/// if (helper) {
///         transport_helper_init(ret, helper);
/// ```
///
/// The helper speaks the remote-helper protocol (`capabilities`, `list`,
/// `import`/`export`) over its own stdio, and this port has no
/// `transport-helper.c` — its `remote-ext`/`remote-fd`/`remote-http` verbs are
/// individual helpers, not the machinery that drives one. So the setting is
/// read and the command *refuses*, rather than ignoring it and connecting to
/// the URL directly with the git protocol: for a `[remote "hg"] vcs = hg`
/// remote that URL is not a git repository at all, and the git-protocol attempt
/// would fail somewhere further along with a diagnostic about the wrong thing.
///
/// Returns the configured helper name, or `None` when the key is unset.
///
/// An **empty** value counts as set: `git_config_string()` stores `""`, so
/// `transport_get()`'s `if (helper)` is still true and stock reaches for
/// `git-remote-` — `git: 'remote-' is not a git command.` followed by
/// `fatal: remote helper '' aborted session`, exit 128. Treating it as unset
/// here would connect over the git protocol instead, which is the silent wrong
/// thing this gate exists to prevent.
pub(super) fn foreign_vcs(repo: &gix::Repository, remote_name: Option<&str>) -> Option<String> {
    let name = remote_name?;
    repo.config_snapshot()
        .plumbing()
        .string_by("remote", Some(gix::bstr::BStr::new(name.as_bytes())), "vcs")
        .map(|v| v.to_string())
}

/// [`foreign_vcs`] as a gate: `Some(code)` is the exit status the caller must
/// return, after the refusal has been reported.
pub(super) fn reject_foreign_vcs(
    repo: &gix::Repository,
    remote_name: Option<&str>,
) -> Option<ExitCode> {
    let vcs = foreign_vcs(repo, remote_name)?;
    let name = remote_name.unwrap_or_default();
    eprintln!(
        "fatal: remote.{name}.vcs={vcs} needs the git-remote-{vcs} helper protocol, \
         which is not ported"
    );
    Some(ExitCode::from(128))
}

/// The program to run instead of `git-upload-pack` on the other end.
///
/// `--upload-pack` wins over `remote.<name>.uploadpack`, which git reads in `get_upload_pack()`.
pub(super) fn upload_pack_program(
    repo: &gix::Repository,
    remote_name: Option<&str>,
    opts_upload_pack: Option<&str>,
) -> Option<BString> {
    if let Some(program) = opts_upload_pack {
        return Some(program.into());
    }
    let name = remote_name?;
    repo.config_snapshot()
        .string(&format!("remote.{name}.uploadpack"))
        .map(|v| v.to_owned())
}

/// The program the other end runs for `service`, with this binary supplied as
/// the default when the other end is *this* machine.
///
/// `explicit` is what the command line and `remote.<name>.uploadpack` /
/// `receivepack` already settled between them (`get_upload_pack()` /
/// `get_receive_pack()`), and it always wins: a caller that named a program
/// gets that program.
///
/// What this adds is the case where neither named one and the URL is local.
/// git still spawns the bare name `git-upload-pack` there, but only after
/// `setup_path()` (`exec-cmd.c`, called from `git.c:main()`) has prepended its
/// own exec-path to `PATH`, so the service is always the same installation as
/// the parent process. zvcs has no directory of `git-*` programs to prepend —
/// one binary serves every helper — so the equivalent is to name that binary
/// outright. Without this a local clone or fetch hands pack generation to
/// whichever `git-upload-pack` happens to come first on `PATH`, which can be a
/// different git, or a different *build* of zvcs, and the pack that lands is
/// then not the one this process would have written.
///
/// Remote URLs are deliberately left alone. Over ssh, `git://` or http the
/// value names a program on the far machine, where a path out of this
/// process's filesystem means nothing — so those keep the bare service name
/// and the far end resolves it.
pub(super) fn local_service_program(
    url: Option<&gix::Url>,
    explicit: Option<BString>,
    service: &str,
) -> Option<BString> {
    if explicit.is_some() {
        return explicit;
    }
    // `Scheme::File` is what gix classifies both `file://` URLs and bare
    // filesystem paths as, which is the same set git treats as local.
    if url?.scheme != gix::url::Scheme::File {
        return None;
    }
    let exe = std::env::current_exe().ok()?;
    let exe = gix::path::into_bstr(exe).into_owned();

    // The local transport hands an override to a shell (`conn->use_shell = 1`
    // in `git_connect()`, and `command_may_be_shell_script()` here), so the
    // path has to survive word splitting. Single-quote it, closing and
    // reopening around any embedded quote, which is the only byte that can end
    // the literal.
    let mut out = BString::from("'");
    for &byte in exe.iter() {
        if byte == b'\'' {
            out.extend_from_slice(b"'\\''");
        } else {
            out.push(byte);
        }
    }
    out.extend_from_slice(b"' ");
    out.extend_from_slice(service.as_bytes());
    Some(out)
}

/// The protocol-v2 server options to transmit.
///
/// Resolve one `--negotiation-restrict`/`--negotiation-tip`/`--negotiation-include` argument into the
/// commits it names, appending them to `out`.
///
/// This is git's `get_negotiation_tips()`. An argument carrying glob specials is matched against ref
/// names — with `refs/` prepended when it doesn't start there already, as `for_each_glob_ref_in()`
/// does — and warns when it matches nothing. Anything else is resolved as a revision; a full object
/// id that isn't in the database is fatal, while a name that simply doesn't resolve contributes no
/// tip and no diagnostic.
fn resolve_negotiation_tip(
    repo: &gix::Repository,
    flag: &str,
    arg: &str,
    out: &mut Vec<gix::ObjectId>,
) -> Result<Option<Verdict>> {
    if arg.contains(['*', '?', '[']) {
        let pattern = if arg.starts_with("refs/") {
            arg.to_string()
        } else {
            format!("refs/{arg}")
        };
        let before = out.len();
        for r in repo.references()?.all()? {
            let mut r = r.map_err(anyhow::Error::msg)?;
            if gix::glob::wildmatch(
                pattern.as_str().into(),
                r.name().as_bstr(),
                gix::glob::wildmatch::Mode::NO_MATCH_SLASH_LITERAL,
            ) {
                out.push(r.peel_to_id_in_place()?.detach());
            }
        }
        if out.len() == before {
            eprintln!("warning: ignoring {flag}={arg} because it does not match any refs");
        }
        return Ok(None);
    }
    match repo.rev_parse_single(arg) {
        Ok(id) => out.push(id.detach()),
        Err(_) => {
            // A fully spelled object id is a promise that the object exists, so git says so and stops
            // rather than quietly negotiating without it.
            if arg.len() == repo.object_hash().len_in_hex() && arg.bytes().all(|b| b.is_ascii_hexdigit()) {
                eprintln!("fatal: the object {arg} does not exist");
                return Ok(Some(Verdict::Fatal));
            }
        }
    }
    Ok(None)
}

/// Resolve every negotiation tip and include for one remote, honouring
/// `remote.<name>.negotiationInclude` when `--negotiation-include` wasn't given.
fn negotiation_restrictions(
    repo: &gix::Repository,
    remote_name: Option<&str>,
    opts: &FetchOpts,
) -> Result<Result<gix::protocol::fetch::negotiate::Restrictions, Verdict>> {
    let mut out = gix::protocol::fetch::negotiate::Restrictions::default();
    // `set_transport_options()` falls back on `remote.<name>.negotiationRestrict` when the command line
    // named no tip, and reports the same diagnostics under the config key's name.
    let restrict: Vec<String> = match &opts.negotiation_restrict {
        Some(args) => args.clone(),
        None => negotiation_restrict_config(repo, remote_name),
    };
    if !restrict.is_empty() {
        let mut tips = Vec::new();
        for arg in &restrict {
            if let Some(verdict) = resolve_negotiation_tip(repo, "--negotiation-restrict", arg, &mut tips)? {
                return Ok(Err(verdict));
            }
        }
        out.tips = Some(tips);
    }
    // "If this option is not specified on the command line, then any `remote.<name>.negotiationInclude`
    // config values for the current remote are used instead."
    let include: Vec<String> = match &opts.negotiation_include {
        Some(args) => args.clone(),
        None => remote_name
            .map(|name| {
                repo.config_snapshot()
                    .strings(&format!("remote.{name}.negotiationInclude"))
                    .map(|values| values.iter().map(ToString::to_string).collect())
                    .unwrap_or_default()
            })
            .unwrap_or_default(),
    };
    for arg in &include {
        if let Some(verdict) = resolve_negotiation_tip(repo, "--negotiation-include", arg, &mut out.always_have)? {
            return Ok(Err(verdict));
        }
    }
    Ok(Ok(out))
}

/// `remote.<name>.negotiationRestrict`, the per-remote default for `--negotiation-restrict`.
///
/// It is a `parse_transport_option()` list, like `remote.<name>.serverOption`: repeatable, and a blank
/// value in a higher-priority file discards everything inherited so far.
pub(super) fn negotiation_restrict_config(repo: &gix::Repository, remote_name: Option<&str>) -> Vec<String> {
    let Some(name) = remote_name else {
        return Vec::new();
    };
    repo.config_snapshot()
        .strings(&format!("remote.{name}.negotiationRestrict"))
        .map(|values| {
            values.into_iter().fold(Vec::new(), |mut acc, value| {
                if value.is_empty() {
                    acc.clear();
                } else {
                    acc.push(value.to_string());
                }
                acc
            })
        })
        .unwrap_or_default()
}

/// `--server-option` replaces `remote.<name>.serverOption` rather than adding to it, as documented: "These
/// server options can be overridden by the `--server-option=` command line arguments."
pub(super) fn server_options_for(
    repo: &gix::Repository,
    remote_name: Option<&str>,
    from_command_line: &[BString],
) -> Vec<BString> {
    if !from_command_line.is_empty() {
        return from_command_line.to_vec();
    }
    let Some(name) = remote_name else {
        return Vec::new();
    };
    repo.config_snapshot()
        .strings(&format!("remote.{name}.serverOption"))
        .map(|values| {
            values
                .into_iter()
                // An empty value in a higher-priority file clears everything inherited so far.
                .fold(Vec::new(), |mut acc, v| {
                    if v.is_empty() {
                        acc.clear();
                    } else {
                        acc.push(v.to_owned());
                    }
                    acc
                })
        })
        .unwrap_or_default()
}

/// Whether git's `remote_get(NULL)` would come back NULL here — no remote is named
/// by `branch.<current>.remote`, there is no sole configured remote to fall back on,
/// and `origin` is not configured either.
///
/// Only that one gitoxide error stands for git's NULL. The rest (an unparsable URL,
/// a malformed refspec on a remote that *is* configured) are failures git reaches
/// with a remote in hand and dies on, so they must keep unwinding.
fn default_fetch_remote_missing(repo: &gix::Repository) -> bool {
    matches!(
        repo.find_fetch_remote(None),
        Err(gix::remote::find::for_fetch::Error::ExactlyOneRemoteNotAvailable)
    )
}

/// Run the fetch pipeline for a single remote and print its summary.
#[allow(clippy::too_many_arguments)]
fn fetch_one(
    repo: &gix::Repository,
    name_or_url: Option<&BStr>,
    refspecs: &[&str],
    opts: &FetchOpts,
    upstream: Option<&(String, String)>,
    fetch_head: &mut FetchHead,
    progress: &mut prodash::tree::Item,
) -> Result<Verdict> {
    // A local path that is not a repository never reaches the transport in git:
    // `enter_repo()` fails on the other side and `die_initial_contact()` follows.
    if let Some(spec) = name_or_url {
        let spec = spec.to_string();
        if repo.try_find_remote(spec.as_str()).is_none() {
            if let Some(bad) = super::send_pack::local_dest_that_is_not_a_repository(&spec) {
                eprintln!("fatal: '{bad}' does not appear to be a git repository");
                eprintln!(
                    "fatal: Could not read from remote repository.\n\n\
                     Please make sure you have the correct access rights\n\
                     and the repository exists."
                );
                return Ok(Verdict::Fatal);
            }
        }
    }
    let mut remote = repo.find_fetch_remote(name_or_url)?;
    let remote_name = remote.name().map(|n| n.as_bstr().to_string());

    // `do_fetch()` resolves pruning only once the remote is in hand, because
    // `remote.<name>.prune`/`remote.<name>.pruneTags` sit between the command line and
    // `fetch.prune`/`fetch.pruneTags`. `opts` already carries the outer two levels collapsed,
    // so the remote layer only applies when the command line was silent.
    let prune = if opts.prune_from_cli {
        opts.prune
    } else {
        remote_name
            .as_deref()
            .and_then(|name| repo.config_snapshot().boolean(&format!("remote.{name}.prune")))
            .or(opts.prune)
    };
    let prune_tags = if opts.prune_tags_from_cli {
        opts.prune_tags
    } else {
        remote_name
            .as_deref()
            .and_then(|name| repo.config_snapshot().boolean(&format!("remote.{name}.pruneTags")))
            .or(opts.prune_tags)
    };

    // `transfer.credentialsInUrl` is checked before any connection is opened,
    // where git checks it.
    if credentials_in_url(repo, remote.url(gix::remote::Direction::Fetch)) == Verdict::Fatal {
        return Ok(Verdict::Fatal);
    }

    // `transport_check_allowed()`, which `git_connect()` reaches for every scheme
    // it opens itself and `transport_helper_init()` for the rest. It matters most
    // here: a fetch is what a submodule update runs, and git clears
    // `$GIT_PROTOCOL_FROM_USER` around exactly that, so a `.gitmodules` URL naming
    // a local path or an `ext::` command is refused where a typed one is not.
    if let Some(remote_url) = remote.url(gix::remote::Direction::Fetch) {
        if crate::setup::check_url_allowed(remote_url).is_some() {
            return Ok(Verdict::Fatal);
        }
    }

    // The configured fetch refspecs, captured before command-line refspecs replace them: with explicit
    // refspecs they become git's *opportunistic* second stage, mapping the refs the command line selected onto
    // the tracking refs they would normally land in (`get_ref_map` in `builtin/fetch.c`).
    let has_configured_refspecs = !remote.refspecs(gix::remote::Direction::Fetch).is_empty();
    let configured_refspecs: Vec<gix::refspec::RefSpec> = remote
        .refspecs(gix::remote::Direction::Fetch)
        .iter()
        .map(|s| s.to_ref().to_owned())
        .collect();

    // Tag handling: `-t` → all tags, `-n` → none. Injected as an implicit
    // `refs/tags/*:refs/tags/*` refspec by the ref-map builder.
    if let Some(tags) = opts.tags {
        remote = remote.with_fetch_tags(tags);
    }

    // `get_ref_map()` (`builtin/fetch.c`): with nothing on the command line and
    // nothing configured for this remote — `git fetch <url>`, or a remote whose
    // `fetch` key is unset — git does not fail. It falls back to
    //
    // ```c
    // } else if (!prefetch) {
    //         ref_map = get_remote_ref(remote_refs, "HEAD");
    //         if (!ref_map)
    //                 die(_("couldn't find remote ref HEAD"));
    //         ref_map->fetch_head_status = FETCH_HEAD_MERGE;
    // ```
    //
    // — one entry for the remote's `HEAD` with no peer ref, so nothing but
    // `FETCH_HEAD` moves and the summary reads `* branch  HEAD -> FETCH_HEAD`.
    // That is exactly a bare `HEAD` refspec, so it is injected as one; `HEAD`
    // carries no destination, which also keeps automatic tag following off the
    // way git's untouched `*autotags` does. `--prefetch` is excluded by the
    // `!prefetch` guard and fetches nothing at all.
    //
    // The one configured case git still routes through the branch's upstream is
    // `has_merge && !strcmp(branch->remote_name, remote->name)`: an upstream
    // pointing at *this* remote supplies the refspec instead.
    let branch_upstream_is_this_remote = upstream
        .is_some_and(|(remote, _)| Some(remote.as_str()) == remote_name.as_deref());
    let head_fallback = ["HEAD"];
    let refspecs: &[&str] = if refspecs.is_empty()
        && !has_configured_refspecs
        && !branch_upstream_is_this_remote
        && !opts.prefetch
    {
        &head_fallback
    } else {
        refspecs
    };

    // Refspec selection. Explicit command-line refspecs replace the configured
    // set and additionally make every FETCH_HEAD row a merge candidate, as git's
    // `get_ref_map` does when `refspec_count > 0`.
    let explicit_refspecs = !refspecs.is_empty();
    if explicit_refspecs {
        let specs: Vec<BString> = refspecs
            .iter()
            .map(|r| {
                let s = BString::from(*r);
                if opts.force {
                    forced(s)
                } else {
                    s
                }
            })
            .collect();
        remote.replace_refspecs(specs, gix::remote::Direction::Fetch)?;
    } else if opts.force {
        let specs: Vec<BString> = remote
            .refspecs(gix::remote::Direction::Fetch)
            .iter()
            .map(|s| forced(s.to_ref().to_bstring()))
            .collect();
        remote.replace_refspecs(specs, gix::remote::Direction::Fetch)?;
    }

    // git's `*autotags`: automatic tag following is armed by a command-line refspec only if that refspec has a
    // destination. `git fetch <remote> <branch>` therefore fetches no tags at all, while
    // `git fetch <remote> <branch>:<dst>` does. An explicit `--tags`/`--no-tags` decides on its own.
    if explicit_refspecs && opts.tags.is_none() {
        let any_destination = remote
            .refspecs(gix::remote::Direction::Fetch)
            .iter()
            .any(|s| s.to_ref().destination().is_some());
        if !any_destination {
            remote = remote.with_fetch_tags(Tags::None);
        }
    }

    // `--prefetch` rewrites every destination under `refs/prefetch/` and forces
    // it; specs that would land in `refs/tags/` are dropped entirely.
    if opts.prefetch {
        let specs: Vec<BString> = remote
            .refspecs(gix::remote::Direction::Fetch)
            .iter()
            .filter_map(|s| prefetch_spec(s.to_ref().to_bstring().as_bstr()))
            .collect();
        remote.replace_refspecs(specs, gix::remote::Direction::Fetch)?;
        // Tag following would reintroduce `refs/tags/*`, which git's prefetch
        // filter removes, so it is switched off for the duration.
        remote = remote.with_fetch_tags(Tags::None);
    }

    // Destination prefixes to prune (glob refspec destinations only), captured
    // before the remote is consumed by `connect`.
    let mut prune_prefixes: Vec<Vec<u8>> = Vec::new();
    if prune == Some(true) {
        for s in remote.refspecs(gix::remote::Direction::Fetch) {
            if let Some(dst) = s.to_ref().destination() {
                let dst: &[u8] = dst.as_ref();
                if let Some(star) = dst.iter().position(|&b| b == b'*') {
                    prune_prefixes.push(dst[..star].to_vec());
                }
            }
        }
        // `-P` adds the tags refspec, so its destination joins the prune set.
        if prune_tags == Some(true) {
            prune_prefixes.push(b"refs/tags/".to_vec());
        }
        prune_prefixes.sort();
        prune_prefixes.dedup();
    }

    // `-P` fetches all tags via an implicit refspec so pruning has the full
    // remote tag set to diff against, without persisting the spec to config.
    let mut extra_refspecs = Vec::new();
    if prune_tags == Some(true) {
        extra_refspecs.push(
            gix::refspec::parse(
                "refs/tags/*:refs/tags/*".into(),
                gix::refspec::parse::Operation::Fetch,
            )?
            .to_owned(),
        );
    }
    // git's second matching stage. With command-line refspecs the configured refspecs no longer select refs;
    // they map the refs that were selected onto their tracking refs, so `git fetch origin main` still moves
    // `refs/remotes/origin/main`. `--refmap` replaces them for that purpose only.
    let mut opportunistic_refspecs = if explicit_refspecs {
        opts.refmap.clone().unwrap_or(configured_refspecs)
    } else {
        Vec::new()
    };
    if opts.prefetch {
        // `filter_prefetch_refspec` rewrites `remote->fetch` as well as the command-line refspecs.
        opportunistic_refspecs = opportunistic_refspecs
            .iter()
            .filter_map(|s| prefetch_spec(s.to_ref().to_bstring().as_bstr()))
            .filter_map(|s| {
                gix::refspec::parse(s.as_ref(), gix::refspec::parse::Operation::Fetch)
                    .ok()
                    .map(|s| s.to_owned())
            })
            .collect();
    } else if opts.force {
        opportunistic_refspecs = opportunistic_refspecs
            .iter()
            .filter_map(|s| {
                gix::refspec::parse(
                    forced(s.to_ref().to_bstring()).as_ref(),
                    gix::refspec::parse::Operation::Fetch,
                )
                .ok()
                .map(|s| s.to_owned())
            })
            .collect();
    }

    // `do_set_head` also decides whether the advertisement has to include `HEAD`: git pushes the
    // literal prefix onto the ls-refs list so `set_head()` has something to guess from.
    // `remote.<name>.followRemoteHEAD` is parsed once, as git parses it once into `struct remote`.
    let follow_head = remote_name.as_deref().map(|name| follow_remote_head(repo, name));
    let want_head = !explicit_refspecs
        && has_configured_refspecs
        && follow_head.as_ref().is_some_and(|mode| *mode != FollowRemoteHead::Never);
    let map_options = gix::remote::ref_map::Options {
        extra_refspecs,
        opportunistic_refspecs,
        extra_ref_prefixes: if want_head { vec![BString::from("HEAD")] } else { Vec::new() },
        ..Default::default()
    };

    // `remote.<name>.vcs` routes the whole connection through `git-remote-<vcs>`
    // rather than the URL's own transport, which this port cannot drive — see
    // [`foreign_vcs`]. Refused here, before the URL is displayed, so nothing
    // claims to be fetching from a repository that was never going to be read
    // with the git protocol.
    if foreign_vcs(repo, remote_name.as_deref()).is_some() {
        let _ = reject_foreign_vcs(repo, remote_name.as_deref());
        return Ok(Verdict::Fatal);
    }

    let raw_url = remote
        .url(gix::remote::Direction::Fetch)
        .map(ToString::to_string)
        .or_else(|| remote.name().map(|n| n.as_bstr().to_string()))
        .unwrap_or_default();
    let url = display_url(&raw_url).to_string();
    let abbrev = abbrev_len(repo);

    let connect_options = gix::remote::connect::Options {
        upload_pack: local_service_program(
            remote.url(gix::remote::Direction::Fetch),
            upload_pack_program(repo, remote_name.as_deref(), opts.upload_pack.as_deref()),
            "upload-pack",
        ),
        address_family: opts.address_family,
        // `git fetch` never connects for push.
        receive_pack: None,
    };
    let server_options = server_options_for(repo, remote_name.as_deref(), &opts.server_options);

    let restrictions = match negotiation_restrictions(repo, remote_name.as_deref(), opts)? {
        Ok(r) => r,
        Err(verdict) => return Ok(verdict),
    };

    // `--negotiate-only` never lists refs and never asks for a pack: it runs the negotiation on its
    // own and prints the commits the remote acknowledged as common, one per line.
    if opts.negotiate_only {
        let common = remote
            .connect_with_options(gix::remote::Direction::Fetch, connect_options)?
            .with_server_options(server_options)
            .negotiate_only(&mut *progress, restrictions)?;
        let mut out = String::new();
        for id in common {
            out.push_str(&id.to_hex().to_string());
            out.push('\n');
        }
        print!("{out}");
        return Ok(Verdict::Ok);
    }

    let should_interrupt = AtomicBool::new(false);
    let prepared = match remote
        .connect_with_options(gix::remote::Direction::Fetch, connect_options)?
        .with_server_options(server_options)
        .prepare_fetch(&mut *progress, map_options)
    {
        Ok(p) => p,
        // git's `die_if_server_options()` also prints the advice line, and both it and the
        // "server doesn't support" case are `fatal:` exits rather than per-remote failures.
        Err(gix::remote::fetch::prepare::Error::RefMap(
            e @ gix::remote::ref_map::Error::ServerOptionsRequireV2,
        )) => {
            eprintln!("hint: see protocol.version in 'git help config' for more details");
            eprintln!("fatal: {e}");
            return Ok(Verdict::Fatal);
        }
        Err(gix::remote::fetch::prepare::Error::RefMap(
            e @ gix::remote::ref_map::Error::ServerOptionsUnsupported,
        )) => {
            eprintln!("fatal: {e}");
            return Ok(Verdict::Fatal);
        }
        Err(e) => {
            // An ssh transport that never connected is git's own `die()`: the
            // child's stderr, then the fixed block, exit 128.
            let err = anyhow::Error::from(e);
            if crate::transport_err::ssh_fatal(&url, &err).is_some() {
                return Ok(Verdict::Fatal);
            }
            // A server that refused the request with an `ERR` line said why; git
            // prints that message and dies.
            if crate::transport_err::remote_error_fatal(&err).is_some() {
                return Ok(Verdict::Fatal);
            }
            return Err(err);
        }
    };

    // `get_fetch_map()` in `remote.c` is called with `missing_ok == 0` for every refspec that came
    // from the command line or from `remote.<name>.fetch`, so a refspec that names one exact ref the
    // remote does not have is a `fatal:` before a single object moves - not a summary at the end.
    if let Some(missing) = missing_remote_ref(prepared.ref_map()) {
        eprintln!("fatal: couldn't find remote ref {missing}");
        return Ok(Verdict::Fatal);
    }

    // `fetch_pack_config()` (`fetch-pack.c:1995`) reads `fetch.fsck.<msg-id>`,
    // `fetch.fsck.skipList`, `fetch.fsckObjects` and `transfer.fsckObjects` from
    // inside `fetch_pack()` — after the ref map is in hand, so it is diagnosed
    // *after* `couldn't find remote ref` and *before* any object moves.
    // Confirmed against git 2.55.0: `-c fetch.fsck.badTree=bogus` next to a
    // refspec the remote does not have reports only `couldn't find remote ref`,
    // while the same against a ref that exists — even one already up to date —
    // reports `fatal: Unknown fsck message type: 'bogus'`.
    //
    // The value is validated whether or not the check will run, because
    // `fetch_pack_fsck_config()` calls `is_valid_msg_type()` on every
    // `fetch.fsck.` variable it sees (`fetch-pack.c:1974`).
    let fsck_msgs = match super::fsck::MsgConfig::new(repo, super::fsck::MsgSource::Fetch) {
        Ok(config) => config,
        Err(text) => {
            eprintln!("fatal: {text}");
            return Ok(Verdict::Fatal);
        }
    };
    // `fetch_pack_fsck_objects()` (`fetch-pack.c:2158`): `fetch.fsckObjects`
    // first, `transfer.fsckObjects` as the fallback, off when neither is set.
    let fsck_objects = {
        let snapshot = repo.config_snapshot();
        snapshot
            .boolean("fetch.fsckObjects")
            .or_else(|| snapshot.boolean("transfer.fsckObjects"))
            .unwrap_or(false)
    };

    let outcome = prepared
        .with_dry_run(opts.dry_run)
        .with_shallow(opts.shallow.clone().unwrap_or_default())
        .with_shallow_update(if opts.update_shallow {
            gix::remote::fetch::ShallowUpdate::Update
        } else {
            gix::remote::fetch::ShallowUpdate::Reject
        })
        .with_negotiation_restrictions(restrictions)
        .with_refetch(opts.refetch)
        .with_atomic(opts.atomic)
        .with_reflog_message(RefLogMessage::Prefixed {
            action: opts.reflog_action.clone().into(),
        })
        .receive(&mut *progress, &should_interrupt);
    let outcome = match outcome {
        Ok(outcome) => outcome,
        // The post-fetch connectivity check (`connected.c`) said the pack was short of what the
        // offered refs need. git's `store_updated_refs()` reports it against the remote's URL and
        // gives up on this remote without storing a single ref, which is a plain `error()` and so
        // exit code 1 rather than a `fatal:`.
        Err(gix::remote::fetch::Error::NotConnected) => {
            eprintln!("error: {url} did not send all necessary objects");
            return Ok(Verdict::Rejected);
        }
        Err(e) => {
            // `ERR <message>` mid-response: the server's own refusal (an
            // unreachable want, a hidden ref), which git reports verbatim.
            let err = anyhow::Error::from(e);
            if crate::transport_err::remote_error_fatal(&err).is_some() {
                return Ok(Verdict::Fatal);
            }
            return Err(err);
        }
    };

    // Refs the remote could only offer by making us adopt one of its shallow roots. git leaves
    // them out of both the summary and FETCH_HEAD and warns about each, naming the local
    // tracking ref when there is one and the remote ref otherwise.
    for mapping in &outcome.rejected_shallow {
        let name = mapping
            .local
            .as_ref()
            .map(ToString::to_string)
            .or_else(|| mapping.remote.as_name().map(ToString::to_string))
            .unwrap_or_default();
        eprintln!("warning: rejected {name} because shallow roots are not allowed to be updated");
    }

    // `fetch.fsckObjects` / `transfer.fsckObjects`: every object the pack
    // delivered is linted before a single ref moves, and the first error kills
    // the fetch. This runs before [`explode_small_pack`] because with the check
    // on `fetch_pack()` always picks `index-pack` — `do_keep || from_promisor ||
    // index_pack_args || fsck_objects` at `fetch-pack.c:1007` — so the loose
    // shortcut is not taken at all, and `index-pack` is also the name in the
    // `%s failed` that follows the child's own diagnostic.
    if let Status::Change { write_pack_bundle, .. } = &outcome.status {
        if fsck_objects {
            if let Err(message) = fsck_fetched(repo, write_pack_bundle, &fsck_msgs) {
                eprintln!("fatal: {message}");
                eprintln!("fatal: index-pack failed");
                return Ok(Verdict::Fatal);
            }
        }
    }

    // `fetch_pack()` chooses `unpack-objects` over `index-pack` for a small pack, so a
    // fetch of a handful of objects leaves them loose rather than packed.
    if let Status::Change { write_pack_bundle, .. } = &outcome.status {
        if !fsck_objects {
            explode_small_pack(repo, write_pack_bundle)?;
        }
    }

    // Both status variants carry the ref-update outcome; the ref_map ties each
    // update back to its remote/local mapping.
    let ref_map = &outcome.ref_map;
    let update_refs = match &outcome.status {
        Status::NoPackReceived { update_refs, .. } => update_refs,
        Status::Change { update_refs, .. } => update_refs,
    };

    let null = gix::ObjectId::null(repo.object_hash());

    // --- build the git-style per-ref summary ------------------------------
    let mut update_lines: Vec<Line> = Vec::new();
    let mut fetch_head_rows: Vec<(String, bool)> = Vec::new();
    let mut rejected = false;
    // Set when a refspec would overwrite a ref some worktree has checked out and
    // `--update-head-ok` was not given: git turns that into a fatal for the whole
    // command rather than a per-ref rejection.
    let mut checked_out: Option<(String, std::path::PathBuf)> = None;

    for (update, mapping, spec, edit) in update_refs.iter_mapping_updates(
        &ref_map.mappings,
        &ref_map.refspecs,
        &ref_map.extra_refspecs,
    ) {
        let remote_full = mapping
            .remote
            .as_name()
            .map(|n| n.to_string())
            .unwrap_or_default();
        let remote_id = mapping.remote.as_id().map(ToOwned::to_owned);

        // Opportunistic mappings exist only to move the tracking ref. git marks them `FETCH_HEAD_IGNORE`
        // because their row would duplicate the one the command-line refspec already contributed.
        let opportunistic = ref_map.is_opportunistic(mapping);
        // git marks every entry a *command-line* refspec produced `FETCH_HEAD_MERGE`. Refs that only
        // automatic tag following pulled in are added afterwards and keep the default `not-for-merge`.
        let from_command_line = explicit_refspecs
            && matches!(
                mapping.spec_index,
                gix::protocol::fetch::refmap::SpecIndex::ExplicitInRemote(_)
            );

        let from = mapping
            .remote
            .as_name()
            .and_then(|n| FullName::try_from(n).ok())
            .map(|f| f.shorten().to_string())
            .or_else(|| mapping.remote.as_id().map(|id| id.to_hex_with_len(abbrev).to_string()))
            .unwrap_or_default();

        // A mapping with no local destination lands in FETCH_HEAD only, which git
        // reports as a `* <kind> <from> -> FETCH_HEAD` row.
        let local_full = match mapping.local.as_ref() {
            Some(name) => match FullName::try_from(BStr::new(name)) {
                Ok(f) => f,
                Err(_) => continue,
            },
            None => {
                // `--no-write-fetch-head` drops the row as well as the file;
                // `--dry-run` keeps the row and skips only the file.
                if !opts.write_fetch_head {
                    continue;
                }
                if let Some(id) = remote_id {
                    let for_merge = from_command_line
                        || (!explicit_refspecs
                            && upstream.is_some_and(|(r, m)| {
                                Some(r.as_str()) == remote_name.as_deref() && *m == remote_full
                            }));
                    fetch_head_rows.push((
                        fetch_head_note(id, for_merge, &remote_full, &url),
                        for_merge,
                    ));
                    let kind = match remote_full.as_str() {
                        r if r.starts_with("refs/heads/") => "branch",
                        r if r.starts_with("refs/tags/") => "tag",
                        r if r.starts_with("refs/remotes/") => "remote-tracking branch",
                        _ => "",
                    };
                    update_lines.push(Line {
                        flag: '*',
                        summary: kind.to_string(),
                        from,
                        to: "FETCH_HEAD".to_string(),
                        reason: "",
                        old: null,
                        new: id,
                        full: "FETCH_HEAD".to_string(),
                    });
                }
                continue;
            }
        };
        let to = local_full.shorten().to_string();
        let is_tag = matches!(local_full.category(), Some(Category::Tag));

        // A tag the repository already has is invisible under automatic tag
        // following: git's `find_non_local_tags` only proposes tags that are
        // missing locally, so such a tag never enters the ref map and appears in
        // neither the summary (not even under `-v`) nor FETCH_HEAD. gitoxide's
        // implicit tag refspec maps it regardless, so it is dropped here. An
        // explicit `--tags` fetches the whole namespace and does list them.
        if is_tag && !matches!(opts.tags, Some(Tags::All)) && update.mode == Mode::NoChangeNeeded {
            continue;
        }

        // Every mapping with a local destination contributes a FETCH_HEAD row,
        // whether or not the tracking ref actually moved.
        if let (Some(id), false) = (remote_id, opportunistic) {
            let for_merge = from_command_line
                || (!explicit_refspecs
                    && upstream.is_some_and(|(r, m)| {
                        Some(r.as_str()) == remote_name.as_deref() && *m == remote_full
                    }));
            fetch_head_rows.push((
                fetch_head_note(id, for_merge, &remote_full, &url),
                for_merge,
            ));
        }

        // Old/new ids for range summaries, extracted from the applied edit.
        let (old_id, new_id) = match edit.map(|e| &e.change) {
            Some(Change::Update { expected, new, .. }) => {
                let old = match expected {
                    PreviousValue::MustExistAndMatch(Target::Object(id)) => Some(*id),
                    _ => None,
                };
                let new = match new {
                    Target::Object(id) => Some(*id),
                    _ => None,
                };
                (old, new)
            }
            _ => (None, None),
        };
        let range = |sep: &str| match (old_id, new_id) {
            (Some(o), Some(n)) => {
                format!("{}{sep}{}", o.to_hex_with_len(abbrev), n.to_hex_with_len(abbrev))
            }
            _ => String::new(),
        };

        let (flag, summary, reason): (char, String, &'static str) = match &update.mode {
            Mode::New => {
                let s = if is_tag { "[new tag]" } else { "[new branch]" };
                ('*', s.to_string(), "")
            }
            Mode::FastForward => (' ', range(".."), ""),
            // `--no-show-forced-updates` / `fetch.showForcedUpdates=false` skips
            // the forced-update check outright, so git reports the ref as an
            // ordinary fast-forward: a blank flag, a `..` range and no note.
            Mode::Forced if !opts.show_forced_updates => (' ', range(".."), ""),
            Mode::Forced => ('+', range("..."), "  (forced update)"),
            Mode::NoChangeNeeded => {
                if !opts.verbose {
                    continue;
                }
                ('=', "[up to date]".to_string(), "")
            }
            Mode::ImplicitTagNotSentByRemote => continue,
            Mode::RejectedNonFastForward => {
                rejected = true;
                ('!', "[rejected]".to_string(), "  (non-fast-forward)")
            }
            Mode::RejectedTagUpdate => {
                rejected = true;
                ('!', "[rejected]".to_string(), "  (would clobber existing tag)")
            }
            Mode::RejectedCurrentlyCheckedOut { worktree_dirs } => {
                // `-u`/`--update-head-ok` lifts the guard gitoxide applies to the
                // ref a worktree has checked out. The pack is already local at
                // this point, so the update is applied here with the same
                // fast-forward rule the refspec carries, and reported like any
                // other update rather than as a rejection.
                match (opts.update_head_ok, remote_id) {
                    (true, Some(id)) => {
                        match update_checked_out_ref(
                            repo,
                            &local_full,
                            id,
                            opts,
                            spec.is_some_and(|s| s.allow_non_fast_forward()),
                        )? {
                            Some((f, s, r)) => (f, s, r),
                            None => continue,
                        }
                    }
                    // Without it git refuses the whole command up front, naming
                    // the ref and the worktree that holds it, and exits 128
                    // without a summary or a FETCH_HEAD.
                    _ => {
                        checked_out = Some((
                            local_full.as_bstr().to_string(),
                            worktree_dirs.first().cloned().unwrap_or_default(),
                        ));
                        break;
                    }
                }
            }
            Mode::RejectedToReplaceWithUnborn => {
                rejected = true;
                ('!', "[rejected]".to_string(), "  (would replace with unborn)")
            }
            Mode::RejectedSourceObjectNotFound { .. } => {
                rejected = true;
                ('!', "[rejected]".to_string(), "  (source object not found)")
            }
        };
        // `--porcelain`'s two id columns: a ref that did not exist before shows
        // the null id on the left, and one that stayed put repeats its own id on
        // both sides (git prints `<old-object-id> <new-object-id>` either way).
        let (porcelain_old, porcelain_new) = match &update.mode {
            Mode::New => (null, remote_id.unwrap_or(null)),
            _ => (
                old_id.or(remote_id).unwrap_or(null),
                new_id.or(remote_id).unwrap_or(null),
            ),
        };
        update_lines.push(Line {
            flag,
            summary,
            from,
            to,
            reason,
            old: porcelain_old,
            new: porcelain_new,
            full: local_full.as_bstr().to_string(),
        });
    }

    if let Some((name, worktree)) = checked_out {
        // gitoxide reports the worktree as the repository was discovered (often
        // `.`); git names it absolutely, so the path is anchored on the current
        // directory and lexically normalized — no symlink resolution, which git
        // does not do either.
        let cwd = std::env::current_dir().unwrap_or_default();
        let absolute: std::path::PathBuf = if worktree.is_absolute() {
            worktree.clone()
        } else {
            cwd.join(&worktree)
        }
        .components()
        .filter(|c| !matches!(c, std::path::Component::CurDir))
        .collect();
        let worktree = gix::path::normalize(std::borrow::Cow::Owned(absolute.clone()), &cwd)
            .map(std::borrow::Cow::into_owned)
            .unwrap_or(absolute);
        eprintln!(
            "fatal: refusing to fetch into branch '{name}' checked out at '{}'",
            worktree.display()
        );
        return Ok(Verdict::Fatal);
    }

    // `--atomic` aborted the transaction: no ref moved, so nothing may be pruned and no FETCH_HEAD
    // row may be recorded either. git leaves the file truncated and empty, and still prints the
    // summary of what it would have done.
    let atomic_abort = update_refs.rejected_atomically;

    // --- prune stale tracking refs ----------------------------------------
    let mut prune_lines: Vec<Line> = Vec::new();
    if !prune_prefixes.is_empty() && !atomic_abort {
        // Every local ref the remote still advertises is kept; the rest under a
        // pruned prefix are deleted (git's `prune_refs`).
        let kept: HashSet<BString> = ref_map
            .mappings
            .iter()
            .filter_map(|m| m.local.clone())
            .collect();
        let mut pruned: HashSet<BString> = HashSet::new();

        // Collect candidates first, then delete: mutating refs while the ref
        // iterator still borrows the store would be unsound.
        let mut to_delete: Vec<(FullName, String, gix::ObjectId)> = Vec::new();
        for prefix in &prune_prefixes {
            for r in repo.references()?.prefixed(&prefix[..])? {
                let r = r.map_err(anyhow::Error::msg)?;
                // Never prune symbolic tracking refs like `refs/remotes/*/HEAD`.
                if matches!(r.target(), TargetRef::Symbolic(_)) {
                    continue;
                }
                let full = r.name().as_bstr().to_owned();
                if kept.contains(&full) || !pruned.insert(full.clone()) {
                    continue;
                }
                let id = match r.target() {
                    TargetRef::Object(id) => id.to_owned(),
                    TargetRef::Symbolic(_) => continue,
                };
                to_delete.push((
                    FullName::try_from(full.as_bstr())?,
                    r.name().shorten().to_string(),
                    id,
                ));
            }
        }

        for (name, short, id) in to_delete {
            if !opts.dry_run {
                repo.edit_reference(RefEdit {
                    change: Change::Delete {
                        expected: PreviousValue::Any,
                        log: RefLog::AndReference,
                        message: Default::default(),
                    },
                    name: name.clone(),
                    deref: false,
                })?;
            }
            prune_lines.push(Line {
                flag: '-',
                summary: "[deleted]".to_string(),
                from: "(none)".to_string(),
                to: short,
                reason: "",
                old: id,
                new: null,
                full: name.as_bstr().to_string(),
            });
        }
    }

    fetch_head.write(if atomic_abort { &[] } else { &fetch_head_rows })?;

    // --- print the summary ------------------------------------------------
    // `store_updated_refs()` walks the ref map, which git builds with the *current
    // branch's* upstream first (`get_ref_map()` resolves it ahead of the rest), so
    // that row heads the summary and the others follow in ref-map order. gitoxide's
    // mappings come back in advertisement order alone, which puts the current
    // branch wherever its name happens to sort — measured against stock: on `main`
    // with branches `aaa`/`mmm`/`zzz`, git prints `main` first and this printed
    // `aaa`.
    hoist_current_branch(repo, &mut update_lines);

    // Pruned refs are reported first, mirroring git's prune-before-fetch order.
    let mut lines = prune_lines;
    lines.extend(update_lines);

    if !opts.quiet && !lines.is_empty() {
        if opts.porcelain {
            // Machine-readable: `<flag> <old> <new> <local-ref>` on stdout, with
            // no `From <url>` header — git documents this as the parseable form.
            let mut out = String::new();
            for l in &lines {
                out.push_str(&format!(
                    "{} {} {} {}\n",
                    l.flag,
                    l.old.to_hex(),
                    l.new.to_hex(),
                    l.full
                ));
            }
            print!("{out}");
        } else {
            let rendered: Vec<(String, String)> = lines
                .iter()
                .map(|l| {
                    if opts.compact {
                        compact(&l.from, &l.to)
                    } else {
                        (l.from.clone(), l.to.clone())
                    }
                })
                .collect();
            // git's columns are fixed, not fitted: the summary is padded to
            // `TRANSPORT_SUMMARY_WIDTH` (`2 * <abbrev> + 3`, wide enough for an
            // `<old>...<new>` range) and the `<from>` column starts at
            // `REFCOL_WIDTH` and only grows past it for a longer name.
            let sw = 2 * abbrev + 3;
            let fw = rendered
                .iter()
                .map(|(f, _)| f.chars().count())
                .max()
                .unwrap_or(0)
                .max(REFCOL_WIDTH);
            eprintln!("From {url}");
            for (l, (from, to)) in lines.iter().zip(&rendered) {
                eprintln!(
                    " {} {:<sw$} {:<fw$} -> {}{}",
                    l.flag, l.summary, from, to, l.reason,
                );
            }
        }
    }

    // git's `do_set_head`: only a refspec-less fetch of a configured remote follows the remote's
    // `HEAD`, and only when `remote.<name>.followRemoteHEAD` is not `never`.
    if !opts.dry_run && !explicit_refspecs && has_configured_refspecs {
        if let Some(name) = remote_name.as_deref() {
            set_head_from_remote(repo, name, follow_head.unwrap_or(FollowRemoteHead::Create), ref_map, opts)?;
        }
    }

    Ok(if rejected { Verdict::Rejected } else { Verdict::Ok })
}

/// `remote.<name>.followRemoteHEAD`, git's `enum follow_remote_head_settings`.
#[derive(Debug, Clone, PartialEq, Eq)]
enum FollowRemoteHead {
    /// Never create or move `refs/remotes/<name>/HEAD`.
    Never,
    /// Create it when absent, never move an existing one. git's default.
    Create,
    /// Like `create`, plus a message when the remote's `HEAD` differs from ours. The payload is
    /// `warn-if-not-<branch>`'s branch, for which the message is suppressed.
    Warn(Option<String>),
    /// Silently update it to whatever the remote says.
    Always,
}

/// Read `remote.<name>.followRemoteHEAD`; an unrecognized value is a warning and leaves the default.
fn follow_remote_head(repo: &gix::Repository, remote_name: &str) -> FollowRemoteHead {
    let Some(value) = repo
        .config_snapshot()
        .string(&format!("remote.{remote_name}.followRemoteHEAD"))
        .map(|v| v.to_string())
    else {
        return FollowRemoteHead::Create;
    };
    match value.as_str() {
        "never" => FollowRemoteHead::Never,
        "create" => FollowRemoteHead::Create,
        "warn" => FollowRemoteHead::Warn(None),
        "always" => FollowRemoteHead::Always,
        other => match other.strip_prefix("warn-if-not-") {
            Some(branch) => FollowRemoteHead::Warn(Some(branch.to_owned())),
            None => {
                eprintln!("warning: unrecognized followRemoteHEAD value '{value}' ignored");
                FollowRemoteHead::Create
            }
        },
    }
}

/// Port of git's `set_head()`: point `refs/remotes/<name>/HEAD` at the branch the remote's `HEAD`
/// names, under the policy `remote.<name>.followRemoteHEAD` sets.
///
/// git ignores every failure here ("way too many cases where this can go wrong"), so an
/// undeterminable or unadvertised `HEAD` simply leaves the ref alone.
fn set_head_from_remote(
    repo: &gix::Repository,
    remote_name: &str,
    follow: FollowRemoteHead,
    ref_map: &gix::remote::fetch::RefMap,
    opts: &FetchOpts,
) -> Result<()> {
    if follow == FollowRemoteHead::Never {
        return Ok(());
    }
    let heads = super::remote::remote_head_names(ref_map);
    // Zero or several candidates leave `HEAD` undetermined, which git treats as nothing to do.
    let [head_name] = heads.as_slice() else {
        return Ok(());
    };

    // A bare mirror keeps its own `HEAD` in step instead of a tracking `HEAD`.
    let bare_mirror = repo.worktree().is_none()
        && repo
            .config_snapshot()
            .boolean(&format!("remote.{remote_name}.mirror"))
            .unwrap_or(false);
    let (head_ref, target) = if bare_mirror {
        ("HEAD".to_string(), format!("refs/heads/{head_name}"))
    } else {
        (
            format!("refs/remotes/{remote_name}/HEAD"),
            format!("refs/remotes/{remote_name}/{head_name}"),
        )
    };
    if !bare_mirror && repo.try_find_reference(target.as_str())?.is_none() {
        return Ok(());
    }

    // `create_only` is what makes `create` and `warn` leave an existing `HEAD` where it is; only
    // `always` (and a bare mirror) rewrites it.
    let create_only = follow != FollowRemoteHead::Always && !bare_mirror;
    let (previous, was_detached) = super::remote::symref_prev(repo, head_ref.as_str())?;
    if !(create_only && previous.is_some()) {
        repo.edit_reference(RefEdit {
            change: Change::Update {
                log: LogChange {
                    mode: RefLog::AndReference,
                    force_create_reflog: false,
                    message: "fetch".into(),
                },
                expected: PreviousValue::Any,
                new: Target::Symbolic(full_ref_name(&target)?),
            },
            name: full_ref_name(&head_ref)?,
            deref: false,
        })?;
    }

    // `report_set_head()`, gated on `verbosity >= 0` exactly as git gates it.
    if let (FollowRemoteHead::Warn(no_warn_branch), false) = (&follow, opts.quiet) {
        if no_warn_branch.as_deref() != Some(head_name.as_str()) {
            report_set_head_warn(remote_name, head_name, previous.as_deref(), was_detached);
        }
    }
    Ok(())
}

/// git's `report_set_head()` plus the `advice.fetchSetHeadWarn` hint it ends with.
fn report_set_head_warn(remote: &str, head_name: &str, previous: Option<&str>, was_detached: bool) {
    let prefix = format!("refs/remotes/{remote}/");
    let tracked = previous.and_then(|p| p.strip_prefix(prefix.as_str()));
    match tracked {
        Some(prev_head) if prev_head != head_name => {
            println!("'HEAD' at '{remote}' is '{head_name}', but we have '{prev_head}' locally.");
        }
        _ if was_detached && previous.is_some_and(|p| !p.is_empty()) => {
            let previous = previous.unwrap_or_default();
            println!(
                "'HEAD' at '{remote}' is '{head_name}', but we have a detached HEAD pointing to '{previous}' locally."
            );
        }
        _ => return,
    }
    if !crate::advice::enabled("fetchRemoteHEADWarn") {
        return;
    }
    let mut lines = vec![
        format!("Run 'git remote set-head {remote} {head_name}' to follow the change, or set"),
        format!("'remote.{remote}.followRemoteHEAD' configuration option to a different value"),
        "if you do not want to see this message. Specifically running".to_string(),
        format!("'git config set remote.{remote}.followRemoteHEAD warn-if-not-branch-{head_name}'"),
        "will disable the warning until the remote changes HEAD to something else.".to_string(),
    ];
    // `advise_if_enabled()`'s trailer, which git appends only while the slot is unconfigured.
    let unconfigured = gix::discover(".")
        .map(|repo| {
            repo.config_snapshot()
                .boolean("advice.fetchRemoteHEADWarn")
                .is_none()
        })
        .unwrap_or(true);
    if unconfigured {
        lines.push(
            "Disable this message with \"git config set advice.fetchRemoteHEADWarn false\"".to_string(),
        );
    }
    for line in lines {
        eprintln!("hint: {line}");
    }
}

/// Validate a full ref name the way the ref edits in this module need it.
fn full_ref_name(name: &str) -> Result<FullName> {
    Ok(gix::refs::FullName::try_from(BString::from(name))?)
}

/// Apply the update gitoxide refused because the destination is checked out in a
/// worktree, which is what `-u`/`--update-head-ok` asks for.
///
/// The refspec's own force bit still decides whether a non-fast-forward is
/// allowed, so the outcome is one of git's ordinary summary rows: a fast-forward,
/// a forced update, "up to date", or a non-fast-forward rejection. `None` means
/// there is nothing to report (the ref already pointed at `new_id` and the
/// summary is not verbose).
fn update_checked_out_ref(
    repo: &gix::Repository,
    name: &FullName,
    new_id: gix::ObjectId,
    opts: &FetchOpts,
    allow_non_fast_forward: bool,
) -> Result<Option<(char, String, &'static str)>> {
    let existing = repo.find_reference(name.as_bstr())?;
    let old_id = existing.clone().peel_to_id()?.detach();
    if old_id == new_id {
        return Ok(if opts.verbose {
            Some(('=', "[up to date]".to_string(), ""))
        } else {
            None
        });
    }
    // A fast-forward is an update whose old value is an ancestor of the new one —
    // except that `--no-show-forced-updates` skips the check outright and treats
    // every update as one (`fast_forward = 1`, fetch.c:1046-1056). That is not
    // only a reporting shortcut: it decides which of git's three branches runs,
    // so it governs the rejection and the reflog message as well as the summary.
    let fast_forward = !opts.show_forced_updates
        || repo
            .merge_base(old_id, new_id)
            .map(|base| base.detach() == old_id)
            .unwrap_or(false);
    if !fast_forward && !allow_non_fast_forward {
        return Ok(Some((
            '!',
            "[rejected]".to_string(),
            "  (non-fast-forward)",
        )));
    }
    if !opts.dry_run {
        // `s_update_ref()` (fetch.c:641-655) composes every fetch reflog entry as
        // `<reflog action>: <what>`, where the action is `GIT_REFLOG_ACTION` or
        // the command line git rebuilt for itself, and `<what>` is the verdict
        // this branch reached — `fast-forward` or `forced-update`.
        let action = if fast_forward {
            "fast-forward"
        } else {
            "forced-update"
        };
        repo.edit_reference(RefEdit {
            change: Change::Update {
                log: LogChange {
                    mode: RefLog::AndReference,
                    force_create_reflog: false,
                    message: format!("{}: {action}", opts.reflog_action).into(),
                },
                expected: PreviousValue::MustExistAndMatch(Target::Object(old_id)),
                new: Target::Object(new_id),
            },
            name: name.clone(),
            // Deliberately not a deref: the destination is a branch, and the
            // matching `logs/HEAD` entry is the ref store's `split_head_update()`
            // to write, not this caller's.
            deref: false,
        })?;
    }
    let abbrev = abbrev_len(repo);
    let range = |sep: &str| {
        format!(
            "{}{sep}{}",
            old_id.to_hex_with_len(abbrev),
            new_id.to_hex_with_len(abbrev)
        )
    };
    Ok(Some(if fast_forward {
        (' ', range(".."), "")
    } else {
        ('+', range("..."), "  (forced update)")
    }))
}

/// `--recurse-submodules[=yes]`: run this binary's own `fetch` inside every
/// populated submodule, `--jobs` at a time.
///
/// git fetches in submodules with the superproject's flags; only the ones that
/// make sense below the top level are forwarded here (verbosity, prune, tags and
/// the recursion itself), since the superproject's refspecs and remote names do
/// not apply to a submodule. Returns `true` if any submodule fetch failed.
fn fetch_submodules(repo: &gix::Repository, opts: &FetchOpts) -> Result<bool> {
    let Some(modules) = repo.submodules()? else {
        return Ok(false);
    };
    let mut dirs: Vec<std::path::PathBuf> = Vec::new();
    for sm in modules {
        if !sm.is_active().unwrap_or(false) {
            continue;
        }
        // An unpopulated submodule has no repository to fetch into; git skips it.
        if matches!(sm.open(), Ok(Some(_))) {
            if let Ok(dir) = sm.work_dir() {
                dirs.push(dir);
            }
        }
    }
    if dirs.is_empty() {
        return Ok(false);
    }

    let exe = std::env::current_exe()?;
    let mut forwarded: Vec<String> = vec!["fetch".into(), "--recurse-submodules".into()];
    if opts.quiet {
        forwarded.push("--quiet".into());
    }
    if opts.verbose {
        forwarded.push("--verbose".into());
    }
    if opts.prune == Some(true) {
        forwarded.push("--prune".into());
    }
    if matches!(opts.tags, Some(Tags::All)) {
        forwarded.push("--tags".into());
    }
    if matches!(opts.tags, Some(Tags::None)) {
        forwarded.push("--no-tags".into());
    }
    forwarded.push(format!("--jobs={}", opts.jobs));
    // `add_options_to_argv()` forwards the address family into the submodule fetch too.
    match opts.address_family {
        Some(gix::protocol::transport::AddressFamily::V4) => forwarded.push("--ipv4".into()),
        Some(gix::protocol::transport::AddressFamily::V6) => forwarded.push("--ipv6".into()),
        None => {}
    }

    let failed = AtomicBool::new(false);
    let next = std::sync::atomic::AtomicUsize::new(0);
    let workers = opts.jobs.min(dirs.len()).max(1);
    std::thread::scope(|scope| {
        for _ in 0..workers {
            let (next, failed, dirs, exe, forwarded) =
                (&next, &failed, &dirs, &exe, &forwarded);
            scope.spawn(move || loop {
                let i = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                let Some(dir) = dirs.get(i) else { break };
                let status = std::process::Command::new(exe)
                    .arg("-C")
                    .arg(dir)
                    .args(forwarded)
                    .status();
                if !status.map(|s| s.success()).unwrap_or(false) {
                    failed.store(true, std::sync::atomic::Ordering::Relaxed);
                }
            });
        }
    });
    Ok(failed.load(std::sync::atomic::Ordering::Relaxed))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The `From <url>` line drops the userinfo, keeps the scheme, and loses the
    /// trailing `.git` — every row measured against stock git 2.55.0 rather than
    /// inferred, because the scp-like and URL spellings delimit the authority
    /// differently and only the measurement says where the `@` stops counting.
    #[test]
    fn the_from_line_url_drops_userinfo_and_keeps_the_scheme() {
        for (raw, want) in [
            // Measured: `git fetch` over each of these against a real remote.
            ("git@github.com:owner/repo.git", "github.com:owner/repo"),
            ("ssh://git@github.com/owner/repo.git", "ssh://github.com/owner/repo"),
            ("https://github.com/owner/repo.git", "https://github.com/owner/repo"),
            // A password is userinfo too, and must not reach the terminal.
            ("https://user:pw@example.com/x.git", "https://example.com/x"),
            // No userinfo: unchanged apart from the suffix rules.
            ("https://example.com/x/", "https://example.com/x"),
            ("/srv/local/repo.git", "/srv/local/repo"),
            // `.git` needs more than one character ahead of it to be a suffix.
            ("x.git", "x.git"),
            // An `@` in the PATH is not userinfo: the authority ended at the
            // first `:` (scp-like) or `/` (URL), so the path keeps its component.
            ("host:mail@archive", "host:mail@archive"),
            ("https://example.com/mail@archive", "https://example.com/mail@archive"),
        ] {
            assert_eq!(display_url(raw), want, "{raw}");
        }
    }

    /// git's prefetch filter moves the destination under `refs/prefetch/`,
    /// forces it, and drops the tag refspec entirely.
    #[test]
    fn prefetch_rewrites_destinations_and_drops_tags() {
        assert_eq!(
            prefetch_spec(BStr::new("+refs/heads/*:refs/remotes/origin/*"))
                .unwrap()
                .to_string(),
            "+refs/heads/*:refs/prefetch/remotes/origin/*"
        );
        assert_eq!(
            prefetch_spec(BStr::new("refs/heads/main:refs/heads/main"))
                .unwrap()
                .to_string(),
            "+refs/heads/main:refs/prefetch/heads/main"
        );
        assert!(prefetch_spec(BStr::new("refs/tags/*:refs/tags/*")).is_none());
        assert!(prefetch_spec(BStr::new("refs/heads/main")).is_none());
    }

    /// The compact `fetch.output` layout substitutes `*` for whichever of the
    /// two names is contained in the other.
    #[test]
    fn compact_substitutes_the_contained_name() {
        assert_eq!(
            compact("main", "origin/main"),
            ("main".to_string(), "origin/*".to_string())
        );
        assert_eq!(
            compact("origin/main", "main"),
            ("origin/*".to_string(), "main".to_string())
        );
        assert_eq!(
            compact("main", "other"),
            ("main".to_string(), "other".to_string())
        );
    }

    /// The FETCH_HEAD row is `<oid> TAB <not-for-merge|> TAB <kind> '<what>' of <url>`.
    #[test]
    fn fetch_head_rows_match_gits_layout() {
        let id = gix::ObjectId::null(gix::hash::Kind::Sha1);
        assert_eq!(
            fetch_head_note(id, true, "refs/heads/main", "/tmp/o"),
            format!("{}\t\tbranch 'main' of /tmp/o", id.to_hex())
        );
        assert_eq!(
            fetch_head_note(id, false, "refs/tags/v1", "/tmp/o"),
            format!("{}\tnot-for-merge\ttag 'v1' of /tmp/o", id.to_hex())
        );
        assert_eq!(
            fetch_head_note(id, false, "HEAD", "/tmp/o"),
            format!("{}\tnot-for-merge\t/tmp/o", id.to_hex())
        );
    }
}

/// `fetch.fsckObjects`: run the object-content message layer over everything the
/// fetch delivered, at the severities `fetch.fsck.<msg-id>` selects.
///
/// git does this inside the `index-pack --strict<list>` child `fetch_pack()`
/// starts (`fetch-pack.c:1061`), so the message text is `index-pack`'s spelling —
/// it names the object, not its type — and a failure kills the fetch before a
/// single ref moves. The child dies before renaming its temporary into
/// `objects/pack`, so nothing of the pack survives; gitoxide has already written
/// the pack by the time we get here, so it is removed on the way out. Confirmed
/// against git 2.55.0: a fetch that fails this check leaves an empty
/// `for-each-ref` and no `.pack`/`.idx` behind.
///
/// The two-phase structure and its two `die()`s are `index-pack`'s: the
/// per-object pass says `fsck error in packed object`, `fsck_finish()` says
/// `fsck error in pack objects`, and git dies at the first of the two it reaches.
/// See [`super::receive_pack`] for the same shape on the push side, including why
/// a `.gitmodules` blob's position in the pack decides which pass lints it.
fn fsck_fetched(
    repo: &gix::Repository,
    bundle: &gix::odb::pack::bundle::write::Outcome,
    msgs: &super::fsck::MsgConfig,
) -> std::result::Result<(), String> {
    use super::fsck::{big_file_threshold, check_blob, check_object, Severity};

    let discard = || {
        for path in [bundle.data_path.clone(), bundle.index_path.clone(), bundle.keep_path.clone()] {
            if let Some(path) = path {
                let _ = std::fs::remove_file(path);
            }
        }
    };

    // `--strict=<list>`'s own `die()`s: the demote rule and an unreadable
    // skip list are both reached inside the child, not while `fetch` read its
    // configuration. See `MsgConfig::deferred_fatal`.
    if let Some(text) = &msgs.deferred_fatal {
        discard();
        return Err(text.clone());
    }

    let (Some(index_path), Some(_)) = (&bundle.index_path, &bundle.data_path) else {
        return Ok(());
    };
    let index = match gix::odb::pack::index::File::at(index_path, repo.object_hash()) {
        Ok(index) => index,
        Err(e) => {
            discard();
            return Err(e.to_string());
        }
    };
    // Pack order, not index order: which pass lints a named blob depends on
    // whether the naming tree came earlier *in the pack*.
    let mut entries: Vec<(u64, gix::ObjectId)> =
        index.iter().map(|e| (e.pack_offset, e.oid)).collect();
    entries.sort_unstable();

    let threshold = big_file_threshold(repo);
    let mut failed = false;
    let mut gitmodules: std::collections::HashSet<gix::ObjectId> = Default::default();
    let mut gitattributes: std::collections::HashSet<gix::ObjectId> = Default::default();
    let mut done: std::collections::HashSet<gix::ObjectId> = Default::default();
    let mut report = |finding: &super::fsck::Finding, id: &gix::ObjectId, failed: &mut bool| {
        match msgs.severity(finding, id) {
            Severity::Ignore => {}
            Severity::Info | Severity::Warn => {
                eprintln!("warning: object {id}: {}: {}", finding.msg.id, finding.text);
            }
            Severity::Error | Severity::Fatal => {
                eprintln!("error: object {id}: {}: {}", finding.msg.id, finding.text);
                *failed = true;
            }
        }
    };

    // `parse_pack_objects()`'s delay list (`builtin/index-pack.c:1279`): a blob
    // over `core.bigFileThreshold` is inflated into a fixed scratch buffer and
    // handed to `fsck_object()` as `NULL` only after the whole pack has been read
    // (`builtin/index-pack.c:1308`), so it is checked against the complete
    // `gitmodules_found` set no matter where it sat in the pack. That null buffer
    // is the only thing that ever reports `gitmodulesLarge`.
    let mut delayed: Vec<gix::ObjectId> = Vec::new();
    for (_, id) in &entries {
        let Ok(object) = repo.find_object(*id) else { continue };
        if object.kind == gix::object::Kind::Blob {
            if object.data.len() as u64 > threshold {
                delayed.push(*id);
                continue;
            }
            let as_modules = gitmodules.contains(id);
            let as_attrs = gitattributes.contains(id);
            if as_modules || as_attrs {
                done.insert(*id);
                for finding in check_blob(Some(&object.data), as_modules, as_attrs) {
                    report(&finding, id, &mut failed);
                }
            }
            continue;
        }
        let checked = check_object(object.kind, &object.data, true, repo.object_hash().len_in_hex());
        for line in &checked.raw {
            eprintln!("{line}");
        }
        gitmodules.extend(checked.gitmodules);
        gitattributes.extend(checked.gitattributes);
        for finding in &checked.findings {
            report(finding, id, &mut failed);
        }
    }
    for id in &delayed {
        let as_modules = gitmodules.contains(id);
        let as_attrs = gitattributes.contains(id);
        if !as_modules && !as_attrs {
            continue;
        }
        done.insert(*id);
        for finding in check_blob(None, as_modules, as_attrs) {
            report(&finding, id, &mut failed);
        }
    }

    // `fsck_finish()`: every blob the trees named that the per-object pass did
    // not already lint, whether or not the pack carried it.
    let failed_before_finish = failed;
    let mut queue: Vec<gix::ObjectId> = entries
        .iter()
        .map(|(_, id)| *id)
        .filter(|id| !done.contains(id))
        .filter(|id| gitmodules.contains(id) || gitattributes.contains(id))
        .collect();
    let mut rest: Vec<gix::ObjectId> = gitmodules
        .union(&gitattributes)
        .copied()
        .filter(|id| !done.contains(id) && !queue.contains(id))
        .collect();
    rest.sort();
    queue.append(&mut rest);

    for id in queue {
        let as_modules = gitmodules.contains(&id);
        let as_attrs = gitattributes.contains(&id);
        // `fsck_blobs()` reads the whole object (`fsck.c:1337`), so no blob is
        // streamed here; it reports an unreadable or non-blob object once per
        // sweep that named it.
        let (missing, non_blob) = match repo.find_object(id) {
            Ok(object) if object.kind == gix::object::Kind::Blob => {
                for finding in check_blob(Some(&object.data), as_modules, as_attrs) {
                    report(&finding, &id, &mut failed);
                }
                continue;
            }
            Ok(_) => (false, true),
            Err(_) => (true, false),
        };
        for (present, missing_msg, blob_msg, label) in [
            (
                as_modules,
                &super::fsck::GITMODULES_MISSING,
                &super::fsck::GITMODULES_BLOB,
                ".gitmodules",
            ),
            (
                as_attrs,
                &super::fsck::GITATTRIBUTES_MISSING,
                &super::fsck::GITATTRIBUTES_BLOB,
                ".gitattributes",
            ),
        ] {
            if !present {
                continue;
            }
            let finding = if missing {
                super::fsck::Finding { msg: missing_msg, text: format!("unable to read {label} blob") }
            } else {
                debug_assert!(non_blob);
                super::fsck::Finding { msg: blob_msg, text: format!("non-blob found at {label}") }
            };
            report(&finding, &id, &mut failed);
        }
    }

    if failed {
        discard();
        return Err(if failed_before_finish {
            "fsck error in packed object".into()
        } else {
            "fsck error in pack objects".into()
        });
    }
    Ok(())
}

/// `fetch_pack()`'s `unpack-objects` path: a pack carrying fewer objects than
/// `fetch.unpackLimit` (falling back to `transfer.unpackLimit`, then git's 100) is
/// exploded into loose objects and dropped, because indexing a tiny pack costs more
/// than the objects are worth.
///
/// gitoxide always indexes, so the pack is written first and taken apart here; the
/// object database ends up holding what git's would.
fn explode_small_pack(
    repo: &gix::Repository,
    bundle: &gix::odb::pack::bundle::write::Outcome,
) -> Result<()> {
    let limit = {
        let snap = repo.config_snapshot();
        snap.integer("fetch.unpackLimit")
            .or_else(|| snap.integer("transfer.unpackLimit"))
            .unwrap_or(100)
    };
    // `0` disables the shortcut, and a negative value is git's "always unpack".
    if limit == 0 {
        return Ok(());
    }
    let count = bundle.index.num_objects;
    if limit > 0 && u64::from(count) >= limit as u64 {
        return Ok(());
    }
    let (Some(index_path), Some(data_path)) = (&bundle.index_path, &bundle.data_path) else {
        return Ok(());
    };

    use gix::objs::Write as _;
    let pack = gix::odb::pack::Bundle::at(index_path, repo.object_hash())?;
    let mut buf = Vec::with_capacity(64 * 1024);
    let mut inflate = gix::zlib::Inflate::default();
    let mut cache = gix::odb::pack::cache::Never;
    for idx in 0..pack.index.num_objects() {
        let id = pack.index.oid_at_index(idx).to_owned();
        let (object, _) = pack.get_object_by_index(idx, &mut buf, &mut inflate, &mut cache)?;
        repo.objects
            .write_buf_with_known_id(object.kind, object.data, id)
            .map_err(|e| anyhow::anyhow!("{e}"))?;
    }
    drop(pack);

    // The `.keep` file guards the pack until its refs point at it; with the objects
    // loose there is nothing left to guard.
    for path in [
        Some(data_path.clone()),
        Some(index_path.clone()),
        bundle.keep_path.clone(),
        Some(data_path.with_extension("rev")),
    ]
    .into_iter()
    .flatten()
    {
        let _ = std::fs::remove_file(path);
    }
    Ok(())
}
