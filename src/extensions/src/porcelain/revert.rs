//! `git revert <commit>...` — record new commits that undo earlier ones.
//!
//! A revert is a three-way merge with the roles rotated: the *base* is the tree
//! of the commit being reverted, *ours* is the current `HEAD` tree, and *theirs*
//! is the tree of that commit's parent. Applying it yields a tree in which the
//! reverted commit's changes are backed out while later work is preserved.
//!
//! The option grammar mirrors git's two-phase handling, because the order in
//! which git rejects things is observable:
//!
//!   * **parse phase**, left to right — `-m`/`--mainline` validates its value
//!     immediately (`expects a number greater than zero`, exit 129), and the
//!     `--quit`/`--continue`/`--abort`/`--skip` command modes reject a second,
//!     different mode on the spot (exit 129). Unknown options are *kept*, as
//!     git does, so they are not diagnosed until the operand phase.
//!   * **post-parse phase**, in a fixed order — `--cleanup` mode validation
//!     (`fatal: Invalid cleanup mode <arg>`, exit 128), then the
//!     command-mode/option compatibility list, then the "a command mode takes
//!     no operands" usage error, then "no operands at all" usage error.
//!
//! That ordering is why `--cleanup=bogus --mainline=0` reports the mainline
//! problem while `--cleanup=bogus --abort -n` reports the cleanup one.
//!
//! What this port covers, byte-for-byte against stock git:
//!   * `git revert <commit>...`, including `<a>..<b>` ranges, `<a>...<b>`,
//!     `^<commit>` exclusions, the `<a>^!`/`<a>^@`/`<a>^-[<n>]` parent
//!     spellings and the `--not`/`--all`/`--branches`/`--tags`/`--remotes`/
//!     `--glob=`/`--exclude=` pseudo-revisions, resolved through the one
//!     revision walk [`crate::sequencer::prepare_revs`] runs, as git's sequencer
//!     does. A revert never reverses that walk, so a range is backed out newest
//!     first — the opposite of `cherry-pick`
//!   * `-n`/`--no-commit`, `-s`/`--signoff`, `-m <n>`/`--mainline <n>`,
//!     `-e`/`--edit`, `--no-edit`, `--reference`, `--cleanup=<mode>`
//!   * `--strategy`/`-X`, which git's sequencer ignores outright for a revert:
//!     `do_pick_commit` routes `TODO_REVERT` to the recursive merge regardless
//!     of the selected strategy, so an unknown strategy name is not an error
//!   * `--rerere-autoupdate`/`--no-rerere-autoupdate` and `--no-gpg-sign`,
//!     accepted and without effect on a conflict-free revert
//!   * the generated message (`Revert "…"` / `Reapply "…"`, the reference
//!     format `<comment> *** SAY WHY … ***` plus `<short> (<subject>, <date>)`, the
//!     `, reversing / changes made to` merge variant, the `Signed-off-by`
//!     trailer) and the `--cleanup` mode applied to it
//!   * the summary block (`[<branch> <short-oid>] <subject>`, the ` Date:` line
//!     the sequencer always prints, the short-stat — gitlink changes included,
//!     which the blob differ cannot see — and create/delete/mode lines)
//!   * `--no-commit` merging against the index rather than `HEAD`, so a
//!     pre-existing staged change is carried through and repeated `-n` steps
//!     stack; the index tree and the merge result tree are written to the object
//!     database before the checkout, as git's do, so even a refused `-n` revert
//!     leaves the same objects behind
//!   * the `revert: <subject>` reflog message, and the `REVERT_HEAD`,
//!     `MERGE_MSG` and `AUTO_MERGE` files written by `--no-commit`
//!   * the refusal paths in git's own order: bad revision, an unmerged index,
//!     an index that differs from `HEAD`, merge without `-m`, missing parent,
//!     and affected files that are locally modified or would clobber untracked
//!     files — same text, same exit codes (128/129)
//!   * the whole sequencer, on disk and in the verbs. Two or more operands — or
//!     any range/`^`-exclusion, which git walks — take
//!     `sequencer_pick_revisions`' non-`single_pick` path, so
//!     [`crate::sequencer`] writes `head`, `opts` and `abort-safety` up front
//!     and rewrites `todo` before every instruction. `--continue` commits the
//!     stopped revert and replays the rest; `--abort` rewinds to
//!     `sequencer/head` when `abort-safety` still matches `HEAD` and warns
//!     `You seem to have moved HEAD.` when it does not; `--skip` resets the
//!     stopped revert away and resumes; `--quit` drops the state. With no
//!     sequence live each falls back to its single-pick form, which is where the
//!     "nothing in progress" refusals come from
//!   * a no-op single pick against a dirty worktree, which git finishes by
//!     running `git commit` — nothing to commit, so it prints the working-tree
//!     status (byte-identical to `git status`) and exits 1; served by delegating
//!     to the ported `status` driver
//!   * the full three-way merge through `gix`'s tree merge (`gix-merge`, enabled
//!     by this crate's `merge` feature): content-level blob merges and rename
//!     following are served, not approximated. A path both sides changed away
//!     from the base is merged hunk-by-hunk. An unresolved conflict stops the
//!     revert exactly as git's sequencer does: the merge result — `<<<<<<<` /
//!     `>>>>>>>` markers included — is checked out, the conflicting paths get
//!     stage 1/2/3 index entries, an `Auto-merging`/`CONFLICT (...)` line goes to
//!     stdout, `REVERT_HEAD` and a `MERGE_MSG` carrying the `# Conflicts:` hint
//!     are written, and the exit status is 1
//!   * `AUTO_MERGE`: `merge_switch_to_result()` records the merged tree for
//!     every result merge-ort checks out, so it is written for a *successful*
//!     revert too and survives the commit — `git rev-parse AUTO_MERGE` resolves
//!     after `git revert HEAD`. A revert always takes the in-process merge
//!     (`do_pick_commit` routes `TODO_REVERT` there whatever `--strategy` says),
//!     so there is no strategy-child exception as there is for `cherry-pick`.
//!
//!   * `-e`/`--edit` and the tri-state default behind it. `should_edit()`
//!     (sequencer.c:2203-2212) edits when `-e` was given, never when `--no-edit`
//!     was, and for an unqualified revert only at a terminal — a redirected stdin
//!     silently means "no editor", which is what makes scripted reverts quiet.
//!     When it does edit, `do_commit()` stops writing the object itself and
//!     delegates the whole commit to `git commit -e` (sequencer.c:1728,1750-1754),
//!     which is what [`super::replay_commit`] reproduces. That is observable well
//!     past the message: the summary loses the ` Date:` line the sequencer's own
//!     `SUMMARY_SHOW_AUTHOR_DATE` adds, `AUTO_MERGE` does not survive, a failing
//!     or emptying editor aborts at 1, and the reflog still reads `revert:`
//!     because the child inherits `GIT_REFLOG_ACTION`.
//!
//!   * `-S`/`--gpg-sign[=<key-id>]`, `--no-gpg-sign` and the `commit.gpgSign`
//!     default behind them. `opts->gpg_sign` reaches `commit_tree_extended()`
//!     directly on the in-process path (sequencer.c:1685) and is spelled back out
//!     as `-S<key>` on the editor path (sequencer.c:1157-1160), so both arms sign
//!     with the same key; `save_opts()` records it as `options.gpg-sign` so a
//!     resumed sequence keeps signing. A stopped revert's `--continue` is the one
//!     exception, and it is git's: `continue_single_pick()` spawns a plain
//!     `git commit` with no `-S` (sequencer.c:5232-5257), so only `commit.gpgSign`
//!     applies there. The same spawn is why a resumed revert's reflog line reads
//!     `commit: Revert "…"` and not `revert: …` — the child names itself, and
//!     `sequencer_determine_whence()` does not look at `REVERT_HEAD`; see
//!     [`super::replay_commit::continue_reflog_action`].

use anyhow::Result;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::process::ExitCode;
use std::sync::atomic::AtomicBool;

use gix::bstr::{BStr, BString, ByteSlice};
use gix::hash::ObjectId;
use gix::index::entry::{Flags, Mode, Stat};
use gix::objs::tree::{EntryKind, EntryMode};
use gix::prelude::ObjectIdExt;
use gix::refs::transaction::{Change, LogChange, PreviousValue, RefEdit, RefLog};
use gix::refs::Target;

/// Verbatim `git revert -h` text, printed on a usage error exactly as git does.
///
/// The trailing blank line is git's, not padding: `usage_with_options_internal()`
/// closes every block with `fputc('\n', outfile)` after the last option.
const USAGE: &str = "\
usage: git revert [--[no-]edit] [-n] [-m <parent-number>] [-s] [-S[<keyid>]] <commit>...
   or: git revert (--continue | --skip | --abort | --quit)

    --quit                end revert or cherry-pick sequence
    --continue            resume revert or cherry-pick sequence
    --abort               cancel revert or cherry-pick sequence
    --skip                skip current commit and continue
    --[no-]cleanup <mode> how to strip spaces and #comments from message
    -n, --no-commit       don't automatically commit
    --commit              opposite of --no-commit
    -e, --[no-]edit       edit the commit message
    -s, --[no-]signoff    add a Signed-off-by trailer
    -m, --[no-]mainline <parent-number>
                          select mainline parent
    --[no-]rerere-autoupdate
                          update the index with reused conflict resolution if possible
    --[no-]strategy <strategy>
                          merge strategy
    -X, --[no-]strategy-option <option>
                          option for merge strategy
    -S, --[no-]gpg-sign[=<key-id>]
                          GPG sign commit
    --[no-]reference      use the 'reference' format to refer to commits

";

/// `usage_with_options_internal()`'s `USAGE_FULL` rendering — what `--help-all`
/// prints. It is [`USAGE`] with the `PARSE_OPT_HIDDEN` entries left in:
/// `-r`.
/// Captured byte-for-byte from stock git 2.55.0's `git revert --help-all`.
const USAGE_ALL: &str = r#"usage: git revert [--[no-]edit] [-n] [-m <parent-number>] [-s] [-S[<keyid>]] <commit>...
   or: git revert (--continue | --skip | --abort | --quit)

    --quit                end revert or cherry-pick sequence
    --continue            resume revert or cherry-pick sequence
    --abort               cancel revert or cherry-pick sequence
    --skip                skip current commit and continue
    --[no-]cleanup <mode> how to strip spaces and #comments from message
    -n, --no-commit       don't automatically commit
    --commit              opposite of --no-commit
    -e, --[no-]edit       edit the commit message
    -r                    no-op (backward compatibility)
    -s, --[no-]signoff    add a Signed-off-by trailer
    -m, --[no-]mainline <parent-number>
                          select mainline parent
    --[no-]rerere-autoupdate
                          update the index with reused conflict resolution if possible
    --[no-]strategy <strategy>
                          merge strategy
    -X, --[no-]strategy-option <option>
                          option for merge strategy
    -S, --[no-]gpg-sign[=<key-id>]
                          GPG sign commit
    --[no-]reference      use the 'reference' format to refer to commits

"#;

/// The title git puts on a `--reference` revert, left for the user to replace.
///
/// ```c
/// strbuf_commented_addf(message, comment_line_str,
///                       "*** SAY WHY WE ARE REVERTING ON THE TITLE LINE ***");
/// ```
///
/// (sequencer.c:5647.) The prefix is `comment_line_str`, so `core.commentChar`
/// / `core.commentString` decides it — this is the body, without one.
const REFERENCE_TITLE: &str = "*** SAY WHY WE ARE REVERTING ON THE TITLE LINE ***";

/// A flattened tree: repository-relative path → (blob/tree-leaf id, entry kind).
type Flat = BTreeMap<BString, (ObjectId, EntryKind)>;

/// The `--quit`/`--continue`/`--abort`/`--skip` command modes. Exactly one may
/// be in effect; a second, different one is a usage error.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Cmd {
    Quit,
    Continue,
    Abort,
    Skip,
}

impl Cmd {
    fn flag(self) -> &'static str {
        match self {
            Cmd::Quit => "--quit",
            Cmd::Continue => "--continue",
            Cmd::Abort => "--abort",
            Cmd::Skip => "--skip",
        }
    }
}

/// How `--cleanup` says the message should be tidied before committing.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Cleanup {
    Verbatim,
    Whitespace,
    Strip,
    Scissors,
    Default,
}

impl Cleanup {
    fn parse(arg: &str) -> Option<Cleanup> {
        Some(match arg {
            "verbatim" => Cleanup::Verbatim,
            "whitespace" => Cleanup::Whitespace,
            "strip" => Cleanup::Strip,
            "scissors" => Cleanup::Scissors,
            "default" => Cleanup::Default,
            _ => return None,
        })
    }

    /// git's `describe_cleanup_mode()`, the spelling `save_opts` records.
    /// `builtin/revert.c` resolves the mode with `get_cleanup_mode(arg, 1)`, so
    /// `default` names `strip`.
    fn name(self) -> &'static str {
        match self {
            Cleanup::Verbatim => "verbatim",
            Cleanup::Whitespace => "whitespace",
            Cleanup::Strip | Cleanup::Default => "strip",
            Cleanup::Scissors => "scissors",
        }
    }
}

/// Everything the option parser collects, in git's own shape.
#[derive(Default)]
struct Options {
    no_commit: bool,
    signoff: bool,
    /// `opts->edit`, git's tri-state: `None` is the C's `-1` — never given — and
    /// is what makes an unqualified `git revert` edit only at a terminal
    /// (`should_edit()`, sequencer.c:2203-2212). `save_opts` writes the key for
    /// either explicit spelling and omits it otherwise, which is the same state.
    edit_given: Option<bool>,
    reference: bool,
    /// 0 means "not given"; git stores it the same way.
    mainline: usize,
    cleanup: Option<String>,
    /// `opts->strategy`. A revert never *runs* a strategy — `do_pick_commit()`
    /// routes `TODO_REVERT` through merge-ort whatever this says — but
    /// `save_opts()` still records it, so a stopped sequence remembers the name
    /// across a `--continue`.
    strategy: Option<String>,
    /// `opts->xopts`. Reported as incompatible with a command mode, and
    /// persisted by `save_opts()`; never consulted while reverting.
    xopts: Vec<String>,
    /// `Some(true)` for `--rerere-autoupdate`, `Some(false)` for the negation.
    rerere: Option<bool>,
    /// `opts->gpg_sign`, whose three states git keeps in one `char *`: `None` is
    /// NULL (do not sign), `Some("")` is the empty string `-S` and
    /// `commit.gpgSign` both leave behind (sign with `get_signing_key()`'s
    /// choice), and `Some(key)` is `-S<key>`.
    gpg_sign: Option<String>,
    mode: Option<Cmd>,
}

pub fn revert(args: &[String]) -> Result<ExitCode> {
    // `dispatch` hands over the operand list without the verb; tolerate a
    // leading literal `revert` so the module also works if it is ever wired
    // with the full argv.
    let args = match args.first() {
        Some(a) if a == "revert" => &args[1..],
        _ => args,
    };

    let mut o = Options::default();
    // git keeps unrecognized options in the operand list (`PARSE_OPT_KEEP_UNKNOWN_OPT`)
    // and only diagnoses them once the revision parser gets to them.
    let mut specs: Vec<String> = Vec::new();
    let mut no_more_opts = false;
    // git reads `revert.reference` in `git_revert_config` before parse_options,
    // so it is only the default: an explicit `--reference`/`--no-reference` on
    // the command line wins. Track whether either was seen so the config is
    // applied only when neither was.
    let mut gpg_sign_explicit = false;
    let mut reference_explicit = false;

    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();
        if no_more_opts || !a.starts_with('-') || a == "-" {
            specs.push(a.to_string());
            i += 1;
            continue;
        }
        match a {
            "--" => no_more_opts = true,
            // parse_options_step() answers `-h` where it meets it, on stdout at
            // 129 — `usage_error()`'s stderr is for rejections only.
            "-h" => return Ok(super::show_usage(USAGE)),
            // `if (internal_help && !strcmp(arg + 2, "help-all"))`
            // (parse-options.c:1122): an exact match, never an abbreviation and
            // never with an `=<value>`, rendering `USAGE_FULL`.
            "--help-all" => return Ok(super::show_usage(USAGE_ALL)),
            "-n" | "--no-commit" => o.no_commit = true,
            "--commit" => o.no_commit = false,
            "-s" | "--signoff" => o.signoff = true,
            "--no-signoff" => o.signoff = false,
            "-e" | "--edit" => o.edit_given = Some(true),
            "--no-edit" => o.edit_given = Some(false),
            "--reference" => {
                o.reference = true;
                reference_explicit = true;
            }
            "--no-reference" => {
                o.reference = false;
                reference_explicit = true;
            }
            "--rerere-autoupdate" => o.rerere = Some(true),
            "--no-rerere-autoupdate" => o.rerere = Some(false),
            // `xstrdup_or_null(NULL)` for the negation (builtin/revert.c:256-259).
            "--no-gpg-sign" => {
                o.gpg_sign = None;
                gpg_sign_explicit = true;
            }
            "--no-mainline" => o.mainline = 0,
            "--no-cleanup" => o.cleanup = None,
            "--no-strategy" => o.strategy = None,
            "--no-strategy-option" => o.xopts.clear(),
            // `optname()` follows the spelling that was typed, so `-m` is
            // ``switch `m'`` and `--mainline` is ``option `mainline'``
            // (parse-options.c:30-45); naming the long form for both was wrong
            // for every short spelling here.
            "-m" | "--mainline" => {
                i += 1;
                let v = super::value_at(args, i, a)?;
                match parse_mainline(v) {
                    Some(n) => o.mainline = n,
                    None => return Ok(bad_mainline()),
                }
            }
            "--cleanup" => {
                i += 1;
                o.cleanup = Some(super::value_at(args, i, a)?.to_string());
            }
            "--strategy" => {
                i += 1;
                o.strategy = Some(super::value_at(args, i, a)?.to_string());
            }
            "-X" | "--strategy-option" => {
                i += 1;
                o.xopts.push(super::value_at(args, i, a)?.to_string());
            }
            "--quit" => {
                if let Some(code) = set_mode(&mut o, Cmd::Quit) {
                    return Ok(code);
                }
            }
            "--continue" => {
                if let Some(code) = set_mode(&mut o, Cmd::Continue) {
                    return Ok(code);
                }
            }
            "--abort" => {
                if let Some(code) = set_mode(&mut o, Cmd::Abort) {
                    return Ok(code);
                }
            }
            "--skip" => {
                if let Some(code) = set_mode(&mut o, Cmd::Skip) {
                    return Ok(code);
                }
            }
            _ if a.starts_with("--mainline=") => match parse_mainline(&a["--mainline=".len()..]) {
                Some(n) => o.mainline = n,
                None => return Ok(bad_mainline()),
            },
            _ if a.starts_with("-m") && !a.starts_with("--") => match parse_mainline(&a[2..]) {
                Some(n) => o.mainline = n,
                None => return Ok(bad_mainline()),
            },
            _ if a.starts_with("--cleanup=") => {
                o.cleanup = Some(a["--cleanup=".len()..].to_string());
            }
            _ if a.starts_with("--strategy-option=") => {
                o.xopts.push(a["--strategy-option=".len()..].to_string());
            }
            _ if a.starts_with("-X") && !a.starts_with("--") => {
                o.xopts.push(a[2..].to_string());
            }
            _ if a.starts_with("--strategy=") => {
                o.strategy = Some(a["--strategy=".len()..].to_string());
            }
            // ```c
            // { .type = OPTION_STRING, .short_name = 'S', .long_name = "gpg-sign",
            //   .value = &gpg_sign, .flags = PARSE_OPT_OPTARG, .defval = (intptr_t) "" }
            // ```
            //
            // (builtin/revert.c:136-145.) `PARSE_OPT_OPTARG` takes a value only
            // when it is attached, so `-S <rev>` signs with the default key and
            // leaves `<rev>` an operand; `.defval = ""` is what makes the bare
            // form sign at all rather than parse as `--no-gpg-sign`.
            "-S" | "--gpg-sign" => {
                o.gpg_sign = Some(String::new());
                gpg_sign_explicit = true;
            }
            _ if a.starts_with("--gpg-sign=") => {
                o.gpg_sign = Some(a["--gpg-sign=".len()..].to_string());
                gpg_sign_explicit = true;
            }
            _ if a.starts_with("-S") && !a.starts_with("--") => {
                o.gpg_sign = Some(a[2..].to_string());
                gpg_sign_explicit = true;
            }
            // Unknown: git keeps it for the revision parser, which then fails
            // with the usage text. Mirror that by deferring the diagnosis.
            _ => specs.push(a.to_string()),
        }
        i += 1;
    }

    // Post-parse, in git's order: cleanup mode, then command-mode compatibility.
    let cleanup = match o.cleanup.as_deref() {
        None => None,
        Some(arg) => match Cleanup::parse(arg) {
            Some(c) => Some(c),
            None => {
                eprintln!("fatal: Invalid cleanup mode {arg}");
                return Ok(ExitCode::from(128));
            }
        },
    };

    let repo = gix::discover(".")?;
    if repo.workdir().is_none() {
        eprintln!("fatal: this operation must be run in a work tree");
        return Ok(ExitCode::from(128));
    }
    // `revert.reference` is the default for `--reference`; an explicit flag on
    // the command line already set `o.reference` and takes precedence.
    if !reference_explicit {
        if let Some(v) = repo.config_snapshot().boolean("revert.reference") {
            o.reference = v;
        }
    }
    // ```c
    // if (!strcmp(k, "commit.gpgsign")) {
    //         free(opts->gpg_sign);
    //         opts->gpg_sign = git_config_bool(k, v) ? xstrdup("") : NULL;
    //         return 0;
    // }
    // ```
    //
    // (sequencer.c:302-306, `git_sequencer_config`.) `sequencer_init_config()`
    // runs before `parse_args()`, so this is a *default* that either spelling of
    // the flag overrides. Ignoring it was not a missing flag but a silent one: a
    // repository that asks for signed commits got unsigned revert commits at
    // exit 0, with nothing said.
    if !gpg_sign_explicit && repo.config_snapshot().boolean("commit.gpgSign") == Some(true) {
        o.gpg_sign = Some(String::new());
    }
    // Every step below mutates the index, the worktree and a ref: serialize the
    // whole sequence through the repo coordinator, as the other writers do.
    let _lock = crate::lock::RepoLock::acquire(repo.git_dir());

    if let Some(mode) = o.mode {
        // git's `verify_opt_compatible` walks this list and reports the first
        // option that is set. Its `"--strategy", opts->strategy ? 1 : 0` entry
        // is absent here because it is dead in stock too: `run_sequencer`
        // parses `--strategy` into a local pointer and only copies it into
        // `opts->strategy` after this check runs (`builtin/revert.c:116,133`
        // versus `:211` and `:260-262`), so the field is still NULL when it is
        // read and `git revert --strategy=ort --quit` exits 0.
        for (name, active) in [
            ("--no-commit", o.no_commit),
            ("--signoff", o.signoff),
            ("--mainline", o.mainline != 0),
            ("--strategy-option", !o.xopts.is_empty()),
            ("--rerere-autoupdate", o.rerere == Some(true)),
            ("--no-rerere-autoupdate", o.rerere == Some(false)),
        ] {
            if active {
                eprintln!("fatal: revert: {name} cannot be used with {}", mode.flag());
                return Ok(ExitCode::from(128));
            }
        }
        if !specs.is_empty() {
            return Ok(usage_error());
        }
        return run_mode(&repo, mode);
    }

    if specs.is_empty() {
        return Ok(usage_error());
    }
    // Options git did not recognize reach the revision parser. It scans the
    // operand list left to right: a bad *revision* is diagnosed the moment it is
    // reached (`fatal: bad revision …`, exit 128), while an unrecognized dash
    // operand is only deferred and reported as the usage text (exit 129) after
    // the whole list is walked. So a bad revision outranks any dash token that
    // follows it — the diagnosis order is handled inside `resolve_specs`, not by
    // a blanket pre-scan here.

    let revs = crate::sequencer::prepare_revs(&repo, &specs, crate::sequencer::Action::Revert)?;
    // Everything `setup_revisions()` itself refuses — a bad revision, an absent
    // full-length object name, an unusable range — dies at 128 in the same
    // wording `cherry-pick` uses, since neither command has been reached yet.
    if let Some(message) = revs.setup_revisions_fatal() {
        eprint!("{message}");
        return Ok(ExitCode::from(128));
    }
    let (commits, sequencer) = match revs {
        crate::sequencer::Revs::Picks { commits, sequencer } => (commits, sequencer),
        // `sequencer_pick_revisions()`'s pending-object loop, whose message
        // says "cherry-pick" for a revert too.
        crate::sequencer::Revs::NotACommit { name, kind } => {
            return Ok(sequencer_failed(&format!("{name}: can't cherry-pick a {kind}")));
        }
        crate::sequencer::Revs::UnknownOption => return Ok(usage_error()),
        crate::sequencer::Revs::BadRevision(_)
        | crate::sequencer::Revs::BadObject(_)
        | crate::sequencer::Revs::InvalidRange { .. } => {
            unreachable!("reported by setup_revisions_fatal above")
        }
    };
    // `walk_revs_populate_todo`'s tail: a selection that names no commit at all
    // is refused before any sequencer state is written.
    if commits.is_empty() {
        return Ok(sequencer_failed("empty commit set passed"));
    }

    let todo = build_todo(&repo, &commits)?;
    if sequencer {
        // `create_seq_dir()` / `save_head()` / `save_opts()` /
        // `update_abort_safety_file()`, in `sequencer_pick_revisions()`'s order.
        let git_dir = repo.git_dir();
        if crate::sequencer::create(git_dir)?.is_err() {
            eprintln!("fatal: revert failed");
            return Ok(ExitCode::from(128));
        }
        let head = repo.head_id()?.detach();
        crate::sequencer::save_head(git_dir, head)?;
        let xopts: Vec<&str> = o.xopts.iter().map(String::as_str).collect();
        crate::sequencer::save_opts(git_dir, &saved_opts(&o, &xopts, cleanup))?;
        crate::sequencer::update_abort_safety_file(&repo)?;
    }
    // With `-n` nothing is committed between steps; each further revert stacks
    // because it re-reads the index the previous one left behind.
    run_todo(&repo, &o, cleanup, &todo, 0, sequencer)
}

/// `walk_revs_populate_todo`: one `revert <abbrev> <subject>` instruction per
/// commit, in replay order.
fn build_todo(
    repo: &gix::Repository,
    commits: &[ObjectId],
) -> Result<Vec<crate::sequencer::TodoItem>> {
    commits
        .iter()
        .map(|id| {
            let commit = repo.find_commit(*id)?;
            let subject = gix::objs::commit::MessageRef::from_bytes(commit.message_raw()?)
                .summary()
                .to_str_lossy()
                .into_owned();
            Ok(crate::sequencer::TodoItem {
                action: crate::sequencer::Action::Revert,
                oid: *id,
                abbrev: id.attach(repo).shorten_or_id().to_string(),
                subject,
            })
        })
        .collect()
}

/// `save_opts`'s view of the command line. A revert reaches far fewer of the
/// keys than a cherry-pick does — `--reference` is `opts->commit_use_reference`,
/// which `save_opts` does not persist at all, so it is lost across a
/// `--continue` in stock too.
///
/// `--strategy` and `-X` are persisted even though a revert never runs a
/// strategy: `save_opts()` writes `options.strategy` and
/// `options.strategy-option` from `opts->strategy`/`opts->xopts` with no regard
/// for the action (sequencer.c). Dropping them left a stopped
/// `git revert --strategy=<x> A B` with no `.git/sequencer/opts` at all where
/// stock has one — the fuzz corpus found it as
/// `revert --strategy=<TAB> -- HEAD^ v0.1.0 HEAD`.
fn saved_opts<'a>(
    o: &'a Options,
    xopts: &'a [&'a str],
    cleanup: Option<Cleanup>,
) -> crate::sequencer::SavedOpts<'a> {
    crate::sequencer::SavedOpts {
        no_commit: o.no_commit,
        edit: o.edit_given,
        signoff: o.signoff,
        mainline: o.mainline as u32,
        strategy: o.strategy.as_deref(),
        xopts,
        allow_rerere_auto: o.rerere,
        default_msg_cleanup: cleanup.map(Cleanup::name),
        // `save_opts()` (sequencer.c:3711-3713) writes `options.gpg-sign` only
        // when `opts->gpg_sign` is set, so an unsigned sequence leaves the key
        // out entirely rather than recording an empty one.
        gpg_sign: o.gpg_sign.as_deref(),
        ..Default::default()
    }
}

/// `pick_commits()`: replay `todo[start..]`, keeping the on-disk sequencer state
/// in step when one is live.
///
/// `save_todo()` runs at the top of each instruction and keeps it in the file, so
/// a stop leaves the todo beginning with the revert that stopped;
/// `update_abort_safety_file()` runs at the bottom (git's `leave:` label) whether
/// the revert landed or not.
fn run_todo(
    repo: &gix::Repository,
    o: &Options,
    cleanup: Option<Cleanup>,
    todo: &[crate::sequencer::TodoItem],
    start: usize,
    sequencer: bool,
) -> Result<ExitCode> {
    let git_dir = repo.git_dir().to_owned();
    for (i, item) in todo.iter().enumerate().skip(start) {
        if sequencer {
            crate::sequencer::save_todo(&git_dir, todo, i)?;
        }
        let step = revert_one(repo, item.oid, o, cleanup, sequencer);
        if sequencer {
            crate::sequencer::update_abort_safety_file(repo)?;
        }
        match step? {
            Step::Failed(code) => return Ok(code),
            Step::Done => {}
        }
    }
    // "Sequence of picks finished successfully; cleanup by removing the
    // .git/sequencer directory."
    if sequencer {
        crate::sequencer::remove_state(&git_dir);
    }
    Ok(ExitCode::SUCCESS)
}

/// Record a command mode, rejecting a second and different one the way git's
/// `OPT_CMDMODE` does: the newly seen flag is named first.
fn set_mode(o: &mut Options, new: Cmd) -> Option<ExitCode> {
    match o.mode {
        Some(old) if old != new => {
            eprintln!(
                "error: options '{}' and '{}' cannot be used together",
                new.flag(),
                old.flag()
            );
            Some(ExitCode::from(129))
        }
        _ => {
            o.mode = Some(new);
            None
        }
    }
}

/// Run a `--quit`/`--continue`/`--abort`/`--skip` command mode.
///
/// These are the same `sequencer.c` entry points `git cherry-pick` reaches, only
/// with `revert` in the diagnostics: `sequencer_remove_state()` for `--quit`,
/// `sequencer_continue()`, `sequencer_rollback()` and `sequencer_skip()` for the
/// rest. Each one falls back to its single-pick form (`continue_single_pick()`,
/// `rollback_single_pick()`, `skip_single_pick()`) when no `.git/sequencer` is
/// live, which is also where the "nothing in progress" refusals come from.
fn run_mode(repo: &gix::Repository, mode: Cmd) -> Result<ExitCode> {
    let git_dir = repo.git_dir().to_owned();
    match mode {
        // `cmd == 'q'`: `sequencer_remove_state()` then `remove_branch_state()`.
        Cmd::Quit => {
            crate::sequencer::remove_state(&git_dir);
            let _ = std::fs::remove_file(git_dir.join("REVERT_HEAD"));
            let _ = std::fs::remove_file(git_dir.join("MERGE_MSG"));
            let _ = std::fs::remove_file(git_dir.join("AUTO_MERGE"));
            Ok(ExitCode::SUCCESS)
        }
        Cmd::Abort => sequencer_rollback(repo, &git_dir),
        Cmd::Skip => sequencer_skip(repo, &git_dir),
        Cmd::Continue => sequencer_continue(repo, &git_dir),
    }
}

/// `reset_merge()` (sequencer.c): `git reset --merge [<commit>]`. The two-tree
/// reset discards the staged pick and restores the paths it touched while
/// keeping unrelated local changes, and drops `REVERT_HEAD` / `MERGE_MSG` /
/// `AUTO_MERGE`. It prints nothing (it is not a hard reset), which `--quiet`
/// keeps true of the ported reset too.
fn reset_merge(to: ObjectId) -> Result<ExitCode> {
    super::reset::reset(&[
        "--merge".to_string(),
        "--quiet".to_string(),
        to.to_string(),
    ])
}

/// `sequencer_rollback()`: rewind to the pre-sequence `HEAD`, or undo the single
/// stopped pick when there is no sequence.
fn sequencer_rollback(repo: &gix::Repository, git_dir: &std::path::Path) -> Result<ExitCode> {
    let head_file = crate::sequencer::head_path(git_dir);
    let Ok(text) = std::fs::read_to_string(&head_file) else {
        // `rollback_single_pick()`.
        if !git_dir.join("REVERT_HEAD").exists() && !git_dir.join("CHERRY_PICK_HEAD").exists() {
            eprintln!("error: no cherry-pick or revert in progress");
            eprintln!("fatal: revert failed");
            return Ok(ExitCode::from(128));
        }
        return reset_merge(repo.head_id()?.detach());
    };
    let Some(oid) = text
        .lines()
        .next()
        .and_then(|line| ObjectId::from_hex(line.trim().as_bytes()).ok())
    else {
        eprintln!(
            "error: stored pre-cherry-pick HEAD file '{}' is corrupt",
            head_file.display()
        );
        eprintln!("fatal: revert failed");
        return Ok(ExitCode::from(128));
    };
    if crate::sequencer::rollback_is_safe(repo) {
        reset_merge(oid)?;
    } else {
        // A hand-made commit moved HEAD past the sequence; git declines the
        // rewind rather than discarding it, and still succeeds.
        eprintln!("warning: You seem to have moved HEAD. Not rewinding, check your HEAD!");
    }
    crate::sequencer::remove_state(git_dir);
    Ok(ExitCode::SUCCESS)
}

/// `sequencer_skip()`: drop the stopped revert, then resume the sequence if one
/// is live.
fn sequencer_skip(repo: &gix::Repository, git_dir: &std::path::Path) -> Result<ExitCode> {
    if !git_dir.join("REVERT_HEAD").exists() {
        if crate::sequencer::get_last_command(git_dir) != Some(crate::sequencer::Action::Revert) {
            eprintln!("error: no revert in progress");
            eprintln!("fatal: revert failed");
            return Ok(ExitCode::from(128));
        }
        if !crate::sequencer::rollback_is_safe(repo) {
            eprintln!("error: there is nothing to skip");
            crate::advice::Advice::ResolveConflict
                .advise_plain("have you committed already?\ntry \"git revert --continue\"");
            eprintln!("fatal: revert failed");
            return Ok(ExitCode::from(128));
        }
    }
    // `skip_single_pick()` is `reset_merge(HEAD)`.
    reset_merge(repo.head_id()?.detach())?;
    if !crate::sequencer::dir(git_dir).is_dir() {
        return Ok(ExitCode::SUCCESS);
    }
    sequencer_continue(repo, git_dir)
}

/// `sequencer_continue()`: commit the revert that stopped, then replay whatever
/// the todo list still holds.
fn sequencer_continue(repo: &gix::Repository, git_dir: &std::path::Path) -> Result<ExitCode> {
    if !crate::sequencer::todo_path(git_dir).exists() {
        return Ok(match continue_single_pick(repo, git_dir)? {
            Ok(()) => ExitCode::SUCCESS,
            Err(code) => code,
        });
    }

    let todo = crate::sequencer::read_todo(repo, git_dir)?;
    if git_dir.join("REVERT_HEAD").exists() || git_dir.join("CHERRY_PICK_HEAD").exists() {
        if let Err(code) = continue_single_pick(repo, git_dir)? {
            return Ok(code);
        }
    }

    // `index_differs_from(r, "HEAD", NULL, 0)` → `error_dirty_index()`.
    let index = repo.open_index()?;
    let head_tree = repo.head_commit()?.tree_id()?.detach();
    if index_tree(repo, &index)? != head_tree {
        eprintln!("error: your local changes would be overwritten by revert.");
        crate::advice::Advice::CommitBeforeMerge
            .advise_plain("commit your changes or stash them to proceed.");
        eprintln!("fatal: revert failed");
        return Ok(ExitCode::from(128));
    }

    // `read_populate_opts()`: the command line the sequence recorded.
    let loaded = crate::sequencer::read_opts(git_dir);
    let o = Options {
        no_commit: loaded.no_commit,
        signoff: loaded.signoff,
        edit_given: loaded.edit,
        reference: false,
        mainline: loaded.mainline as usize,
        cleanup: loaded.default_msg_cleanup.clone(),
        strategy: loaded.strategy.clone(),
        xopts: loaded.xopts.clone(),
        rerere: loaded.allow_rerere_auto,
        gpg_sign: loaded.gpg_sign.clone(),
        mode: None,
    };
    let cleanup = o.cleanup.as_deref().and_then(Cleanup::parse);
    // git's `todo_list.current++`: the instruction that stopped is finished.
    run_todo(repo, &o, cleanup, &todo, 1, true)
}

/// `continue_single_pick()`: finish the revert that stopped.
///
/// git spawns `git commit --no-edit --cleanup=strip`, which recovers the message
/// from `MERGE_MSG` and the "which commit" from `REVERT_HEAD`. The revert commit
/// takes the *current* user as author (unlike a cherry-pick, which preserves the
/// original), so nothing but the message has to be recovered.
///
/// `Err(code)` is a refusal git already reported.
fn continue_single_pick(
    repo: &gix::Repository,
    git_dir: &std::path::Path,
) -> Result<std::result::Result<(), ExitCode>> {
    if !git_dir.join("REVERT_HEAD").exists() && !git_dir.join("CHERRY_PICK_HEAD").exists() {
        eprintln!("error: no cherry-pick or revert in progress");
        eprintln!("fatal: revert failed");
        return Ok(Err(ExitCode::from(128)));
    }

    let index = repo.open_index()?;
    if index.entries().iter().any(|e| e.stage_raw() != 0) {
        return Ok(Err(super::commit::die_resolve_conflict(&index)));
    }
    let tree_id = index_tree(repo, &index)?;
    let head_id = repo.head_id()?.detach();

    // `--cleanup=strip` is what git passes, so the `# Conflicts:` block the stop
    // appended is dropped along with every other comment line.
    let raw = std::fs::read_to_string(git_dir.join("MERGE_MSG")).unwrap_or_default();
    let message = stripspace(
        &raw,
        Some(&super::commit::comment_prefix(&repo.config_snapshot())),
    );

    let author = repo
        .author()
        .ok_or_else(|| anyhow::anyhow!("no author identity configured"))??;
    let author_time = author.time()?;
    let committer = repo
        .committer()
        .ok_or_else(|| anyhow::anyhow!("no committer identity configured"))??;
    let author_ident = format!("{} <{}>", author.name, author.email);
    let committer_ident = format!("{} <{}>", committer.name, committer.email);
    let new_id = super::commit::write_commit_object(
        repo,
        &committer.into(),
        &author.into(),
        message.as_bytes().as_bstr(),
        tree_id,
        vec![head_id],
        super::commit::commit_config_signer(repo).as_ref(),
    )?;
    let subject = message.lines().next().unwrap_or("").to_string();
    repo.edit_reference(RefEdit {
        change: Change::Update {
            log: LogChange {
                mode: RefLog::AndReference,
                force_create_reflog: false,
                // `continue_single_pick()` hands the work to a plain `git commit`
                // without exporting `GIT_REFLOG_ACTION` (sequencer.c:5232-5257), and
                // `sequencer_determine_whence()` recognises `CHERRY_PICK_HEAD` alone —
                // so a resumed *revert* logs as `commit: Revert "…"`, not `revert:`.
                // See [`super::replay_commit::continue_reflog_action`].
                message: format!(
                    "{}: {subject}",
                    super::replay_commit::continue_reflog_action(git_dir)
                )
                .into(),
            },
            expected: PreviousValue::MustExistAndMatch(Target::Object(head_id)),
            new: Target::Object(new_id),
        },
        name: "HEAD"
            .try_into()
            .map_err(|e| anyhow::anyhow!("invalid ref name HEAD: {e}"))?,
        deref: true,
    })?;

    let _ = std::fs::remove_file(git_dir.join("MERGE_MSG"));
    crate::sequencer::post_commit_cleanup(repo)?;

    let head_tree = repo.find_commit(head_id)?.tree_id()?.detach();
    print_summary(
        repo,
        new_id,
        &subject,
        &author_ident,
        &committer_ident,
        &author_time,
        head_tree,
        tree_id,
        false,
    )?;
    Ok(Ok(()))
}

/// A tree object built from the stage-0 entries of `index` — git's
/// `write_index_as_tree()` for an index the caller has already proved resolved.
fn index_tree(repo: &gix::Repository, index: &gix::index::File) -> Result<ObjectId> {
    let mut flat: Flat = BTreeMap::new();
    let backing = index.path_backing();
    for entry in index.entries() {
        if entry.stage_raw() != 0 {
            continue;
        }
        let kind = entry
            .mode
            .to_tree_entry_mode()
            .ok_or_else(|| anyhow::anyhow!("index entry has an unrepresentable mode"))?
            .kind();
        flat.insert(entry.path_in(backing).to_owned(), (entry.id, kind));
    }
    write_tree(repo, &flat)
}

/// Outcome of one `<commit>` operand.
enum Step {
    /// git reported a refusal itself (text already on stderr); stop with `code`.
    Failed(ExitCode),
    /// Applied.
    Done,
}

/// Revert a single commit, advancing `HEAD` unless `--no-commit` is set.
///
/// Under `--no-commit` the *ours* side is the current index written out as a
/// tree, exactly as git's `write_index_as_tree` does — so a pre-existing staged
/// change is merged through, and repeated `-n` steps stack on what the previous
/// one left staged. `Err` is reserved for the cases this port genuinely cannot
/// serve.
fn revert_one(
    repo: &gix::Repository,
    target_id: ObjectId,
    o: &Options,
    cleanup: Option<Cleanup>,
    sequencer: bool,
) -> Result<Step> {
    let target = repo.find_commit(target_id)?;
    let parents: Vec<ObjectId> = target.parent_ids().map(|id| id.detach()).collect();
    let is_merge = parents.len() > 1;

    let head = repo.head()?;
    if head.is_unborn() {
        eprintln!("fatal: Your current branch does not have any commits yet");
        return Ok(Step::Failed(ExitCode::from(128)));
    }
    let head_id = head
        .id()
        .ok_or_else(|| anyhow::anyhow!("HEAD does not point to a commit"))?
        .detach();
    drop(head);

    let hash = repo.object_hash();
    let head_tree = repo.find_commit(head_id)?.tree_id()?.detach();

    // git checks the index before it even looks at the commit's parents, so an
    // unmerged or dirty index outranks "is a merge but no -m was given". Under
    // `--no-commit` the only demand is that the index be merged, because the
    // index itself then becomes the *ours* side and a staged change is fine.
    let index_state = read_index_state(repo, head_tree)?;
    if o.no_commit {
        if index_state.unmerged {
            eprintln!("error: your index file is unmerged.");
            return Ok(Step::Failed(ExitCode::from(128)));
        }
    } else if index_state.unmerged {
        eprintln!("error: Reverting is not possible because you have unmerged files.");
        // `error_resolve_conflict` (sequencer.c) prints the error unconditionally
        // and the two-line direction only under `advice.resolveConflict`.
        crate::advice::Advice::ResolveConflict.advise_plain(
            "Fix them up in the work tree, and then use 'git add/rm <file>'\n\
             as appropriate to mark resolution and make a commit.",
        );
        eprintln!("fatal: revert failed");
        return Ok(Step::Failed(ExitCode::from(128)));
    } else if index_state.differs_from_head {
        eprintln!("error: your local changes would be overwritten by revert.");
        // `error_dirty_index` (sequencer.c) gates its one-line direction on
        // `advice.commitBeforeMerge`; the `error:` line above always prints.
        crate::advice::Advice::CommitBeforeMerge
            .advise_plain("commit your changes or stash them to proceed.");
        eprintln!("fatal: revert failed");
        return Ok(Step::Failed(ExitCode::from(128)));
    }

    // *ours* is `HEAD`, or — under `--no-commit` — the index written out as a
    // tree. git does this here, before it even looks at the commit's parents, so
    // the tree object lands in the object database even on the refusals below.
    let ours_tree = if o.no_commit {
        match &index_state.staged {
            Some(staged) => write_tree(repo, staged)?,
            None => head_tree,
        }
    } else {
        head_tree
    };

    // Parent selection — git's rules, including `-m 1` being a silent no-op on a
    // non-merge commit and `-m N>1` there being an error.
    let parent_id: Option<ObjectId> = if is_merge {
        if o.mainline == 0 {
            eprintln!("error: commit {target_id} is a merge but no -m option was given.");
            eprintln!("fatal: revert failed");
            return Ok(Step::Failed(ExitCode::from(128)));
        }
        match parents.get(o.mainline - 1) {
            Some(p) => Some(*p),
            None => {
                eprintln!("error: commit {target_id} does not have parent {}", o.mainline);
                eprintln!("fatal: revert failed");
                return Ok(Step::Failed(ExitCode::from(128)));
            }
        }
    } else {
        if o.mainline > 1 {
            eprintln!("error: commit {target_id} does not have parent {}", o.mainline);
            eprintln!("fatal: revert failed");
            return Ok(Step::Failed(ExitCode::from(128)));
        }
        parents.first().copied()
    };

    let base_tree = target.tree_id()?.detach();
    // A root commit has no parent: reverting it means going back to nothing.
    let theirs_tree = match parent_id {
        Some(p) => repo.find_commit(p)?.tree_id()?.detach(),
        None => ObjectId::empty_tree(hash),
    };

    // --- the merge --------------------------------------------------------
    //
    // A revert is a three-way merge with the roles rotated: the *ancestor* is the
    // reverted commit's tree, *ours* is `HEAD` (or the index under `-n`), *theirs*
    // is the reverted commit's parent. The marker labels match git's sequencer
    // `get_message`: the ancestor is `<short> (<subject>)` of the reverted commit,
    // *theirs* is `parent of <that>`, and *ours* is the literal `HEAD`.
    let short = target_id.attach(repo).shorten_or_id().to_string();
    let subject_label = gix::objs::commit::MessageRef::from_bytes(target.message_raw()?)
        .summary()
        .to_str_lossy()
        .into_owned();
    let ancestor_label = format!("{short} ({subject_label})");
    let other_label = if parent_id.is_some() {
        format!("parent of {ancestor_label}")
    } else {
        "(empty tree)".to_string()
    };
    let mut merge = repo.merge_trees(
        base_tree,
        ours_tree,
        theirs_tree,
        gix::merge::blob::builtin_driver::text::Labels {
            ancestor: Some(BStr::new(ancestor_label.as_bytes())),
            current: Some(BStr::new("HEAD")),
            other: Some(BStr::new(other_label.as_bytes())),
        },
        repo.tree_merge_options()?,
    )?;
    // The tree — conflict markers and all — is written before anything is checked
    // out, so the object exists even when a checkout below is refused.
    let merged_tree = merge.tree.write()?.detach();
    let merged = flatten(repo, merged_tree)?;
    let ours = flatten(repo, ours_tree)?;

    // --- refuse to clobber ------------------------------------------------
    let changed: Vec<BString> = ours
        .keys()
        .chain(merged.keys())
        .filter(|p| ours.get(*p) != merged.get(*p))
        .cloned()
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();

    let wt = scan_worktree(repo)?;
    let mut clobbered: Vec<&BString> = changed
        .iter()
        .filter(|p| wt.modified.contains(*p))
        // A path the revert deletes that is already gone from the worktree has
        // nothing left to overwrite; git does not list it either.
        .filter(|p| {
            merged.contains_key(*p)
                || repo
                    .workdir_path(p.as_bstr())
                    .is_some_and(|full| full.exists())
        })
        .collect();
    if !clobbered.is_empty() {
        clobbered.sort();
        eprintln!("error: Your local changes to the following files would be overwritten by merge:");
        for p in clobbered {
            eprintln!("\t{}", quote_path(p));
        }
        eprintln!("Please commit your changes or stash them before you merge.");
        eprintln!("Aborting");
        eprintln!("fatal: revert failed");
        return Ok(Step::Failed(ExitCode::from(128)));
    }
    let mut untracked: Vec<&BString> = changed
        .iter()
        .filter(|p| !ours.contains_key(*p) && merged.contains_key(*p) && wt.untracked.contains(*p))
        .collect();
    if !untracked.is_empty() {
        untracked.sort();
        eprintln!("error: The following untracked working tree files would be overwritten by merge:");
        for p in untracked {
            eprintln!("\t{}", quote_path(p));
        }
        eprintln!("Please move or remove them before you merge.");
        eprintln!("Aborting");
        eprintln!("fatal: revert failed");
        return Ok(Step::Failed(ExitCode::from(128)));
    }

    // `merge_switch_to_result()`'s `write_auto_merge` region (merge-ort.c:4959-4971):
    // it records every result it checked out as `AUTO_MERGE`, clean or conflicted
    // and whether or not a commit follows, which is why stock leaves
    // `.git/AUTO_MERGE` behind after a *successful* `git revert`. A revert always
    // takes the in-process merge (`do_pick_commit` routes `TODO_REVERT` there
    // regardless of `--strategy`), so there is no strategy-child exception here.
    //
    // **After the two refusals above, not before.** They stand in for
    // `checkout(opt, head, result->tree)` (merge-ort.c:4936), whose failure sets
    // `result->clean = -1` and `return`s from merge_switch_to_result — past the
    // `record_conflicted_index_entries()` call and past this write. Writing first
    // left a `.git/AUTO_MERGE` stock never creates, and every later `git status`
    // and `git diff AUTO_MERGE` read it as a merge in progress.
    crate::merge_apply::write_auto_merge(repo, merged_tree)?;

    // `merge_display_update_messages()` (merge-ort.c:4973-4974), the call after
    // the write above and therefore also after the refusals: merge-ort emits an
    // `Auto-merging` line for every path it ran a blob merge on, then a
    // `CONFLICT (...)` line for the ones left unresolved. A path both sides
    // changed identically resolves trivially and is reported by neither, which is
    // why reverting an already-reverted change stays silent — and a refused
    // checkout reports none of them at all.
    let unresolved = gix::merge::tree::TreatAsUnresolved::git();
    let mut conflicted: Vec<BString> = Vec::new();
    for conflict in &merge.conflicts {
        let path = conflict.changes_in_resolution().0.location().to_owned();
        if conflict.content_merge().is_some() {
            println!("Auto-merging {path}");
        }
        if !conflict.is_unresolved(unresolved) {
            continue;
        }
        // merge-ort reports a path one side modified and the other deleted with
        // its own message naming both operands, not the generic content notice.
        // Reverting a root commit hits this for every file the commit added:
        // "theirs" is the empty tree, so each one is deleted there and modified
        // in HEAD. The modified side is the one whose tree still carries `path`.
        if matches!(
            conflict.resolution,
            Err(gix::merge::tree::ResolutionFailure::OursModifiedTheirsDeleted)
        ) {
            let (modify, delete) = if ours.contains_key(&path) {
                ("HEAD", other_label.as_str())
            } else {
                (other_label.as_str(), "HEAD")
            };
            println!(
                "CONFLICT (modify/delete): {path} deleted in {delete} and modified in {modify}.  \
                 Version {modify} of {path} left in tree."
            );
            conflicted.push(path);
            continue;
        }
        // merge-ort's `filemask == 6`: no ancestor stage means both sides added
        // the path, reported as `add/add` rather than `content`.
        let kind = if conflict.entries()[0].is_none() {
            "add/add"
        } else {
            "content"
        };
        println!("CONFLICT ({kind}): Merge conflict in {path}");
        conflicted.push(path);
    }

    // --- message ----------------------------------------------------------
    let committer = repo
        .committer()
        .ok_or_else(|| anyhow::anyhow!("no committer identity configured"))??;
    let message = build_message(
        repo,
        &target,
        parent_id.filter(|_| is_merge),
        o,
        cleanup,
        committer,
    )?;
    let subject = message.lines().next().unwrap_or("").to_string();
    // `do_pick_commit` writes the message to `MERGE_MSG` for every built-in
    // pick, before the merge even runs, and the commit that concludes the pick
    // unlinks it again. So it is only *observable* when the pick does not
    // commit: the conflict stop overwrites it with the `# Conflicts:` version
    // below, `--no-commit` leaves it as-is, and a pick that stops because its
    // result is empty leaves exactly this.
    std::fs::write(repo.git_dir().join("MERGE_MSG"), &message)?;

    // --- apply to index + worktree ---------------------------------------
    let changed_set: HashSet<BString> = changed.iter().cloned().collect();

    // An unresolved merge stops the revert the way git's sequencer does: check out
    // the marker'd tree, give the conflicting paths stage 1/2/3 index entries, and
    // record `REVERT_HEAD` and `MERGE_MSG` before exiting 1 (`AUTO_MERGE` was
    // already written above, by the merge itself).
    if !conflicted.is_empty() {
        let mut new_index = apply(repo, &changed_set, merged_tree, &merged)?;
        merge.index_changed_after_applying_conflicts(
            &mut new_index,
            unresolved,
            gix::merge::tree::apply_index_entries::RemovalMode::Prune,
        );
        new_index.write(Default::default())?;

        let git_dir = repo.git_dir();
        std::fs::write(git_dir.join("REVERT_HEAD"), format!("{target_id}\n"))?;

        // git's `append_conflicts_hint`: a blank line, `# Conflicts:`, then one
        // commented path per unresolved conflict, appended to the commit message.
        let mut merge_msg = message.clone();
        if !merge_msg.ends_with('\n') {
            merge_msg.push('\n');
        }
        merge_msg.push_str("\n# Conflicts:\n");
        for path in &conflicted {
            merge_msg.push_str("#\t");
            merge_msg.push_str(&path.to_str_lossy());
            merge_msg.push('\n');
        }
        std::fs::write(git_dir.join("MERGE_MSG"), &merge_msg)?;

        // git's `do_pick_commit` names the reverted commit and *its* subject here,
        // not the generated `Revert "…"` message, then prints the revert advice.
        eprintln!("error: could not revert {short}... {subject_label}");
        // `print_advice(r, res == 1, opts)`: `--no-commit` picks the two-line
        // variant regardless of the action, since with no commit pending there
        // is no `--continue` to point at.
        crate::sequencer::print_advice(repo, crate::sequencer::Action::Revert, o.no_commit);
        return Ok(Step::Failed(ExitCode::from(1)));
    }

    apply(repo, &changed_set, merged_tree, &merged)?;

    if o.no_commit {
        let git_dir = repo.git_dir();
        std::fs::write(git_dir.join("REVERT_HEAD"), format!("{target_id}\n"))?;
        std::fs::write(git_dir.join("MERGE_MSG"), &message)?;
        return Ok(Step::Done);
    }

    // `unsigned int flags = should_edit(opts) ? EDIT_MSG : 0;` (sequencer.c:2269),
    // and `do_commit()` only writes the object itself when *neither* `EDIT_MSG`
    // nor `VERIFY_MSG` is set (sequencer.c:1728). With an editor wanted the pick
    // hands the whole commit to `git commit -e`, which seeds `COMMIT_EDITMSG`
    // from the `MERGE_MSG` written above, runs the editor, and commits what comes
    // back — including the nothing-to-commit report an empty result earns, which
    // is why this sits above the empty-result guard rather than below it.
    if super::replay_commit::should_edit(o.edit_given, super::replay_commit::Action::Revert) {
        let code = super::replay_commit::run_git_commit(
            super::replay_commit::Action::Revert,
            o.gpg_sign.as_deref(),
            false,
        )?;
        return Ok(if code == ExitCode::SUCCESS {
            Step::Done
        } else {
            Step::Failed(code)
        });
    }

    // A revert that changes nothing produces no commit; git reports this via the
    // commit machinery and exits 1.
    if merged_tree == ours_tree {
        if !wt.modified.is_empty() || !wt.untracked.is_empty() {
            // git finishes a no-op pick by running `git commit`, which finds
            // nothing to commit and prints the working-tree status — byte-identical
            // to `git status` — before exiting 1. Reuse the ported status driver
            // rather than re-rolling the changes/untracked sections; it reads the
            // live `.git/sequencer/todo` itself, so the "Revert currently in
            // progress" block appears for a walked pick and not for the
            // single-pick fast path, exactly as `wt_status_get_state()` decides.
            super::status::status(&[])?;
            return Ok(Step::Failed(ExitCode::from(1)));
        }
        match repo.head_name()? {
            Some(name) => println!("On branch {}", name.shorten()),
            None => println!("HEAD detached at {}", head_id.to_hex_with_len(7)),
        }
        // The upstream relation and its blank line, as `wt_status_print` emits
        // them under the header; empty for a branch that tracks nothing.
        print!("{}", super::status::tracking_block(repo));
        // A sequencer pick that reverts to nothing stops mid-sequence rather
        // than ending, so `git commit`'s status carries the in-progress advice
        // git's `wt_status_get_state` prints from the live sequencer todo. The
        // single-pick fast path has no sequencer, so it omits this block.
        if sequencer {
            println!("Revert currently in progress.");
            println!("  (run \"git revert --continue\" to continue)");
            println!("  (use \"git revert --skip\" to skip this patch)");
            println!("  (use \"git revert --abort\" to cancel the revert operation)");
            println!();
        }
        println!("nothing to commit, working tree clean");
        return Ok(Step::Failed(ExitCode::from(1)));
    }

    // --- write the commit and move HEAD ----------------------------------
    let author = repo
        .author()
        .ok_or_else(|| anyhow::anyhow!("no author identity configured"))??;
    let author_time = author.time()?;
    let author_ident = format!("{} <{}>", author.name, author.email);
    let committer_ident = format!("{} <{}>", committer.name, committer.email);
    let signer = super::commit::sequencer_signer(repo, o.gpg_sign.as_deref());
    // `commit_tree_extended(msg, ..., opts->gpg_sign, extra)` (sequencer.c:1685):
    // the in-process commit signs with exactly the key the sequencer carries.
    let new_id = super::commit::write_commit_object(
        repo,
        &committer.into(),
        &author.into(),
        message.as_bytes().as_bstr(),
        merged_tree,
        vec![head_id],
        signer.as_ref(),
    )?;
    repo.edit_reference(RefEdit {
        change: Change::Update {
            log: LogChange {
                mode: RefLog::AndReference,
                force_create_reflog: false,
                message: format!("revert: {subject}").into(),
            },
            expected: PreviousValue::MustExistAndMatch(Target::Object(head_id)),
            new: Target::Object(new_id),
        },
        name: "HEAD"
            .try_into()
            .map_err(|e| anyhow::anyhow!("invalid ref name HEAD: {e}"))?,
        deref: true,
    })?;

    // A committed revert clears the in-progress markers, as git does.
    // `AUTO_MERGE` is *not* one of them: `sequencer_post_commit_cleanup()` only
    // runs when a pick was stopped, and a revert that lands never was — so stock
    // leaves the merge result behind for `git rev-parse AUTO_MERGE` to find.
    for f in ["REVERT_HEAD", "MERGE_MSG"] {
        let _ = std::fs::remove_file(repo.git_dir().join(f));
    }

    print_summary(
        repo,
        new_id,
        &subject,
        &author_ident,
        &committer_ident,
        &author_time,
        ours_tree,
        merged_tree,
        true,
    )?;
    Ok(Step::Done)
}

/// `-m <n>` value parsing with `strtol` semantics: leading blanks are skipped,
/// trailing garbage is rejected, and only values above zero are accepted — git
/// reports all three failures with the same message.
fn parse_mainline(v: &str) -> Option<usize> {
    let t = v.trim_start();
    let t = t.strip_prefix('+').unwrap_or(t);
    if t.is_empty() || !t.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    t.parse::<usize>().ok().filter(|n| *n > 0)
}

fn bad_mainline() -> ExitCode {
    eprintln!("error: option `mainline' expects a number greater than zero");
    ExitCode::from(129)
}

/// git's parse-options failure: full usage on stderr, exit 129.
fn usage_error() -> ExitCode {
    eprint!("{USAGE}");
    ExitCode::from(129)
}

/// git's sequencer failure shape: the specific `error:` line the sequencer
/// printed, then the generic `fatal: revert failed` `cmd_revert` dies with,
/// status 128.
fn sequencer_failed(message: &str) -> ExitCode {
    eprintln!("error: {message}");
    eprintln!("fatal: revert failed");
    ExitCode::from(128)
}

/// Flatten a tree into `path -> (id, kind)` via the index representation, which
/// already expands nested trees into full slash-separated paths.
fn flatten(repo: &gix::Repository, tree: ObjectId) -> Result<Flat> {
    let index = repo.index_from_tree(&tree)?;
    let backing = index.path_backing();
    let mut out = Flat::new();
    for e in index.entries() {
        let path = e.path_in(backing);
        let mode: EntryMode = e
            .mode
            .to_tree_entry_mode()
            .ok_or_else(|| anyhow::anyhow!("tree entry `{path}` has an unrepresentable mode"))?;
        out.insert(path.to_owned(), (e.id, mode.kind()));
    }
    Ok(out)
}

/// What git checks about the index before it will start a revert.
struct IndexState {
    /// Any entry is at a conflict stage.
    unmerged: bool,
    /// The index does not match the `HEAD` tree.
    differs_from_head: bool,
    /// The index flattened the same way a tree is, so `--no-commit` can write it
    /// out as the *ours* side. `None` when there is no readable index at all.
    staged: Option<Flat>,
}

fn read_index_state(repo: &gix::Repository, head_tree: ObjectId) -> Result<IndexState> {
    let Ok(index) = repo.index() else {
        return Ok(IndexState {
            unmerged: false,
            differs_from_head: false,
            staged: None,
        });
    };
    let unmerged = index
        .entries()
        .iter()
        .any(|e| e.stage() != gix::index::entry::Stage::Unconflicted);
    if unmerged {
        return Ok(IndexState {
            unmerged: true,
            differs_from_head: true,
            staged: None,
        });
    }
    let backing = index.path_backing();
    let mut staged = Flat::new();
    for e in index.entries() {
        let path = e.path_in(backing);
        let Some(mode) = e.mode.to_tree_entry_mode() else {
            continue;
        };
        staged.insert(path.to_owned(), (e.id, mode.kind()));
    }
    let head = flatten(repo, head_tree)?;
    Ok(IndexState {
        unmerged: false,
        differs_from_head: staged != head,
        staged: Some(staged),
    })
}

/// Write a flattened tree back into the object database.
fn write_tree(repo: &gix::Repository, flat: &Flat) -> Result<ObjectId> {
    let mut editor =
        gix::objs::tree::Editor::new(gix::objs::Tree::empty(), &repo.objects, repo.object_hash());
    for (path, (id, kind)) in flat {
        editor.upsert(path.split(|&b| b == b'/').map(|c| c.as_bstr()), *kind, *id)?;
    }
    Ok(editor.write(|tree| repo.write_object(tree).map(|id| id.detach()))?)
}

/// The parts of `git status` this command needs to decide whether it may write.
struct WorktreeState {
    /// Tracked paths whose worktree content differs from the index.
    modified: HashSet<BString>,
    /// Untracked worktree paths.
    untracked: HashSet<BString>,
}

fn scan_worktree(repo: &gix::Repository) -> Result<WorktreeState> {
    let mut state = WorktreeState {
        modified: HashSet::new(),
        untracked: HashSet::new(),
    };
    let patterns: Vec<BString> = Vec::new();
    for item in repo.status(gix::progress::Discard)?.into_iter(patterns)? {
        match item? {
            gix::status::Item::TreeIndex(_) => {}
            gix::status::Item::IndexWorktree(iw) => {
                use gix::status::index_worktree::Item;
                use gix::status::plumbing::index_as_worktree::EntryStatus;
                match iw {
                    Item::Modification { rela_path, status, .. } => match status {
                        // Unmerged paths are diagnosed from the index before the
                        // scan runs; reaching one here is not a reason to stop.
                        EntryStatus::Conflict { .. } => {
                            state.modified.insert(rela_path);
                        }
                        EntryStatus::IntentToAdd => {
                            state.modified.insert(rela_path);
                        }
                        EntryStatus::NeedsUpdate(_) => {}
                        EntryStatus::Change(_) => {
                            state.modified.insert(rela_path);
                        }
                    },
                    Item::DirectoryContents { entry, .. } => {
                        if matches!(entry.status, gix::dir::entry::Status::Untracked) {
                            state.untracked.insert(entry.rela_path);
                        }
                    }
                    Item::Rewrite { .. } => {}
                }
            }
        }
    }
    Ok(state)
}

/// Move the index and worktree onto `merged_tree`, touching only `changed`.
///
/// Unrelated index entries are carried over verbatim (with their stats), so a
/// locally modified file outside the revert's footprint stays exactly as it was
/// — matching git, which only refuses when an *affected* path is dirty.
fn apply(
    repo: &gix::Repository,
    changed: &HashSet<BString>,
    merged_tree: ObjectId,
    merged: &Flat,
) -> Result<gix::index::File> {
    if changed.is_empty() {
        return Ok(repo.index_or_load_from_head()?.into_owned());
    }
    let workdir = repo
        .workdir()
        .ok_or_else(|| anyhow::anyhow!("bare repository has no worktree to update"))?
        .to_owned();

    // Check out just the changed paths that exist in the merged tree.
    let mut subset = repo.index_from_tree(&merged_tree)?;
    subset.remove_entries(|_, path, _| !changed.contains(&path.to_owned()));

    // A revert that only deletes leaves nothing to check out. The checkout
    // itself must not be called then: it takes the path storage out of the
    // index, and `gix-index` asserts that an entry-less state has no storage
    // left — `remove_entries` drops entries but keeps the storage.
    let mut fresh: HashMap<BString, (ObjectId, Mode, Flags, Stat)> = HashMap::new();
    if !subset.entries().is_empty() {
        let should_interrupt = AtomicBool::new(false);
        let mut opts =
            repo.checkout_options(gix::worktree::stack::state::attributes::Source::IdMapping)?;
        opts.destination_is_initially_empty = false;
        opts.overwrite_existing = true;
        let odb = repo.objects.clone().into_arc()?;
        let discard_files = gix::progress::Discard;
        let discard_bytes = gix::progress::Discard;
        crate::worktree::checkout_subset(
            &mut subset,
            workdir.as_path(),
            odb,
            &discard_files,
            &discard_bytes,
            &should_interrupt,
            opts,
        )?;

        // Fresh stats produced by that checkout, plus the entry shape to stage.
        let backing = subset.path_backing();
        for e in subset.entries() {
            fresh.insert(e.path_in(backing).to_owned(), (e.id, e.mode, e.flags, e.stat));
        }
    }

    // Delete worktree entries the revert removes. A gitlink leaves a populated
    // directory behind, which git reports and then leaves alone.
    for path in changed {
        if merged.contains_key(path) {
            continue;
        }
        let Some(full) = repo.workdir_path(path.as_bstr()) else {
            continue;
        };
        if full.is_dir() {
            if std::fs::remove_dir(&full).is_err() {
                eprintln!("warning: unable to rmdir '{path}': Directory not empty");
            }
        } else {
            let _ = std::fs::remove_file(full);
        }
    }

    // Restage: drop every changed path from the current index, then push back
    // the ones the merged tree still has. Untouched entries keep their stats.
    let mut index = repo.index_or_load_from_head()?.into_owned();
    index.remove_entries(|_, path, _| changed.contains(&path.to_owned()));
    for path in changed {
        if let Some((id, mode, flags, stat)) = fresh.get(path) {
            index.dangerously_push_entry(*stat, *id, *flags, *mode, path.as_bstr());
        }
    }
    index.sort_entries();
    index.remove_tree();
    index.write(Default::default())?;
    Ok(index)
}

/// Build the revert commit message exactly as `sequencer_format_revert_message`
/// does, then apply the `--cleanup` mode the way the commit machinery would.
fn build_message(
    repo: &gix::Repository,
    target: &gix::Commit<'_>,
    merge_parent: Option<ObjectId>,
    o: &Options,
    cleanup: Option<Cleanup>,
    committer: gix::actor::SignatureRef<'_>,
) -> Result<String> {
    let raw = target.message_raw()?.to_string();
    let orig_subject = raw
        .lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("")
        .to_string();

    let comment = super::commit::comment_prefix(&repo.config_snapshot());
    let mut msg = if o.reference {
        format!("{comment} {REFERENCE_TITLE}\n")
    } else {
        // Reverting a revert reads better as "Reapply"; the original subject
        // already carries the closing quote. git leaves an already-nested
        // `Revert "Revert "…""` alone rather than unwinding it.
        match orig_subject.strip_prefix("Revert \"") {
            Some(rest) if !rest.starts_with("Revert \"") => format!("Reapply \"{rest}\n"),
            _ => format!("Revert \"{orig_subject}\"\n"),
        }
    };
    msg.push_str("\nThis reverts commit ");
    msg.push_str(&refer_to(repo, target.id, o.reference)?);
    if let Some(p) = merge_parent {
        msg.push_str(", reversing\nchanges made to ");
        msg.push_str(&refer_to(repo, p, o.reference)?);
    }
    msg.push_str(".\n");
    if o.signoff {
        msg.push_str(&format!(
            "\nSigned-off-by: {} <{}>\n",
            committer.name, committer.email
        ));
    }

    // Without an explicit `--cleanup` the message git generates never needs
    // tidying, so it is left byte-for-byte as built.
    let Some(mode) = cleanup else {
        return Ok(msg);
    };
    Ok(match mode {
        Cleanup::Verbatim => msg,
        // `opts->default_msg_cleanup = get_cleanup_mode(cleanup_arg, 1)`
        // (builtin/revert.c:189) — the `use_editor` argument is the literal `1`,
        // so `--cleanup=default` is `COMMIT_MSG_CLEANUP_ALL` for a revert whether
        // or not an editor runs. Measured: `git revert --cleanup=default
        // --reference` drops the `*** SAY WHY … ***` title with stdin redirected.
        Cleanup::Strip | Cleanup::Default => stripspace(&msg, Some(&comment)),
        // `scissors` only cuts at the scissors line, which this message never
        // has, so what remains is the plain whitespace tidy-up.
        Cleanup::Whitespace | Cleanup::Scissors => stripspace(&msg, None),
    })
}

/// How git refers to a commit in the message: the full hex id normally, and
/// `%h (%s, %ad)` with a short date under `--reference`.
fn refer_to(repo: &gix::Repository, id: ObjectId, reference: bool) -> Result<String> {
    if !reference {
        return Ok(id.to_string());
    }
    let commit = repo.find_commit(id)?;
    let short = id.attach(repo).shorten_or_id();
    let raw = commit.message_raw()?.to_string();
    let subject = raw
        .lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("")
        .to_string();
    let date = commit
        .author()?
        .time()?
        .format_or_unix(gix::date::time::format::SHORT);
    Ok(format!("{short} ({subject}, {date})"))
}

/// git's `strbuf_stripspace`: drop trailing whitespace, collapse runs of blank
/// lines to one, remove leading and trailing blanks, and — when a comment prefix
/// is supplied — drop whole comment lines. The prefix is `comment_line_str`, not
/// a literal `#`: `strbuf_stripspace(msg, cleanup == ALL ? comment_line_str : NULL)`
/// (sequencer.c:1629-1630).
fn stripspace(s: &str, comment: Option<&str>) -> String {
    let mut out = String::new();
    let mut wrote = false;
    let mut pending_blank = false;
    for line in s.lines() {
        if comment.is_some_and(|c| line.starts_with(c)) {
            continue;
        }
        let trimmed = line.trim_end();
        if trimmed.is_empty() {
            pending_blank = wrote;
            continue;
        }
        if pending_blank {
            out.push('\n');
            pending_blank = false;
        }
        out.push_str(trimmed);
        out.push('\n');
        wrote = true;
    }
    out
}

/// The block git prints after a successful revert: the id/subject line, the
/// `Author:` line a divergent identity earns, the author `Date:` line the
/// sequencer always requests, the short-stat, and the create/delete/mode-change
/// summary.
#[allow(clippy::too_many_arguments)]
fn print_summary(
    repo: &gix::Repository,
    new_id: ObjectId,
    subject: &str,
    author_ident: &str,
    committer_ident: &str,
    author_time: &gix::date::Time,
    old_tree: ObjectId,
    new_tree: ObjectId,
    show_author_date: bool,
) -> Result<()> {
    let label = match repo.head_name()? {
        Some(name) => name.shorten().to_string(),
        None => "detached HEAD".to_string(),
    };
    let short = new_id.attach(repo).shorten_or_id();
    println!("[{label} {short}] {subject}");
    // ```c
    // format_commit_message(commit, "%an <%ae>", &author_ident, &pctx);
    // format_commit_message(commit, "%cn <%ce>", &committer_ident, &pctx);
    // if (strbuf_cmp(&author_ident, &committer_ident)) {
    //         strbuf_addstr(&format, "\n Author: ");
    // ```
    //
    // (sequencer.c:1339-1344). The comparison is of the *new* commit's two
    // identities, so it is `GIT_AUTHOR_*` at revert time — not the reverted
    // commit's author, which a revert never reuses — that decides the line.
    if author_ident != committer_ident {
        println!(" Author: {author_ident}");
    }
    // `SUMMARY_SHOW_AUTHOR_DATE`: the sequencer always sets it, but the child
    // `git commit` a `--continue` runs only does when `author_date_is_interesting()`
    // — `author_message || force_date`. A revert reuses no author, so its
    // `--continue` summary carries no ` Date:` line.
    if show_author_date {
        println!(
            " Date: {}",
            author_time.format_or_unix(gix::date::time::format::DEFAULT)
        );
    }

    // ```c
    // rev.diff = 1;
    // rev.diffopt.output_format = DIFF_FORMAT_SHORTSTAT | DIFF_FORMAT_SUMMARY;
    // rev.show_root_diff = 1;
    // rev.diffopt.detect_rename = DIFF_DETECT_RENAME;
    // ...
    // log_tree_commit(&rev, commit);
    // ```
    //
    // (sequencer.c:1462-1490.) `print_commit_summary()` counts no tree entries of
    // its own: it hands the commit to the ordinary revision/diff machinery, with
    // rename detection **on**, and lets `log_tree_commit()` diff it against its
    // first parent. So this delegates to the port's own `diff-tree` — the engine
    // `git diff --shortstat --summary -M` already agrees with stock on — exactly
    // as [`super::commit`] and [`super::cherry_pick`] do.
    //
    // The walk this replaces got the same two things wrong they did: it reported
    // a rename as a create plus a delete with their full line counts, and it took
    // its line counts from `gix`'s tree-diff statistics, which run both blobs
    // through the `Mode::ToGit` conversion pipeline first — so a reverted CRLF/LF
    // rewrite scored ` 1 file changed, 0 insertions(+), 0 deletions(-)` against
    // stock's ` 3 insertions(+), 3 deletions(-)`. The gitlink hand-count that sat
    // beside it goes too: `diff-tree` sees a `Subproject commit <oid>` line the
    // way git does.
    super::diff_tree::diff_tree(&[
        "-r".to_string(),
        "-M".to_string(),
        "--shortstat".to_string(),
        "--summary".to_string(),
        old_tree.to_string(),
        new_tree.to_string(),
    ])?;
    Ok(())
}

/// git's `quote_c_style`, which every path in the summary block goes through.
///
/// A path is left alone unless it holds a byte that needs escaping; then the
/// whole thing is wrapped in double quotes with C escapes. With `core.quotePath`
/// at its default, that includes every byte above ASCII, so `üñïçødé.txt` prints
/// as `"\303\274\303\261\303\257\303\247\303\270d\303\251.txt"` — while a plain
/// space, which needs no escape, keeps the path unquoted.
fn quote_path(path: &BString) -> String {
    crate::quote::quoted_name_string(path.as_slice())
}
