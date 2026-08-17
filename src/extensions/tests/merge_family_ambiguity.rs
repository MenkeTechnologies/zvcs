//! `warning: refname '<40-hex>' is ambiguous.` across the merge family.
//!
//! A name that is exactly `hexsz` hex digits is decoded as an object id by
//! `get_oid_basic()`'s very first branch, before any ref lookup — so it wins over
//! a ref that happens to share the name. git warns about that collision from
//! inside that branch (`object-name.c:689-702`), which means every command that
//! resolves an operand goes through it and every command warns *as many times as
//! it resolves*. That count is the contract these tests pin, because it is the
//! part a port gets wrong in both directions: silently, by resolving through a
//! path that never reaches `get_oid_basic()`, and loudly, by resolving one
//! operand twice where git resolves it once.
//!
//! The gates are pinned too. `core.warnAmbiguousRefs` (default true) silences the
//! whole thing; `advice.objectNameWarning` silences only the explanatory
//! paragraph and leaves the `warning:` line, because that line is not advice.
//!
//! The fixture is built with the binary under test, so the tests need nothing
//! installed; when the machine has a stock git the exact-match cases are
//! additionally diffed against it, which is what catches an expectation that is
//! self-consistent but not git's.
//!
//! Two commands are deliberately checked more loosely — see
//! `rebase_start_warns_for_its_operands` and `bisect_start_warns_per_rev_operand`
//! for why: stock also warns from resolutions of `oid_to_hex()` round-trips that
//! never touch the command line, and this port reproduces the operand
//! resolutions only.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

/// The `warning:` line for `name`, as `object-name.c`'s `warn_msg` formats it.
fn warning_line(name: &str) -> String {
    format!("warning: refname '{name}' is ambiguous.\n")
}

/// The first line of `object_name_msg`, enough to tell the paragraph apart from
/// the `warning:` line it follows.
const ADVICE_FIRST_LINE: &str = "Git normally never creates a ref that ends with 40 hex characters";

/// A stock git to compare against, or `None` when the machine has no foreign git
/// installed.
///
/// Resolved explicitly rather than through `PATH`: on a machine where zvcs
/// shadows `git` a `PATH` lookup silently makes the oracle the thing under test.
/// The newest installed git wins, which is the policy `src/parity/src/stock.rs`
/// uses.
fn stock_git() -> Option<String> {
    if let Ok(p) = std::env::var("ZVCS_STOCK_GIT") {
        return Path::new(&p).exists().then_some(p);
    }
    ["/usr/bin/git", "/opt/homebrew/bin/git", "/usr/local/bin/git"]
        .into_iter()
        .filter(|p| Path::new(p).exists())
        .filter_map(|p| Some((version_of(p)?, p.to_owned())))
        // The comparisons below are byte-for-byte over stderr, and stderr here
        // carries advice text that git itself rewords between releases — the
        // `git config set …` spelling in the merge-conflict hint is 2.46 and
        // newer. An older git is a different oracle, not a worse one, so a
        // machine that only has one simply runs the counts without it.
        .filter(|(v, _)| *v >= (2, 55, 0))
        .max()
        .map(|(_, p)| p)
}

/// `git version X.Y.Z` as a comparable tuple, or `None` when it will not answer.
fn version_of(bin: &str) -> Option<(u32, u32, u32)> {
    let out = Command::new(bin).arg("--version").env_clear().output().ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    let rest = text.trim().strip_prefix("git version ")?;
    let mut parts = rest.split(['.', ' ', '-']).filter_map(|p| p.parse::<u32>().ok());
    Some((parts.next()?, parts.next().unwrap_or(0), parts.next().unwrap_or(0)))
}

/// A fixture repository plus the binary that built it.
///
/// ```text
/// c1 - c2 - c3        (main, HEAD)
///   \
///    s1 - s2          (side)
/// ```
///
/// `f.txt` is rewritten on both sides, so every replay of a `side` commit onto
/// `main` conflicts — which is what puts the merge-conflict advice on stderr
/// beside the warnings.
struct Repo {
    bin: String,
    dir: PathBuf,
    home: PathBuf,
}

impl Repo {
    fn git(&self, args: &[&str]) -> Output {
        self.git_env(&[], args)
    }

    /// [`Repo::git`] with extra environment, for the one variable
    /// `print_advice()` reads.
    fn git_env(&self, extra: &[(&str, &str)], args: &[&str]) -> Output {
        let mut cmd = Command::new(&self.bin);
        cmd.args(args)
            .current_dir(&self.dir)
            // `print_advice()`'s first branch is a bare `getenv() != NULL`, so a
            // value inherited from whoever ran the suite would replace every
            // conflict hint below. Cleared first, so `extra` can put it back.
            .env_remove("GIT_CHERRY_PICK_HELP")
            .env("HOME", &self.home)
            .env("ZVCS_HOME", &self.home)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("LC_ALL", "C")
            .env("TZ", "UTC")
            .env("TERM", "dumb")
            .env("NO_COLOR", "1")
            .env("GIT_EDITOR", "true")
            .env("GIT_SEQUENCE_EDITOR", "true")
            .env("GIT_MERGE_AUTOEDIT", "no")
            .env("GIT_AUTHOR_NAME", "A U Thor")
            .env("GIT_AUTHOR_EMAIL", "author@example.com")
            .env("GIT_COMMITTER_NAME", "C O Mitter")
            .env("GIT_COMMITTER_EMAIL", "committer@example.com")
            .env("GIT_AUTHOR_DATE", "2023-01-01 00:00:00 +0000")
            .env("GIT_COMMITTER_DATE", "2023-01-01 00:00:00 +0000");
        for (k, v) in extra {
            cmd.env(k, v);
        }
        cmd.output().unwrap()
    }

    fn stderr(&self, args: &[&str]) -> String {
        String::from_utf8_lossy(&self.git(args).stderr).into_owned()
    }

    fn rev(&self, spec: &str) -> String {
        String::from_utf8_lossy(&self.git(&["rev-parse", spec]).stdout).trim().to_owned()
    }

    /// `git update-ref refs/heads/<40-hex> <id>` — the accident the warning is
    /// about, made on purpose.
    fn hex_ref(&self, spec: &str) -> String {
        self.hex_ref_at(spec, spec)
    }

    /// The same accident, with the ref pointing somewhere *other* than the commit
    /// its name spells.
    ///
    /// That separation is what tells the two resolutions apart:
    /// `get_oid_basic()`'s full-hex branch answers with the object those 40
    /// characters decode to and never looks at the ref, while `refs_read_ref()`
    /// answers with the ref's tip and never reaches `get_oid_basic()`. When the
    /// ref points at its own name they agree and a port can take either path
    /// undetected.
    fn hex_ref_at(&self, named_after: &str, points_at: &str) -> String {
        let id = self.rev(named_after);
        let target = self.rev(points_at);
        let full = format!("refs/heads/{id}");
        let out = self.git(&["update-ref", &full, &target]);
        assert!(out.status.success(), "update-ref {full}: {}", String::from_utf8_lossy(&out.stderr));
        id
    }

    fn commit(&self, file: &str, body: &str, msg: &str) {
        std::fs::write(self.dir.join(file), format!("{body}\n")).unwrap();
        assert!(self.git(&["add", file]).status.success(), "add {file}");
        let out = self.git(&["commit", "-q", "-m", msg]);
        assert!(out.status.success(), "commit {msg}: {}", String::from_utf8_lossy(&out.stderr));
    }
}

fn fixture(bin: &str, tag: &str) -> Repo {
    let root =
        std::env::temp_dir().join(format!("zvcs-mfa-{tag}-{}-{:p}", std::process::id(), &tag));
    let _ = std::fs::remove_dir_all(&root);
    let home = root.join("home");
    let dir = root.join("repo");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&dir).unwrap();
    let repo = Repo { bin: bin.to_owned(), dir, home };
    assert!(repo.git(&["init", "-q", "-b", "main", "."]).status.success(), "init");
    repo.git(&["config", "user.name", "A U Thor"]);
    repo.git(&["config", "user.email", "author@example.com"]);
    repo.commit("f.txt", "one", "c1");
    repo.commit("f.txt", "two", "c2");
    repo.commit("f.txt", "three", "c3");
    assert!(repo.git(&["checkout", "-q", "-b", "side", "main~2"]).status.success(), "branch side");
    repo.commit("f.txt", "sideval", "s1");
    repo.commit("h.txt", "more", "s2");
    assert!(repo.git(&["checkout", "-q", "main"]).status.success(), "back to main");
    repo
}

/// How many `warning: refname …` lines `text` holds.
fn warnings(text: &str) -> usize {
    text.lines().filter(|l| l.starts_with("warning: refname ")).count()
}

/// Run one case in a fresh fixture under zvcs and — when the machine has one —
/// under stock git, asserting the two produce identical stderr.
///
/// `setup` receives the repo and returns the argv to run, so a case can mint the
/// 40-hex refs and read the ids it needs on each side independently: the ids are
/// identical across the two runs (the dates are pinned) but each side must build
/// its own repository, since every verb here mutates one.
fn both<F>(tag: &str, setup: F) -> String
where
    F: Fn(&Repo) -> Vec<String>,
{
    let zvcs = fixture(BIN, &format!("{tag}-zvcs"));
    let zargs = setup(&zvcs);
    let zerr = zvcs.stderr(&zargs.iter().map(String::as_str).collect::<Vec<_>>());

    if let Some(bin) = stock_git() {
        let stock = fixture(&bin, &format!("{tag}-stock"));
        let sargs = setup(&stock);
        assert_eq!(sargs, zargs, "{tag}: the two fixtures must produce the same command line");
        let serr = stock.stderr(&sargs.iter().map(String::as_str).collect::<Vec<_>>());
        assert_eq!(serr, zerr, "{tag}: stderr must match stock for {zargs:?}");
    }
    zerr
}

// ---------------------------------------------------------------- merge -----

/// `collect_parents()` resolves each operand with `get_merge_parent()`, and then
/// `merge_name()` resolves the surviving heads a second time to build the
/// generated merge message — so an ordinary `git merge <40-hex>` warns twice.
#[test]
fn merge_warns_once_per_collect_parents_pass() {
    let err = both("merge-two", |r| {
        let id = r.hex_ref("side");
        vec!["merge".into(), "--no-edit".into(), id]
    });
    assert_eq!(warnings(&err), 2, "merge <40-hex-ref> warns twice:\n{err}");
}

/// `merge_name()` is only reached when a message has to be *generated*
/// (`if (merge_msg && (!have_message || shortlog_len))`), so `-m` drops the
/// second pass — and `--log` puts it back even with `-m`.
#[test]
fn merge_message_options_gate_the_second_pass() {
    let one = both("merge-m", |r| {
        let id = r.hex_ref("side");
        vec!["merge".into(), "--no-edit".into(), "-m".into(), "msg".into(), id]
    });
    assert_eq!(warnings(&one), 1, "merge -m <40-hex-ref> warns once:\n{one}");

    let two = both("merge-m-log", |r| {
        let id = r.hex_ref("side");
        vec!["merge".into(), "--no-edit".into(), "-m".into(), "msg".into(), "--log".into(), id]
    });
    assert_eq!(warnings(&two), 2, "merge -m --log <40-hex-ref> warns twice:\n{two}");
}

/// `reduce_parents()` runs between the two passes and drops every head `HEAD`
/// already reaches, so an ancestor operand is resolved once and not twice.
#[test]
fn merge_ancestor_operand_is_dropped_before_the_second_pass() {
    let err = both("merge-ancestor", |r| {
        let id = r.hex_ref("main~1");
        vec!["merge".into(), "--no-edit".into(), id]
    });
    assert_eq!(warnings(&err), 1, "merge <ancestor-40-hex-ref> warns once:\n{err}");
}

/// No ref by that name, no warning: the message is about a ref created by
/// accident, not about the id.
#[test]
fn no_ref_by_that_name_is_silent() {
    let err = both("merge-noref", |r| {
        let id = r.rev("side");
        vec!["merge".into(), "--no-edit".into(), id]
    });
    assert_eq!(warnings(&err), 0, "a 40-hex operand with no ref by that name is silent:\n{err}");
}

// ----------------------------------------------------------- the gates ------

/// `core.warnAmbiguousRefs` is the third of `get_oid_basic()`'s four conditions
/// and defaults to true, so silence takes setting it false — and then it is
/// total, paragraph included.
#[test]
fn warn_ambiguous_refs_false_silences_everything() {
    let err = both("gate-core", |r| {
        let id = r.hex_ref("side");
        r.git(&["config", "core.warnAmbiguousRefs", "false"]);
        vec!["merge".into(), "--no-edit".into(), id]
    });
    assert_eq!(warnings(&err), 0, "core.warnAmbiguousRefs=false silences the warning:\n{err}");
    assert!(!err.contains(ADVICE_FIRST_LINE), "and the paragraph with it:\n{err}");
}

/// `advice.objectNameWarning` gates only the explanatory paragraph. The
/// `warning:` line is printed with a bare `fprintf(stderr, …)` rather than
/// through `advise()`, so it survives.
#[test]
fn object_name_warning_false_keeps_the_warning_line() {
    let err = both("gate-advice", |r| {
        let id = r.hex_ref("side");
        r.git(&["config", "advice.objectNameWarning", "false"]);
        vec!["merge".into(), "--no-edit".into(), id]
    });
    assert_eq!(warnings(&err), 2, "the warning line is not advice:\n{err}");
    assert!(!err.contains(ADVICE_FIRST_LINE), "but the paragraph is:\n{err}");
}

/// The default is the other way round: the paragraph is on, once per warning.
#[test]
fn the_paragraph_follows_every_warning_by_default() {
    let err = both("gate-default", |r| {
        let id = r.hex_ref("side");
        vec!["merge".into(), "--no-edit".into(), id]
    });
    assert_eq!(
        err.matches(ADVICE_FIRST_LINE).count(),
        warnings(&err),
        "one paragraph per warning:\n{err}"
    );
}

// ----------------------------------------------------------- merge-tree -----

/// `merge-tree` resolves each side once — `get_merge_parent()` for the real
/// merge, `repo_get_oid_treeish()` for the `--merge-base` form — so it warns
/// once per 40-hex operand and never twice.
#[test]
fn merge_tree_warns_once_per_operand() {
    let err = both("mt-two", |r| {
        let side = r.hex_ref("side");
        let main = r.hex_ref("main");
        vec!["merge-tree".into(), "--write-tree".into(), main, side]
    });
    assert_eq!(warnings(&err), 2, "merge-tree warns once per side:\n{err}");

    let with_base = both("mt-base", |r| {
        let side = r.hex_ref("side");
        let main = r.hex_ref("main");
        let base = r.hex_ref("main~2");
        vec!["merge-tree".into(), "--write-tree".into(), format!("--merge-base={base}"), main, side]
    });
    assert_eq!(warnings(&with_base), 3, "--merge-base adds a third operand:\n{with_base}");
}

/// `--messages` makes this port read the two sides back to attribute a path to a
/// side. git holds the trees from the original resolution and never asks again,
/// so that second read must stay silent.
#[test]
fn merge_tree_messages_does_not_warn_twice() {
    let err = both("mt-messages", |r| {
        let side = r.hex_ref("side");
        let main = r.hex_ref("main");
        vec!["merge-tree".into(), "--write-tree".into(), "--messages".into(), main, side]
    });
    assert_eq!(warnings(&err), 2, "--messages must not add a resolution:\n{err}");
}

// ---------------------------------------------------- cherry-pick/revert ----

/// `setup_revisions()` resolves the operand, and then
/// `sequencer_pick_revisions()`'s opening loop resolves every queued *name* all
/// over again — so cherry-pick and revert warn twice where merge-tree warns once.
#[test]
fn cherry_pick_warns_in_both_sequencer_passes() {
    let err = both("cp-one", |r| {
        let id = r.hex_ref("side~1");
        vec!["cherry-pick".into(), id]
    });
    assert_eq!(warnings(&err), 2, "cherry-pick <40-hex-ref> warns twice:\n{err}");
}

/// A range queues its two endpoints separately, so both passes see both: four
/// warnings for one operand, in `a b a b` order.
#[test]
fn cherry_pick_range_warns_for_both_endpoints_twice() {
    let err = both("cp-range", |r| {
        let a = r.hex_ref("main~2");
        let b = r.hex_ref("side");
        vec!["cherry-pick".into(), format!("{a}..{b}")]
    });
    let names: Vec<&str> = err.lines().filter(|l| l.starts_with("warning: refname ")).collect();
    assert_eq!(names.len(), 4, "a range warns four times:\n{err}");
    assert_eq!(names[0], names[2], "left endpoint, then again in the second pass:\n{err}");
    assert_eq!(names[1], names[3], "right endpoint, then again:\n{err}");
    assert_ne!(names[0], names[1], "the two endpoints are different names:\n{err}");
}

/// The two `get_oid_with_context()` calls at the top of `handle_dotdot_1()` are
/// joined by `||`, so a left endpoint that does not resolve means the right one
/// is never looked at — and never warns.
#[test]
fn cherry_pick_range_short_circuits_on_the_left_endpoint() {
    let err = both("cp-range-left-bad", |r| {
        let b = r.hex_ref("side");
        vec!["cherry-pick".into(), format!("nosuchref..{b}")]
    });
    assert_eq!(warnings(&err), 0, "an unresolvable left endpoint silences the right:\n{err}");
}

/// `<a>^@` is claimed by `add_parents_only()`, which makes
/// `handle_revision_arg_1()` return before its own resolution — one first-pass
/// warning, and one per parent queued in the second.
#[test]
fn cherry_pick_parents_only_mark_resolves_once_per_pass() {
    let err = both("cp-parents-only", |r| {
        let id = r.hex_ref("side~1");
        vec!["cherry-pick".into(), format!("{id}^@")]
    });
    assert_eq!(warnings(&err), 2, "<40-hex>^@ warns once per pass:\n{err}");
}

/// `<a>^!` does *not* return early: `arg` is replaced by the base and resolved a
/// second time, and the second pass then sees the parent and the commit — four.
#[test]
fn cherry_pick_exclude_parents_mark_resolves_twice_per_pass() {
    let err = both("cp-exclude-parents", |r| {
        let id = r.hex_ref("side~1");
        vec!["cherry-pick".into(), format!("{id}^!")]
    });
    assert_eq!(warnings(&err), 4, "<40-hex>^! warns twice per pass:\n{err}");
}

/// The peeled spellings are cut off before `get_oid_basic()` sees the name, so
/// the warning quotes the 40 hex characters and not the operand.
#[test]
fn a_peeled_operand_is_named_by_its_hex_alone() {
    let err = both("cp-peel", |r| {
        let id = r.hex_ref("side~1");
        vec!["cherry-pick".into(), format!("{id}^{{commit}}")]
    });
    let id = err
        .lines()
        .find_map(|l| l.strip_prefix("warning: refname '")?.strip_suffix("' is ambiguous."))
        .expect("a warning was printed");
    assert_eq!(id.len(), 40, "the warning names the hex, not the operand:\n{err}");
    assert_eq!(warnings(&err), 2, "and still once per pass:\n{err}");
}

/// `revert` runs the same two passes as `cherry-pick`; only the advice wording
/// downstream differs.
#[test]
fn revert_warns_in_both_sequencer_passes() {
    let err = both("rv-one", |r| {
        let id = r.hex_ref("main~1");
        vec!["revert".into(), "--no-edit".into(), id]
    });
    assert_eq!(warnings(&err), 2, "revert <40-hex-ref> warns twice:\n{err}");
}

// ------------------------------------------------- cherry-pick advice -------

/// `print_advice()` reaches every one of its branches through
/// `advise_if_enabled(ADVICE_MERGE_CONFLICT, …)`, so the stopped pick carries the
/// `Disable this message …` trailer while the slot is unconfigured.
#[test]
fn a_stopped_cherry_pick_offers_to_disable_the_hint() {
    let err = both("cp-advice-range", |_| {
        vec!["cherry-pick".into(), "HEAD~2..side".into()]
    });
    assert!(
        err.contains("hint: \"git cherry-pick --continue\".\n"),
        "the committing pick gets the six-line variant:\n{err}"
    );
    assert!(
        err.contains(
            "hint: Disable this message with \"git config set advice.mergeConflict false\"\n"
        ),
        "and the advice trailer:\n{err}"
    );
}

/// `advice.mergeConflict=false` drops the whole block, trailer included —
/// `vadvise()`'s `display_instructions` is `!advice_setting[type].level`, so
/// configuring the slot either way removes the offer to configure it.
#[test]
fn merge_conflict_advice_can_be_turned_off() {
    let err = both("cp-advice-off", |r| {
        r.git(&["config", "advice.mergeConflict", "false"]);
        vec!["cherry-pick".into(), "HEAD~2..side".into()]
    });
    assert!(err.contains("error: could not apply "), "the error itself stays:\n{err}");
    assert!(!err.contains("hint: "), "and every hint line goes:\n{err}");
}

/// `if (opts->no_commit)` is the *first* test in `print_advice()`, ahead of the
/// per-action ones — so `-n` gets the lowercase two-line variant, which names no
/// `--continue` because with no commit pending there is no sequence to continue.
#[test]
fn no_commit_outranks_the_action_in_print_advice() {
    let err = both("cp-advice-nocommit", |_| {
        vec!["cherry-pick".into(), "-n".into(), "HEAD~2..HEAD~1".into()]
    });
    assert!(
        err.contains("error: could not apply "),
        "the pick has to stop for there to be advice:\n{err}"
    );
    assert!(
        err.contains("hint: after resolving the conflicts, mark the corrected paths\n"),
        "-n selects the --no-commit variant:\n{err}"
    );
    assert!(
        !err.contains("--continue"),
        "which never mentions --continue:\n{err}"
    );
    assert!(
        err.contains(
            "hint: Disable this message with \"git config set advice.mergeConflict false\"\n"
        ),
        "and it is advice too:\n{err}"
    );
}

/// The same rule with the action reversed: `revert -n` gets the identical
/// lowercase variant, because the branch that picks it never looks at the action.
#[test]
fn revert_no_commit_gets_the_same_variant_as_cherry_pick() {
    let err = both("rv-advice-nocommit", |_| {
        vec!["revert".into(), "-n".into(), "main~1".into()]
    });
    assert!(
        err.contains("hint: after resolving the conflicts, mark the corrected paths\n"),
        "revert -n selects the --no-commit variant too:\n{err}"
    );
}

/// `print_advice()`'s *first* branch, ahead of both variants above:
///
/// ```c
/// msg = getenv("GIT_CHERRY_PICK_HELP");
/// if (msg) {
///         advise_if_enabled(ADVICE_MERGE_CONFLICT, "%s", msg);
///         refs_delete_ref(get_main_ref_store(r), "", "CHERRY_PICK_HEAD", NULL, REF_NO_DEREF);
///         return;
/// }
/// ```
///
/// The porcelain that sets the variable is taking the commit over, so the hint is
/// replaced wholesale *and* `CHERRY_PICK_HEAD` goes — which is the observable half:
/// a pick that kept it would have `git status` reporting a cherry-pick in progress
/// that the porcelain never started. `REVERT_HEAD` is not named there and stays.
#[test]
fn cherry_pick_help_replaces_the_hint_and_drops_the_pick_head() {
    for (tag, argv) in [
        ("cph-pick", vec!["cherry-pick", "side~1"]),
        ("cph-pick-n", vec!["cherry-pick", "-n", "side~1"]),
    ] {
        let repo = fixture(BIN, tag);
        let out = repo.git_env(&[("GIT_CHERRY_PICK_HELP", "porcelain says so")], &argv);
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(err.contains("hint: porcelain says so\n"), "{tag}: the override is the hint:\n{err}");
        assert!(!err.contains("--continue"), "{tag}: and it replaces the variant:\n{err}");
        assert!(
            err.contains("hint: Disable this message with \"git config set advice.mergeConflict false\"\n"),
            "{tag}: still through the advice slot:\n{err}"
        );
        assert!(
            !repo.dir.join(".git/CHERRY_PICK_HEAD").exists(),
            "{tag}: the same branch deletes CHERRY_PICK_HEAD"
        );
    }

    // `revert` reaches the same branch, and `REVERT_HEAD` — which the C does not
    // name — survives it.
    let repo = fixture(BIN, "cph-revert");
    let out = repo.git_env(
        &[("GIT_CHERRY_PICK_HELP", "porcelain says so")],
        &["revert", "--no-edit", "main~1"],
    );
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("hint: porcelain says so\n"), "revert takes the override too:\n{err}");
    assert!(repo.dir.join(".git/REVERT_HEAD").exists(), "REVERT_HEAD is not the ref that is deleted");
}

/// `CHERRY_PICK_HEAD` is written only `&& !opts->no_commit`
/// (`sequencer.c:2474-2479`) — the ref exists for the `git commit` that follows,
/// and `--no-commit` is the spelling that says none does.
#[test]
fn no_commit_leaves_no_cherry_pick_head_behind() {
    let repo = fixture(BIN, "cp-nocommit-state");
    let out = repo.git(&["cherry-pick", "-n", "side~1"]);
    assert_eq!(out.status.code(), Some(1), "the pick conflicts, which is the case under test");
    assert!(
        !repo.dir.join(".git/CHERRY_PICK_HEAD").exists(),
        "cherry-pick -n must not claim a pick is in progress"
    );
    if let Some(bin) = stock_git() {
        let stock = fixture(&bin, "cp-nocommit-state-stock");
        stock.git(&["cherry-pick", "-n", "side~1"]);
        assert!(!stock.dir.join(".git/CHERRY_PICK_HEAD").exists(), "stock agrees");
    }
}

// ---------------------------------------------------------------- rebase ----

/// `cmd_rebase()` resolves `options.upstream_name` at `builtin/rebase.c:1663`
/// and `options.onto_name` at `:1760` — and without `--onto` those are the same
/// string (`:1747`), so one operand is resolved twice.
///
/// An up-to-date rebase is used deliberately: it returns before
/// `get_revision_ranges()` builds its `<hex>...<hex>` range and before the todo
/// list is parsed, and those are resolutions of `oid_to_hex()` output rather than
/// of anything from the command line. This port reproduces the operand
/// resolutions only, so a rebase that actually replays commits still warns fewer
/// times than stock — see `rebase_start_warns_for_its_operands`.
#[test]
fn rebase_resolves_the_upstream_operand_as_upstream_and_as_onto() {
    let err = both("rb-uptodate", |r| {
        let id = r.hex_ref("main~2");
        vec!["rebase".into(), id]
    });
    assert_eq!(warnings(&err), 2, "upstream doubles as onto:\n{err}");
}

/// `--onto` separates them again, and the operand that is *not* hex contributes
/// nothing: `git rebase --onto <40-hex-ref> <upstream> <branch>` warns exactly
/// once, for the `--onto`.
///
/// The `--onto` commit is chosen off the branch being rebased on purpose. Every
/// hex stock re-resolves internally is `oid_to_hex()` of something it is already
/// holding — the replay range's endpoints and each generated `pick <40-hex>` line
/// — so an operand that is none of those keeps stock's count at the operand count
/// and lets this compare stderr byte for byte. `rebase_start_warns_for_its_operands`
/// is the same command shape without that restriction, and is checked loosely for
/// exactly that reason.
#[test]
fn rebase_onto_is_a_separate_operand() {
    let err = both("rb-onto-only", |r| {
        // `refs/heads/<hex of side~1>`: not the upstream, not `HEAD`, and not one
        // of the commits `main~2..main` replays.
        let onto = r.hex_ref("side~1");
        vec!["rebase".into(), "--onto".into(), onto, "main~2".into(), "main".into()]
    });
    assert_eq!(warnings(&err), 1, "only the --onto operand is a 40-hex ref:\n{err}");
}

/// A `<branch>` that really is a local branch is read straight out of the ref
/// store:
///
/// ```c
/// strbuf_addf(&buf, "refs/heads/%s", branch_name);
/// if (!refs_read_ref(get_main_ref_store(the_repository), buf.buf, &branch_oid)) {
///         …
///         options.orig_head = lookup_commit_object(the_repository, &branch_oid);
/// } else {
///         options.orig_head = lookup_commit_reference_by_name(branch_name);
/// ```
///
/// so it never reaches `get_oid_basic()` and must not warn even when its name is
/// 40 hex digits — *and* what gets rebased is the ref's tip, not the object those
/// 40 characters decode to. The ref is deliberately pointed somewhere else so the
/// two answers differ: resolving this operand through a revspec parser rebases the
/// wrong commit and then fails the `refs/heads/<hex>` update with an old-value
/// mismatch.
#[test]
fn rebase_branch_operand_is_read_from_the_ref_store() {
    let err = both("rb-branch-is-branch", |r| {
        // `refs/heads/<hex of main~1>` → `side`.
        let id = r.hex_ref_at("main~1", "side");
        vec!["rebase".into(), "main".into(), id]
    });
    assert_eq!(
        warnings(&err),
        0,
        "a `<branch>` resolved through the ref store does not warn:\n{err}"
    );

    // The rebase has to have moved the hex-named branch, not the commit its name
    // spells: it replays `side` onto `main` and stops on the conflict, so the
    // branch is the one being rebased and `REBASE_HEAD` is set.
    let repo = fixture(BIN, "rb-branch-is-branch-state");
    let id = repo.hex_ref_at("main~1", "side");
    let out = repo.git(&["rebase", "main", &id]);
    assert_eq!(out.status.code(), Some(1), "the replay conflicts: {}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(
        repo.rev("HEAD"),
        repo.rev("main"),
        "the replay starts from `main`, which is where `<branch>` is being moved to"
    );
    assert!(
        repo.dir.join(".git/rebase-merge").exists(),
        "a rebase of the *branch* is in progress"
    );
}

/// The end-to-end shape, checked as properties rather than as a count: stock also
/// warns from `get_revision_ranges()`'s `"<hex>...<hex>"` and from parsing the
/// generated todo list twice, and neither of those resolves anything the user
/// typed. What is pinned here is that the operand itself is no longer silent, and
/// that `core.warnAmbiguousRefs` still governs it.
#[test]
fn rebase_start_warns_for_its_operands() {
    let repo = fixture(BIN, "rb-replay");
    let id = repo.hex_ref("main");
    let err = repo.stderr(&["rebase", &id, "side"]);
    assert!(err.contains(&warning_line(&id)), "the upstream operand warns:\n{err}");

    let quiet = fixture(BIN, "rb-replay-quiet");
    let id = quiet.hex_ref("main");
    quiet.git(&["config", "core.warnAmbiguousRefs", "false"]);
    let err = quiet.stderr(&["rebase", &id, "side"]);
    assert_eq!(warnings(&err), 0, "and core.warnAmbiguousRefs still silences it:\n{err}");
}

// ---------------------------------------------------------------- bisect ----

/// `bisect_start()` resolves each revision operand with
/// `get_oidf(&oid, "%s^{commit}", arg)` (`builtin/bisect.c:776`) — the only place
/// it sees the operand as typed. It afterwards resolves `oid_to_hex()` of what it
/// found, several times over, which is why stock's total is higher than the
/// operand count; those are round-trips through git's own output rather than
/// resolutions of a command line, and this port does not reproduce them.
#[test]
fn bisect_start_warns_per_rev_operand() {
    let repo = fixture(BIN, "bi-start");
    let bad = repo.hex_ref("main");
    let good = repo.hex_ref("main~2");
    let err = repo.stderr(&["bisect", "start", &bad, &good]);
    assert!(err.contains(&warning_line(&bad)), "the bad operand warns:\n{err}");
    assert!(err.contains(&warning_line(&good)), "and the good one:\n{err}");
}

/// The gate applies here too — and `bisect start` is the case where a port that
/// bolted the warning onto its own resolver instead of onto the shared rule would
/// keep warning with the config off.
#[test]
fn bisect_start_honours_warn_ambiguous_refs() {
    let repo = fixture(BIN, "bi-start-quiet");
    let bad = repo.hex_ref("main");
    let good = repo.hex_ref("main~2");
    repo.git(&["config", "core.warnAmbiguousRefs", "false"]);
    let err = repo.stderr(&["bisect", "start", &bad, &good]);
    assert_eq!(warnings(&err), 0, "core.warnAmbiguousRefs=false silences bisect too:\n{err}");
}

/// And with no ref by those names there is nothing to warn about, which is the
/// over-warning guard: `bisect start <hex> <hex>` is an everyday spelling and
/// must stay silent in a repository that has no 40-hex refs.
#[test]
fn bisect_start_is_silent_without_a_ref_by_that_name() {
    let repo = fixture(BIN, "bi-start-noref");
    let bad = repo.rev("main");
    let good = repo.rev("main~2");
    let err = repo.stderr(&["bisect", "start", &bad, &good]);
    assert_eq!(warnings(&err), 0, "plain ids must not warn:\n{err}");
}
