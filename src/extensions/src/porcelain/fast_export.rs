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
//! recursive `diff_tree_oid` emission order, which the stream's `M`/`D` line
//! order and the blob export order both depend on.
//!
//! ### Argument handling
//!
//! git processes a `fast-export` command line in a fixed order, and the exit
//! code depends on which stage rejects it. This module reproduces that order:
//!
//! 1. no arguments at all → the option usage on stderr, exit 129
//! 2. `setup_revisions` classifies each positional: a spec that resolves is a
//!    revision; one that names a working-tree path is a pathspec; one that is
//!    neither is `fatal: ambiguous argument ...`, exit 128. A `--` here is inert
//!    — git consumes it and classifies the rest identically.
//! 3. leftover/unknown arguments → the option usage on stderr, exit 129
//! 4. `--anonymize-map` without `--anonymize` → fatal, exit 128
//! 5. `--ancestry-path` with no negative revision → fatal, exit 128
//!
//! ### Covered (byte-identical stdout, exit code and marks file against stock git)
//!
//! * `fast-export --all`, `--branches`, `--tags`, `--remotes`, `--reflog`
//! * `<rev>...`, `<a>..<b>`, `<a>...<b>`, `^<rev>`, `--not`
//! * blob / commit / `reset` / lightweight-tag / annotated-tag stanzas, including
//!   the trailing `reset`+`tag` block emitted in git's reverse-sorted ref order,
//!   with `from <null-oid>` for refs whose commit was excluded
//! * `--no-data`, `--data`, `--full-tree`, `--use-done-feature`,
//!   `--show-original-ids`, `--mark-tags`, `--progress=<n>`, `--export-marks=<file>`
//! * `--import-marks=<file>` / `--import-marks-if-exists=<file>` — pre-seed the
//!   mark table from a prior export: already-marked blobs/commits/tags are not
//!   re-emitted, the id counter continues past the highest imported mark, and
//!   `from :<mark>` links an incremental commit to its imported parent.
//!   `--import-marks` dies `could not open '<file>' for reading` (128) on a
//!   missing file; the `-if-exists` form treats it as empty
//! * `--reference-excluded-parents` — a parent outside the stream is named by raw
//!   object id (`from <oid>` / `merge <oid>`) instead of being dropped
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
//! * `--ancestry-path` (with negative revisions), `--boundary` (with negative
//!   revisions), and magic/glob pathspecs (`:(glob)`, `:!exclude`, `*.rs`), whose
//!   matcher this port does not reproduce.

use anyhow::{anyhow, bail, Result};
use std::cmp::Ordering;
use std::collections::HashMap;
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

/// git's `die()` for a revision that neither resolves nor names a path.
fn fatal_ambiguous(arg: &str) -> ExitCode {
    eprint!(
        "fatal: ambiguous argument '{arg}': unknown revision or path not in the working tree.\n\
         Use '--' to separate paths from revisions, like this:\n\
         'git <command> [<revision>...] -- [<file>...]'\n"
    );
    ExitCode::from(128)
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
    fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "verbatim" => SignedMode::Verbatim,
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
enum Order {
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
    let mut rev_tokens: Vec<(String, bool)> = Vec::new();
    let mut negate_rest = false;
    let (mut use_all, mut use_branches, mut use_tags, mut use_remotes, mut use_reflog) =
        (false, false, false, false, false);

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

    for a in args {
        let s = a.as_str();
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
            // git keeps `--` in argv but, once its own `parse_options` has run,
            // `setup_revisions` classifies each following token exactly as it
            // classifies one before `--`: a rev, an existing path, or a fatal.
            // For fast-export the separator therefore has no observable effect,
            // so it is consumed and the tokens after it flow through the same
            // rev/pathspec resolution as the ones before (see the resolve stage).
            "--" => {}

            // ---- fast-export's own options ----
            "--no-data" => opts.no_data = true,
            "--data" => opts.no_data = false,
            "--full-tree" => opts.full_tree = true,
            "--use-done-feature" => opts.use_done = true,
            "--show-original-ids" => opts.show_original_ids = true,
            "--mark-tags" => opts.mark_tags = true,
            "--fake-missing-tagger" => opts.fake_missing_tagger = true,
            "--anonymize" => opts.anonymize = true,
            "--reference-excluded-parents" => opts.reference_excluded_parents = true,

            // ---- rev-list selection ----
            "--all" => use_all = true,
            "--branches" => use_branches = true,
            "--tags" => use_tags = true,
            "--remotes" => use_remotes = true,
            "--reflog" => use_reflog = true,
            "--not" => negate_rest = true,

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

            _ if s.starts_with("--progress=") => {
                // git's `--progress` is a parse-options `OPTION_INTEGER`: base-0
                // magnitude, an optional k/m/g suffix, a signed C-int range, and
                // a *usage* (129) error on anything else.
                let v = &s["--progress=".len()..];
                match parse_progress_int(v) {
                    Some(n) => opts.progress = Some(n),
                    None => return Ok(usage_exit()),
                }
            }
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
            _ if s.starts_with("--signed-tags=") => {
                match SignedMode::parse(&s["--signed-tags=".len()..]) {
                    Some(m) => opts.signed_tags = m,
                    None => return Ok(usage_exit()),
                }
            }
            _ if s.starts_with("--signed-commits=") => {
                match SignedMode::parse(&s["--signed-commits=".len()..]) {
                    Some(m) => opts.signed_commits = m,
                    None => return Ok(usage_exit()),
                }
            }
            _ if s.starts_with("--tag-of-filtered-object=") => {
                opts.filtered_tag = match &s["--tag-of-filtered-object=".len()..] {
                    "abort" => FilteredTagMode::Abort,
                    "drop" => FilteredTagMode::Drop,
                    "rewrite" => FilteredTagMode::Rewrite,
                    _ => return Ok(usage_exit()),
                };
            }
            _ if s.starts_with("--reencode=") => {
                // git's `parse_opt_reencode_mode`: `abort`, or a
                // `git_parse_maybe_bool` value (so `yes`/`true`/`on`/`1`/any
                // non-zero int → yes, `no`/`false`/`off`/`0`/empty → no).
                // Unrecognised values are a usage error (129).
                match parse_reencode(&s["--reencode=".len()..]) {
                    Some(m) => opts.reencode = m,
                    None => return Ok(usage_exit()),
                }
            }

            // Anything else beginning with `-` survives both option parsers and
            // ends up as a leftover argument, which git turns into a usage error.
            _ if s.starts_with('-') && s != "-" => leftover = true,

            _ => rev_tokens.push((s.to_string(), negate_rest)),
        }
    }

    let repo = gix::discover(".")?;

    // ---- Stage 2: `setup_revisions` resolves revisions and dies on the first bad one. ----
    // A token that resolves is a revision; one that does not is, as in git's
    // `handle_revision_arg`, a pathspec when it names a working-tree path and a
    // `fatal: ambiguous argument` otherwise. Magic/glob pathspecs are matched by
    // git's own parser, which this port does not reproduce, so they bail terse.
    let mut sel = Selection::default();
    for (tok, negated) in &rev_tokens {
        if add_rev_token(&repo, tok, *negated, &mut sel).is_ok() {
            continue;
        }
        if *negated {
            return Ok(fatal_ambiguous(tok));
        }
        if is_magic_or_glob(tok) {
            bail!("magic pathspecs are not supported");
        }
        if is_worktree_path(&repo, tok) {
            pathspecs.push(tok.clone());
        } else {
            return Ok(fatal_ambiguous(tok));
        }
    }

    // ---- Stage 3: leftover arguments. ----
    if leftover {
        return Ok(usage_exit());
    }

    // ---- Stage 4/5: the two late fatals, in git's order. ----
    if !anonymize_map.is_empty() && !opts.anonymize {
        return Ok(fatal("the option '--anonymize-map' requires '--anonymize'"));
    }
    if ancestry_path && sel.hidden.is_empty() {
        return Ok(fatal("--ancestry-path given but there are no bottom commits"));
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
    if ancestry_path {
        bail!("--ancestry-path is not supported");
    }
    if boundary && !sel.hidden.is_empty() {
        bail!("--boundary with negative revisions is not supported");
    }
    if !anonymize_map.is_empty() {
        bail!("--anonymize-map is not supported");
    }

    // ---- `--refspec`: rename exported refs through push-style refspecs. ----
    opts.refspecs = refspecs.iter().map(|s| BString::from(s.as_str())).collect();

    // ---- Ref selection (`--all` and friends), in git's iteration order. ----
    let mut cmdline: Vec<(BString, ObjectId)> = Vec::new();
    if use_all || use_branches || use_tags || use_remotes {
        let mut names: Vec<BString> = Vec::new();
        for reference in repo.references()?.all()? {
            let reference = reference.map_err(|e| anyhow!("{e}"))?;
            let name = reference.name().as_bstr().to_owned();
            let keep = use_all
                || (use_branches && name.starts_with(b"refs/heads/"))
                || (use_tags && name.starts_with(b"refs/tags/"))
                || (use_remotes && name.starts_with(b"refs/remotes/"));
            if keep {
                names.push(name);
            }
        }
        names.sort();
        for name in names {
            let spec = name.to_str().map_err(|_| anyhow!("non-UTF-8 ref {name:?}"))?;
            let id = repo.rev_parse_single(spec)?.detach();
            cmdline.push((name, id));
        }
        // git's `--all` also feeds `head_ref`, which only contributes a distinct
        // entry when HEAD is detached; otherwise it resolves to a ref already listed.
        if use_all && repo.head()?.is_detached() {
            if let Ok(id) = repo.rev_parse_single("HEAD") {
                cmdline.push((BString::from("HEAD"), id.detach()));
            }
        }
    }
    for (name, target) in &sel.named {
        cmdline.push((name.clone(), *target));
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

    for (name, target) in &cmdline {
        let object = repo.find_object(*target)?;
        let is_tag = object.kind == gix::object::Kind::Tag;
        let Ok(commit) = object.peel_to_commit() else {
            continue; // a ref to a blob or tree is not exportable, as in git
        };
        let commit_id = commit.id;
        if is_tag {
            tag_refs.push((name.clone(), *target));
        } else {
            commit_refs.push((name.clone(), commit_id));
        }
        sources.entry(commit_id).or_insert_with(|| name.clone());
        tips.push(commit_id);
    }
    // Positional revisions that named no ref (a raw commit id) still contribute
    // history, behind the refs `--all` and friends put in front of them.
    tips.extend(sel.tips.iter().copied());

    // `--reflog` contributes tips with no name at all: git adds every object a
    // reflog mentions to the pending list under an empty name, so a commit
    // reached that way prints under an empty refname instead of inheriting a
    // branch's. Claiming the source here rather than leaving the entry vacant is
    // what stops the propagation walk below from labelling it.
    if use_reflog {
        let mut reflog_tips: Vec<ObjectId> = Vec::new();
        collect_reflog_tips(&repo, &mut reflog_tips)?;
        for id in &reflog_tips {
            sources.entry(*id).or_default();
        }
        tips.extend(reflog_tips);
    }

    // git dedupes pending objects through the `SEEN` flag: the first mention of
    // an object wins and the order of the rest is left alone.
    dedup_first_wins(&mut tips);

    let hidden = sel.hidden.clone();

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
            for parent in &info.parent_ids {
                sources.entry(*parent).or_insert_with(|| src.clone());
            }
        }
    }

    // ---- Emission order: `rev-list [--topo-order|--date-order] --reverse`. ----
    let mut order_list: Vec<gix::traverse::commit::Info> = Vec::new();
    if !tips.is_empty() {
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
        .parents(if first_parent {
            gix::traverse::commit::Parents::First
        } else {
            gix::traverse::commit::Parents::All
        })
        .build()?;
        for info in topo {
            order_list.push(info?);
        }
    }

    // ---- Path limiting: git's default history simplification with parent
    // rewriting (`revs.prune_data` + `revs.rewrite_parents`). ----
    // A commit is shown iff its pathspec-restricted diff against the parent it
    // follows is non-empty (git's `try_to_simplify_commit`). Each shown commit's
    // parents, and every ref that pointed at a pruned commit, are then rewritten
    // to the nearest shown ancestor by following first parents through the pruned
    // (TREESAME) run — exactly `rewrite_one`.
    let specs: Vec<Vec<u8>> = pathspecs.iter().map(|s| s.as_bytes().to_vec()).collect();
    let filtering = !specs.is_empty();
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
        if let Some(f) = emit_commit(&repo, info, &opts, &sources, &mut st, &specs, override_parents)?
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
            // The commit was excluded from this export; git points the ref at the
            // null oid, which fast-import reads as "delete this branch".
            None => st
                .out
                .extend_from_slice(format!("\nfrom {NULL_OID}\n\n").as_bytes()),
        }
    }

    tag_refs.sort();
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

/// What the positional arguments selected: named cmdline refs, positive tips,
/// and the negative (`^rev`, left side of a range) commits.
#[derive(Default)]
struct Selection {
    named: Vec<(BString, ObjectId)>,
    tips: Vec<ObjectId>,
    hidden: Vec<ObjectId>,
}

/// Resolve one positional revision argument, in git's `handle_revision_arg` shape.
///
/// `Err(())` means the whole argument is neither a revision nor a path, which the
/// caller turns into git's `ambiguous argument` fatal.
fn add_rev_token(
    repo: &gix::Repository,
    tok: &str,
    negated: bool,
    sel: &mut Selection,
) -> std::result::Result<(), ()> {
    if tok.is_empty() {
        return Err(());
    }
    if let Some(rest) = tok.strip_prefix('^') {
        let id = commit_of(repo, rest).ok_or(())?;
        sel.hidden.push(id);
        return Ok(());
    }
    if let Some((l, r)) = tok.split_once("...") {
        let (l, r) = (default_head(l), default_head(r));
        let (lc, rc) = (commit_of(repo, l).ok_or(())?, commit_of(repo, r).ok_or(())?);
        // `A...B` is `A B --not $(git merge-base --all A B)`.
        for base in repo.merge_bases_many(lc, &[rc]).map_err(|_| ())? {
            sel.hidden.push(base.detach());
        }
        add_positive(repo, l, lc, sel);
        add_positive(repo, r, rc, sel);
        return Ok(());
    }
    if let Some((l, r)) = tok.split_once("..") {
        let (l, r) = (default_head(l), default_head(r));
        sel.hidden.push(commit_of(repo, l).ok_or(())?);
        let rc = commit_of(repo, r).ok_or(())?;
        add_positive(repo, r, rc, sel);
        return Ok(());
    }
    let id = commit_of(repo, tok).ok_or(())?;
    if negated {
        sel.hidden.push(id);
    } else {
        add_positive(repo, tok, id, sel);
    }
    Ok(())
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

/// git's `looks_like_pathspec` territory this port does not implement: a magic
/// pathspec (`:(glob)`, `:!exclude`, `:/`) or one carrying glob metacharacters.
fn is_magic_or_glob(tok: &str) -> bool {
    tok.starts_with(':') || tok.bytes().any(|b| matches!(b, b'*' | b'?' | b'['))
}

/// git's `file_exists`: whether a plain pathspec names something in the working
/// tree. `lstat`-style so a directory or a broken symlink still counts.
fn is_worktree_path(repo: &gix::Repository, tok: &str) -> bool {
    repo.workdir()
        .map(|wd| wd.join(tok).symlink_metadata().is_ok())
        .unwrap_or(false)
}

/// git's plain (non-magic) pathspec match: a pathspec matches a path when equal
/// to it or a leading directory prefix ending at a component boundary, so `dir`
/// matches `dir/file` but `fil` does not match `file`.
fn path_matches(path: &[u8], specs: &[Vec<u8>]) -> bool {
    specs.iter().any(|spec| {
        let spec = spec.strip_suffix(b"/").unwrap_or(spec);
        spec.is_empty()
            || path == spec
            || (path.len() > spec.len() && path.starts_with(spec) && path[spec.len()] == b'/')
    })
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
    specs: &[Vec<u8>],
) -> Result<bool> {
    let changes = collect(repo, old, Some(new))?;
    Ok(changes.iter().any(|c| path_matches(c.path.as_bstr(), specs)))
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
            Some(s) => match s.followed.first() {
                Some(p) => id = *p,
                None => return None,
            },
        }
    }
    Some(id)
}

/// Record a positive tip, plus a cmdline ref entry when the spec dwims to a ref.
///
/// git's `get_tags_and_duplicates` only names arguments that `dwim_ref` resolves;
/// a raw commit id contributes history but no label.
fn add_positive(repo: &gix::Repository, spec: &str, commit_id: ObjectId, sel: &mut Selection) {
    sel.tips.push(commit_id);
    if let Some((name, target)) = dwim_ref(repo, spec) {
        sel.named.push((name, target));
    }
}

/// Resolve a revision to the id of the commit it names, peeling tags.
fn commit_of(repo: &gix::Repository, spec: &str) -> Option<ObjectId> {
    Some(
        repo.rev_parse_single(spec)
            .ok()?
            .object()
            .ok()?
            .peel_to_commit()
            .ok()?
            .id,
    )
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
    blob_counter: u32,
    message_counter: u32,
    tag_message_counter: u32,
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

    /// Rewrite every `/`-separated component through `table`, minting
    /// `<prefix><n>` for components never seen before. `n` is the table size, so
    /// tokens are handed out in first-mention order across the whole stream.
    fn map_components(
        table: &mut HashMap<BString, BString>,
        input: &[u8],
        prefix: &str,
        out: &mut BString,
    ) {
        for (i, component) in input.split(|b| *b == b'/').enumerate() {
            if i > 0 {
                out.push(b'/');
            }
            let key = BString::from(component.to_vec());
            if !table.contains_key(&key) {
                let value = BString::from(format!("{prefix}{}", table.len()));
                table.insert(key.clone(), value);
            }
            out.extend_from_slice(&table[&key]);
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

    fn tag_message(&mut self) -> Vec<u8> {
        let n = self.tag_message_counter;
        self.tag_message_counter += 1;
        format!("tag message {n}").into_bytes()
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
    specs: &[Vec<u8>],
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

    // git diffs against the first parent only when that parent is itself in the
    // stream; otherwise the commit is emitted as a root against the empty tree.
    let base = if opts.full_tree {
        None
    } else {
        match parents.first() {
            Some(p) if st.marks.contains_key(p) => Some(repo.find_object(*p)?.peel_to_tree()?.id),
            _ => None,
        }
    };
    let mut changes = collect(repo, base, Some(tree))?;
    // Under a pathspec, `show_filemodify` only emits — and only exports blobs for
    // — changes matching it, exactly as git's diff is pathspec-limited.
    if !specs.is_empty() {
        changes.retain(|c| path_matches(c.path.as_bstr(), specs));
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
    let author = st.anon_ident_line(opts, author);
    st.out.extend_from_slice(&author);
    st.out.push(b'\n');
    let committer = st.anon_ident_line(opts, committer);
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
        let reference = match st.marks.get(p).copied() {
            Some(pmark) => format!(":{pmark}"),
            None if opts.reference_excluded_parents => p.to_hex().to_string(),
            None => continue,
        };
        st.out
            .extend_from_slice(if printed == 0 { b"from " } else { b"merge " });
        st.out.extend_from_slice(reference.as_bytes());
        st.out.push(b'\n');
        printed += 1;
    }

    if opts.full_tree {
        st.out.extend_from_slice(b"deleteall\n");
    }
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
    // A tag already carrying a mark (seeded by `--import-marks` alongside
    // `--mark-tags`) has been exported before; git skips it.
    if st.marks.contains_key(&tag_id) {
        return Ok(None);
    }
    let data = repo.find_object(tag_id)?.data.clone();
    let (headers, mut message) = split_object(&data);
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
        st.anon.tag_message()
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
    let needs_quote = path
        .iter()
        .any(|b| *b < 0x20 || *b >= 0x7f || *b == b'"' || *b == b'\\');
    if needs_quote {
        out.push(b'"');
        for b in path.iter().copied() {
            match b {
                0x07 => out.extend_from_slice(b"\\a"),
                0x08 => out.extend_from_slice(b"\\b"),
                b'\t' => out.extend_from_slice(b"\\t"),
                b'\n' => out.extend_from_slice(b"\\n"),
                0x0b => out.extend_from_slice(b"\\v"),
                0x0c => out.extend_from_slice(b"\\f"),
                b'\r' => out.extend_from_slice(b"\\r"),
                b'"' => out.extend_from_slice(b"\\\""),
                b'\\' => out.extend_from_slice(b"\\\\"),
                b if b < 0x20 || b >= 0x7f => {
                    out.extend_from_slice(format!("\\{b:03o}").as_bytes());
                }
                b => out.push(b),
            }
        }
        out.push(b'"');
    } else if path.contains(&b' ') {
        out.push(b'"');
        out.extend_from_slice(path);
        out.push(b'"');
    } else {
        out.extend_from_slice(path);
    }
}
