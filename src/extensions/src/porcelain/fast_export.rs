//! `git fast-export` — dump revisions in the `git fast-import` stream format.
//!
//! Backed entirely by the vendored gitoxide (`src/ported`). The commit order is
//! stock git's `rev-list --topo-order --reverse`, produced by
//! `gix_traverse::commit::topo` (a port of git's `sort_in_topological_order`);
//! the per-commit ref label is git's `--source` decoration, propagated over a
//! commit-date-ordered walk exactly as `add_parents_to_list` does it. Both of
//! those break ties by the order `revs->pending` was filled in, so that order is
//! preserved rather than sorted, and the topo seed is reversed to stand in for
//! the `prio_queue_reverse` gix's queue does not do. The
//! tree-vs-tree walk is implemented here rather than through
//! `gix::Repository::diff_tree_to_tree` so the change order matches git's
//! recursive `diff_tree_oid` emission order, which the blob export order depends
//! on; the `M`/`D` lines are then re-sorted with git's own `depth_first`
//! comparator, as `show_filemodify` does.
//!
//! ### Argument handling
//!
//! git processes a `fast-export` command line in a fixed order, and the exit code
//! depends on which stage rejects it. The order is *not* argv order: `cmd_
//! fast_export` runs its own `parse_options` to completion before `setup_revisions`
//! ever sees argv, so every option fast-export owns is diagnosed ahead of every
//! rev-list option, diff option and revision argument, wherever they sit relative
//! to one another. This module reproduces the stages:
//!
//! 1. no arguments at all → the option usage on stderr, exit 129
//! 2. `parse_options` sweeps the whole command line for fast-export's own table
//!    (`--signed-tags`, `--signed-commits`, `--tag-of-filtered-object`,
//!    `--reencode`, `--progress`, `--anonymize`, `--export-marks`, …). A bad value
//!    is its callback's own `error: <msg>` line — no option list behind it — and
//!    exit 129. `PARSE_OPT_KEEP_UNKNOWN_OPT` copies everything else through in
//!    order for stage 3, and `--` ends this sweep and is dropped, so an option
//!    behind the separator is never fast-export's to parse (nor is a `-h`).
//! 3. `setup_revisions` walks what survived, left to right, so here the *earlier*
//!    argument wins: `--max-count`/`--skip` die `fatal: '<v>': not an integer`
//!    (128), a malformed `-M`/`-C`/`-B` score is `error: …` (129), and a positional
//!    that resolves is a revision. One that does not ends revision parsing
//!    outright: it and every argument after it are pathspecs, checked by
//!    `verify_filename` — so a later `--max-count=1` is reported as a path
//!    beginning with `-`, not applied. A `^` that fails is `fatal: bad revision`,
//!    since it could only ever have been a revision.
//! 4. leftover/unknown arguments → the option usage on stderr, exit 129
//! 5. `--anonymize-map` without `--anonymize` → fatal, exit 128
//! 6. `--ancestry-path` with no negative revision → fatal, exit 128
//!
//! ### Covered (byte-identical stdout, exit code and marks file against stock git)
//!
//! * `fast-export --all`, `--branches`, `--tags`, `--remotes`, `--reflog`
//! * `<rev>...`, `<a>..<b>`, `<a>...<b>`, `^<rev>`, `--not`
//! * the `--not` flag word and its XOR. `--not` *toggles* `UNINTERESTING |
//!   BOTTOM` rather than setting it, and every later argument XORs its own
//!   contribution into the result: `^X` under `--not` is **positive**, a second
//!   `--not` cancels the first, `--all`/`--branches`/`--tags`/`--remotes`/
//!   `--reflog` become negative while it is in force, `A..B` swaps ends, and
//!   `A...B` shows its merge bases while hiding both endpoints
//! * command-line order and duplication in `revs->cmdline`: each pseudo-option is
//!   its own `handle_refs` pass, so `--tags --all` labels a commit `refs/tags/…`
//!   where `--all --tags` labels it `refs/heads/…`, and a ref two selectors both
//!   reach is filed twice — which is what makes an annotated tag's block appear
//!   once per entry (`tag_refs` is never sorted or deduplicated, and `handle_tag`
//!   has no already-emitted guard, so `--mark-tags` hands the second copy a fresh
//!   mark)
//! * `M`/`D` line order: git's `depth_first` (a common-prefix comparison where
//!   the longer name wins a tie), applied after the blob export, so blob marks
//!   still follow tree order while `C.a`, `C2`, `C3/x` and everything under a
//!   deleted `d/` all precede the plain `C` or the file that replaces `d`
//! * the source label a commit prints under: the dwimmed cmdline ref name when
//!   there is one, and otherwise the pending entry's name — the argument as
//!   typed minus any `^` — so a raw object id and a `--not ^main` both label
//!   their commits with the spec the user wrote
//! * git's sticky `UNINTERESTING`: a commit excluded once (`^rev`, the left side
//!   of a range, `--not`) stays excluded however many times it is named
//!   positively afterwards — by `--all`/`--branches`/`--tags`/`--remotes` picking
//!   up the ref it points at, or by a bare `fast-export main ^main`. The ref is
//!   still recorded and still gets its trailing `reset <ref>\nfrom <null-oid>`,
//!   because `get_tags_and_duplicates` skips on the command-line *entry's* flags
//!   rather than the object's
//! * blob / commit / `reset` / lightweight-tag / annotated-tag stanzas, including
//!   the trailing `reset` block (`extra_refs`, sorted-unique and walked
//!   backwards) and `tag` block (`tag_refs`, walked backwards in insertion
//!   order), with `from <null-oid>` for refs whose commit was excluded
//! * `--no-data`, `--data`, `--full-tree`, `--use-done-feature`,
//!   `--show-original-ids`, `--mark-tags`, `--progress=<n>`, `--export-marks=<file>`
//! * `--import-marks=<file>` / `--import-marks-if-exists=<file>` — pre-seed the
//!   mark table from a prior export: already-marked blobs and commits are not
//!   re-emitted (a tag always is — `handle_tag` never consults the mark table,
//!   and `export_marks` only writes commit marks anyway), the id counter
//!   continues past the highest imported mark, and
//!   `from :<mark>` links an incremental commit to its imported parent.
//!   `--import-marks` dies `could not open '<file>' for reading` (128) on a
//!   missing file; the `-if-exists` form treats it as empty
//! * `--reference-excluded-parents` — a parent outside the stream is named by raw
//!   object id (`from <oid>` / `merge <oid>`) instead of being dropped, and naming
//!   it changes two more things: the commit is diffed against that parent's tree
//!   rather than emitted as a root against the empty one, and a ref left on an
//!   excluded commit is `reset` to its object id instead of the null oid. Under
//!   `--anonymize` the `from` goes through `anonymize_oid` but the trailing `reset`
//!   does not, matching git
//! * `--refspec=<src>:<dst>` — renames exported ref labels/resets/tags through the
//!   exact and single-`*` wildcard forms; a ref matching no refspec passes through
//! * `--anonymize` with `--no-data`, `--show-original-ids`, or a gitlink entry —
//!   `original-oid` keeps git's real id, and hash-named object refs (`--no-data`
//!   blobs, gitlinks) use git's `anonymize_oid` sequential fake ids
//! * `--signed-tags=(verbatim|warn|warn-verbatim|warn-strip|strip|abort)`
//! * `--tag-of-filtered-object=(abort|drop)`
//! * `--signed-commits=(strip|warn-strip|abort)`, `--reencode=(no|abort)`; the
//!   two `abort` modes reproduce git's `die()` message and exit 128, and all
//!   modes are accepted at parse time since most commits trigger none of them
//! * `--anonymize`
//! * rev-list limiting: `--max-count=<n>`, `--skip=<n>`, `--no-merges`,
//!   `--merges`, `--first-parent`, `--topo-order`, `--date-order`, `--reverse`
//! * `--ancestry-path` (without a pathspec) — git's `limit_to_ancestry` over the
//!   walked list, keeping only the commits that descend from a bottom commit
//!   (every negative revision is one). A commit whose first parent is dropped
//!   that way is exported against the empty tree, and a ref left on a dropped
//!   commit takes the same trailing `reset` an excluded ref gets
//! * accepted no-ops (as in git for a pathspec-less export): `--full-history`,
//!   `--simplify-merges`, `--sparse`, `--dense`, `--boundary` (without negative
//!   revisions)
//! * the diffcore rename/copy/break-detection family that `setup_revisions`
//!   forwards — `-M`/`-C`/`-B` and their `--find-renames`/`--find-copies`/
//!   `--break-rewrites` long forms (with an optional `<n>`/`<n>%`/`<n>/<m>`
//!   score), plus `--find-copies-harder`, `--irreversible-delete`/`-D`,
//!   `--no-renames`, and `--rename-empty`/`--no-rename-empty`. git accepts these
//!   and, on history that contains no rename or copy, emits the identical stream
//!   (diffcore-rename finds nothing, so no `R`/`C` stanza appears); this port
//!   accepts them the same way and validates a malformed score exactly as git's
//!   `diff_scoreopt_parse` does — the bare `error: invalid argument to
//!   find-renames` line, exit 129. Actual `R`/`C` emission on a rename is the one
//!   piece not reproduced: gix-diff's rename detection is documented to differ
//!   from git's diffcore-rename, so a repository whose history contains a rename
//!   would export `M`/`D` pairs where git prints `R`/`C` — semantically the same
//!   import, a different byte stream.
//! * path limiting: a plain pathspec — whether after `--` or bare, since for
//!   fast-export `--` only separates and never changes classification — filters
//!   the export to commits whose diff touches it, with git's default history
//!   simplification and parent rewriting so pruned parents and refs re-point at
//!   the nearest shown ancestor (or the null oid when none survives)
//! * integer flag values matched to git's own parsers: `--progress` accepts a
//!   base-0, k/m/g-suffixed, signed value and rejects the rest with a usage
//!   error; `--max-count`/`--skip` take a strict signed decimal (negative =
//!   "no limit" / "no skip") and die `not an integer` on garbage; `--reencode`
//!   accepts a `git_parse_maybe_bool` value or `abort`
//!
//! ### Honest limitations (bailed on with a precise message, never silently ignored)
//!
//! * `--anonymize-map=<from>[:<to>]` — git's seed interacts with a single shared
//!   token table (refs, paths and idents draw from the same map) whose exact
//!   structure this port's per-category tables do not reproduce.
//! * `--signed-commits=(verbatim|warn-verbatim)` on a signed commit — emitting
//!   `gpgsig` stanzas requires the experimental signed-commit stream extension.
//! * `--reencode=yes` on a commit carrying an `encoding` header — needs iconv;
//!   no re-encoding substrate is vendored.
//! * `--tag-of-filtered-object=rewrite` on a filtered tag — needs rev-list
//!   parent rewriting.
//! * a nested tag (a tag whose object is another tag) — git flattens the chain to
//!   the innermost tag's content under the outer tag's name, a convoluted shape
//!   not reproduced here.
//! * `--ancestry-path` together with a pathspec — the option also clears
//!   `revs->simplify_history`, and the path limit here implements git's default
//!   simplification rather than the full-history one that leaves a TREESAME merge
//!   standing. `--ancestry-path=<commit>` is not recognised either: git adds that
//!   commit to the pending list under its own `ANCESTRY_PATH` flag and keeps the
//!   commit's ancestors as well, which the walk here does not model.
//! * `--boundary` (with negative revisions), and magic/glob pathspecs
//!   (`:(glob)`, `:!exclude`, `*.rs`), whose matcher this port does not
//!   reproduce.

use anyhow::{anyhow, bail, Result};
use std::cmp::Ordering;
use std::collections::{BinaryHeap, HashMap};
use std::io::Write;
use std::process::ExitCode;

use gix::bstr::{BStr, BString, ByteSlice};
use gix::hash::ObjectId;
use gix::objs::tree::{EntryKind, EntryMode};

/// git's `fast_export_usage` block, byte-for-byte, including the trailing blank
/// line. Printed to stderr for both "no arguments" and "leftover arguments".
const USAGE: &str = "\
usage: git fast-export [<rev-list-opts>]

    --[no-]progress <n>   show progress after <n> objects
    --[no-]signed-tags <mode>
                          select handling of signed tags
    --[no-]signed-commits <mode>
                          select handling of signed commits
    --[no-]tag-of-filtered-object <mode>
                          select handling of tags that tag filtered objects
    --[no-]reencode <mode>
                          select handling of commit messages in an alternate encoding
    --[no-]export-marks <file>
                          dump marks to this file
    --[no-]import-marks <file>
                          import marks from this file
    --[no-]import-marks-if-exists <file>
                          import marks from this file if it exists
    --[no-]fake-missing-tagger
                          fake a tagger when tags lack one
    --[no-]full-tree      output full tree for each commit
    --[no-]use-done-feature
                          use the done feature to terminate the stream
    --no-data             skip output of blob data
    --data                opposite of --no-data
    --[no-]refspec <refspec>
                          apply refspec to exported refs
    --[no-]anonymize      anonymize output
    --anonymize-map <from:to>
                          convert <from> to <to> in anonymized output
    --[no-]reference-excluded-parents
                          reference parents which are not in fast-export stream by object id
    --[no-]show-original-ids
                          show original object ids of blobs/commits
    --[no-]mark-tags      label tags with mark ids

";

/// git's `usage_with_options`: the option list on stderr, exit 129.
fn usage_exit() -> ExitCode {
    eprint!("{USAGE}");
    ExitCode::from(129)
}

/// git's `error()` followed by an option-parsing failure: a single `error: <msg>`
/// line on stderr (no option list) and exit 129. This is what `diff_scoreopt_parse`
/// reaching a bad rename/copy/break score produces.
fn usage_error(msg: &str) -> ExitCode {
    eprintln!("error: {msg}");
    ExitCode::from(129)
}

/// git's `die()`: `fatal: <msg>` on stderr, exit 128.
fn fatal(msg: &str) -> ExitCode {
    eprintln!("fatal: {msg}");
    ExitCode::from(128)
}

/// A `die()` reached while the stream is already being produced.
///
/// git writes the stream as it goes, so everything emitted before the failure is
/// still on stdout when it exits; this port buffers, so the buffer is flushed
/// first to keep both streams byte-identical.
struct Fatal(String);

fn die_midstream(out: &[u8], f: &Fatal) -> ExitCode {
    let mut stdout = std::io::stdout().lock();
    let _ = stdout.write_all(out);
    let _ = stdout.flush();
    fatal(&f.0)
}

/// How a signature found in a tag (or commit) is dealt with.
#[derive(Clone, Copy, PartialEq, Eq)]
enum SignedMode {
    Verbatim,
    WarnVerbatim,
    WarnStrip,
    Strip,
    Abort,
}

impl SignedMode {
    /// git's `parse_sign_mode` (`gpg-interface.c`) as narrowed by fast-export's
    /// `parse_opt_sign_mode`, which additionally rejects the three
    /// `*-if-invalid` modes `parse_sign_mode` accepts — they are for signing, not
    /// exporting, so fast-export treats them as unknown like any other word.
    fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "verbatim" | "ignore" => SignedMode::Verbatim,
            "warn" | "warn-verbatim" => SignedMode::WarnVerbatim,
            "warn-strip" => SignedMode::WarnStrip,
            "strip" => SignedMode::Strip,
            "abort" => SignedMode::Abort,
            _ => return None,
        })
    }
}

/// `--tag-of-filtered-object`: what to do with a tag whose object was not exported.
#[derive(Clone, Copy, PartialEq, Eq)]
enum FilteredTagMode {
    Abort,
    Drop,
    Rewrite,
}

/// `--reencode`: what to do with a commit carrying an `encoding` header.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ReencodeMode {
    Yes,
    No,
    Abort,
}

/// The traversal order; git's `--topo-order` (fast-export's default) or `--date-order`.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Order {
    Topo,
    Date,
}

/// Parsed command-line options for a single `fast-export` invocation.
struct Opts {
    no_data: bool,           // --no-data / --data: refer to blobs by hash
    full_tree: bool,         // --full-tree: `deleteall` plus the whole tree per commit
    use_done: bool,          // --use-done-feature: `feature done` header and `done` trailer
    show_original_ids: bool, // --show-original-ids: `original-oid <sha>` directives
    mark_tags: bool,         // --mark-tags: give annotated tags a mark too
    fake_missing_tagger: bool, // --fake-missing-tagger
    progress: Option<i64>,   // --progress=<n>: a `progress` line every <n> objects
    export_marks: Option<String>, // --export-marks=<file>
    signed_tags: SignedMode, // --signed-tags=<mode>
    signed_commits: SignedMode, // --signed-commits=<mode>
    filtered_tag: FilteredTagMode, // --tag-of-filtered-object=<mode>
    reencode: ReencodeMode,  // --reencode=<mode>
    anonymize: bool,         // --anonymize
    reference_excluded_parents: bool, // --reference-excluded-parents
    refspecs: Vec<BString>,  // --refspec=<refspec> (applied to exported ref names)
}

/// The tagger git invents for a tag object that has none, when asked to.
const FAKE_TAGGER: &str = "tagger <unknown> <unknown> 0 +0000";

/// git's `null_oid()` as printed in a `reset` for an excluded commit.
const NULL_OID: &str = "0000000000000000000000000000000000000000";

/// `git fast-export` — see the module documentation for the covered surface.
pub fn fast_export(args: &[String]) -> Result<ExitCode> {
    // Dispatch passes the subcommand itself at index 0.
    let args = match args.first().map(String::as_str) {
        Some("fast-export") => &args[1..],
        _ => args,
    };

    // git: `if (argc == 1) usage_with_options(...)` — a bare `fast-export` is a
    // usage error, while any single option makes it a (possibly empty) export.
    if args.is_empty() {
        return Ok(usage_exit());
    }

    let mut opts = Opts {
        no_data: false,
        full_tree: false,
        use_done: false,
        show_original_ids: false,
        mark_tags: false,
        fake_missing_tagger: false,
        progress: None,
        export_marks: None,
        signed_tags: SignedMode::Abort,
        signed_commits: SignedMode::Strip,
        filtered_tag: FilteredTagMode::Abort,
        reencode: ReencodeMode::Abort,
        anonymize: false,
        reference_excluded_parents: false,
        refspecs: Vec::new(),
    };

    // Revision selection, in command-line order so `--not` scopes correctly.
    //
    // `negate` is git's `flags` word carried across `setup_revisions`' sweep,
    // narrowed to the one bit fast-export cares about. `--not` *toggles* it
    // (`*flags ^= UNINTERESTING | BOTTOM`, revision.c:2907), so a second `--not`
    // turns negation back off, and every later argument — pseudo-option and
    // revision alike — is read through its current value.
    let mut negate = false;
    let mut use_reflog = false;
    let mut reflog_negated = false;

    // rev-list limiting.
    let mut order = Order::Topo;
    let mut first_parent = false;
    let mut no_merges = false;
    let mut only_merges = false;
    let mut max_count: Option<usize> = None;
    let mut skip: usize = 0;
    let mut ancestry_path = false;
    let mut boundary = false;

    // Deferred diagnostics — git reports these only after the revision walk has
    // been set up, so the order of checks below has to match.
    let mut leftover = false;
    let mut anonymize_map: Vec<String> = Vec::new();
    // (path, if_exists): --import-marks dies on a missing file, --import-marks-if-exists is silent.
    let mut import_marks: Option<(String, bool)> = None;
    let mut refspecs: Vec<String> = Vec::new();
    let mut pathspecs: Vec<String> = Vec::new();

    // ---- Stage 1: git's `parse_options` over fast-export's own option table. ----
    // `cmd_fast_export` runs `parse_options(..., PARSE_OPT_KEEP_ARGV0 |
    // PARSE_OPT_KEEP_UNKNOWN_OPT)` to completion *before* `setup_revisions` ever
    // looks at argv, so this is a full left-to-right sweep of its own table and
    // nothing else: an option it owns reports its error here, ahead of anything a
    // later rev-list option, diff option or revision argument would have said.
    // Options it does not own — and every positional — are kept, in order, for
    // stage 2. `parse_options_step()` breaks on `--` and, with
    // `PARSE_OPT_KEEP_DASHDASH` unset, drops it, so a token behind the separator
    // is no longer fast-export's to parse and reaches `setup_revisions` verbatim.
    // The `-h` test lives inside that same loop, which is why a `-h` behind the
    // separator is not a help request either.
    let mut rest: Vec<&str> = Vec::new();
    let mut argv = args.iter();
    while let Some(a) = argv.next() {
        let s = a.as_str();
        match s {
            "--" => {
                rest.extend(argv.map(String::as_str));
                break;
            }

            // parse_options()'s own `-h`: the block on stdout, exit 129 — not
            // `usage_exit()`, whose stderr is reserved for rejections.
            // `--help-all` reaches the same renderer with USAGE_FULL, which this
            // table renders identically: it has no `PARSE_OPT_HIDDEN` entry.
            "-h" | "--help-all" => return Ok(super::show_usage(USAGE)),

            "--no-data" => opts.no_data = true,
            "--data" => opts.no_data = false,
            "--full-tree" => opts.full_tree = true,
            "--use-done-feature" => opts.use_done = true,
            "--show-original-ids" => opts.show_original_ids = true,
            "--mark-tags" => opts.mark_tags = true,
            "--fake-missing-tagger" => opts.fake_missing_tagger = true,
            "--anonymize" => opts.anonymize = true,
            "--reference-excluded-parents" => opts.reference_excluded_parents = true,

            _ if s.starts_with("--progress=") => {
                // git's `--progress` is a parse-options `OPTION_INTEGER`: base-0
                // magnitude, an optional k/m/g suffix, a signed C-int range. A
                // value outside that is `opterror()`'s single `error:` line and
                // exit 129, with no option list behind it.
                let v = &s["--progress=".len()..];
                match parse_progress_int(v) {
                    Some(n) => opts.progress = Some(n),
                    None => {
                        return Ok(usage_error(
                            "option `progress' expects an integer value with an optional k/m/g suffix",
                        ))
                    }
                }
            }
            _ if s.starts_with("--export-marks=") => {
                opts.export_marks = Some(s["--export-marks=".len()..].to_string());
            }
            _ if s.starts_with("--import-marks=") => {
                import_marks = Some((s["--import-marks=".len()..].to_string(), false));
            }
            _ if s.starts_with("--import-marks-if-exists=") => {
                import_marks = Some((s["--import-marks-if-exists=".len()..].to_string(), true));
            }
            _ if s.starts_with("--refspec=") => {
                refspecs.push(s["--refspec=".len()..].to_string());
            }
            _ if s.starts_with("--anonymize-map=") => {
                anonymize_map.push(s["--anonymize-map=".len()..].to_string());
            }
            // git's `parse_opt_sign_mode` names the failing option through
            // `opt->long_name`, so the two sign-mode options share one message
            // shape and differ only in that name.
            _ if s.starts_with("--signed-tags=") => {
                let v = &s["--signed-tags=".len()..];
                match SignedMode::parse(v) {
                    Some(m) => opts.signed_tags = m,
                    None => return Ok(usage_error(&format!("unknown signed-tags mode: {v}"))),
                }
            }
            _ if s.starts_with("--signed-commits=") => {
                let v = &s["--signed-commits=".len()..];
                match SignedMode::parse(v) {
                    Some(m) => opts.signed_commits = m,
                    None => return Ok(usage_error(&format!("unknown signed-commits mode: {v}"))),
                }
            }
            _ if s.starts_with("--tag-of-filtered-object=") => {
                let v = &s["--tag-of-filtered-object=".len()..];
                opts.filtered_tag = match v {
                    "abort" => FilteredTagMode::Abort,
                    "drop" => FilteredTagMode::Drop,
                    "rewrite" => FilteredTagMode::Rewrite,
                    // `parse_opt_tag_of_filtered_mode` spells the option short.
                    _ => return Ok(usage_error(&format!("unknown tag-of-filtered mode: {v}"))),
                };
            }
            _ if s.starts_with("--reencode=") => {
                // git's `parse_opt_reencode_mode`: a `git_parse_maybe_bool` value
                // (so `yes`/`true`/`on`/`1`/any non-zero int → yes, `no`/`false`/
                // `off`/`0`/empty → no), else a case-insensitive `abort`.
                let v = &s["--reencode=".len()..];
                match parse_reencode(v) {
                    Some(m) => opts.reencode = m,
                    None => return Ok(usage_error(&format!("unknown reencoding mode: {v}"))),
                }
            }

            // Not fast-export's: `PARSE_OPT_KEEP_UNKNOWN_OPT` copies it through
            // for `setup_revisions`, and so does a bare positional.
            _ => rest.push(s),
        }
    }

    let repo = gix::discover(".")?;

    // ---- Stage 2: `setup_revisions` over what `parse_options` kept. ----
    // Before its own sweep, `setup_revisions` searches the argv it was handed for
    // a `--` and truncates there, pushing everything behind the separator into
    // `prune_data` as pathspecs without inspecting them at all. fast-export's
    // `parse_options` already consumed the *first* `--`, so this only ever fires
    // on a second one — and when it does, `seen_dashdash` also declares every
    // argument in front of it a revision.
    let mut seen_dashdash = false;
    if let Some(p) = rest.iter().position(|t| *t == "--") {
        pathspecs.extend(rest[p + 1..].iter().map(|t| t.to_string()));
        rest.truncate(p);
        seen_dashdash = true;
    }

    // Then one left-to-right sweep, so the first rejection in *this* stream wins
    // — a bad `--max-count` before a bad revision reports the integer, and the
    // other way round reports the ambiguous argument.
    let mut sel = Selection::default();
    let mut i = 0;
    while i < rest.len() {
        let s = rest[i];
        i += 1;
        // git's `setup_revisions` forwards the diffcore rename/copy/break-detection
        // options (`-M`/`-C`/`-B` and their long forms) straight into
        // `diff_scoreopt_parse`. They only steer diffcore-rename, whose `R`/`C`
        // stanzas this port does not emit (see the module note), so a well-formed
        // value is inert; a malformed score is the same usage error (exit 129, the
        // bare `error:` line with no option list) git's parser produces.
        match classify_rename_opt(s) {
            RenameOpt::Ok => continue,
            RenameOpt::Usage(msg) => return Ok(usage_error(msg)),
            RenameOpt::Other => {}
        }
        match s {
            // ---- rev-list selection ----
            // Each of these is a fresh `handle_refs(refs, revs, *flags, …)` pass
            // over its own ref subset (revision.c:2808-2841), so naming one twice
            // — or naming a ref both through `--all` and through `--tags` — files
            // the ref *twice* in `revs->cmdline`. That duplication is visible in
            // the output, so the passes are recorded rather than folded into a set.
            "--all" => sel.args.push(CmdArg::Refs(RefKind::All, negate)),
            "--branches" => sel.args.push(CmdArg::Refs(RefKind::Branches, negate)),
            "--tags" => sel.args.push(CmdArg::Refs(RefKind::Tags, negate)),
            "--remotes" => sel.args.push(CmdArg::Refs(RefKind::Remotes, negate)),
            "--reflog" => {
                use_reflog = true;
                reflog_negated = negate;
            }
            "--not" => negate = !negate,

            // ---- rev-list ordering and limiting ----
            "--topo-order" => order = Order::Topo,
            "--date-order" | "--author-date-order" => order = Order::Date,
            // fast-export sets `revs.reverse` itself after parsing, so an
            // explicit `--reverse` on the command line has no effect.
            "--reverse" => {}
            "--first-parent" => first_parent = true,
            "--no-merges" => no_merges = true,
            "--merges" => only_merges = true,
            "--ancestry-path" => ancestry_path = true,
            "--boundary" => boundary = true,
            // History simplification without a pathspec leaves the commit set
            // untouched, which is the only way fast-export can be invoked here.
            "--full-history" | "--simplify-merges" | "--sparse" | "--dense" => {}

            _ if s.starts_with("--max-count=") => {
                // rev-list's `--max-count` is a strict signed decimal (`atoi`
                // family): garbage dies `fatal: '<v>': not an integer` (128, not
                // the 129 usage path), and a negative value means "no limit".
                let v = &s["--max-count=".len()..];
                match parse_signed_int(v) {
                    Some(n) => max_count = if n < 0 { None } else { Some(n as usize) },
                    None => return Ok(fatal(&format!("'{v}': not an integer"))),
                }
            }
            _ if s.starts_with("--skip=") => {
                // Same parser as `--max-count`; git clamps a negative skip to 0.
                let v = &s["--skip=".len()..];
                match parse_signed_int(v) {
                    Some(n) => skip = if n < 0 { 0 } else { n as usize },
                    None => return Ok(fatal(&format!("'{v}': not an integer"))),
                }
            }

            // Anything else beginning with `-` survives both option parsers and
            // ends up as a leftover argument, which git turns into a usage error
            // — but only once the whole revision walk has been set up, so a bad
            // revision later on the line still reports first.
            _ if s.starts_with('-') && s != "-" => leftover = true,

            _ => {
                match add_rev_token(&repo, s, negate, &mut sel) {
                    Ok(()) => continue,
                    // A `die()` raised from inside `handle_revision_arg_1()`
                    // rather than a `-1` returned from it: the operand never
                    // reaches the pathspec sweep below.
                    Err(Some(message)) => {
                        eprint!("{message}");
                        return Ok(ExitCode::from(128));
                    }
                    Err(None) => {}
                }
                // `handle_revision_arg` failed — but two shapes never *return* a
                // failure at all, they die inside it: a range whose endpoints
                // resolved to objects the database does not have, and a
                // full-length hex name `get_oid()` decoded and `parse_object()`
                // could not find. Neither reaches the `^` fatal or the pathspec
                // sweep below, so both are asked about first.
                if let Some(message) = super::log::early_revision_fatal(&repo, s, seen_dashdash) {
                    eprint!("{message}");
                    return Ok(ExitCode::from(128));
                }
                // A `^` could only ever have been a revision, and so could
                // anything before a `--` the sweep above found, so both die
                // outright instead of falling back to a path. Note that `--not`
                // is *not* such a case: it flags the revisions that follow
                // without declaring them revisions, so an argument after it can
                // still turn out to be a pathspec.
                if seen_dashdash || s.starts_with('^') {
                    eprintln!("fatal: bad revision '{s}'");
                    return Ok(ExitCode::from(128));
                }
                // Otherwise this argument *and every one after it* are pathspecs:
                // `setup_revisions` runs `verify_filename` over `argv + i`, pushes
                // the whole tail into `prune_data` and breaks out of the loop. So
                // revision and option parsing both stop here — which is why a
                // later `--max-count=1` is not an option at all but a path that
                // begins with `-`, and reported as one.
                for (n, t) in rest[i - 1..].iter().enumerate() {
                    if let Some(code) = verify_filename(t, n == 0) {
                        return Ok(code);
                    }
                }
                pathspecs.extend(rest[i - 1..].iter().map(|t| t.to_string()));
                break;
            }
        }
    }

    // ---- Stage 3: leftover arguments. ----
    if leftover {
        return Ok(usage_exit());
    }

    // ---- Stage 4: the `--anonymize-map` fatal. ----
    // The `--ancestry-path` one comes later: git raises it from `limit_list`,
    // inside `prepare_revision_walk`, which runs after the marks file is read
    // and after the ref pseudo-options have been expanded — and `--not --all`
    // is a way of supplying bottom commits without naming one positionally.
    if !anonymize_map.is_empty() && !opts.anonymize {
        return Ok(fatal("the option '--anonymize-map' requires '--anonymize'"));
    }

    // ---- `--import-marks[-if-exists]`: seed the mark table from a prior export. ----
    // git's `read_marks`: each `:<mark> <oid>` line pre-marks that object so it is
    // never re-emitted, and the id counter continues past the highest imported
    // mark. `--import-marks` dies on a missing file; `--import-marks-if-exists`
    // treats a missing file as empty.
    let mut imported_marks: Vec<(u32, ObjectId)> = Vec::new();
    let mut imported_max: u32 = 0;
    if let Some((path, if_exists)) = &import_marks {
        match std::fs::read(path) {
            Ok(bytes) => {
                for line in bytes.split(|b| *b == b'\n') {
                    if line.is_empty() {
                        continue;
                    }
                    // `:<decimal-mark> <hex-oid>`
                    let Some(rest) = line.strip_prefix(b":") else {
                        continue;
                    };
                    let Some(sp) = rest.iter().position(|b| *b == b' ') else {
                        continue;
                    };
                    let (mark_bytes, oid_bytes) = (&rest[..sp], &rest[sp + 1..]);
                    let Ok(mark) = std::str::from_utf8(mark_bytes)
                        .unwrap_or("")
                        .parse::<u32>()
                    else {
                        continue;
                    };
                    let Ok(id) = ObjectId::from_hex(oid_bytes) else {
                        continue;
                    };
                    imported_max = imported_max.max(mark);
                    imported_marks.push((mark, id));
                }
            }
            Err(e) if *if_exists && e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => {
                let reason = if e.kind() == std::io::ErrorKind::NotFound {
                    "No such file or directory".to_string()
                } else {
                    e.to_string()
                };
                return Ok(fatal(&format!(
                    "could not open '{path}' for reading: {reason}"
                )));
            }
        }
    }

    // ---- Options this port does not implement: refuse rather than mis-export. ----
    // `--ancestry-path` also sets `revs->simplify_history = 0`, which only bites
    // once a pathspec is in play: the path limit below implements git's *default*
    // simplification, where a TREESAME merge collapses onto that parent. Full
    // history keeps the merge instead, so the two are only interchangeable
    // without a pathspec.
    if ancestry_path && !pathspecs.is_empty() {
        bail!("--ancestry-path with a pathspec is not supported");
    }
    if !anonymize_map.is_empty() {
        bail!("--anonymize-map is not supported");
    }

    // ---- `--refspec`: rename exported refs through push-style refspecs. ----
    opts.refspecs = refspecs.iter().map(|s| BString::from(s.as_str())).collect();

    // ---- Ref selection (`--all` and friends), in git's iteration order. ----
    // Each pseudo-option is expanded where it stood on the command line, so
    // `revs->cmdline` comes out in argv order — and a ref two selectors both
    // reach (`--all --tags` on `refs/tags/v1`) is filed twice, exactly as git's
    // two `handle_refs` passes file it. That duplication is not cosmetic: it is
    // what makes `handle_tags_and_duplicates` emit the annotated tag's block
    // once per entry.
    let mut cmdline: Vec<Pending> = Vec::new();
    for arg in &sel.args {
        let (kind, negated) = match arg {
            CmdArg::Rev(p) => {
                cmdline.push(p.clone());
                continue;
            }
            CmdArg::Refs(kind, negated) => (*kind, *negated),
        };
        let prefix: Option<&[u8]> = match kind {
            RefKind::All => None,
            RefKind::Branches => Some(b"refs/heads/"),
            RefKind::Tags => Some(b"refs/tags/"),
            RefKind::Remotes => Some(b"refs/remotes/"),
        };
        let mut names: Vec<BString> = Vec::new();
        for reference in repo.references()?.all()? {
            let reference = reference.map_err(|e| anyhow!("{e}"))?;
            let name = reference.name().as_bstr().to_owned();
            if prefix.is_none_or(|p| name.starts_with(p)) {
                names.push(name);
            }
        }
        names.sort();
        // git's `--all` also feeds `head_ref` after `for_each_ref`, which only
        // contributes a distinct entry when HEAD is detached; otherwise it
        // resolves to a ref already listed.
        if kind == RefKind::All && repo.head()?.is_detached() {
            names.push(BString::from("HEAD"));
        }
        for name in names {
            let spec = name.to_str().map_err(|_| anyhow!("non-UTF-8 ref {name:?}"))?;
            let Ok(target) = repo.rev_parse_single(spec) else {
                continue;
            };
            let target = target.detach();
            let Ok(commit) = repo.find_object(target)?.peel_to_commit() else {
                continue; // a ref to a blob or tree is not exportable, as in git
            };
            cmdline.push(Pending {
                dwim: Some((name.clone(), target)),
                pending_name: name,
                commit: commit.id,
                negated,
            });
        }
    }

    // ---- Ref bookkeeping, mirroring `get_tags_and_duplicates`. ----
    // `sources` is git's `--source` decoration: the ref name a commit is printed
    // under. The first cmdline ref reaching a commit wins; later ones become
    // standalone `reset` stanzas. Annotated tags claim a source too, but never
    // produce a duplicate `reset`.
    let mut sources: HashMap<ObjectId, BString> = HashMap::new();
    // Every commit-valued cmdline ref, whether or not it ends up labelling a
    // commit. git's comment on this list: "make sure this ref gets properly
    // updated eventually, whether through a commit or manually at the end".
    let mut commit_refs: Vec<(BString, ObjectId)> = Vec::new();
    let mut tag_refs: Vec<(BString, ObjectId)> = Vec::new();

    // `revs->pending`, in the order `setup_revisions` filled it. The order is
    // load-bearing twice over, so it is kept rather than sorted: `--source` hands
    // a shared ancestor the name of the *first* pending tip to reach it, and
    // `sort_in_topological_order` seeds its queue from this same list.
    let mut tips: Vec<ObjectId> = Vec::new();
    // The `UNINTERESTING` half of the same list: `^rev`, the excluded end of a
    // range, a `...` merge base, anything under an odd number of `--not`s.
    let mut hidden: Vec<ObjectId> = Vec::new();

    for p in &cmdline {
        if p.negated {
            hidden.push(p.commit);
            // `get_tags_and_duplicates` skips a cmdline entry whose *flags* are
            // UNINTERESTING (fast-export.c:1065-1066), so a negative ref labels
            // nothing, contributes no tag block and gets no trailing `reset`.
            continue;
        }
        tips.push(p.commit);
        // `repo_dwim_ref(e->name)` failing is the other `continue` there: a raw
        // object id, or a `^main` whose recorded name still has the caret.
        let Some((name, target)) = &p.dwim else {
            continue;
        };
        if repo.find_object(*target)?.kind == gix::object::Kind::Tag {
            tag_refs.push((name.clone(), *target));
        } else {
            commit_refs.push((name.clone(), p.commit));
        }
        sources.entry(p.commit).or_insert_with(|| name.clone());
    }

    // Whatever `get_tags_and_duplicates` left unclaimed is filled in by
    // `prepare_revision_walk`, which labels a pending commit with the *pending*
    // name — the argument as typed, minus any `^` (revision.c:437-442). That is
    // the only reason `fast-export <oid>` prints `commit <oid>` rather than an
    // empty refname, and why `--not ^main` prints `commit main` and not
    // `commit refs/heads/main`.
    for p in &cmdline {
        sources
            .entry(p.commit)
            .or_insert_with(|| p.pending_name.clone());
    }

    // `--reflog` contributes tips with no name at all: git adds every object a
    // reflog mentions to the pending list under an empty name, so a commit
    // reached that way prints under an empty refname instead of inheriting a
    // branch's. Claiming the source here rather than leaving the entry vacant is
    // what stops the propagation walk below from labelling it.
    if use_reflog {
        let mut reflog_tips: Vec<ObjectId> = Vec::new();
        collect_reflog_tips(&repo, &mut reflog_tips)?;
        if reflog_negated {
            hidden.extend(reflog_tips);
        } else {
            for id in &reflog_tips {
                sources.entry(*id).or_default();
            }
            tips.extend(reflog_tips);
        }
    }

    // git dedupes pending objects through the `SEEN` flag: the first mention of
    // an object wins and the order of the rest is left alone.
    //
    // Both walks below apply that same rule to their own seeds, so this is not what keeps a
    // repeated ref out of the output. It has to happen *here* because of the ordering: git
    // deduplicates while filling `revs->commits` and only then calls `prio_queue_reverse` on it,
    // whereas the `Order::Topo` seed below is this list reversed. Deduplicating after the reversal
    // — which is all the walk itself can do — would keep each commit's *last* mention instead of
    // its first, and the seed order is what breaks ties between commits sharing a commit date.
    dedup_first_wins(&mut tips);
    dedup_first_wins(&mut hidden);

    // ---- The two checks that need the whole negative set. ----
    // git raises the `--ancestry-path` fatal from `limit_list`, once every
    // pseudo-option has contributed its bottoms, so `--not --all` satisfies it.
    if ancestry_path && hidden.is_empty() {
        return Ok(fatal("--ancestry-path given but there are no bottom commits"));
    }
    if boundary && !hidden.is_empty() {
        bail!("--boundary with negative revisions is not supported");
    }

    // A commit named on both sides — `--all`/`--branches`/`--tags`/`--remotes`
    // re-adding a ref that `^feature` already excluded, or a repeated
    // `fast-export main ^main` — stays uninteresting: git's `UNINTERESTING` is a
    // flag on the *object*, not on the pending entry that mentioned it. Both
    // walks below enforce that themselves, so `tips` is left as git's pending
    // list is (`gix-traverse`'s `Simple` drops such a commit with the rest of the
    // hidden frontier, and `topo`'s seeding skips it), and the ref bookkeeping
    // above is deliberately left untouched too: `get_tags_and_duplicates` skips
    // on the *entry's* flags (`e->flags & UNINTERESTING`), not the object's, so
    // the `--all` entry for a hidden ref is still recorded and still gets its
    // trailing `reset <ref>\nfrom <null>`.

    // ---- Source propagation over the commit-date walk git uses for it. ----
    if !tips.is_empty() {
        let mut platform = repo
            .rev_walk(tips.clone())
            .sorting(gix::revision::walk::Sorting::ByCommitTime(
                gix::traverse::commit::simple::CommitTimeOrder::NewestFirst,
            ));
        if first_parent {
            platform = platform.first_parent_only();
        }
        if !hidden.is_empty() {
            platform = platform.with_hidden(hidden.clone());
        }
        for info in platform.all()? {
            let info = info?;
            let Some(src) = sources.get(&info.id).cloned() else {
                continue;
            };
            // `add_parents_to_list` assigns `revs->sources` inside the loop it breaks out of
            // under `first_parent_only`, so a merge's later parents inherit nothing there — and
            // `Info::parent_ids` reports the full parent list regardless of the walk mode.
            let followed = if first_parent { 1 } else { info.parent_ids.len() };
            for parent in info.parent_ids.iter().take(followed) {
                sources.entry(*parent).or_insert_with(|| src.clone());
            }
        }
    }

    // ---- Emission order: `rev-list [--topo-order|--date-order] --reverse`. ----
    let mut order_list: Vec<gix::traverse::commit::Info> = Vec::new();
    if !tips.is_empty() && first_parent {
        // `--first-parent` is the one mode where the two halves of git's walk
        // disagree, so it gets git's own two-step rather than one traversal.
        //
        // `limit_list()` (revision.c:1438-1515) is the half `first_parent_only`
        // reaches: it drains a `prio_queue` ordered by
        // `compare_commits_by_commit_date` — newest first, insertion order
        // breaking ties — and the `process_parents()` it calls for each commit
        // `break`s after the first parent under that flag (revision.c:1211).
        // What comes out is *which* commits are in `revs->commits`, in commit
        // date order.
        //
        // That list is then handed to [`sort_in_topological_order`], which
        // follows **every** parent link there is. A traversal that limits and
        // orders in one pass cannot express the pair, and ordering a
        // first-parent selection as if the second parents were absent is what
        // puts a merge ahead of the side branch it merges.
        let mut platform = repo
            .rev_walk(tips.clone())
            .sorting(gix::revision::walk::Sorting::ByCommitTime(
                gix::traverse::commit::simple::CommitTimeOrder::NewestFirst,
            ))
            .first_parent_only();
        if !hidden.is_empty() {
            platform = platform.with_hidden(hidden.clone());
        }
        let mut limited: Vec<gix::traverse::commit::Info> = Vec::new();
        for info in platform.all()? {
            let info = info?;
            limited.push(gix::traverse::commit::Info {
                id: info.id,
                parent_ids: info.parent_ids.clone(),
                commit_time: info.commit_time,
            });
        }
        order_list = sort_in_topological_order(limited, order);
    } else if !tips.is_empty() {
        // git seeds `sort_in_topological_order` from the pending list and then
        // calls `prio_queue_reverse` on it, so that tips sharing a commit date
        // come back out in pending order. gix's topo queue keeps the seed order
        // and pops from the back without that reversal, so the reversal is
        // applied to the seed instead. Only ties are affected: once the commit
        // dates differ the queue's own sort decides and the seed order stops
        // mattering. `--date-order` uses a comparison queue, which git leaves
        // un-reversed.
        let seed: Vec<ObjectId> = match order {
            Order::Topo => tips.iter().rev().copied().collect(),
            Order::Date => tips.clone(),
        };
        let topo = gix::traverse::commit::topo::Builder::from_iters(
            &repo.objects,
            seed,
            Some(hidden.clone()),
        )
        .sorting(match order {
            Order::Topo => gix::traverse::commit::topo::Sorting::TopoOrder,
            Order::Date => gix::traverse::commit::topo::Sorting::DateOrder,
        })
        .parents(gix::traverse::commit::Parents::All)
        .build()?;
        for info in topo {
            order_list.push(info?);
        }
    }

    // ---- `--ancestry-path`: git's `limit_to_ancestry`, applied where
    // `limit_list` applies it — to the walked list, ahead of the path limit and
    // of every output-time filter. Only commits that reach a bottom commit
    // through the walked history survive; the rest are marked UNINTERESTING and
    // drop out, which is what leaves a merge's other side out of the stream.
    //
    // `collect_bottom_commits` reads the BOTTOM flag, and
    // `handle_revision_arg_1` sets it on every negative revision
    // (`flags = flags & UNINTERESTING ? flags | BOTTOM : flags & ~BOTTOM`) —
    // `^rev`, the left side of a range, a `...` merge base and a `--not` alike —
    // so the bottom list is exactly the hidden set the fatal above checked for.
    // Bottoms are themselves UNINTERESTING and so never in the list.
    if ancestry_path {
        let parents_of: HashMap<ObjectId, Vec<ObjectId>> = order_list
            .iter()
            .map(|i| (i.id, i.parent_ids.iter().copied().collect()))
            .collect();
        let ids: Vec<ObjectId> = order_list.iter().map(|i| i.id).collect();
        let kept: std::collections::HashSet<ObjectId> =
            super::rev_list::limit_to_ancestry(&hidden, &ids, &parents_of)
                .into_iter()
                .collect();
        order_list.retain(|i| kept.contains(&i.id));
    }

    // ---- Path limiting: git's default history simplification with parent
    // rewriting (`revs.prune_data` + `revs.rewrite_parents`). ----
    // A commit is shown iff its pathspec-restricted diff against the parent it
    // follows is non-empty (git's `try_to_simplify_commit`). Each shown commit's
    // parents, and every ref that pointed at a pruned commit, are then rewritten
    // to the nearest shown ancestor by following first parents through the pruned
    // (TREESAME) run — exactly `rewrite_one`.
    let specs = super::log::PathspecMatcher::new(&repo, &pathspecs)?;
    let filtering = !pathspecs.is_empty();
    let mut simpl: HashMap<ObjectId, Simpl> = HashMap::new();
    let mut emit_parents: HashMap<ObjectId, Vec<ObjectId>> = HashMap::new();
    if filtering {
        for info in &order_list {
            let real: Vec<ObjectId> = info.parent_ids.iter().copied().collect();
            let tree = commit_tree_id(&repo, info.id)?;
            let (treesame, followed) = if real.is_empty() {
                // A root is TREESAME (pruned) when it carries no matching path.
                (!diff_matches(&repo, None, tree, &specs)?, Vec::new())
            } else {
                let considered = if first_parent { &real[..1] } else { &real[..] };
                let mut ts = false;
                let mut followed = real.clone();
                for p in considered {
                    let pt = commit_tree_id(&repo, *p)?;
                    if !diff_matches(&repo, Some(pt), tree, &specs)? {
                        ts = true;
                        followed = vec![*p];
                        break;
                    }
                }
                (ts, followed)
            };
            simpl.insert(info.id, Simpl { treesame, followed });
        }
        // Keep only shown (non-TREESAME) commits.
        order_list.retain(|i| simpl.get(&i.id).is_none_or(|s| !s.treesame));
        // A shown commit keeps all its real parents (only the first under
        // `--first-parent`); rewrite each to the nearest shown ancestor and drop
        // duplicates (git's `remove_duplicate_parents`).
        for info in &order_list {
            let take = if first_parent { 1 } else { info.parent_ids.len() };
            let mut ep: Vec<ObjectId> = Vec::new();
            for p in info.parent_ids.iter().take(take) {
                if let Some(rp) = rewrite_one(*p, &simpl) {
                    if !ep.contains(&rp) {
                        ep.push(rp);
                    }
                }
            }
            emit_parents.insert(info.id, ep);
        }
    }
    let pcount = |i: &gix::traverse::commit::Info| -> usize {
        if filtering {
            emit_parents.get(&i.id).map_or(0, Vec::len)
        } else {
            i.parent_ids.len()
        }
    };

    // git applies `commit_ignore` (`--no-merges`/`--merges`), then `--skip`, then
    // `--max-count`, all in rev-list order — before fast-export reverses. Under a
    // pathspec the parent count is the rewritten one, matching git's post-prune view.
    if no_merges {
        order_list.retain(|i| pcount(i) <= 1);
    }
    if only_merges {
        order_list.retain(|i| pcount(i) > 1);
    }
    if skip > 0 {
        order_list.drain(..skip.min(order_list.len()));
    }
    if let Some(n) = max_count {
        order_list.truncate(n);
    }
    order_list.reverse();

    let mut st = State {
        out: Vec::new(),
        marks: HashMap::new(),
        commit_marks: Vec::new(),
        last_mark: 0,
        counter: 0,
        labels: std::collections::HashSet::new(),
        anon: Anon::default(),
    };

    // Seed the mark table from `--import-marks`: pre-marked objects are skipped by
    // the blob/commit/tag emitters, and `last_mark` continues past the highest
    // imported id so new objects never collide. Imported marks are re-dumped by
    // `--export-marks`, as git's do.
    st.last_mark = imported_max;
    for (mark, id) in &imported_marks {
        st.marks.insert(*id, *mark);
        st.commit_marks.push((*mark, *id));
    }

    if opts.use_done {
        st.out.extend_from_slice(b"feature done\n");
    }

    for info in &order_list {
        let override_parents = emit_parents.get(&info.id).map(Vec::as_slice);
        if let Some(f) = emit_commit(&repo, info, &opts, &sources, &mut st, filtering.then_some(&specs), override_parents)?
        {
            return Ok(die_midstream(&st.out, &f));
        }
    }

    // ---- Trailing `reset`/`tag` block. ----
    // A cmdline ref that never appeared as a commit label still has to be
    // pointed somewhere, so git emits a `reset` for it: at the mark of the
    // commit it names, or at the null oid when that commit was not exported.
    // The list is sorted by ref name and walked backwards.
    let mut trailing: Vec<(BString, ObjectId)> = commit_refs
        .into_iter()
        .filter(|(name, _)| !st.labels.contains(name))
        .collect();
    trailing.sort();
    trailing.dedup();
    for (name, commit_id) in trailing.iter().rev() {
        let printed = st.anon_refname(&opts, name.as_bstr());
        // Under a pathspec the ref's own commit may have been pruned; git points
        // the ref at the nearest shown ancestor (the same `rewrite_one` used for
        // parents), and only at the null oid when no ancestor survives.
        let target = if filtering {
            rewrite_one(*commit_id, &simpl)
        } else {
            Some(*commit_id)
        };
        let mark = target.and_then(|id| st.marks.get(&id).copied());
        st.out.extend_from_slice(b"reset ");
        st.out.extend_from_slice(&printed);
        match mark {
            Some(mark) => {
                st.out
                    .extend_from_slice(format!("\nfrom :{mark}\n\n").as_bytes());
                // `handle_tags_and_duplicates` counts a re-pointed ref as an
                // exported object; the null-oid arm below `continue`s past the
                // same `show_progress()` call, so only this one ticks.
                st.tick(&opts);
            }
            // The commit was excluded from this export. git's default reading is
            // "the user wants the branch exported but every commit in its history
            // deleted", so it points the ref at the null oid, which fast-import
            // takes as a branch deletion. `--reference-excluded-parents` says the
            // opposite — the excluded commits are assumed to be in the importing
            // repository already — so the ref is set to that commit's raw object
            // id instead. git prints it unanonymized here (unlike the `from` in a
            // commit stanza), and skips `show_progress()` on both arms.
            None => {
                let target = match target {
                    // `rewrite_commit()` returned NULL: the ref's history was
                    // filtered away entirely, which is a deletion either way.
                    Some(id) if opts.reference_excluded_parents => id.to_hex().to_string(),
                    _ => NULL_OID.to_string(),
                };
                st.out
                    .extend_from_slice(format!("\nfrom {target}\n\n").as_bytes());
            }
        }
    }

    // Unlike `extra_refs`, which `get_tags_and_duplicates` ends with a
    // `string_list_sort_u`, `tag_refs` is never sorted or deduplicated
    // (fast-export.c:1043 appends, 1119 sorts only the other list). It is walked
    // back to front in *insertion* order, so a tag reached by two selectors —
    // `--all --tags` on the same `refs/tags/v1` — has its whole block emitted
    // twice, and `--tags --all` emits the two selectors' tags in their own
    // command-line order rather than by name.
    for (name, tag_id) in tag_refs.iter().rev() {
        if let Some(f) = emit_tag(&repo, name.as_bstr(), *tag_id, &opts, &mut st)? {
            return Ok(die_midstream(&st.out, &f));
        }
    }

    if opts.use_done {
        st.out.extend_from_slice(b"done\n");
    }

    let mut stdout = std::io::stdout().lock();
    stdout.write_all(&st.out)?;
    stdout.flush()?;

    if let Some(path) = &opts.export_marks {
        if !st.commit_marks.is_empty() {
            let mut buf = String::new();
            for (mark, id) in &st.commit_marks {
                buf.push_str(&format!(":{mark} {id}\n"));
            }
            std::fs::write(path, buf)?;
        }
    }

    Ok(ExitCode::SUCCESS)
}

// ---------------------------------------------------------------------------
// Revision selection
// ---------------------------------------------------------------------------

/// Every argument `setup_revisions` accepted, in command-line order.
///
/// git keeps two parallel lists — `revs->cmdline` (what the user named) and
/// `revs->pending` (the objects those names resolved to) — and fills both as it
/// sweeps argv. Order and duplication are both load-bearing downstream, so this
/// is a list rather than the sets the selection used to be reduced to.
#[derive(Default)]
struct Selection {
    args: Vec<CmdArg>,
}

/// One accepted argument: either a pseudo-option standing for a whole ref
/// subset, or a revision that resolved to an object.
enum CmdArg {
    /// `--all`, `--branches`, `--tags`, `--remotes`, and whether the `--not`
    /// state in force at that point made the whole pass negative.
    Refs(RefKind, bool),
    Rev(Pending),
}

/// Which `handle_refs` pass a [`CmdArg::Refs`] stands for (revision.c:2808-2841).
#[derive(Clone, Copy, PartialEq, Eq)]
enum RefKind {
    All,
    Branches,
    Tags,
    Remotes,
}

/// A `revs->cmdline` entry paired with the `revs->pending` entry beside it.
#[derive(Clone)]
struct Pending {
    /// `repo_dwim_ref(e->name)`: the full ref name the argument named, plus the
    /// object that ref points at, unpeeled. `None` when the argument named no
    /// ref — a raw object id, a `HEAD~1`, or the `^main` of `--not ^main`, whose
    /// `e->name` still carries the `^` that stops it dwimming
    /// (`add_rev_cmdline(revs, object, arg_, …)`, revision.c:2234).
    dwim: Option<(BString, ObjectId)>,
    /// `add_pending_object`'s name, which is `arg` — the argument with a leading
    /// `^` already consumed. `handle_commit` labels a commit with it when the
    /// dwim above found nothing (revision.c:437-442), which is why
    /// `fast-export --not ^main` prints `commit main` and `fast-export <oid>`
    /// prints `commit <oid>`.
    pending_name: BString,
    /// The commit the argument peels to.
    commit: ObjectId,
    /// `e->flags & UNINTERESTING`.
    negated: bool,
}

/// Resolve one positional revision argument, in git's `handle_revision_arg` shape.
///
/// `negated` is the `--not` state in force here. git carries it in `flags` and
/// **XOR**s the argument's own contribution into it, so every shape below flips
/// when `--not` is on: `^X` becomes positive, `A..B` hides B and shows A, and
/// `A...B` shows its merge bases and hides both endpoints.
///
/// `Err(None)` is `handle_revision_arg()`'s non-zero return: the argument is
/// neither a revision nor a path, which the caller turns into git's
/// `ambiguous argument` fatal. `Err(Some(message))` is a `die()` raised *inside*
/// `handle_revision_arg_1()` — `add_parents_only()`'s `get_reference()` is one —
/// which never falls back to a pathspec and is reported verbatim.
fn add_rev_token(
    repo: &gix::Repository,
    tok: &str,
    negated: bool,
    sel: &mut Selection,
) -> std::result::Result<(), Option<String>> {
    if tok.is_empty() {
        return Err(None);
    }
    // `handle_revision_arg_1()` keeps the operand it was called with in `arg_`
    // and only ever moves `arg`. `add_rev_cmdline(revs, object, arg_, …)` at the
    // end of the function therefore records the operand **as typed**, mark and
    // all, while `add_pending_object_with_path(revs, object, arg, …)` records
    // the truncated one — which is why `git fast-export main^!` prints
    // `commit main` and not `commit refs/heads/main`: `main^!` does not dwim to
    // a ref, so the pending name is what labels the commit.
    let cmdline = tok;
    // `handle_revision_arg_1()`'s three-mark block. It belongs between the range
    // rule and the single-name rule, but a marked operand is never also a range
    // — `<a>..<b>^!`'s right endpoint does not resolve, so `handle_dotdot()` has
    // already declined it — so testing it first is the same pass. The C is
    // quoted on [`crate::objname::parents_only`]; what it decides here is which
    // name is resolved at all, because `get_oid_1()` has no case for `^@`, `^!`
    // or `^-<n>`.
    let tok: &str = match crate::objname::parents_only(tok) {
        crate::objname::ParentsOnly::Absent => tok,
        // `if (strtol_i(…) || exclude_parent < 1) { ret = -1; goto out; }`:
        // `add_parents_only()` is never reached and `handle_revision_arg()`
        // returns non-zero, which is this function's `Err(None)`.
        crate::objname::ParentsOnly::BadParent => return Err(None),
        crate::objname::ParentsOnly::Mark { base, nth, replaces } => {
            // `^@` keeps `flags`; `^!` and `^-<n>` use `flags ^ (UNINTERESTING |
            // BOTTOM)`, so `--not` flips all three.
            let sense = if replaces { negated } else { !negated };
            // `add_rev_cmdline(revs, it, arg_, …)` records the base *with* its
            // `^`, while `add_pending_object(revs, it, arg)` names it without.
            //
            // The dwim is built by hand rather than through [`pending`] because
            // of what `get_tags_and_duplicates()` branches on: `e->item->type`,
            // the object the *cmdline entry* holds. For every other operand that
            // object is whatever the name resolved to, so the ref's own target
            // stands in for it; here the entry holds the parent while the ref
            // still points at the base. Reading the ref's target instead turns
            // `fast-export v1^@` into `tag … tags unexported object`, where
            // stock 2.55.0 simply labels the parents `refs/tags/v1`.
            let mut queue = |name: &str, parent, uninteresting| {
                sel.args.push(CmdArg::Rev(Pending {
                    dwim: dwim_ref(repo, base).map(|(full, _)| (full, parent)),
                    pending_name: BString::from(name),
                    commit: parent,
                    negated: uninteresting,
                }));
            };
            match crate::objname::add_parents_only(repo, base, sense, nth, &mut queue) {
                // `get_reference()`'s `die(_("bad object %s"), name)`, naming
                // the base rather than the operand — the mark and any leading
                // `^` are already off by the time `get_reference()` sees it.
                crate::objname::Parents::BadObject => {
                    let name = crate::objname::uninteresting_mark(base).0;
                    return Err(Some(format!("fatal: bad object {name}\n")));
                }
                crate::objname::Parents::None => tok,
                // `^@` returns from `handle_revision_arg_1()` on success, so the
                // named commit itself is never pended.
                crate::objname::Parents::Queued if replaces => return Ok(()),
                crate::objname::Parents::Queued => base,
            }
        }
    };
    if let Some(rest) = tok.strip_prefix('^') {
        // revision.c:2210-2213 sets `local_flags = UNINTERESTING | BOTTOM` for a
        // leading `^`, and 2229/2234 file the object under `flags ^ local_flags`.
        // Under `--not` the two cancel and `^X` is *positive*: this is the XOR,
        // not an OR, and it is what `fast-export --not ^main base` relies on.
        let id = commit_of(repo, rest).ok_or(None)?;
        sel.args
            .push(CmdArg::Rev(pending(repo, cmdline, rest, id, !negated)));
        return Ok(());
    }
    if let Some((l, r)) = tok.split_once("...") {
        let (l, r) = (default_head(l), default_head(r));
        // `handle_dotdot_1()` resolves both endpoints *before* either is looked
        // up, and joins the two `repo_get_oid_with_context()` calls with `||` —
        // so the warning set belongs to the token, not to the endpoint
        // resolutions below, which stop at the first absent object and would
        // leave the right endpoint unwarned.
        let _quiet = warn_range_once(repo, tok);
        let (lc, rc) = (commit_of(repo, l).ok_or(None)?, commit_of(repo, r).ok_or(None)?);
        // `handle_dotdot_1` (revision.c:2087-2107): the merge bases go in under
        // `flags_exclude` (`flags ^ (UNINTERESTING | BOTTOM)`) and both endpoints
        // under `flags`, in that order — so plain `A...B` is
        // `A B --not $(git merge-base --all A B)` and `--not A...B` is its
        // mirror image.
        for base in repo.merge_bases_many(lc, &[rc]).map_err(|_| None)? {
            let id = base.detach();
            // `add_rev_cmdline_list` names a merge base by its own hex id.
            let hex = id.to_hex().to_string();
            sel.args
                .push(CmdArg::Rev(pending(repo, &hex, &hex, id, !negated)));
        }
        sel.args
            .push(CmdArg::Rev(pending(repo, l, l, lc, negated)));
        sel.args
            .push(CmdArg::Rev(pending(repo, r, r, rc, negated)));
        return Ok(());
    }
    if let Some((l, r)) = tok.split_once("..") {
        // `b_flags = flags; a_flags = flags_exclude` (revision.c:2083-2086).
        let (l, r) = (default_head(l), default_head(r));
        let _quiet = warn_range_once(repo, tok);
        let lc = commit_of(repo, l).ok_or(None)?;
        let rc = commit_of(repo, r).ok_or(None)?;
        sel.args
            .push(CmdArg::Rev(pending(repo, l, l, lc, !negated)));
        sel.args
            .push(CmdArg::Rev(pending(repo, r, r, rc, negated)));
        return Ok(());
    }
    let id = commit_of(repo, tok).ok_or(None)?;
    sel.args
        .push(CmdArg::Rev(pending(repo, cmdline, tok, id, negated)));
    Ok(())
}

/// `handle_dotdot_1()`'s share of `get_oid_basic()`'s ambiguity warning for a
/// range operand, plus the guard that keeps the resolution which follows from
/// adding a second one.
///
/// The two things have to happen together. git resolves a range's endpoints in
/// one `||`-joined pair:
///
/// ```c
/// if (repo_get_oid_with_context(revs->repo, a_name, oc_flags, &a_oid, a_oc) ||
///     repo_get_oid_with_context(revs->repo, b_name, oc_flags, &b_oid, b_oc))
///         return -1;
/// a_obj = parse_object(revs->repo, &a_oid);
/// b_obj = parse_object(revs->repo, &b_oid);
/// ```
///
/// — so *both* endpoints are resolved, and both warn, before either object is
/// looked up. [`commit_of`] cannot reproduce that on its own: it fails on the
/// left endpoint the moment that endpoint's object is missing, which for a
/// full-length hex is exactly the case that warns, and the right endpoint then
/// never resolves and never warns.
/// [`crate::objname::warn_dotdot_endpoints`] carries the `||`'s own
/// short-circuit — a left endpoint that does not resolve *at all* still stops
/// the right one.
///
/// The returned guard must be held for as long as the endpoint resolutions run;
/// dropping it early puts the warning back on and doubles the count.
#[must_use = "the guard silences the endpoint resolutions and must outlive them"]
fn warn_range_once(repo: &gix::Repository, tok: &str) -> crate::objname::AmbiguityWarnings {
    crate::objname::warn_dotdot_endpoints(repo, tok);
    crate::objname::AmbiguityWarnings::off()
}

/// Build one [`Pending`], dwimming the name git's `add_rev_cmdline` records.
fn pending(
    repo: &gix::Repository,
    cmdline_name: &str,
    pending_name: &str,
    commit: ObjectId,
    negated: bool,
) -> Pending {
    Pending {
        dwim: dwim_ref(repo, cmdline_name),
        pending_name: BString::from(pending_name),
        commit,
        negated,
    }
}

/// Drop repeated ids, keeping the first occurrence and the surrounding order.
///
/// git gets this from the `SEEN` flag it sets while draining `revs->pending`:
/// the first mention of an object is the one that counts, and the pending order
/// the tie-breaks depend on survives.
fn dedup_first_wins(ids: &mut Vec<ObjectId>) {
    let mut seen = std::collections::HashSet::new();
    ids.retain(|id| seen.insert(*id));
}

/// git's `sort_in_topological_order()` (commit.c:945-1054), the sort every
/// `fast-export` stream comes out of.
///
/// `cmd_fast_export()` sets `revs.topo_order = 1` (builtin/fast-export.c:1377),
/// and with no commit-graph to supply generation numbers `setup_revisions()`
/// turns that into `revs->limited = 1`:
///
/// ```c
/// if (revs->topo_order && !generation_numbers_enabled(the_repository))
///         revs->limited = 1;
/// ```
///
/// (revision.c:3157-3158) — so `prepare_revision_walk()` takes the
/// `limit_list()` + `sort_in_topological_order()` branch and never
/// `init_topo_walk()` (revision.c:4011-4017).
///
/// The function is reproduced here because of what it does **not** contain:
/// there is no `first_parent_only` test anywhere in it. `limit_list()` already
/// chose the members of the list; this only orders them, and it counts
/// in-degrees and enqueues parents over the *whole* parent list:
///
/// ```c
/// for (next = orig; next; next = next->next) {
///         struct commit_list *parents = next->item->parents;
///         while (parents) {
///                 struct commit *parent = parents->item;
///                 int *pi = indegree_slab_at(&indegree, parent);
///                 if (*pi)
///                         (*pi)++;
///                 parents = parents->next;
///         }
/// }
/// ```
///
/// `if (*pi)` is what keeps it to the list: the slab is 0 for every commit
/// `limit_list()` left out, and the emission loop skips those the same way.
///
/// The queue is git's `prio_queue`, which is a **LIFO stack** while
/// `compare == NULL` (prio-queue.c:49 and 86) — that is `REV_SORT_IN_GRAPH_ORDER`,
/// i.e. plain `--topo-order`. The seed is therefore reversed
/// (`prio_queue_reverse`) so the tips come back out in list order, exactly the
/// comment git leaves there. `--date-order` installs
/// `compare_commits_by_commit_date` instead (commit.c:930-940), making it a
/// newest-first heap with insertion order breaking ties (prio-queue.c:4-11), and
/// is left un-reversed.
pub(super) fn sort_in_topological_order(
    list: Vec<gix::traverse::commit::Info>,
    order: Order,
) -> Vec<gix::traverse::commit::Info> {
    if list.is_empty() {
        return list;
    }
    let info_of: HashMap<ObjectId, &gix::traverse::commit::Info> =
        list.iter().map(|i| (i.id, i)).collect();

    // "Mark them and clear the indegree", then "update the indegree". A commit
    // outside the list has no slab entry at all, which is git's implicit 0.
    let mut indegree: HashMap<ObjectId, usize> = list.iter().map(|i| (i.id, 1)).collect();
    for info in &list {
        for parent in info.parent_ids.iter() {
            if let Some(pi) = indegree.get_mut(parent) {
                if *pi != 0 {
                    *pi += 1;
                }
            }
        }
    }

    // "find the tips": the list members nothing else in the list reaches.
    let mut queue = TopoQueue::new(order);
    for info in &list {
        if indegree.get(&info.id) == Some(&1) {
            queue.put(info);
        }
    }
    queue.reverse_if_lifo();

    let mut out: Vec<gix::traverse::commit::Info> = Vec::with_capacity(list.len());
    while let Some(commit) = queue.get() {
        for parent in commit.parent_ids.iter() {
            let Some(pi) = indegree.get_mut(parent) else {
                continue;
            };
            if *pi == 0 {
                continue;
            }
            *pi -= 1;
            // "parents are only enqueued for emission when all their children
            // have been emitted thereby guaranteeing topological order."
            if *pi == 1 {
                queue.put(info_of[parent]);
            }
        }
        indegree.insert(commit.id, 0);
        out.push(commit.clone());
    }
    out
}

/// git's `prio_queue` (prio-queue.c) in the two shapes
/// [`sort_in_topological_order`] uses it: a LIFO stack for
/// `REV_SORT_IN_GRAPH_ORDER` and a newest-commit-date-first heap for
/// `REV_SORT_BY_COMMIT_DATE`.
enum TopoQueue<'a> {
    /// `queue.compare = NULL`, which `prio_queue_put`/`prio_queue_get` then
    /// treat as a plain stack — append at the end, take from the end.
    Lifo(Vec<&'a gix::traverse::commit::Info>),
    /// `queue.compare = compare_commits_by_commit_date`, with `inserted`
    /// standing in for the entry `ctr` git's `compare()` falls back to.
    ByDate { heap: BinaryHeap<Newest<'a>>, inserted: u64 },
}

impl<'a> TopoQueue<'a> {
    fn new(order: Order) -> Self {
        match order {
            Order::Topo => TopoQueue::Lifo(Vec::new()),
            Order::Date => TopoQueue::ByDate { heap: BinaryHeap::new(), inserted: 0 },
        }
    }

    fn put(&mut self, info: &'a gix::traverse::commit::Info) {
        match self {
            TopoQueue::Lifo(entries) => entries.push(info),
            TopoQueue::ByDate { heap, inserted } => {
                heap.push(Newest { info, ctr: *inserted });
                *inserted += 1;
            }
        }
    }

    /// `prio_queue_reverse()`, which git calls only for
    /// `REV_SORT_IN_GRAPH_ORDER` — and `BUG()`s on any queue with a `compare`,
    /// which is why the other shape has nothing to do here.
    fn reverse_if_lifo(&mut self) {
        if let TopoQueue::Lifo(entries) = self {
            entries.reverse();
        }
    }

    fn get(&mut self) -> Option<&'a gix::traverse::commit::Info> {
        match self {
            // `return queue->array[--queue->nr].data; /* LIFO */`
            TopoQueue::Lifo(entries) => entries.pop(),
            TopoQueue::ByDate { heap, .. } => heap.pop().map(|e| e.info),
        }
    }
}

/// One heap entry, ordered so that `BinaryHeap`'s *maximum* is the element
/// `prio_queue_get` would return.
///
/// `compare_commits_by_commit_date` (commit.c:930-940) returns -1 for the newer
/// commit and git's `prio_queue` pops its minimum, so the newest date wins; when
/// the comparison is 0 the entry with the smaller insertion counter wins
/// (prio-queue.c:4-11). Both are inverted here because the heap pops the
/// greatest.
struct Newest<'a> {
    info: &'a gix::traverse::commit::Info,
    ctr: u64,
}

impl Ord for Newest<'_> {
    fn cmp(&self, other: &Self) -> Ordering {
        let (a, b) = (self.info.commit_time.unwrap_or(0), other.info.commit_time.unwrap_or(0));
        a.cmp(&b).then_with(|| other.ctr.cmp(&self.ctr))
    }
}

impl PartialOrd for Newest<'_> {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl PartialEq for Newest<'_> {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}

impl Eq for Newest<'_> {}

/// An omitted range endpoint means `HEAD`, as in `..main` or `main..`.
fn default_head(s: &str) -> &str {
    if s.is_empty() { "HEAD" } else { s }
}

// ---------------------------------------------------------------------------
// Flag-value parsers (matched byte-for-byte in behaviour to git's)
// ---------------------------------------------------------------------------

/// git's parse-options `OPTION_INTEGER` value parser, used by `--progress`:
/// an optional sign, a base-0 magnitude (`0x…` hex, leading-`0` octal, else
/// decimal), an optional single `k`/`m`/`g` (1024) suffix, and a result that
/// fits a signed C `int`. Returns `None` for anything git rejects with a usage
/// error (empty, non-numeric, bad suffix, trailing junk, out of range).
fn parse_progress_int(s: &str) -> Option<i64> {
    let b = s.as_bytes();
    let (neg, mut i) = match b.first() {
        Some(b'+') => (false, 1),
        Some(b'-') => (true, 1),
        _ => (false, 0),
    };
    let radix: u32 = if b[i..].starts_with(b"0x") || b[i..].starts_with(b"0X") {
        i += 2;
        16
    } else if b.get(i) == Some(&b'0') {
        8
    } else {
        10
    };
    let start = i;
    // Checked throughout so an absurdly long value overflows to `None` (a usage
    // error, as git's own range check would give) rather than panicking.
    let mut val: i128 = 0;
    while let Some(d) = b.get(i).and_then(|c| (*c as char).to_digit(radix)) {
        val = val.checked_mul(radix as i128)?.checked_add(d as i128)?;
        i += 1;
    }
    if i == start {
        return None; // no digits consumed
    }
    if let Some(&c) = b.get(i) {
        let mult: i128 = match c {
            b'k' | b'K' => 1024,
            b'm' | b'M' => 1024 * 1024,
            b'g' | b'G' => 1024 * 1024 * 1024,
            _ => return None,
        };
        val = val.checked_mul(mult)?;
        i += 1;
    }
    if i != b.len() {
        return None; // junk after the suffix
    }
    let result = if neg { -val } else { val };
    if result < i32::MIN as i128 || result > i32::MAX as i128 {
        return None;
    }
    Some(result as i64)
}

/// git's `git_parse_signed` as used by rev-list's `--max-count`/`--skip`: an
/// optional sign and base-10 digits, whitespace-trimmed, with nothing else. No
/// hex, no suffix — `0x10` and `3abc` both fail, and git then dies "not an
/// integer".
fn parse_signed_int(s: &str) -> Option<i64> {
    s.trim_matches(|c: char| c.is_ascii_whitespace())
        .parse::<i64>()
        .ok()
}

/// git's `parse_opt_reencode_mode`: `abort`, else a `git_parse_maybe_bool` value
/// (`yes`/`true`/`on`/any non-zero int → yes; `no`/`false`/`off`/`0`/empty → no).
fn parse_reencode(s: &str) -> Option<ReencodeMode> {
    if s.eq_ignore_ascii_case("abort") {
        return Some(ReencodeMode::Abort);
    }
    match s.to_ascii_lowercase().as_str() {
        "true" | "yes" | "on" => Some(ReencodeMode::Yes),
        "false" | "no" | "off" | "" => Some(ReencodeMode::No),
        // The integer fallback uses base-0 like git_parse_int; only the truth of
        // the value matters, and no fixture commit carries an encoding header, so
        // the resulting mode never changes the emitted stream.
        _ => match parse_progress_int(s) {
            Some(0) => Some(ReencodeMode::No),
            Some(_) => Some(ReencodeMode::Yes),
            None => None,
        },
    }
}

// ---------------------------------------------------------------------------
// Rename/copy/break detection options (`-M`/`-C`/`-B` and long forms)
// ---------------------------------------------------------------------------

/// The outcome of classifying one argument against the diffcore rename family.
enum RenameOpt {
    /// Not a rename/copy/break-detection option — fall through to the main parser.
    Other,
    /// A well-formed member; accepted and inert (no `R`/`C` stanzas are emitted).
    Ok,
    /// A malformed score: git's `error: <msg>` on stderr, exit 129.
    Usage(&'static str),
}

/// git's `diff_scoreopt_parse` reachable through `fast-export`'s `setup_revisions`.
///
/// The rename/copy/break-detection options are diff options, so git parses them
/// here rather than in `fast-export`'s own option table. This port emits none of
/// the `R`/`C` stanzas they configure, but it must still classify each argument
/// exactly as git does: accept the well-formed forms (they leave the stream
/// unchanged on rename-free history) and reject a malformed score with git's own
/// message and exit code.
fn classify_rename_opt(s: &str) -> RenameOpt {
    // Value-less members: always accepted, never carry a score.
    match s {
        "--find-copies-harder"
        | "--irreversible-delete"
        | "-D"
        | "--no-renames"
        | "--rename-empty"
        | "--no-rename-empty" => return RenameOpt::Ok,
        _ => {}
    }

    // Score-bearing members. Each resolves to a command letter (`M`/`C`/`B`, the
    // last taking an `<n>/<m>` form) and the value slice after the option name.
    let (cmd, val) = if let Some(v) = s.strip_prefix("-M") {
        (b'M', v)
    } else if let Some(v) = s.strip_prefix("-C") {
        (b'C', v)
    } else if let Some(v) = s.strip_prefix("-B") {
        (b'B', v)
    } else if let Some(v) = long_score(s, "--find-renames") {
        (b'M', v)
    } else if let Some(v) = long_score(s, "--find-copies") {
        (b'C', v)
    } else if let Some(v) = long_score(s, "--break-rewrites") {
        (b'B', v)
    } else {
        return RenameOpt::Other;
    };

    if valid_score(val, cmd == b'B') {
        RenameOpt::Ok
    } else {
        RenameOpt::Usage(match cmd {
            b'M' => "invalid argument to find-renames",
            b'C' => "invalid argument to find-copies",
            _ => "break-rewrites expects <n>/<m> form",
        })
    }
}

/// The value slice of a long rename option: `Some("")` for the bare `--name`,
/// `Some(v)` for `--name=v`, `None` when `s` is not that option at all.
fn long_score<'a>(s: &'a str, name: &str) -> Option<&'a str> {
    if s == name {
        return Some("");
    }
    s.strip_prefix(name).and_then(|rest| rest.strip_prefix('='))
}

/// git's `diff_scoreopt_parse` leftover check: run `parse_rename_score` over the
/// value and require nothing left, except that a break score may be followed by a
/// second `/`-separated score. Empty (the bare option) is always well-formed.
fn valid_score(val: &str, is_break: bool) -> bool {
    let b = val.as_bytes();
    let mut i = 0;
    consume_rename_score(b, &mut i);
    if !is_break {
        return i == b.len();
    }
    if i == b.len() {
        return true;
    }
    if b[i] != b'/' {
        return false;
    }
    i += 1;
    consume_rename_score(b, &mut i);
    i == b.len()
}

/// git's `parse_rename_score`, reduced to how far it advances: it consumes digits
/// and at most one `.`, stopping (and swallowing) a trailing `%`, and stops at the
/// first other byte. Only the consumed length matters here since this port does
/// not act on the score itself.
fn consume_rename_score(b: &[u8], i: &mut usize) {
    let mut dot = false;
    while *i < b.len() {
        match b[*i] {
            b'.' if !dot => dot = true,
            b'%' => {
                *i += 1;
                break;
            }
            c if c.is_ascii_digit() => {}
            _ => break,
        }
        *i += 1;
    }
}

// ---------------------------------------------------------------------------
// Pathspec classification and path-limited history simplification
// ---------------------------------------------------------------------------

/// A commit's place in git's default history simplification: `treesame` is set
/// when it introduces no change under the pathspec (so it is pruned from the
/// output), and `followed` is the single parent it is TREESAME to, empty for a
/// pruned root.
struct Simpl {
    treesame: bool,
    followed: Vec<ObjectId>,
}

/// [`crate::setup::verify_filename`], reported and turned into git's exit code.
///
/// `first` is git's `diagnose_misspelt_rev`, set only for the argument that failed
/// revision resolution; the ones trailing it were already known to be paths, so
/// they get the plainer wording.
fn verify_filename(arg: &str, first: bool) -> Option<ExitCode> {
    let msg = crate::setup::verify_filename(arg, first)?;
    eprintln!("fatal: {msg}");
    Some(ExitCode::from(128))
}

/// The id of a commit's tree, for the pathspec-restricted TREESAME comparisons.
fn commit_tree_id(repo: &gix::Repository, id: ObjectId) -> Result<ObjectId> {
    Ok(repo.find_object(id)?.peel_to_tree()?.id)
}

/// Whether the diff turning `old` (empty when `None`) into `new` touches any
/// pathspec — the negation of git's TREESAME. Uses the same recursive walk as
/// the emission diff so the two never disagree.
fn diff_matches(
    repo: &gix::Repository,
    old: Option<ObjectId>,
    new: ObjectId,
    specs: &super::log::PathspecMatcher,
) -> Result<bool> {
    let changes = collect(repo, old, Some(new))?;
    Ok(changes.iter().any(|c| specs.matches(c.path.as_bstr())))
}

/// git's `rewrite_one`: replace a parent (or a ref target) with the nearest
/// shown ancestor by following first parents through the pruned (TREESAME) run.
/// `None` means the run reached a pruned root, so the link is dropped entirely.
fn rewrite_one(mut id: ObjectId, simpl: &HashMap<ObjectId, Simpl>) -> Option<ObjectId> {
    // The parent chain strictly shrinks, so `simpl.len() + 1` steps always
    // terminate; the bound is belt-and-braces against a malformed graph.
    for _ in 0..=simpl.len() {
        match simpl.get(&id) {
            // Outside the walked set (e.g. a boundary): treat as shown.
            None => return Some(id),
            Some(s) if !s.treesame => return Some(id),
            Some(s) => {
                let p = s.followed.first()?;
                id = *p
            },
        }
    }
    Some(id)
}

/// Resolve a revision to the id of the commit it names, peeling tags.
///
/// `handle_revision_arg_1()` reaches `repo_get_oid_with_context()` once for each
/// name it takes off the command line — the single operand, or each endpoint of
/// a range — so this is where [`crate::objname::resolve`] belongs and where
/// `get_oid_basic()`'s `warning: refname … is ambiguous.` is emitted. Resolving
/// through it rather than `rev_parse_single()` also keeps git's ordering: a
/// full-length hex is decoded without asking the object database, so an id whose
/// object is missing gets past this and fails at the `parse_object()` below,
/// which is what `get_reference()`'s `bad object` diagnostic reports on.
fn commit_of(repo: &gix::Repository, spec: &str) -> Option<ObjectId> {
    let id = crate::objname::resolve(repo, spec)?;
    Some(repo.find_object(id).ok()?.peel_to_commit().ok()?.id)
}

/// git's `repo_dwim_ref`: the fully-resolved ref name a spec names, if any.
///
/// Symrefs are followed to their final target, which is why `HEAD` on an attached
/// worktree labels commits `refs/heads/<branch>` rather than `HEAD`.
fn dwim_ref(repo: &gix::Repository, spec: &str) -> Option<(BString, ObjectId)> {
    let mut reference = repo.try_find_reference(spec).ok().flatten()?;
    while let Some(Ok(next)) = reference.follow() {
        reference = next;
    }
    let name = reference.name().as_bstr().to_owned();
    let target = match reference.target() {
        gix::refs::TargetRef::Object(id) => id.to_owned(),
        gix::refs::TargetRef::Symbolic(_) => return None,
    };
    Some((name, target))
}

/// git's `add_reflogs_to_pending`: every object a reflog ever pointed at becomes
/// an unnamed tip.
fn collect_reflog_tips(repo: &gix::Repository, tips: &mut Vec<ObjectId>) -> Result<()> {
    let mut refs: Vec<gix::Reference<'_>> = Vec::new();
    if let Ok(head) = repo.find_reference("HEAD") {
        refs.push(head);
    }
    let platform = repo.references()?;
    for reference in platform.all()?.flatten() {
        refs.push(reference);
    }
    for reference in &refs {
        let mut platform = reference.log_iter();
        let Ok(Some(iter)) = platform.all() else {
            continue;
        };
        for line in iter {
            let Ok(line) = line else { continue };
            for id in [line.previous_oid(), line.new_oid()] {
                if id.is_null() {
                    continue;
                }
                if let Some(commit) = repo
                    .find_object(id)
                    .ok()
                    .and_then(|o| o.peel_to_commit().ok())
                {
                    tips.push(commit.id);
                }
            }
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Anonymization (`--anonymize`)
// ---------------------------------------------------------------------------

/// git's anonymization tables: one generated token per distinct input, handed out
/// in the order the stream first mentions it.
#[derive(Default)]
struct Anon {
    refs: HashMap<BString, BString>,
    paths: HashMap<BString, BString>,
    idents: HashMap<BString, BString>,
    oids: HashMap<ObjectId, BString>,
    tag_messages: HashMap<BString, BString>,
    blob_counter: u32,
    message_counter: u32,
}

impl Anon {
    /// git's `anonymize_refname`: the `refs/heads/`-style prefix survives, every
    /// remaining component becomes `ref<n>` from one shared counter.
    fn refname(&mut self, name: &BStr) -> BString {
        const PREFIXES: [&[u8]; 4] = [
            b"refs/heads/".as_slice(),
            b"refs/tags/".as_slice(),
            b"refs/remotes/".as_slice(),
            b"refs/".as_slice(),
        ];
        let raw: &[u8] = name;
        let mut out = BString::default();
        let mut rest = raw;
        for p in PREFIXES {
            if let Some(r) = raw.strip_prefix(p) {
                out.extend_from_slice(p);
                rest = r;
                break;
            }
        }
        Self::map_components(&mut self.refs, rest, "ref", &mut out);
        out
    }

    /// git's `anonymize_path`: each `/`-separated component is mapped on its own,
    /// so shared directories keep sharing a generated name.
    fn path(&mut self, path: &BStr) -> BString {
        let mut out = BString::default();
        Self::map_components(&mut self.paths, path, "path", &mut out);
        out
    }

    /// git's `anonymize_path`: rewrite every `/`-separated component through
    /// `table`, minting `<prefix><n>` for components never seen before. `n` is the
    /// table size, so tokens are handed out in first-mention order across the whole
    /// stream.
    ///
    /// ```c
    /// while (*path) {
    ///         const char *end_of_component = strchrnul(path, '/');
    ///         size_t len = end_of_component - path;
    ///         const char *c = anonymize_str(map, generate, path, len);
    ///         strbuf_addstr(out, c);
    ///         path = end_of_component;
    ///         if (*path)
    ///                 strbuf_addch(out, *path++);
    /// }
    /// ```
    ///
    /// The loop tests the *remaining* string, not a component count, so it mints
    /// nothing at all for an empty input and nothing after a trailing `/`. Both
    /// matter: `--reflog` labels a commit it reached through no ref with an empty
    /// refname, and anonymizing it has to leave it empty rather than invent a
    /// `ref0` that the stream then carries.
    fn map_components(
        table: &mut HashMap<BString, BString>,
        mut path: &[u8],
        prefix: &str,
        out: &mut BString,
    ) {
        while !path.is_empty() {
            let end = path.iter().position(|b| *b == b'/').unwrap_or(path.len());
            let key = BString::from(path[..end].to_vec());
            if !table.contains_key(&key) {
                let value = BString::from(format!("{prefix}{}", table.len()));
                table.insert(key.clone(), value);
            }
            out.extend_from_slice(&table[&key]);
            path = &path[end..];
            if let Some((separator, rest)) = path.split_first() {
                out.push(*separator);
                path = rest;
            }
        }
    }

    /// `<name> <<email>>` becomes `User <n> <user<n>@example.com>`; the timestamp
    /// is left alone, as git does.
    fn ident(&mut self, ident: &[u8]) -> BString {
        let key = BString::from(ident.to_vec());
        let next = self.idents.len();
        self.idents
            .entry(key)
            .or_insert_with(|| BString::from(format!("User {next} <user{next}@example.com>")))
            .clone()
    }

    /// git's `anonymize_oid`: each distinct object id is replaced with a decimal
    /// counter, zero-padded to the hash's hex width, handed out from 1 in
    /// first-mention order. Used for `--no-data` blob refs and gitlink entries,
    /// where the stream names an object by hash rather than by mark.
    fn oid(&mut self, id: ObjectId) -> BString {
        let width = id.kind().len_in_hex();
        let next = self.oids.len() + 1;
        self.oids
            .entry(id)
            .or_insert_with(|| BString::from(format!("{next:0width$}")))
            .clone()
    }

    fn blob(&mut self) -> Vec<u8> {
        let n = self.blob_counter;
        self.blob_counter += 1;
        format!("anonymous blob {n}").into_bytes()
    }

    fn message(&mut self) -> Vec<u8> {
        let n = self.message_counter;
        self.message_counter += 1;
        format!("subject {n}\n\nbody\n").into_bytes()
    }

    /// git's tag-message anonymization goes through `anonymize_str(&tags, …)`
    /// (fast-export.c:937-940), so it is keyed on the original message rather
    /// than being a bare counter like a commit message or a blob: two tags
    /// carrying the same text — or one tag emitted twice because two selectors
    /// named it — get the *same* generated string.
    fn tag_message(&mut self, original: &[u8]) -> Vec<u8> {
        let key = BString::from(original.to_vec());
        let next = self.tag_messages.len();
        self.tag_messages
            .entry(key)
            .or_insert_with(|| BString::from(format!("tag message {next}")))
            .clone()
            .into()
    }
}

/// Mutable stream state shared by the blob/commit/tag emitters.
struct State {
    out: Vec<u8>,
    /// Mark assigned to every already-exported blob, commit and (with
    /// `--mark-tags`) tag object.
    marks: HashMap<ObjectId, u32>,
    /// Commit marks in assignment order — the only ones `--export-marks` dumps.
    commit_marks: Vec<(u32, ObjectId)>,
    last_mark: u32,
    /// git's `show_progress` counter: one tick per exported blob and commit.
    counter: u64,
    /// Every ref name that has appeared as a `commit`/`reset` label, before
    /// anonymization. A cmdline ref missing from this set needs a trailing
    /// `reset` so the importer still updates it.
    labels: std::collections::HashSet<BString>,
    anon: Anon,
}

impl State {
    /// git's `mark_next_object`.
    fn next_mark(&mut self, id: ObjectId) -> u32 {
        self.last_mark += 1;
        self.marks.insert(id, self.last_mark);
        self.last_mark
    }

    /// git's `show_progress`, called after each exported blob and commit.
    ///
    /// git guards on `progress` being non-zero, then tests `counter % progress`.
    /// The value is a C `int`, so a negative `--progress` is legal and, because
    /// `counter % -1 == 0` for every counter, prints a line after every object —
    /// reproduced here with a signed remainder.
    fn tick(&mut self, opts: &Opts) {
        self.counter += 1;
        if let Some(n) = opts.progress {
            if n != 0 && (self.counter as i64) % n == 0 {
                self.out
                    .extend_from_slice(format!("progress {} objects\n", self.counter).as_bytes());
            }
        }
    }

    /// The ref name as it should appear in the stream: `--refspec` renaming first
    /// (git applies it while collecting refs), then `--anonymize` token mapping.
    fn anon_refname(&mut self, opts: &Opts, name: &BStr) -> BString {
        let mapped = apply_refspec(&opts.refspecs, name);
        if opts.anonymize {
            self.anon.refname(mapped.as_bstr())
        } else {
            mapped
        }
    }

    /// The `author`/`committer`/`tagger` line as it should appear in the stream.
    fn anon_ident_line(&mut self, opts: &Opts, line: &[u8]) -> Vec<u8> {
        if !opts.anonymize {
            return line.to_vec();
        }
        // `<keyword> <name> <<email>> <timestamp> <tz>`
        let Some(kw_end) = line.iter().position(|b| *b == b' ') else {
            return line.to_vec();
        };
        let Some(gt) = line.iter().rposition(|b| *b == b'>') else {
            return line.to_vec();
        };
        let mut out = line[..=kw_end].to_vec();
        out.extend_from_slice(&self.anon.ident(&line[kw_end + 1..=gt]));
        out.extend_from_slice(&line[gt + 1..]);
        out
    }
}

/// git's `apply_refspec`: map a ref name through the first matching `--refspec`,
/// or return it unchanged when none matches (`query_refspecs` returning non-zero).
///
/// Supports the two forms git's refspec grammar produces here: an exact
/// `<src>:<dst>` and a single-`*` wildcard `<pre>*<suf>:<pre2>*<suf2>`, each with
/// an optional leading `+` (force flag, inert for output). The captured middle of
/// a wildcard source is substituted into the destination's `*`.
fn apply_refspec(specs: &[BString], name: &BStr) -> BString {
    let nb: &[u8] = name;
    for spec in specs {
        let raw: &[u8] = spec;
        let raw = raw.strip_prefix(b"+").unwrap_or(raw);
        let Some(colon) = raw.iter().position(|b| *b == b':') else {
            continue;
        };
        let (src, dst) = (&raw[..colon], &raw[colon + 1..]);
        match (
            src.iter().position(|b| *b == b'*'),
            dst.iter().position(|b| *b == b'*'),
        ) {
            (Some(si), Some(di)) => {
                let (spre, ssuf) = (&src[..si], &src[si + 1..]);
                if nb.len() >= spre.len() + ssuf.len()
                    && nb.starts_with(spre)
                    && nb.ends_with(ssuf)
                {
                    let mid = &nb[spre.len()..nb.len() - ssuf.len()];
                    let mut out = BString::from(dst[..di].to_vec());
                    out.extend_from_slice(mid);
                    out.extend_from_slice(&dst[di + 1..]);
                    return out;
                }
            }
            _ => {
                if src == nb {
                    return BString::from(dst.to_vec());
                }
            }
        }
    }
    name.to_owned()
}

/// Emit one commit: its new blobs first, then the `commit` stanza.
///
/// `specs` is empty unless a pathspec is in force; when set, only changes
/// matching it are exported. `override_parents`, present under path limiting,
/// supplies the rewritten parent list (nearest shown ancestors) that git diffs
/// and links against in place of the commit's literal parents.
fn emit_commit(
    repo: &gix::Repository,
    info: &gix::traverse::commit::Info,
    opts: &Opts,
    sources: &HashMap<ObjectId, BString>,
    st: &mut State,
    specs: Option<&super::log::PathspecMatcher>,
    override_parents: Option<&[ObjectId]>,
) -> Result<Option<Fatal>> {
    let id = info.id;
    // Already exported — typically seeded by `--import-marks`. git's `handle_commit`
    // returns at `get_object_mark` before emitting anything, so this commit's blobs,
    // stanza and mark are all skipped and its ref is left to the trailing `reset`.
    if st.marks.contains_key(&id) {
        return Ok(None);
    }
    let data = repo.find_object(id)?.data.clone();
    let (headers, message) = split_object(&data);
    let tree = header_value(headers, b"tree")
        .ok_or_else(|| anyhow!("commit {id} has no tree header"))?;
    let tree = ObjectId::from_hex(tree).map_err(|e| anyhow!("commit {id}: bad tree id: {e}"))?;
    let author = header_line(headers, b"author")
        .ok_or_else(|| anyhow!("commit {id} has no author header"))?;
    let committer = header_line(headers, b"committer")
        .ok_or_else(|| anyhow!("commit {id} has no committer header"))?;
    let parents: Vec<ObjectId> = match override_parents {
        Some(ps) => ps.to_vec(),
        None => info.parent_ids.iter().copied().collect(),
    };

    // `--reencode` only has anything to decide when the commit declares its own
    // encoding. `no` keeps the header as-is, which is what this port does; the
    // other modes either die (`abort`) or need iconv (`yes`).
    if let Some(encoding) = header_value(headers, b"encoding") {
        match opts.reencode {
            ReencodeMode::No => {}
            ReencodeMode::Abort => {
                let encoding = encoding.to_str_lossy();
                return Ok(Some(Fatal(format!(
                    "encountered commit-specific encoding {encoding} in commit {id}; \
                     use --reencode=[yes|no] to handle it"
                ))));
            }
            ReencodeMode::Yes => bail!("--reencode=yes is not supported (no iconv substrate)"),
        }
    }
    // Likewise for `--signed-commits`: `strip`/`warn-strip` are what dropping the
    // header achieves, `abort` dies, and the rest need the gpgsig stream extension.
    if header_value(headers, b"gpgsig").is_some() {
        match opts.signed_commits {
            SignedMode::Strip => {}
            SignedMode::WarnStrip => {
                eprintln!("warning: stripping signature from commit {id}");
            }
            SignedMode::Abort => {
                return Ok(Some(Fatal(format!(
                    "encountered signed commit {id}; use --signed-commits=<mode> to handle it"
                ))));
            }
            SignedMode::Verbatim | SignedMode::WarnVerbatim => bail!(
                "--signed-commits=(verbatim|warn-verbatim) is not supported \
                 (commit {id} carries a signature)"
            ),
        }
    }

    // git's `handle_commit` picks the diff base from one condition:
    //
    //     if (commit->parents &&
    //         (get_object_mark(&commit->parents->item->object) != 0 ||
    //          reference_excluded_commits) &&
    //         !full_tree)
    //             diff_tree_oid(<first parent's tree>, <this tree>, ...);
    //     else
    //             diff_root_tree_oid(<this tree>, ...);
    //
    // so an *unmarked* first parent normally forces the root diff — the importer
    // has never seen that commit, and a delta against it would not apply. Under
    // `--reference-excluded-parents` the stanza names that parent by raw object
    // id instead of dropping it, which means the importer *does* resolve it, so
    // the same option flips this choice back to the incremental diff. Emitting a
    // full tree there would repeat every path the excluded parent already has.
    let base = if opts.full_tree {
        None
    } else {
        match parents.first() {
            Some(p) if st.marks.contains_key(p) || opts.reference_excluded_parents => {
                Some(repo.find_object(*p)?.peel_to_tree()?.id)
            }
            _ => None,
        }
    };
    let mut changes = collect(repo, base, Some(tree))?;
    // Under a pathspec, `show_filemodify` only emits — and only exports blobs for
    // — changes matching it, exactly as git's diff is pathspec-limited.
    if let Some(specs) = specs {
        changes.retain(|c| specs.matches(c.path.as_bstr()));
    }

    // git exports every referenced blob before the commit that first names it,
    // walking the diff queue in order.
    if !opts.no_data {
        for c in &changes {
            if let Some(new) = c.new {
                if new.mode.kind() != EntryKind::Commit {
                    emit_blob(repo, new.id, opts, st)?;
                }
            }
        }
    }

    // A commit reached only through `--reflog` has no name; git prints an empty one.
    let source = sources.get(&id).cloned().unwrap_or_default();
    st.labels.insert(source.clone());
    let refname = st.anon_refname(opts, source.as_bstr());

    let mark = st.next_mark(id);
    st.commit_marks.push((mark, id));

    if parents.is_empty() {
        st.out.extend_from_slice(b"reset ");
        st.out.extend_from_slice(&refname);
        st.out.push(b'\n');
    }
    st.out.extend_from_slice(b"commit ");
    st.out.extend_from_slice(&refname);
    st.out
        .extend_from_slice(format!("\nmark :{mark}\n").as_bytes());
    if opts.show_original_ids {
        st.out
            .extend_from_slice(format!("original-oid {id}\n").as_bytes());
    }
    // The stanza prints `author` then `committer`, but `handle_commit` anonymizes
    // them the other way round:
    //
    //     anonymize_ident_line(&committer, &committer_end);
    //     anonymize_ident_line(&author, &author_end);
    //
    // and the generated tokens are handed out in call order, so on a commit whose
    // two identities differ the committer is `User 0` and the author `User 1`.
    let committer = st.anon_ident_line(opts, committer);
    let author = st.anon_ident_line(opts, author);
    st.out.extend_from_slice(&author);
    st.out.push(b'\n');
    st.out.extend_from_slice(&committer);
    st.out.push(b'\n');
    let message: Vec<u8> = if opts.anonymize {
        st.anon.message()
    } else {
        message.to_vec()
    };
    st.out
        .extend_from_slice(format!("data {}\n", message.len()).as_bytes());
    st.out.extend_from_slice(&message);

    // Parents that were not exported are skipped entirely, unless
    // `--reference-excluded-parents` asks git to name them by raw object id; the
    // first *printed* parent is `from`, the rest are `merge`.
    let mut printed = 0usize;
    for p in &parents {
        let reference: BString = match st.marks.get(p).copied() {
            Some(pmark) => BString::from(format!(":{pmark}")),
            // `printf("%s\n", anonymize ? anonymize_oid(...) : oid_to_hex(...))`:
            // a raw id would leak real history through an anonymized stream, so
            // git hands it to the same sequential-fake-id table `--no-data` blobs
            // and gitlinks use.
            None if opts.reference_excluded_parents => {
                if opts.anonymize {
                    st.anon.oid(*p)
                } else {
                    BString::from(p.to_hex().to_string())
                }
            }
            None => continue,
        };
        st.out
            .extend_from_slice(if printed == 0 { b"from " } else { b"merge " });
        st.out.extend_from_slice(&reference);
        st.out.push(b'\n');
        printed += 1;
    }

    if opts.full_tree {
        st.out.extend_from_slice(b"deleteall\n");
    }
    // `show_filemodify` reorders the whole diff queue before rendering it
    // (`QSORT(q->queue, q->nr, depth_first)`, fast-export.c:445) — after the blob
    // export above, which is why the mark numbers still follow tree order.
    changes.sort_by(depth_first);
    for c in &changes {
        render_change(c, opts, st)?;
    }
    st.out.push(b'\n');
    st.tick(opts);
    Ok(None)
}

/// git's `export_blob`: a `blob` stanza, once per distinct object.
fn emit_blob(repo: &gix::Repository, id: ObjectId, opts: &Opts, st: &mut State) -> Result<()> {
    if st.marks.contains_key(&id) {
        return Ok(());
    }
    let data = if opts.anonymize {
        st.anon.blob()
    } else {
        repo.find_object(id)?.data.clone()
    };
    let mark = st.next_mark(id);
    st.out
        .extend_from_slice(format!("blob\nmark :{mark}\n").as_bytes());
    if opts.show_original_ids {
        st.out
            .extend_from_slice(format!("original-oid {id}\n").as_bytes());
    }
    st.out
        .extend_from_slice(format!("data {}\n", data.len()).as_bytes());
    st.out.extend_from_slice(&data);
    st.out.push(b'\n');
    st.tick(opts);
    Ok(())
}

/// git's `handle_tag`: the `tag` stanza for an annotated tag.
fn emit_tag(
    repo: &gix::Repository,
    full_name: &BStr,
    tag_id: ObjectId,
    opts: &Opts,
    st: &mut State,
) -> Result<Option<Fatal>> {
    // `handle_tag` has no "already exported" guard of any kind: unlike
    // `export_blob` and `handle_commit`, it never consults `get_object_mark` on
    // the tag itself. A tag reached twice is written twice, and under
    // `--mark-tags` `mark_next_object` (fast-export.c:1015) hands the second copy
    // a *fresh* mark — verified against stock with a marks file naming the tag
    // object, which git re-marks and re-emits regardless. `export_marks` only
    // dumps commit marks, so a tag mark cannot come back through
    // `--import-marks` in the first place.
    let data = repo.find_object(tag_id)?.data.clone();
    let (headers, mut message) = split_object(&data);
    // git anonymizes the message straight off the object, before the signature
    // block is looked at, so the anonymization table is keyed on the message as
    // stored — not on whatever `--signed-tags=strip` leaves of it.
    let original_message = message;
    let target = header_value(headers, b"object")
        .ok_or_else(|| anyhow!("tag {tag_id} has no object header"))?;
    let target = ObjectId::from_hex(target).map_err(|e| anyhow!("tag {tag_id}: {e}"))?;
    if header_value(headers, b"type") == Some(&b"tag"[..]) {
        bail!("nested tags are not supported (tag {tag_id} tags another tag)");
    }
    let commit_id = repo.find_object(target)?.peel_to_commit()?.id;

    let Some(mark) = st.marks.get(&commit_id).copied() else {
        return match opts.filtered_tag {
            FilteredTagMode::Drop => Ok(None),
            FilteredTagMode::Abort => Ok(Some(Fatal(format!(
                "tag {tag_id} tags unexported object; \
                 use --tag-of-filtered-object=<mode> to handle it"
            )))),
            FilteredTagMode::Rewrite => bail!(
                "--tag-of-filtered-object=rewrite is not supported \
                 (tag {tag_id} tags an unexported object)"
            ),
        };
    };

    // git looks for the signature block and applies --signed-tags to it.
    if let Some(pos) = find_sub(message, b"\n-----BEGIN PGP SIGNATURE-----\n") {
        match opts.signed_tags {
            SignedMode::Abort => {
                return Ok(Some(Fatal(format!(
                    "encountered signed tag {tag_id}; use --signed-tags=<mode> to handle it"
                ))));
            }
            SignedMode::WarnVerbatim => eprintln!("warning: exporting signed tag {tag_id}"),
            SignedMode::Verbatim => {}
            SignedMode::WarnStrip => {
                eprintln!("warning: stripping signature from tag {tag_id}");
                message = &message[..pos + 1];
            }
            SignedMode::Strip => message = &message[..pos + 1],
        }
    }

    let printed_name = st.anon_refname(opts, full_name);
    let full: &[u8] = &printed_name;
    let short = full.strip_prefix(&b"refs/tags/"[..]).unwrap_or(full).to_vec();
    st.out.extend_from_slice(b"tag ");
    st.out.extend_from_slice(&short);
    st.out.push(b'\n');
    if opts.mark_tags {
        let tmark = st.next_mark(tag_id);
        st.out
            .extend_from_slice(format!("mark :{tmark}\n").as_bytes());
    }
    st.out
        .extend_from_slice(format!("from :{mark}\n").as_bytes());
    if opts.show_original_ids {
        st.out
            .extend_from_slice(format!("original-oid {tag_id}\n").as_bytes());
    }
    match header_line(headers, b"tagger") {
        Some(line) => {
            let line = st.anon_ident_line(opts, line);
            st.out.extend_from_slice(&line);
            st.out.push(b'\n');
        }
        None if opts.fake_missing_tagger => {
            st.out.extend_from_slice(FAKE_TAGGER.as_bytes());
            st.out.push(b'\n');
        }
        None => {}
    }
    let message: Vec<u8> = if opts.anonymize {
        st.anon.tag_message(original_message)
    } else {
        message.to_vec()
    };
    st.out
        .extend_from_slice(format!("data {}\n", message.len()).as_bytes());
    st.out.extend_from_slice(&message);
    st.out.push(b'\n');
    Ok(None)
}

// ---------------------------------------------------------------------------
// Raw object parsing
// ---------------------------------------------------------------------------

/// Split a commit or tag object into its header block (each line still carrying
/// its terminating newline) and the message that follows the blank line.
fn split_object(data: &[u8]) -> (&[u8], &[u8]) {
    match find_sub(data, b"\n\n") {
        Some(i) => (&data[..i + 1], &data[i + 2..]),
        None => (data, &[]),
    }
}

/// The complete `"<name> <value>"` header line, without its newline.
///
/// Continuation lines (those starting with a space, as `gpgsig` uses) are skipped
/// so they can never be mistaken for a header of their own.
fn header_line<'a>(headers: &'a [u8], name: &[u8]) -> Option<&'a [u8]> {
    for line in headers.split(|b| *b == b'\n') {
        if line.first() == Some(&b' ') {
            continue;
        }
        if line.len() > name.len() && line.starts_with(name) && line[name.len()] == b' ' {
            return Some(line);
        }
    }
    None
}

/// Just the value part of a header line.
fn header_value<'a>(headers: &'a [u8], name: &[u8]) -> Option<&'a [u8]> {
    header_line(headers, name).map(|line| &line[name.len() + 1..])
}

/// Index of the first occurrence of `needle` in `haystack`.
fn find_sub(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    haystack.windows(needle.len()).position(|w| w == needle)
}

// ---------------------------------------------------------------------------
// Tree diff (recursive, git's emission order)
// ---------------------------------------------------------------------------

/// One side of a change: the entry as it exists in that tree.
#[derive(Clone, Copy)]
struct Side {
    mode: EntryMode,
    id: ObjectId,
}

struct Change {
    new: Option<Side>,
    path: BString,
}

/// A tree entry, materialised so the borrow on the tree buffer ends before we recurse.
struct Entry {
    mode: EntryMode,
    name: BString,
    id: ObjectId,
}

fn read_entries(repo: &gix::Repository, id: Option<ObjectId>) -> Result<Vec<Entry>> {
    let Some(id) = id else { return Ok(Vec::new()) };
    let tree = repo.find_tree(id)?;
    Ok(tree
        .decode()?
        .entries
        .iter()
        .map(|e| Entry {
            mode: e.mode,
            name: BString::from(e.filename.to_vec()),
            id: e.oid.to_owned(),
        })
        .collect())
}

/// git's `tree-entry-comparison`: names compare byte-wise with an implicit `/`
/// appended to tree entries.
fn entry_cmp(a: &Entry, b: &Entry) -> Ordering {
    let common = a.name.len().min(b.name.len());
    match a.name[..common].cmp(&b.name[..common]) {
        Ordering::Equal => {
            let ac = a.name.get(common).copied().or(a.mode.is_tree().then_some(b'/'));
            let bc = b.name.get(common).copied().or(b.mode.is_tree().then_some(b'/'));
            ac.cmp(&bc)
        }
        other => other,
    }
}

/// Every change turning `old` into `new`, recursively, in git's emission order.
///
/// Trees themselves are never reported: `fast-export` always sets
/// `diffopt.flags.recursive`, so only leaves reach the `M`/`D` renderer.
fn collect(
    repo: &gix::Repository,
    old: Option<ObjectId>,
    new: Option<ObjectId>,
) -> Result<Vec<Change>> {
    let mut out = Vec::new();
    walk(repo, old, new, BStr::new(""), &mut out)?;
    Ok(out)
}

fn walk(
    repo: &gix::Repository,
    old: Option<ObjectId>,
    new: Option<ObjectId>,
    prefix: &BStr,
    out: &mut Vec<Change>,
) -> Result<()> {
    let lhs = read_entries(repo, old)?;
    let rhs = read_entries(repo, new)?;
    let (mut i, mut j) = (0usize, 0usize);

    while i < lhs.len() || j < rhs.len() {
        let order = match (lhs.get(i), rhs.get(j)) {
            (Some(a), Some(b)) => entry_cmp(a, b),
            (Some(_), None) => Ordering::Less,
            (None, Some(_)) => Ordering::Greater,
            (None, None) => unreachable!("loop condition guarantees one side has an entry"),
        };
        match order {
            Ordering::Equal => {
                let (a, b) = (&lhs[i], &rhs[j]);
                i += 1;
                j += 1;
                if a.mode == b.mode && a.id == b.id {
                    continue;
                }
                let path = join(prefix, a.name.as_bstr());
                if a.mode.is_tree() {
                    walk(repo, Some(a.id), Some(b.id), path.as_bstr(), out)?;
                } else {
                    out.push(Change {
                        new: Some(side(b)),
                        path,
                    });
                }
            }
            Ordering::Less => {
                let a = &lhs[i];
                i += 1;
                let path = join(prefix, a.name.as_bstr());
                if a.mode.is_tree() {
                    walk(repo, Some(a.id), None, path.as_bstr(), out)?;
                } else {
                    out.push(Change { new: None, path });
                }
            }
            Ordering::Greater => {
                let b = &rhs[j];
                j += 1;
                let path = join(prefix, b.name.as_bstr());
                if b.mode.is_tree() {
                    walk(repo, None, Some(b.id), path.as_bstr(), out)?;
                } else {
                    out.push(Change {
                        new: Some(side(b)),
                        path,
                    });
                }
            }
        }
    }
    Ok(())
}

fn side(e: &Entry) -> Side {
    Side {
        mode: e.mode,
        id: e.id,
    }
}

fn join(prefix: &BStr, name: &BStr) -> BString {
    let mut p = BString::from(prefix.to_vec());
    if !p.is_empty() {
        p.push(b'/');
    }
    p.extend_from_slice(name);
    p
}

/// git's `depth_first` (fast-export.c:353-381), the comparator `show_filemodify`
/// sorts its diff queue with.
///
/// Names compare byte-wise over their *common* length, and a tie there puts the
/// **longer** name first — "strcmp will sort 'd' before 'd/e', we want 'd/e'
/// before 'd'", so that everything below a directory is emitted before the entry
/// that replaces the directory itself. The rule is length-based, not `/`-aware,
/// so it separates prefix-related siblings too: `C2`, `C3/x` and `C.a` all come
/// out ahead of plain `C`.
///
/// git's third leg breaks a remaining tie by moving `R`ename pairs last. This
/// port emits no `R`/`C` stanzas (see the module note), so every pair here is an
/// add/modify/delete and the leg is unreachable — and since a tree diff yields
/// at most one pair per path, the two names can never be equal either, which is
/// what makes a stable `sort_by` agree with git's unstable `qsort`.
fn depth_first(a: &Change, b: &Change) -> Ordering {
    let (x, y) = (a.path.as_slice(), b.path.as_slice());
    let common = x.len().min(y.len());
    match x[..common].cmp(&y[..common]) {
        Ordering::Equal => y.len().cmp(&x.len()),
        other => other,
    }
}

/// git's `show_filemodify`: `D <path>` for a removal, `M <mode> <ref> <path>`
/// otherwise, where `<ref>` is a mark for exported blobs and a raw hash for
/// gitlinks and `--no-data`.
fn render_change(c: &Change, opts: &Opts, st: &mut State) -> Result<()> {
    let path = if opts.anonymize {
        st.anon.path(c.path.as_bstr())
    } else {
        c.path.clone()
    };
    match c.new {
        None => {
            st.out.extend_from_slice(b"D ");
            print_path(&mut st.out, path.as_bstr());
            st.out.push(b'\n');
        }
        Some(new) => {
            let mode = new.mode.value();
            let reference: Vec<u8> = if opts.no_data || new.mode.kind() == EntryKind::Commit {
                // git names the object by hash here; `--anonymize` substitutes its
                // generated sequential id (`anonymize_oid`).
                if opts.anonymize {
                    st.anon.oid(new.id).to_vec()
                } else {
                    new.id.to_hex().to_string().into_bytes()
                }
            } else {
                let mark = st
                    .marks
                    .get(&new.id)
                    .ok_or_else(|| anyhow!("blob {} was not exported", new.id))?;
                format!(":{mark}").into_bytes()
            };
            st.out.extend_from_slice(format!("M {mode:06o} ").as_bytes());
            st.out.extend_from_slice(&reference);
            st.out.push(b' ');
            print_path(&mut st.out, path.as_bstr());
            st.out.push(b'\n');
        }
    }
    Ok(())
}

/// git's `print_path`: C-style quoting when a byte needs escaping, plain double
/// quotes when the only special character is a space, bare otherwise.
fn print_path(out: &mut Vec<u8>, path: &BStr) {
    if crate::quote::needs_c_quote(path) {
        out.extend_from_slice(&crate::quote::quoted_name_bytes(path));
    } else if path.contains(&b' ') {
        out.push(b'"');
        out.extend_from_slice(path);
        out.push(b'"');
    } else {
        out.extend_from_slice(path);
    }
}
