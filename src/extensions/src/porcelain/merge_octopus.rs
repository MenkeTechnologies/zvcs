//! `git merge-octopus` — resolve two or more trees (the octopus merge strategy).
//!
//! Stock `git-merge-octopus` is a POSIX shell driver (`git-merge-octopus.sh`)
//! that sources `git-sh-setup` and then orchestrates four plumbing commands per
//! head being merged: `git merge-base --all`, `git read-tree -u -m`,
//! `git write-tree`, and `git merge-index -o git-merge-one-file -a`. It folds
//! each remote head into the accumulated result tree (`$MRT`) — fast-forwarding
//! while the reference set (`$MRC`) is still a single commit that is the merge
//! base, otherwise three-way merging — and records every merged head as a parent
//! of the eventual commit `git merge` writes.
//!
//! The index/worktree mutation the script drives through `read-tree -u -m` and
//! `merge-index -o git-merge-one-file -a` runs here through those same two ports,
//! called in process with the same arguments — the arrangement
//! [`super::merge_resolve`] already uses for `git-merge-resolve.sh`, which chains
//! the same commands. Nothing about a merge is re-derived: the index stages, the
//! worktree bytes, the `Auto-merging <path>` / `Added <path> in both, but
//! differently.` lines, `git-merge-one-file`'s `ERROR: content conflict in <path>` /
//! `fatal: merge program failed` pair and its `.merge_file_XXXXXX` conflict-marker
//! labels all come from those ports, which is the only way to match a chain whose
//! output includes `mkstemp` names.
//!
//! Two things follow from running the real chain rather than a tree merge:
//!
//! * **Every merge base reaches `read-tree`.** `$common` is interpolated unquoted,
//!   so a criss-cross history makes the "simple merge" a **four**-tree `read-tree`,
//!   and `threeway_merge()` records the stage-1 ancestor only
//!   `if (!head_match || !remote_match)` (unpack-trees.c). A path where the head
//!   matches one base and the remote matches the other lands with stages 2 and 3 and
//!   no stage 1, which is what sends `git-merge-one-file` down its `.$2$3` arm.
//!   There is no recursive virtual base anywhere in the octopus.
//! * **`Simple merge did not work` is `write-tree`'s verdict**, not a guess: the
//!   script prints it when `git write-tree` refuses the index `read-tree
//!   --aggressive` just wrote, i.e. exactly when a stage was left behind.
//!
//! ### Covered (verified against git 2.55.0: stdout, stderr, exit code)
//!
//! * `-h` as the first argument — `git-sh-setup`'s `$LONG_USAGE` path with an
//!   empty `USAGE`, i.e. the single line `usage: git merge-octopus ` (note the
//!   trailing space) on **stdout**, exit 0, and no repository required.
//! * `git_dir_init` running before any argument is looked at: outside a
//!   repository, `fatal: not a git repository (or any of the parent
//!   directories): .git` on stderr, exit 128.
//! * The argument split: everything before the first `--` is a merge base and
//!   is discarded, the first argument after it is `$head`, the rest are the
//!   heads to merge.
//! * The "this is not an octopus" guard — fewer than two heads to merge exits 2
//!   silently, so `git merge` can fall back to another strategy.
//! * The `git diff-index --quiet --cached HEAD --` pre-flight: on any
//!   tree↔index difference, `Error: Your local changes to the following files
//!   would be overwritten by merge` followed by the changed paths each indented
//!   by four spaces — both on **stdout**, as `gettextln` and the script's `sed`
//!   pipeline emit them — then exit 2. Paths are quoted per `core.quotePath`.
//! * The merge-base pass over every head: `$GITHEAD_<sha1>` (then the
//!   uppercased `$GITHEAD_<SHA1>`) as the pretty name, `Already up to date with
//!   <name>` on stdout for a head already reachable, and
//!   `Unable to find common commit with <name>` on stderr with exit 1 (the
//!   script's `die`, which prints no `fatal:` prefix) when `merge-base --all`
//!   fails or finds nothing.
//! * The all-heads-already-up-to-date run completes exactly as git does: those
//!   lines on stdout, exit 0, and the repository untouched.
//! * The fast-forward branch (`Fast-forwarding to: <name>`), advancing both the
//!   index/worktree and the `$MRC`/`$MRT` bookkeeping to the head being merged —
//!   including its `read-tree -u -m $head $SHA1` refusals (`Entry '<p>' would be
//!   overwritten by merge.`, `Entry '<p>' not uptodate.`, `Untracked working
//!   tree file '<p>' would be overwritten by merge.`, exit 128), whose old tree
//!   is the original `$head` argument rather than the running `$MRT`, and the
//!   textual `test "$common,$NON_FF_MERGE" = "$MRC,0"` that decides the branch:
//!   `$MRC` holds each fast-forwarded head **as spelled**, so a branch name can
//!   never equal `merge-base --all`'s object ids and the second consecutive
//!   fast-forward only happens when the caller passes full ids (as `git merge`
//!   does).
//! * The three-way branch's `read-tree -u -m --aggressive $common $MRT $SHA1 ||
//!   exit 2` refusals, with the same plumbing wording and exit 2.
//! * The three-way branch: `Trying simple merge with <name>`, the conditional
//!   `Simple merge did not work, trying automatic merge.`, the merge itself, and
//!   the `Automated merge did not work.` / `Should not be doing an octopus.`
//!   refusal (exit 2) when a non-final head leaves an unresolved conflict — over a
//!   criss-cross, where the four-tree `read-tree` leaves an add/add with no stage 1,
//!   that whole sequence including `git-merge-one-file`'s own diagnostics, the one
//!   object it writes and the `AA` entry it leaves in the index.
//! * The final exit status is `$OCTOPUS_FAILURE`: 0 for a fully clean run, 1 when
//!   the last head merged with an unresolved conflict left in the worktree/index.
//!
//! ### Not covered
//!
//! An unborn `HEAD` bails: stock git runs `diff-index` against it twice and lets
//! the resulting `fatal: ambiguous argument 'HEAD'` through, which is not
//! reproduced. So does an unmerged index, whose `U` records the ported
//! `diff-index` does not emit either. Both are rejected by `dirty_paths` before
//! any merging begins.

use anyhow::{bail, Result};
use std::collections::BTreeSet;
use std::process::ExitCode;

use gix::bstr::BString;
use gix::hash::ObjectId;
use gix::Repository;

/// `git-sh-setup`'s `$LONG_USAGE` for a script that sets neither `USAGE` nor
/// `OPTIONS_SPEC`: `usage: $dashless $USAGE` with `$USAGE` empty, so the line
/// ends in a space. `echo` supplies the newline.
const LONG_USAGE: &str = "usage: git merge-octopus \n";

/// The script's argument loop: merge bases, then `--`, then `$head`, then the
/// heads to merge. Bases are collected but unused, exactly as in the script.
struct Args {
    head: Option<String>,
    remotes: Vec<String>,
}

/// Reproduce the `case ",$sep_seen,$head,$arg," in` dispatch verbatim: `--`
/// flips the separator (every time it appears), the first argument after it
/// becomes `$head`, later ones accumulate into `$remotes`, and anything before
/// it is a merge base.
fn parse(args: &[String]) -> Args {
    let mut sep_seen = false;
    let mut head: Option<String> = None;
    let mut remotes = Vec::new();

    for arg in args {
        if arg == "--" {
            sep_seen = true;
        } else if !sep_seen {
            // A merge base; the script keeps these in `$bases` and never reads it.
        } else if head.is_none() {
            head = Some(arg.clone());
        } else {
            remotes.push(arg.clone());
        }
    }

    Args { head, remotes }
}

/// `git merge-octopus` — see the module docs for what is and is not covered.
pub fn merge_octopus(args: &[String]) -> Result<ExitCode> {
    // `git-sh-setup` inspects only `$1`, and does so before `git_dir_init`.
    if args.first().map(String::as_str) == Some("-h") {
        print!("{LONG_USAGE}");
        return Ok(ExitCode::SUCCESS);
    }

    // `git_dir_init`, which every non-`-h` invocation reaches first.
    let Ok(repo) = crate::setup::discover() else {
        eprintln!("fatal: not a git repository (or any of the parent directories): .git");
        return Ok(ExitCode::from(128));
    };

    let parsed = parse(args);

    // `case "$remotes" in ?*' '?*)` — anything but two or more heads to merge
    // is not an octopus, and exits 2 without a word so `git merge` can pick
    // another strategy.
    if parsed.remotes.len() < 2 {
        return Ok(ExitCode::from(2));
    }

    // `if ! git diff-index --quiet --cached HEAD --`
    let dirty = dirty_paths(&repo)?;
    if !dirty.is_empty() {
        println!("Error: Your local changes to the following files would be overwritten by merge");
        for path in &dirty {
            println!("    {}", quote_path(path));
        }
        return Ok(ExitCode::from(2));
    }

    // `MRC=$(git rev-parse --verify -q $head)` — git leaves `$MRC` empty when
    // `$head` does not resolve and lets the first `merge-base` fail; we peel it
    // to a commit (`git merge` always spells `$head` as a commit id) to seed both
    // the merge-base peer set and the running result tree.
    let head_spec = parsed.head.as_deref().unwrap_or("");
    let head_commit = commit_reference(&repo, head_spec);

    // `MRC` — git's "merge reference commit" set: initially just `$head`, later
    // *replaced* by a fast-forwarded head or *extended* by each merged head. It
    // is both the merge-base peer set and (as trees) the accumulated result.
    let mut mrc: Vec<ObjectId> = head_commit.map(|c| vec![c]).unwrap_or_default();
    // `$MRC` is a *shell string*, and the fast-forward test below compares it
    // textually. It starts as `git rev-parse --verify -q $head`, i.e. a full
    // object id whatever `$head` was spelled as, but a fast-forward replaces it
    // with `$SHA1` — the head **as spelled on the command line**. Keeping only
    // the resolved ids made `merge-octopus -- <head> branch1 branch2` fast-forward
    // twice where stock's `$common` (always full ids) can never equal a branch
    // name, so stock three-way merges the second head instead.
    let mut mrc_text: Vec<String> = head_commit.map(|c| vec![c.to_string()]).unwrap_or_default();
    // `MRT=$(git write-tree)` — the "merge result tree", read out of the *index*, which
    // the `diff-index --cached HEAD` pre-flight above has just proved equal to `HEAD`'s
    // tree. It then tracks each folded-in head, while `$head` stays put — the
    // fast-forward's `read-tree` reads the latter, so both are needed.
    //
    // It is a shell *string*, and `write-tree` leaves it empty when the index is
    // unmerged; hence the `Option`, whose `None` is interpolated into the next
    // `read-tree` command line as nothing at all.
    let mut mrt: Option<String> = write_tree(&repo)?.map(|id| id.to_string());
    // `NON_FF_MERGE` is exactly `mrc.len() > 1` (only a three-way merge extends
    // the set), so it needs no separate flag; `OCTOPUS_FAILURE` does.
    let mut octopus_failure = false;
    // `pretty_name` is a plain shell variable that outlives one iteration of the
    // loop below, and a head whose spelling is not a shell name leaves it at the
    // previous iteration's value — see [`pretty_name`]. It starts out unset.
    let mut pretty = String::new();

    for sha1 in &parsed.remotes {
        // `case "$OCTOPUS_FAILURE" in 1)` — a prior head left an unresolved
        // conflict and there is still a head to merge, which an octopus refuses.
        if octopus_failure {
            println!("Automated merge did not work.");
            println!("Should not be doing an octopus.");
            return Ok(ExitCode::from(2));
        }

        pretty = pretty_name(sha1, &pretty);

        // `common=$(git merge-base --all $SHA1 $MRC) || die ...`
        let sha1_commit = commit_reference(&repo, sha1);
        let common = match sha1_commit {
            Some(c) => merge_base_all(&repo, c, &mrc)?,
            None => Vec::new(),
        };
        if common.is_empty() {
            eprintln!("Unable to find common commit with {pretty}");
            return Ok(ExitCode::from(1));
        }

        // `case "$LF$common$LF" in *"$LF$SHA1$LF"*)` — a literal line-wise
        // comparison against the argument as spelled, so only a full object id
        // can match. `git merge` always passes full ids.
        if common.iter().any(|id| id.to_string() == *sha1) {
            println!("Already up to date with {pretty}");
            continue;
        }
        // `common` is non-empty, so `$SHA1` resolved to a commit.
        let sha1_commit = sha1_commit.expect("a non-empty merge base implies a resolved head");

        // `if test "$common,$NON_FF_MERGE" = "$MRC,0"` — while `$MRC` is still a
        // single commit that IS the sole merge base, git fast-forwards to this
        // head instead of three-way merging. `mrc.len() == 1` is `NON_FF_MERGE == 0`.
        // `$common` is `merge-base --all`'s newline-separated output and `$MRC`
        // is the space-separated commit list, compared as whole strings.
        let common_text = common.iter().map(ObjectId::to_string).collect::<Vec<_>>().join("\n");
        if mrc.len() == 1 && common_text == mrc_text.join(" ") {
            // `eval_gettextln "Fast-forwarding to: $pretty_name"`
            println!("Fast-forwarding to: {pretty}");
            // `git read-tree -u -m $head $SHA1 || exit` (git-merge-octopus.sh:90):
            // a two-tree merge, which **refuses** rather than overwrite when the
            // index or the worktree has drifted off the old tree, and whose
            // `die()` takes the script down with it (`|| exit`, i.e. read-tree's
            // own 128).
            //
            // The old tree is `$head` — the original argument — **not** the
            // running `$MRT`. The two coincide only until the first head is
            // folded in, so a *second* consecutive fast-forward hands read-tree
            // an index that no longer matches `$head` and it dies.
            let read_tree_argv = argv(&["-u", "-m", head_spec, sha1]);
            let code = status(super::read_tree::read_tree(&read_tree_argv)?);
            if code != 0 {
                return Ok(ExitCode::from(code));
            }
            // `MRC=$SHA1 MRT=$(git write-tree)`
            mrc = vec![sha1_commit];
            mrc_text = vec![sha1.clone()];
            mrt = write_tree(&repo)?.map(|id| id.to_string());
            continue;
        }

        // `NON_FF_MERGE=1`; `eval_gettextln "Trying simple merge with $pretty_name"`
        println!("Trying simple merge with {pretty}");

        // ```sh
        // git read-tree -u -m --aggressive  $common $MRT $SHA1 || exit 2
        // next=$(git write-tree 2>/dev/null)
        // if test $? -ne 0
        // then
        //         gettextln "Simple merge did not work, trying automatic merge."
        //         git merge-index -o git-merge-one-file -a ||
        //         OCTOPUS_FAILURE=1
        //         next=$(git write-tree 2>/dev/null)
        // fi
        // ```
        //
        // (git-merge-octopus.sh:96-106.) `$common` is unquoted, so a criss-cross
        // history — where `merge-base --all` answers with more than one — makes this a
        // **four**-tree `read-tree`, and `threeway_merge()` keeps the stage-1 ancestor
        // only `if (!head_match || !remote_match)` (unpack-trees.c). A path where the
        // head matches one base and the remote matches the other therefore lands with
        // stages 2 and 3 and no stage 1, and `git-merge-one-file` takes its `.$2$3`
        // arm — `Added <path> in both, but differently.` — rather than merging it.
        // Deriving the answer from the bases one at a time cannot produce that, which
        // is why this runs the same two plumbing commands the script runs, in process,
        // the way [`super::merge_resolve`] runs the same chain for `-s resolve`.
        let mut read_tree_argv = argv(&["-u", "-m", "--aggressive"]);
        read_tree_argv.extend(common.iter().map(ObjectId::to_string));
        read_tree_argv.extend(mrt.clone());
        read_tree_argv.push(sha1.clone());
        if status(super::read_tree::read_tree(&read_tree_argv)?) != 0 {
            // `|| exit 2`: the script spells this refusal 2 rather than letting
            // read-tree's own status through.
            return Ok(ExitCode::from(2));
        }
        let mut next = write_tree(&repo)?;
        if next.is_none() {
            println!("Simple merge did not work, trying automatic merge.");
            // `git merge-index -o git-merge-one-file -a`, which emits
            // `git-merge-one-file`'s own lines — `Auto-merging <path>`,
            // `Added <path> in both, but differently.`, the `ERROR: content conflict
            // in <path>` / `fatal: merge program failed` pair — and whose
            // `.merge_file_XXXXXX` conflict labels no re-derivation could match.
            if status(super::merge_index::merge_index(&argv(&["-o", "git-merge-one-file", "-a"]))?) != 0 {
                // The last head may fail (the loop ends and `exit "$OCTOPUS_FAILURE"`
                // is 1); an earlier one makes the next iteration refuse the octopus.
                octopus_failure = true;
            }
            next = write_tree(&repo)?;
        }

        // `MRC="$MRC $SHA1"; MRT=$next`
        mrc.push(sha1_commit);
        mrc_text.push(sha1.clone());
        mrt = next.map(|id| id.to_string());
    }

    // `exit "$OCTOPUS_FAILURE"`
    if octopus_failure {
        Ok(ExitCode::from(1))
    } else {
        Ok(ExitCode::SUCCESS)
    }
}

/// One plumbing command line, spelled the way the script spells it.
fn argv(args: &[&str]) -> Vec<String> {
    args.iter().map(|s| (*s).to_string()).collect()
}

/// The numeric status an [`ExitCode`] carries; `ExitCode` exposes no accessor on
/// stable Rust, so probe the 256 values it can hold. The script branches on the
/// status of the programs it runs, so the ports of those programs have to hand one
/// back — the same probe [`super::merge_resolve`] needs for the same reason.
fn status(code: ExitCode) -> u8 {
    (0u8..=255).find(|&n| code == ExitCode::from(n)).unwrap_or(1)
}

/// `$(git write-tree 2>/dev/null)`: the index's root tree, or `None` for the empty
/// string the script gets when `write-tree` refuses an unmerged index.
///
/// The refusal is diagnosed on the stderr the script discards, so nothing is printed
/// here either; the caller only ever tests whether there was an answer.
/// [`super::write_tree::refresh_cache_tree`] is `write_index_as_tree()`
/// (cache-tree.c:797-831), which also writes the refreshed cache-tree back into the
/// index — the side effect the next `read-tree` in the loop reads.
fn write_tree(repo: &Repository) -> Result<Option<ObjectId>> {
    let mut index = repo.open_index()?;
    Ok(super::write_tree::refresh_cache_tree(repo, &mut index, false)?.ok())
}

/// `eval pretty_name=\${GITHEAD_$SHA1:-$SHA1}`, then the uppercased retry.
/// `${x:-y}` treats an empty value as unset, hence the `filter`.
///
/// `previous` is the value `pretty_name` still holds from the previous loop
/// iteration. It matters because `$SHA1` is interpolated into a *parameter
/// name*: when the head is spelled as something that is not a shell name — a
/// tag such as `v0.1.0`, say, which `git merge` passes through verbatim — the
/// expansion is a "bad substitution", the whole `eval` fails without assigning,
/// and the loop goes on to print the *stale* `pretty_name`. Both `eval`s share
/// that fate, since uppercasing cannot rescue an invalid name.
fn pretty_name(sha1: &str, previous: &str) -> String {
    let Some(name) = expand_githead(sha1, sha1) else {
        return previous.to_string();
    };
    // `test "$SHA1" = "$pretty_name"`, which only holds when the first expansion
    // fell through to its own default. The retry's default is the value that
    // expansion just assigned, not `$SHA1` — the same string here.
    if name != sha1 {
        return name;
    }
    let upper: String = sha1
        .chars()
        .map(|c| if c.is_ascii_lowercase() { c.to_ascii_uppercase() } else { c })
        .collect();
    expand_githead(&upper, &name).unwrap_or(name)
}

/// The shell's `${...}` expansion of the one string `eval pretty_name=\${GITHEAD_$SHA1:-$SHA1}`
/// builds, which is `GITHEAD_<sha1>:-<default>` **after** `$SHA1` has been
/// interpolated. `None` is a bad substitution: the `eval` fails, assigns nothing,
/// and `pretty_name` keeps whatever it already held.
///
/// The interpolation happens *inside* the braces, so `$SHA1` is not merely the
/// parameter's name — it can carry the operator with it. A head spelled `cc-right`
/// makes the braces read `${GITHEAD_cc-right:-cc-right}`, and `-` ends a parameter
/// name: the shell reads that as `${GITHEAD_cc-<word>}` with the word
/// `right:-cc-right`, so an unset `GITHEAD_cc` produces exactly that as the pretty
/// name. Stock 2.55.0 prints `Trying simple merge with right:-cc-right`, and
/// `oct-a` / `div-cold` come out as `a:-oct-a` / `cold:-div-cold`. Treating any
/// non-name character as a bad substitution printed an empty name for all three.
///
/// `-`/`:-` and `+`/`:+` are the operators reproduced. `=`, `?`, `#` and `%` reach
/// this only from a head whose spelling contains one, and each has a side effect of
/// its own (assignment, an error that kills the `eval`, prefix/suffix removal); they
/// keep the stale-value answer rather than a guess.
fn expand_githead(sha1: &str, default: &str) -> Option<String> {
    let text = format!("GITHEAD_{sha1}:-{default}");
    // The parameter name: `[A-Za-z_][A-Za-z0-9_]*`. The `GITHEAD_` prefix already
    // satisfies the leading-non-digit rule, so this only has to stop at the first
    // byte `$SHA1` contributes that a name cannot hold.
    let name_len = text
        .bytes()
        .take_while(|b| b.is_ascii_alphanumeric() || *b == b'_')
        .count();
    let (name, rest) = text.split_at(name_len);
    let value = std::env::var(name).ok();
    // `${NAME}` with nothing after the name cannot occur here (the `:-` is always
    // appended), but it is the shell's other legal ending and costs nothing to keep.
    if rest.is_empty() {
        return Some(value.unwrap_or_default());
    }
    // `null_is_unset` is the colon: `${x:-y}` treats an empty value as unset,
    // `${x-y}` does not.
    let (op, word) = if let Some(w) = rest.strip_prefix(":-") {
        (('-', true), w)
    } else if let Some(w) = rest.strip_prefix(":+") {
        (('+', true), w)
    } else if let Some(w) = rest.strip_prefix('-') {
        (('-', false), w)
    } else if let Some(w) = rest.strip_prefix('+') {
        (('+', false), w)
    } else {
        return None;
    };
    let (kind, null_is_unset) = op;
    let set = match &value {
        Some(v) => !(null_is_unset && v.is_empty()),
        None => false,
    };
    Some(match (kind, set) {
        ('-', true) => value.unwrap_or_default(),
        ('-', false) => word.to_string(),
        ('+', true) => word.to_string(),
        _ => String::new(),
    })
}

/// `git merge-base --all $SHA1 $MRC`: every best common ancestor of the head
/// commit `sha1` against the accumulated `mrc` commit set. Empty when `mrc` is
/// empty (an unresolvable `$head`) or the histories share no ancestor, which is
/// the script's `die` path either way.
fn merge_base_all(repo: &Repository, sha1: ObjectId, mrc: &[ObjectId]) -> Result<Vec<ObjectId>> {
    if mrc.is_empty() {
        return Ok(Vec::new());
    }
    Ok(repo
        .merge_bases_many(sha1, mrc)?
        .into_iter()
        .map(|id| id.detach())
        .collect())
}

/// Resolve `spec` and peel it to the commit it names, or `None`.
fn commit_reference(repo: &Repository, spec: &str) -> Option<ObjectId> {
    let object = repo.rev_parse_single(spec).ok()?.object().ok()?;
    object.peel_to_commit().ok().map(|c| c.id)
}

/// The paths `git diff-index --cached --name-only HEAD --` would print, sorted
/// bytewise as the index — and therefore git's diff queue — orders them.
fn dirty_paths(repo: &Repository) -> Result<Vec<BString>> {
    use gix::diff::index::ChangeRef;
    use gix::status::tree_index::TrackRenames;

    let head_tree = match repo.head_commit().ok().and_then(|c| c.tree_id().ok()) {
        Some(id) => id.detach(),
        None => anyhow::bail!(
            "unsupported: merge-octopus against an unborn HEAD (git lets diff-index's \
             `fatal: ambiguous argument 'HEAD'` through, which is not reproduced)"
        ),
    };

    let index = repo.index_or_empty()?;
    let index_state: &gix::index::State = &index;
    if index_state.entries().iter().any(|e| e.stage_raw() != 0) {
        bail!("unsupported: unmerged (conflicted) index entries — diff-index's U records are not ported");
    }

    let mut paths: BTreeSet<BString> = BTreeSet::new();
    repo.tree_index_status(
        &head_tree,
        index_state,
        None,
        TrackRenames::Disabled,
        |change, _tree_index, _worktree_index| -> Result<_, std::convert::Infallible> {
            match change {
                ChangeRef::Addition { location, .. } => {
                    paths.insert(location.into_owned());
                }
                ChangeRef::Deletion { location, .. } => {
                    paths.insert(location.into_owned());
                }
                ChangeRef::Modification { location, .. } => {
                    paths.insert(location.into_owned());
                }
                // Rename tracking is disabled above, so this never fires.
                ChangeRef::Rewrite { .. } => {}
            }
            Ok(gix::diff::index::Action::Continue(()))
        },
    )?;

    Ok(paths.into_iter().collect())
}

/// `quote_c_style()`: the name verbatim unless some byte needs escaping, in which
/// case the whole name double-quoted with C escapes. The table and the
/// `core.quotePath` flag it reads live in [`crate::quote`], shared with every
/// other verb that prints a path.
fn quote_path(path: impl AsRef<[u8]>) -> String {
    crate::quote::quoted_name_string(path.as_ref())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(args: &[&str]) -> Vec<String> {
        args.iter().map(|s| s.to_string()).collect()
    }

    /// The `case ",$sep_seen,$head,$arg," in` dispatch: bases are dropped, the
    /// first argument after `--` is the head, the rest are merged.
    #[test]
    fn splits_bases_head_and_remotes() {
        let a = parse(&v(&["base1", "base2", "--", "head", "r1", "r2"]));
        assert_eq!(a.head.as_deref(), Some("head"));
        assert_eq!(a.remotes, v(&["r1", "r2"]));

        // No separator at all: everything is a merge base, so there is nothing
        // to merge and the caller exits 2.
        let a = parse(&v(&["head", "r1", "r2"]));
        assert_eq!(a.head, None);
        assert!(a.remotes.is_empty());

        // A second `--` re-sets `sep_seen`, which is already `yes`, so it is
        // consumed rather than becoming a head — as in the script.
        let a = parse(&v(&["--", "head", "--", "r1"]));
        assert_eq!(a.head.as_deref(), Some("head"));
        assert_eq!(a.remotes, v(&["r1"]));
    }

    /// `${GITHEAD_$SHA1:-$SHA1}` falls back to the id itself when no
    /// `GITHEAD_<id>` is exported, which is the case for this synthetic id.
    #[test]
    fn pretty_name_falls_back_to_the_id() {
        let id = "0123456789abcdef0123456789abcdef01234567";
        assert_eq!(pretty_name(id, "stale"), id);
    }

    /// A head spelled as anything outside the shell's parameter-name character
    /// set makes `${GITHEAD_$SHA1:-$SHA1}` a "bad substitution": the `eval`
    /// aborts before assigning, so the script goes on to print whatever the
    /// previous iteration left in `pretty_name` rather than the head itself.
    /// `git merge octopus main feature v0.1.0` is exactly this — the tag's dots
    /// make stock git announce "Trying simple merge with feature".
    #[test]
    fn pretty_name_keeps_the_previous_value_for_a_non_shell_name() {
        assert_eq!(pretty_name("v0.1.0", "feature"), "feature");
        assert_eq!(pretty_name("refs/tags/v1", "feature"), "feature");
        // Unset at the top of the loop, so the very first head yields nothing.
        assert_eq!(pretty_name("v0.1.0", ""), "");
        // An underscore is a name character, so this one substitutes normally.
        assert_eq!(pretty_name("my_head", "feature"), "my_head");
    }
}
