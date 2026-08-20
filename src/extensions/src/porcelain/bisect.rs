//! `git bisect` — binary-search the history for the commit that introduced a change.
//!
//! The session lives in the same on-disk state stock git uses, so a bisection can
//! be handed back and forth between this implementation and `git` itself:
//! `$GIT_DIR/BISECT_{START,TERMS,NAMES,LOG,EXPECTED_REV,ANCESTORS_OK}` plus the
//! per-worktree `refs/bisect/bad` and `refs/bisect/good-<oid>` loose refs (written
//! directly, since git keeps no reflog for them).
//!
//! Supported subcommands, with stdout/stderr and exit codes matching stock git:
//!   * `git bisect start [--term-(bad|new)=<t> --term-(good|old)=<t>]
//!     [--no-checkout] [--first-parent] [<bad> [<good>...]] [--] [<pathspec>...]`
//!     — the full argument grammar of git's `bisect_start`, including custom
//!     terms (validated by `check_term_format`), the `--term-*` value taken in
//!     either `=value` or following-token form, and git's revision-vs-pathspec
//!     split (an unresolvable token starts the pathspec unless `--` is present,
//!     in which case it is a fatal bad revision, exit 128).
//!   * `git bisect bad|new [<rev>]`
//!   * `git bisect good|old [<rev>...]`
//!   * `git bisect terms [--term-good|--term-old|--term-bad|--term-new]`
//!   * `git bisect log`
//!   * `git bisect reset [<commit>]`
//!   * `git bisect next` — force a step, including git's `warning: bisecting only
//!     with a <bad> commit` path and the "need at least one bad|good" error.
//!   * `git bisect replay <logfile>` — re-drive a session from a saved log.
//!   * `git bisect help` — the usage block on stderr, exit 129; `-h` prints the
//!     same block on stdout, because `parse_options` answers it before
//!     `usage_with_options` is ever reached.
//!
//! A leading `--` ends option parsing without being kept, and the subcommand
//! table is only consulted at `argv[0]`, so `git bisect -- <word>` can only ever
//! reach `help`, a marking term or `unknown command` — never `start`, `log` and
//! the rest of the named subcommands.
//!
//! A good revision that is not an ancestor of the bad one now goes through git's
//! merge-base machinery: the merge base is checked out with `Bisecting: a merge
//! base must be tested`, and the `The merge base <oid> is bad` / `Some '<good>'
//! revs are not ancestors of the '<bad>' rev` outcomes are reproduced with git's
//! exit codes (3 and 1 respectively).
//!
//! The step selection reproduces git's `find_bisection()` exactly, including the
//! `halfway()` short-circuit that decides which of two equally-good midpoints is
//! taken, so the chosen commit, the `Bisecting: N revisions left to test after
//! this (roughly M steps)` line and the `[<oid>] <subject>` line are byte-identical.
//! The terminal report reproduces `git diff-tree --pretty --stat --summary`,
//! including git's diffstat column scaling and truncation.
//!
//! A range containing merges is bisected the way git does it: `find_bisection()`
//! weights every candidate by how many candidates it reaches, then returns the
//! first commit that is *halfway* (`|2 * weight - nr| <= 1`). Two details of that
//! search decide which of several equally good candidates is picked, and both are
//! reproduced — the list is walked oldest first, and a commit seeded with weight 1
//! because it has no interesting parent is never offered to the halfway shortcut.
//!
//! `--first-parent` is honoured on every later step, not just recorded:
//! `BISECT_FIRST_PARENT` sets `revs.first_parent_only` for the candidate walk and
//! stops the weight propagation after the first parent, which is git's
//! `FIND_BISECTION_FIRST_PARENT_ONLY`. A first bad commit that is a merge is
//! reported the way `show_diff_tree()` does it — a `Merge:` header of abbreviated
//! parents, then `--stat`/`--summary` against the first parent alone, which is
//! what `--cc` falls back to for those formats.
//!
//! Custom terms name their own references: `bisect_write` stores the bad side at
//! `refs/bisect/<term-bad>` and each good side at `refs/bisect/<term-good>-<oid>`,
//! and `register_ref` reads them back by the same names, so a session started
//! with `--term-new=broken` is interchangeable with stock git's.
//!
//! `skip` is ported in full: the `refs/bisect/skip-<oid>` refs, both `BISECT_LOG`
//! lines, the `BISECT_EXPECTED_REV`/`BISECT_ANCESTORS_OK` invalidation a marking
//! away from the expected commit performs, and the replacement search itself —
//! `FIND_BISECTION_ALL`'s distance-sorted candidate list, `filter_skipped()`,
//! and `skip_away()`'s `get_prn`/`sqrti` pick, which is a pure function of how
//! many candidates survived the filter. When only skipped commits are left it
//! reports `There are only 'skip'ped commits left to test.` and exits 2, and
//! appends the `# possible first '<term>' commit:` block in the revision walk's
//! order (not the order the same commits are printed in). `bisect replay`
//! replays `skip` lines the same way.
//!
//! Honest limitations — each bails with a precise message rather than guessing:
//!   * `skip <a>..<b>`: upstream expands the range with a revision walk in the
//!     same process as the step that follows, and the `UNINTERESTING` flags it
//!     leaves behind shrink that step's candidate set — measurably: against git
//!     2.55.0, `skip c6..c9` and `skip c7 c8 c9` write identical refs and log
//!     lines and then pick different commits. Reproducing it needs git's object
//!     flag lifetime rather than its skip algorithm, so the range form is
//!     refused and the individual revisions are not.
//!   * `run`: it drives an external command per step and treats exit code 125 as
//!     a skip; the child-process driver is not ported.
//!   * `visualize`/`view`: `bisect_next_check(terms, NULL)` is reproduced (a
//!     silent exit 1 when either side is unmarked); a live session bails, because
//!     the command shells out to gitk / `git log`.
//!   * Pathspec limiting is parsed and recorded in `BISECT_NAMES`, but it does
//!     not constrain candidate selection, so a `--`-limited bisection with
//!     revisions would pick a different midpoint; only the empty-pathspec case
//!     (recording state, then reporting status) is faithful.
//!   * The worktree update goes through this crate's `checkout`, which refuses to
//!     switch across a dirty tracked worktree; stock git refuses with a different
//!     message in the same situation.

use anyhow::{anyhow, bail, Result};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use gix::bstr::ByteSlice;

use super::diffstat::{self, StatWidths};
use gix::diff::blob::{diff_with_slider_heuristics, Algorithm, InternedInput};
use gix::hash::ObjectId;
use gix::object::tree::diff::ChangeDetached;

/// The usage block git prints on a usage error, verbatim.
const USAGE: &str = "\
usage: git bisect start [--term-(bad|new)=<term-new> --term-(good|old)=<term-old>]
                        [--no-checkout] [--first-parent] [<bad> [<good>...]] [--] [<pathspec>...]
   or: git bisect (bad|new|<term-new>) [<rev>]
   or: git bisect (good|old|<term-old>) [<rev>...]
   or: git bisect terms [--term-(good|old) | --term-(bad|new)]
   or: git bisect skip [(<rev>|<range>)...]
   or: git bisect next
   or: git bisect reset [<commit>]
   or: git bisect (visualize|view)
   or: git bisect replay <logfile>
   or: git bisect log
   or: git bisect run <cmd> [<arg>...]
   or: git bisect help

";

pub fn bisect(args: &[String]) -> Result<ExitCode> {
    // Dispatch hands us the subcommand at index 0; tolerate its absence so the
    // module works either way.
    let args: &[String] = match args.first() {
        Some(a) if a == "bisect" => &args[1..],
        _ => args,
    };

    let Some(sub) = args.first().map(String::as_str) else {
        return word_fallback(args);
    };
    let rest = &args[1..];

    match sub {
        "start" => start(rest),
        "terms" => terms_cmd(rest),
        "log" => log_cmd(),
        "reset" => reset_cmd(rest),
        "replay" => replay_cmd(rest),
        "next" => next_cmd(),
        // `parse_options` answers `-h` on stdout, `usage_with_options` (which is
        // what the `help` word reaches) on stderr. Both exit 129. `--help-all`
        // renders `USAGE_FULL`, identical here: no entry is `PARSE_OPT_HIDDEN`.
        "-h" | "--help-all" => {
            print!("{USAGE}");
            Ok(ExitCode::from(129))
        }
        "help" => {
            eprint!("{USAGE}");
            Ok(ExitCode::from(129))
        }
        "skip" => skip_cmd(rest),
        "run" => run_cmd(rest),
        "visualize" | "view" => visualize_cmd(rest),
        // A lone `--` is not a subcommand, and `cmd_bisect` passes no
        // `PARSE_OPT_KEEP_DASHDASH` (bisect.c:1465-1466), so `parse_options`
        // swallows it and stops. The `OPT_SUBCOMMAND` table is only ever
        // consulted at `argv[0]`, so whatever follows lands in the `!fn`
        // fallback as a bare word: `git bisect -- log` is `unknown command:
        // 'log'`, never the `log` subcommand, and `git bisect -- -h` is
        // `unknown command: '-h'`, not parse-options' own refusal.
        "--" => word_fallback(rest),
        // A dashed word is never a marking term: `cmd_bisect`'s table is
        // `OPT_SUBCOMMAND`s only, so `parse_options_step()` hands it to
        // `parse_long_opt()`, finds nothing and answers `PARSE_OPT_UNKNOWN` —
        // parse-options' own refusal (the argument named as typed, `=<value>`
        // and all, then the block, on stderr at 129) rather than bisect's
        // `fatal: unknown command`, which only the legacy word path reaches.
        other if other.len() > 1 && other.starts_with('-') => {
            Ok(super::unknown_option(other, USAGE))
        }
        // Anything else is a marking word — `bad`/`good`, `new`/`old`, or a
        // custom term a stock-git session recorded — or a genuine typo.
        _ => word_fallback(args),
    }
}

/// `cmd_bisect`'s `!fn` branch (bisect.c:1468-1484): no `OPT_SUBCOMMAND`
/// matched, so the first remaining word is `help`, a marking term, or an error.
/// Reached both by a word that simply is not a subcommand and by everything
/// after a `--`, which is why it must not re-examine leading dashes.
fn word_fallback(args: &[String]) -> Result<ExitCode> {
    let Some(word) = args.first().map(String::as_str) else {
        eprint!("fatal: need a command\n\n{USAGE}");
        return Ok(ExitCode::from(129));
    };
    // `if (!strcmp(argv[0], "help")) usage_with_options(...)` sits ahead of the
    // term check, so `--` never hides it.
    if word == "help" {
        eprint!("{USAGE}");
        return Ok(ExitCode::from(129));
    }
    let ctx = Ctx::open()?;
    let is_marking = match read_terms(&ctx)? {
        Some(t) => word == t.bad || word == t.good,
        None => terms_for_first_marking(word).is_some(),
    };
    if is_marking {
        mark(word, &args[1..])
    } else {
        unknown_command(word)
    }
}

// --- state directory ---------------------------------------------------------

/// Repository plus the paths of the bisect state files, which live in the
/// per-worktree `$GIT_DIR` (not the common dir).
struct Ctx {
    repo: gix::Repository,
    git_dir: PathBuf,
}

impl Ctx {
    fn open() -> Result<Self> {
        let repo = gix::discover(".")?;
        let git_dir = repo.git_dir().to_path_buf();
        Ok(Ctx { repo, git_dir })
    }

    fn file(&self, name: &str) -> PathBuf {
        self.git_dir.join(name)
    }

    fn refs_dir(&self) -> PathBuf {
        self.git_dir.join("refs").join("bisect")
    }

    /// git's `file_is_not_empty(git_path_bisect_start())`.
    fn in_progress(&self) -> bool {
        std::fs::metadata(self.file("BISECT_START")).is_ok_and(|m| m.len() > 0)
    }

    /// The bad-side tip, if one has been marked.
    ///
    /// git's `register_ref` compares the trimmed ref name against `term_bad`
    /// itself, so a session opened with `--term-new=broken` keeps its tip in
    /// `refs/bisect/broken`, not `refs/bisect/bad`.
    fn bad(&self, terms: &Terms) -> Result<Option<ObjectId>> {
        read_ref(&self.refs_dir().join(&terms.bad))
    }

    /// Every marked good-side commit, sorted for deterministic iteration.
    /// The refs are named `<term-good>-<oid>`, per `register_ref`'s
    /// `good_prefix`.
    fn goods(&self, terms: &Terms) -> Result<Vec<ObjectId>> {
        let dir = self.refs_dir();
        let Ok(entries) = std::fs::read_dir(&dir) else {
            return Ok(Vec::new());
        };
        let prefix = format!("{}-", terms.good);
        let mut out = Vec::new();
        for entry in entries {
            let entry = entry?;
            let file_name = entry.file_name();
            let Some(name) = file_name.to_str() else {
                continue;
            };
            if !name.starts_with(&prefix) {
                continue;
            }
            if let Some(id) = read_ref(&entry.path())? {
                out.push(id);
            }
        }
        out.sort();
        Ok(out)
    }

    /// `git bisect start --first-parent` records this marker, and every later
    /// step reads it back (`bisect.c:1065`).
    fn first_parent_only(&self) -> bool {
        self.file("BISECT_FIRST_PARENT").exists()
    }

    /// git's `skipped_revs`, which `register_ref()` fills from the refs whose
    /// name starts with `skip-`. That prefix is a literal, not a term: a session
    /// opened with `--term-old`/`--term-new` still records its skips under
    /// `refs/bisect/skip-<oid>`.
    fn skipped(&self) -> Result<Vec<ObjectId>> {
        let Ok(entries) = std::fs::read_dir(self.refs_dir()) else {
            return Ok(Vec::new());
        };
        let mut out = Vec::new();
        for entry in entries {
            let entry = entry?;
            let file_name = entry.file_name();
            let Some(name) = file_name.to_str() else {
                continue;
            };
            if !name.starts_with("skip-") {
                continue;
            }
            if let Some(id) = read_ref(&entry.path())? {
                out.push(id);
            }
        }
        out.sort();
        Ok(out)
    }

    fn append_log(&self, line: &str) -> Result<()> {
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.file("BISECT_LOG"))?;
        f.write_all(line.as_bytes())?;
        Ok(())
    }
}

/// Read a loose ref file (`<40 hex>\n`), returning `None` when it is absent.
fn read_ref(path: &Path) -> Result<Option<ObjectId>> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Ok(None);
    };
    let text = text.trim();
    if text.is_empty() {
        return Ok(None);
    }
    Ok(Some(ObjectId::from_hex(text.as_bytes())?))
}

fn write_ref(path: &Path, id: ObjectId) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, format!("{}\n", id.to_hex()))?;
    Ok(())
}

// --- terms -------------------------------------------------------------------

/// The pair of words naming the two sides of the search. `bad`/`good` by default,
/// `new`/`old` when the session was opened with those, and whatever a stock-git
/// session recorded via `--term-*`.
struct Terms {
    bad: String,
    good: String,
}

/// git's `set_terms(&terms, "bad", "good")`, the seed every entry point uses
/// before it tries `get_terms()`.
fn default_terms() -> Terms {
    Terms {
        bad: "bad".into(),
        good: "good".into(),
    }
}

/// `set_terms(…, "bad", "good"); get_terms(&terms);` in one step.
fn current_terms(ctx: &Ctx) -> Result<Terms> {
    Ok(read_terms(ctx)?.unwrap_or_else(default_terms))
}

fn read_terms(ctx: &Ctx) -> Result<Option<Terms>> {
    let Ok(text) = std::fs::read_to_string(ctx.file("BISECT_TERMS")) else {
        return Ok(None);
    };
    let mut lines = text.lines();
    match (lines.next(), lines.next()) {
        (Some(bad), Some(good)) if !bad.is_empty() && !good.is_empty() => Ok(Some(Terms {
            bad: bad.to_owned(),
            good: good.to_owned(),
        })),
        _ => Ok(None),
    }
}

fn write_terms(ctx: &Ctx, terms: &Terms) -> Result<()> {
    std::fs::write(
        ctx.file("BISECT_TERMS"),
        format!("{}\n{}\n", terms.bad, terms.good),
    )?;
    Ok(())
}

/// Which side of the search a marking lands on.
#[derive(Clone, Copy)]
enum Side {
    Bad,
    Good,
}

/// Resolve the term a marking subcommand names, given the terms already in force.
/// `None` means the word is not a valid marking for this session, which is git's
/// "unknown command" path.
fn side_of(word: &str, terms: &Terms) -> Option<Side> {
    if word == terms.bad {
        Some(Side::Bad)
    } else if word == terms.good {
        Some(Side::Good)
    } else {
        None
    }
}

/// The terms a marking word would establish for a session that has none yet.
fn terms_for_first_marking(word: &str) -> Option<Terms> {
    match word {
        "bad" | "good" => Some(Terms {
            bad: "bad".into(),
            good: "good".into(),
        }),
        "new" | "old" => Some(Terms {
            bad: "new".into(),
            good: "old".into(),
        }),
        _ => None,
    }
}

/// git's `check_term_format`: reject a custom term that is malformed, shadows a
/// builtin subcommand, or would swap the fixed meaning of the `bad`/`new` and
/// `good`/`old` families. `orig_term` is the side being set (`"bad"` or
/// `"good"`). On failure the `Err` string is the exact `error: …` line git
/// writes to stderr; on success `Ok(())`.
fn check_term_format(term: &str, orig_term: &str) -> std::result::Result<(), String> {
    // git validates `refs/bisect/<term>` through `check_refname_format`; the
    // vendored validator answers the same question for a name with slashes.
    let refname = format!("refs/bisect/{term}");
    if gix::validate::reference::name(refname.as_bytes().as_bstr()).is_err() {
        return Err(format!("error: '{term}' is not a valid term"));
    }
    if matches!(
        term,
        "help" | "start" | "skip" | "next" | "reset" | "visualize" | "view" | "replay" | "log"
            | "run" | "terms"
    ) {
        return Err(format!(
            "error: can't use the builtin command '{term}' as a term"
        ));
    }
    if (orig_term != "bad" && matches!(term, "bad" | "new"))
        || (orig_term != "good" && matches!(term, "good" | "old"))
    {
        return Err(format!(
            "error: can't change the meaning of the term '{term}'"
        ));
    }
    Ok(())
}

// --- subcommand: unknown -----------------------------------------------------

/// git prints the "you're currently in a X/Y bisect" hint only when terms exist,
/// then the fatal line and the usage block, and exits 129.
fn unknown_command(word: &str) -> Result<ExitCode> {
    let ctx = Ctx::open()?;
    if let Some(terms) = read_terms(&ctx)? {
        eprintln!(
            "error: Invalid command: you're currently in a {}/{} bisect",
            terms.bad, terms.good
        );
    }
    eprint!("fatal: unknown command: '{word}'\n\n{USAGE}");
    Ok(ExitCode::from(129))
}

// --- subcommands gated on an active session ----------------------------------

/// git's `bisect_autostart`: with nothing started, `fprintf_ln(stderr, "You need
/// to start by \"git bisect start\"\n")` — the format already ends in a newline
/// and `fprintf_ln` adds a second — and the command fails. The interactive
/// "Do you want me to do it for you [Y/n]?" branch is only reached on a tty.
fn autostart(ctx: &Ctx) -> bool {
    if ctx.in_progress() {
        return true;
    }
    eprint!("You need to start by \"git bisect start\"\n\n");
    false
}

/// git's `bisect_next_check(terms, NULL)`. With no `current_term` to fall back
/// on, `decide_next` returns -1 *before* reaching any of its `error()` calls, so
/// a half-marked (or unstarted) session fails silently with exit 1.
fn next_check_silent(ctx: &Ctx, terms: &Terms) -> Result<bool> {
    Ok(ctx.bad(terms)?.is_some() && !ctx.goods(terms)?.is_empty())
}

/// `git bisect skip [(<rev>|<range>)…]` — `bisect_skip()` then `bisect_state()`
/// (builtin/bisect--helper.c:1064-1096).
///
/// Every operand holding `..` is expanded by a revision walk *first*, so
/// `skip A..B` records one `skip` per commit in the range, in the walk's own
/// (newest-first) order. With no operand at all the commit under test is
/// skipped: `BISECT_HEAD` when the session was opened `--no-checkout`, and
/// `HEAD` otherwise.
fn skip_cmd(args: &[String]) -> Result<ExitCode> {
    let ctx = Ctx::open()?;
    // `bisect_state()`'s first act, and the reason an unstarted session fails
    // here rather than at the revision parsing below.
    if !autostart(&ctx) {
        return Ok(ExitCode::from(1));
    }
    let terms = current_terms(&ctx)?;

    let mut specs: Vec<String> = Vec::new();
    for arg in args {
        // The range form is refused rather than approximated. `bisect_skip()`
        // expands it with a `setup_revisions()`/`get_revision()` walk *in the
        // same process* as the bisection that follows, and the object flags that
        // walk leaves behind (`UNINTERESTING` on the excluded endpoint and as
        // much of its ancestry as `still_interesting()`'s slop reached) are not
        // cleared before `bisect_next_all()` runs — so the commits it then
        // considers are fewer than the marked state alone implies.
        //
        // Measured against git 2.55.0 on a 15-commit history bisected `c15`/`c1`:
        // `git bisect skip c6..c9` reports `Bisecting: 4 revisions left … c10`
        // while `git bisect skip c7 c8 c9` — the same three commits, the same
        // refs, the same log — reports `Bisecting: 6 revisions left … c4`. The
        // outcome is therefore not a function of the skip set, and reproducing
        // it needs git's in-process flag lifetime rather than its skip
        // algorithm, which is ported in full below.
        if arg.contains("..") {
            bail!(
                "`bisect skip <a>..<b>` is not supported: upstream expands the range with a \
                 revision walk whose leftover `UNINTERESTING` flags then shrink the candidate \
                 set of the bisection step, so the same skip set reached through a range and \
                 through explicit revisions picks different commits; skip the revisions \
                 individually instead"
            );
        }
        specs.push(arg.clone());
    }
    let no_checkout = ctx.file("BISECT_HEAD").exists();
    if specs.is_empty() {
        // `get_oid("BISECT_HEAD")`, falling back to `HEAD` when that ref is
        // missing — which is the only difference `--no-checkout` makes here.
        specs.push(if no_checkout { "BISECT_HEAD" } else { "HEAD" }.to_string());
    }

    // "All input revs must be checked before executing bisect_write() to discard
    // junk revs" — a bad operand leaves no ref and no log line behind.
    let mut ids = Vec::with_capacity(specs.len());
    for spec in &specs {
        match resolve(&ctx.repo, spec) {
            Ok(id) => ids.push(id),
            Err(_) => {
                eprintln!("error: Bad rev input: {spec}");
                return Ok(ExitCode::from(1));
            }
        }
    }

    let mut verify_expected = read_ref(&ctx.file("BISECT_EXPECTED_REV"))?;
    for id in &ids {
        write_skip(&ctx, *id)?;
        // `bisect_state()`: marking anything other than the commit the last step
        // asked for invalidates both cached answers, once.
        if verify_expected.is_some_and(|expected| expected != *id) {
            let _ = std::fs::remove_file(ctx.file("BISECT_ANCESTORS_OK"));
            let _ = std::fs::remove_file(ctx.file("BISECT_EXPECTED_REV"));
            verify_expected = None;
        }
    }

    auto_next(&ctx, &terms, no_checkout)
}

/// `bisect_write("skip", …)`: the `refs/bisect/skip-<oid>` ref plus the two
/// `BISECT_LOG` lines, without taking a step — which is what `replay` needs and
/// what [`skip_cmd`] does once per operand.
fn write_skip(ctx: &Ctx, id: ObjectId) -> Result<()> {
    write_ref(&ctx.refs_dir().join(format!("skip-{}", id.to_hex())), id)?;
    ctx.append_log(&format!(
        "# skip: [{}] {}\n",
        id.to_hex(),
        subject(&ctx.repo, id)?
    ))?;
    ctx.append_log(&format!("git bisect skip {}\n", id.to_hex()))?;
    Ok(())
}

/// `git bisect visualize|view`: `bisect_next_check(terms, NULL)` first, then the
/// child process.
fn visualize_cmd(_args: &[String]) -> Result<ExitCode> {
    let ctx = Ctx::open()?;
    let terms = current_terms(&ctx)?;
    if !next_check_silent(&ctx, &terms)? {
        return Ok(ExitCode::from(1));
    }
    bail!("`bisect visualize` is not supported (it shells out to gitk/git log)")
}

/// `git bisect run`: `cmd_bisect__run` rejects an empty command line, then
/// `bisect_run` gates on `bisect_next_check(terms, NULL)`.
fn run_cmd(args: &[String]) -> Result<ExitCode> {
    if args.is_empty() {
        eprintln!("error: 'git bisect run' failed: no command provided.");
        return Ok(ExitCode::from(1));
    }
    let ctx = Ctx::open()?;
    let terms = current_terms(&ctx)?;
    if !next_check_silent(&ctx, &terms)? {
        return Ok(ExitCode::from(1));
    }
    bail!(
        "`bisect run` is not supported: it drives an external command per step, reading its \
         exit status to mark, skip (125) or abort the session; the child-process driver is \
         not ported"
    )
}

// --- subcommand: terms -------------------------------------------------------

fn terms_cmd(args: &[String]) -> Result<ExitCode> {
    // git checks the argument count before it even looks for a session.
    if args.len() > 1 {
        eprintln!("error: 'git bisect terms' requires 0 or 1 argument");
        return Ok(ExitCode::from(1));
    }
    let ctx = Ctx::open()?;
    let Some(terms) = read_terms(&ctx)? else {
        eprintln!("error: no terms defined");
        return Ok(ExitCode::from(1));
    };
    match args.first().map(String::as_str) {
        None => {
            println!("Your current terms are '{}' for the old state", terms.good);
            println!("and '{}' for the new state.", terms.bad);
        }
        Some("--term-good" | "--term-old") => println!("{}", terms.good),
        Some("--term-bad" | "--term-new") => println!("{}", terms.bad),
        Some(other) => {
            eprintln!("error: invalid argument {other} for 'git bisect terms'.");
            eprintln!(
                "Supported options are: --term-good|--term-old and --term-bad|--term-new."
            );
            return Ok(ExitCode::from(1));
        }
    }
    Ok(ExitCode::SUCCESS)
}

// --- subcommand: log ---------------------------------------------------------

fn log_cmd() -> Result<ExitCode> {
    let ctx = Ctx::open()?;
    let Ok(text) = std::fs::read(ctx.file("BISECT_LOG")) else {
        eprintln!("error: We are not bisecting.");
        return Ok(ExitCode::from(1));
    };
    std::io::stdout().write_all(&text)?;
    Ok(ExitCode::SUCCESS)
}

// --- subcommand: reset -------------------------------------------------------

fn reset_cmd(args: &[String]) -> Result<ExitCode> {
    if args.len() > 1 {
        eprintln!("error: 'git bisect reset' requires either no argument or a commit");
        return Ok(ExitCode::from(1));
    }

    let ctx = Ctx::open()?;
    // git rev-parses the argument before touching state; a leading `-` is not a
    // flag here, just a commit-ish that fails to resolve.
    if let Some(spec) = args.first() {
        if resolve(&ctx.repo, spec).is_err() {
            eprintln!("error: '{spec}' is not a valid commit");
            return Ok(ExitCode::from(1));
        }
    }
    let target = match args.first() {
        Some(spec) => Some(spec.clone()),
        None => match std::fs::read_to_string(ctx.file("BISECT_START")) {
            Ok(text) => Some(text.trim().to_owned()),
            // Not bisecting and no explicit target: nothing to do, like git.
            Err(_) => None,
        },
    };

    if let Some(target) = target {
        checkout_and_report(&ctx, &target)?;
    }
    clean_state(&ctx)?;
    Ok(ExitCode::SUCCESS)
}

/// Remove every trace of the session: the state files and the `refs/bisect` tree.
fn clean_state(ctx: &Ctx) -> Result<()> {
    for name in [
        "BISECT_ANCESTORS_OK",
        "BISECT_EXPECTED_REV",
        "BISECT_LOG",
        "BISECT_NAMES",
        "BISECT_TERMS",
        "BISECT_HEAD",
        "BISECT_START",
    ] {
        let path = ctx.file(name);
        if path.exists() {
            std::fs::remove_file(path)?;
        }
    }
    let refs = ctx.refs_dir();
    if refs.exists() {
        std::fs::remove_dir_all(refs)?;
    }
    Ok(())
}

/// Check `target` out quietly, then emit git's transition messages on stderr
/// (this crate's `checkout` prints them on stdout, which bisect must not do).
fn checkout_and_report(ctx: &Ctx, target: &str) -> Result<()> {
    let head = ctx.repo.head()?;
    let was_detached = head.is_detached();
    let old_branch = head
        .referent_name()
        .map(|n| n.shorten().to_str_lossy().into_owned());
    let old_id = head.id().map(|id| id.detach());
    drop(head);

    // `bisect reset <commit>` takes any commit-ish, so `target` is routinely not
    // a valid ref name at all (`HEAD~1`); `try_find_reference` reports that as an
    // error rather than "absent", which must not abort the reset.
    let branch_ref = format!("refs/heads/{target}");
    let target_is_branch = matches!(
        ctx.repo.try_find_reference(branch_ref.as_str()),
        Ok(Some(_))
    );

    if !was_detached && target_is_branch && old_branch.as_deref() == Some(target) {
        eprintln!("Already on '{target}'");
        return Ok(());
    }

    // git runs the checkout through `run_command()`, so its output is flushed
    // by the child's `exit()` before anything printed after it here; see
    // `crate::cstdio::run_command`.
    {
        let _child = crate::cstdio::run_command();
        super::checkout::checkout(&["-q".to_string(), target.to_string()])?;
    }

    if was_detached {
        if let Some(id) = old_id {
            eprintln!("Previous HEAD position was {}", describe(&ctx.repo, id)?);
        }
    }
    if target_is_branch {
        eprintln!("Switched to branch '{target}'");
    } else {
        let id = ctx.repo.head_id()?.detach();
        eprintln!("HEAD is now at {}", describe(&ctx.repo, id)?);
    }
    Ok(())
}

/// `<abbreviated oid> <subject>`, as used by checkout's transition messages.
fn describe(repo: &gix::Repository, id: ObjectId) -> Result<String> {
    use gix::prelude::ObjectIdExt;
    let short = id.attach(repo).shorten_or_id().to_string();
    Ok(format!("{short} {}", subject(repo, id)?))
}

// --- subcommand: start -------------------------------------------------------

fn start(args: &[String]) -> Result<ExitCode> {
    let ctx = Ctx::open()?;

    let mut terms = Terms {
        bad: "bad".into(),
        good: "good".into(),
    };
    let mut no_checkout = false;
    let mut first_parent = false;
    let mut must_write_terms = false;
    let mut resolved: Vec<ObjectId> = Vec::new();

    // git scans once for a `--`: its presence turns an unresolvable revision
    // into a hard error rather than the start of the pathspec list.
    let has_double_dash = args.iter().any(|a| a == "--");

    // The argument grammar of `bisect_start`, ported faithfully: options and
    // revisions may interleave, the `--term-*` flags take their value in either
    // `=value` or a following-token form, and everything after `--` (or after
    // the first token that is neither an option nor a resolvable revision) is a
    // pathspec.
    let mut i = 0;
    while i < args.len() {
        let arg = args[i].as_str();
        if arg == "--" {
            break;
        } else if arg == "--no-checkout" {
            no_checkout = true;
        } else if arg == "--first-parent" {
            first_parent = true;
        } else if arg == "--term-good" || arg == "--term-old" {
            i += 1;
            let Some(v) = args.get(i) else {
                eprintln!("error: '' is not a valid term");
                return Ok(ExitCode::from(1));
            };
            must_write_terms = true;
            terms.good = v.clone();
        } else if let Some(v) = arg
            .strip_prefix("--term-good=")
            .or_else(|| arg.strip_prefix("--term-old="))
        {
            must_write_terms = true;
            terms.good = v.to_owned();
        } else if arg == "--term-bad" || arg == "--term-new" {
            i += 1;
            let Some(v) = args.get(i) else {
                eprintln!("error: '' is not a valid term");
                return Ok(ExitCode::from(1));
            };
            must_write_terms = true;
            terms.bad = v.clone();
        } else if let Some(v) = arg
            .strip_prefix("--term-bad=")
            .or_else(|| arg.strip_prefix("--term-new="))
        {
            must_write_terms = true;
            terms.bad = v.to_owned();
        } else if arg.starts_with("--") {
            eprintln!("error: unrecognized option: '{arg}'");
            return Ok(ExitCode::from(1));
        } else {
            // `} else if (!get_oidf(&oid, "%s^{commit}", arg)) {`
            // (`builtin/bisect.c:776`) — one `repo_get_oid()` per revision
            // operand, and the only place `bisect start` resolves the operand as
            // *typed*. `get_oid_with_context_1()` cuts the `^{commit}` back off
            // before `get_oid_basic()` sees it, so the warning names the 40 hex
            // characters the user wrote.
            //
            // It comes first because git's does: the resolution that warns is the
            // one whose failure ends the option scan, so a full-length hex the
            // repository does not have warns and *then* becomes the head of the
            // pathspec list.
            crate::objname::warn_ambiguous_refname(&ctx.repo, arg);
            match resolve(&ctx.repo, arg) {
                Ok(id) => resolved.push(id),
                Err(_) if has_double_dash => {
                    eprintln!("fatal: '{arg}' does not appear to be a valid revision");
                    return Ok(ExitCode::from(128));
                }
                // An unresolvable token with no `--` present starts the pathspec.
                Err(_) => break,
            }
        }
        i += 1;
    }
    // `pathspec_pos`: where the scan stopped — at the `--`, at the first token
    // that is neither an option nor a revision, or past the end.
    let pathspec_pos = i;

    // Naming any revision commits the session to the default terms.
    if !resolved.is_empty() {
        must_write_terms = true;
    }

    // git's `write_terms` gate, in its order: equality first, then the format
    // of each side (bad before good). Each is a plain `error:` line, exit 1.
    if must_write_terms {
        if terms.bad == terms.good {
            eprintln!("error: please use two different terms");
            return Ok(ExitCode::from(1));
        }
        if let Err(msg) = check_term_format(&terms.bad, "bad") {
            eprintln!("{msg}");
            return Ok(ExitCode::from(1));
        }
        if let Err(msg) = check_term_format(&terms.good, "good") {
            eprintln!("{msg}");
            return Ok(ExitCode::from(1));
        }
    }

    // Restarting a live session first returns the worktree to where it began.
    if ctx.in_progress() {
        let start_head = std::fs::read_to_string(ctx.file("BISECT_START"))?
            .trim()
            .to_owned();
        checkout_and_report(&ctx, &start_head)?;
        clean_state(&ctx)?;
    }

    let start_head = head_label(&ctx.repo)?;
    std::fs::create_dir_all(ctx.refs_dir())?;
    std::fs::write(ctx.file("BISECT_START"), format!("{start_head}\n"))?;
    std::fs::write(ctx.file("BISECT_NAMES"), bisect_names(args, pathspec_pos))?;
    std::fs::write(ctx.file("BISECT_LOG"), "")?;
    if first_parent {
        std::fs::write(ctx.file("BISECT_FIRST_PARENT"), "\n")?;
    }
    if no_checkout {
        let head_oid = ctx.repo.head_id()?.detach();
        write_ref(&ctx.file("BISECT_HEAD"), head_oid)?;
    }

    if must_write_terms {
        write_terms(&ctx, &terms)?;
    }

    // The first revision is the bad one; the rest are good.
    for (idx, id) in resolved.iter().enumerate() {
        let (term, path) = if idx == 0 {
            (&terms.bad, ctx.refs_dir().join(&terms.bad))
        } else {
            (
                &terms.good,
                ctx.refs_dir().join(format!("{}-{}", terms.good, id.to_hex())),
            )
        };
        write_ref(&path, *id)?;
        ctx.append_log(&format!(
            "# {term}: [{}] {}\n",
            id.to_hex(),
            subject(&ctx.repo, *id)?
        ))?;
    }

    let quoted: Vec<String> = args.iter().map(|a| sq_quote(a)).collect();
    if quoted.is_empty() {
        ctx.append_log("git bisect start\n")?;
    } else {
        ctx.append_log(&format!("git bisect start {}\n", quoted.join(" ")))?;
    }

    auto_next(&ctx, &terms, no_checkout)
}

/// The whole contents of `BISECT_NAMES`, including its terminating newline.
///
/// ```c
/// if (pathspec_pos < argc - 1)
///         sq_quote_argv(&bisect_names, argv + pathspec_pos);
/// write_file(git_path_bisect_names(), "%s\n", bisect_names.buf);
/// ```
/// — builtin/bisect.c:874. Three details the bytes depend on, all of them git's:
///
///  * the tail starts **at** `pathspec_pos`, so a `--` separator is quoted into
///    the file alongside the paths it introduces;
///  * `sq_quote_argv()` (quote.c) writes a space *before* every argument, so a
///    non-empty list begins with one;
///  * the `< argc - 1` gate means a single trailing token records nothing —
///    `git bisect start no-such-rev` leaves an empty `BISECT_NAMES`, and so does
///    a run whose arguments were all revisions.
fn bisect_names(args: &[String], pathspec_pos: usize) -> String {
    if pathspec_pos + 1 >= args.len() {
        return "\n".to_string();
    }
    let quoted: String =
        args[pathspec_pos..].iter().map(|p| format!(" {}", sq_quote(p))).collect();
    format!("{quoted}\n")
}

/// The label `BISECT_START` records: the branch name, or the full oid when HEAD
/// is detached.
fn head_label(repo: &gix::Repository) -> Result<String> {
    let head = repo.head()?;
    if head.is_unborn() {
        crate::git_fatal!("cannot bisect: HEAD does not point at a commit yet");
    }
    if head.is_detached() {
        let id = head
            .id()
            .ok_or_else(|| anyhow!("cannot resolve detached HEAD"))?
            .detach();
        return Ok(id.to_hex().to_string());
    }
    head.referent_name()
        .map(|n| n.shorten().to_str_lossy().into_owned())
        .ok_or_else(|| anyhow!("cannot determine the current branch"))
}

/// git's `sq_quote_buf`: single-quote unconditionally, escaping `'` and `!`.
fn sq_quote(s: &str) -> String {
    let mut out = String::from("'");
    for c in s.chars() {
        match c {
            '\'' => out.push_str("'\\''"),
            '!' => out.push_str("'\\!'"),
            c => out.push(c),
        }
    }
    out.push('\'');
    out
}

// --- subcommand: bad / good / new / old --------------------------------------

/// Mark one or more revisions, then advance the bisection.
fn mark(word: &str, args: &[String]) -> Result<ExitCode> {
    let ctx = Ctx::open()?;

    // `check_and_set_terms()` runs in `cmd_bisect`'s fallback branch
    // (builtin/bisect.c:1477-1482), *before* `bisect_state()` reaches
    // `bisect_autostart()`. So the marking word settles the terms — and writes
    // `BISECT_TERMS` — even when there is no session to mark, which is why
    // `git bisect bad` in a fresh repository still leaves `bad\ngood\n` behind
    // while `git bisect skip` (a real subcommand, so `check_and_set_terms`
    // returns early for it) leaves nothing.
    let terms = match read_terms(&ctx)? {
        Some(t) => t,
        None => match terms_for_first_marking(word) {
            Some(t) => {
                write_terms(&ctx, &t)?;
                t
            }
            None => return unknown_command(word),
        },
    };
    let Some(side) = side_of(word, &terms) else {
        return unknown_command(word);
    };

    if !ctx.in_progress() {
        eprintln!("You need to start by \"git bisect start\"");
        return Ok(ExitCode::from(1));
    }

    // git treats every argument as a revision; a leading `-` is not a flag, it is
    // a commit-ish that fails to resolve into a `Bad rev input` error below.
    if matches!(side, Side::Bad) && args.len() > 1 {
        eprintln!(
            "error: 'git bisect {}' can take only one argument.",
            terms.bad
        );
        return Ok(ExitCode::from(1));
    }

    let specs: Vec<String> = if args.is_empty() {
        vec!["HEAD".to_string()]
    } else {
        args.to_vec()
    };
    let mut ids = Vec::with_capacity(specs.len());
    for spec in &specs {
        match resolve(&ctx.repo, spec) {
            Ok(id) => ids.push(id),
            Err(_) => {
                eprintln!("error: Bad rev input: {spec}");
                return Ok(ExitCode::from(1));
            }
        }
    }

    // A commit cannot sit on both sides of the search.
    let bad = ctx.bad(&terms)?;
    let goods = ctx.goods(&terms)?;
    for id in &ids {
        let clashes = match side {
            Side::Bad => goods.contains(id),
            Side::Good => bad == Some(*id),
        };
        if clashes {
            println!(
                "{} was both '{}' and '{}'",
                id.to_hex(),
                terms.good,
                terms.bad
            );
            return Ok(ExitCode::from(1));
        }
    }

    for id in &ids {
        let (term, path) = match side {
            Side::Bad => (&terms.bad, ctx.refs_dir().join(&terms.bad)),
            Side::Good => (
                &terms.good,
                ctx.refs_dir().join(format!("{}-{}", terms.good, id.to_hex())),
            ),
        };
        write_ref(&path, *id)?;
        ctx.append_log(&format!(
            "# {term}: [{}] {}\n",
            id.to_hex(),
            subject(&ctx.repo, *id)?
        ))?;
        ctx.append_log(&format!("git bisect {term} {}\n", id.to_hex()))?;
    }

    // A session opened with `--no-checkout` records its position in BISECT_HEAD.
    let no_checkout = ctx.file("BISECT_HEAD").exists();
    auto_next(&ctx, &terms, no_checkout)
}

/// Record one marking (`refs/bisect/<side>` plus the two `BISECT_LOG` lines) the
/// way `mark` does, but without advancing the bisection — used by `replay`, which
/// applies every logged mark and only then takes a single step.
fn write_mark(ctx: &Ctx, term: &str, side: Side, id: ObjectId) -> Result<()> {
    // `bisect_write`: `refs/bisect/<state>` for the bad side, and
    // `refs/bisect/<state>-<rev>` for the good side — `<state>` being the term.
    let path = match side {
        Side::Bad => ctx.refs_dir().join(term),
        Side::Good => ctx.refs_dir().join(format!("{term}-{}", id.to_hex())),
    };
    write_ref(&path, id)?;
    ctx.append_log(&format!(
        "# {term}: [{}] {}\n",
        id.to_hex(),
        subject(&ctx.repo, id)?
    ))?;
    ctx.append_log(&format!("git bisect {term} {}\n", id.to_hex()))?;
    Ok(())
}

// --- subcommand: next --------------------------------------------------------

/// git's `bisect_next`: force a bisection step. Unlike `bisect_auto_next` (which
/// only reports status until both sides are known), `next` will bisect with just
/// a bad commit after a warning, and it errors instead of waiting when a side is
/// missing.
fn next_cmd() -> Result<ExitCode> {
    let ctx = Ctx::open()?;
    if !ctx.in_progress() {
        eprint!("You need to start by \"git bisect start\"\n\n");
        return Ok(ExitCode::from(1));
    }
    let terms = current_terms(&ctx)?;
    let no_checkout = ctx.file("BISECT_HEAD").exists();
    let bad = ctx.bad(&terms)?;
    let goods = ctx.goods(&terms)?;

    match (bad, goods.is_empty()) {
        (Some(bad), false) => take_step(&ctx, &terms, bad, goods, no_checkout),
        (Some(bad), true) => {
            // Have the bad side only: git bisects anyway, less optimally.
            eprintln!("warning: bisecting only with a {} commit", terms.bad);
            take_step(&ctx, &terms, bad, goods, no_checkout)
        }
        (None, false) => {
            eprint!(
                "error: You need to give me at least one bad|new and good|old revision.\n\
                 You can use \"git bisect bad|new\" and \"git bisect good|old\" for that.\n"
            );
            Ok(ExitCode::from(1))
        }
        // Neither side known: git fails silently (its `bisect_next_check` returns
        // an error without a message on this path).
        (None, true) => Ok(ExitCode::from(1)),
    }
}

// --- subcommand: replay ------------------------------------------------------

/// git's `bisect_replay`: re-drive a session from a saved `bisect log`. Each
/// `git bisect …` line is applied — `start` resets and reseeds, every marking
/// writes its ref/log without stepping — and a single `bisect_auto_next` runs at
/// the end, so the terminal output is the start status plus the final step.
fn replay_cmd(args: &[String]) -> Result<ExitCode> {
    let Some(file) = args.first() else {
        eprintln!("error: no logfile given");
        return Ok(ExitCode::from(1));
    };
    let Ok(content) = std::fs::read_to_string(file) else {
        eprintln!("error: cannot read file '{file}' for replaying");
        return Ok(ExitCode::from(1));
    };

    let ctx = Ctx::open()?;
    for raw in content.lines() {
        let line = raw.trim_start();
        let Some(rest) = line
            .strip_prefix("git bisect ")
            .or_else(|| line.strip_prefix("git-bisect "))
        else {
            continue; // comments and blank lines
        };
        let mut toks = rest.split_whitespace();
        let Some(cmd) = toks.next() else {
            continue;
        };
        let cmd_args: Vec<String> = toks.map(str::to_owned).collect();

        if cmd == "start" {
            // `process_replay_line()` hands the rest of the line to
            // `sq_dequote_to_strvec()`, because `bisect_start()` wrote those
            // operands sq-quoted (`git bisect start 'c15' 'c1'`). Splitting on
            // whitespace alone kept the quotes and started a session on branch
            // `'c15'`, which resolves to nothing.
            //
            // [`super::am::sq_dequote`] is the shared inverse; it is laxer than
            // git's, which *fails* on a token that does not open with a quote —
            // so a hand-written log with bare operands replays here and starts an
            // argument-less bisection upstream.
            let text = rest[cmd.len()..].trim_start();
            start(&super::am::sq_dequote(text))?;
            continue;
        }
        // `process_replay_line()` routes `skip` through `bisect_skip()` like any
        // other state word, so a replayed log recreates the `skip-<oid>` refs and
        // its own copy of the two log lines. Only the final `bisect_auto_next()`
        // below takes a step, which is why replaying prints far fewer
        // `Bisecting:` blocks than the original session did.
        if cmd == "skip" {
            for spec in &cmd_args {
                let id = resolve(&ctx.repo, spec)?;
                write_skip(&ctx, id)?;
            }
            continue;
        }

        // Every other keyword is a marking word. Establish terms the way `mark`
        // does on the first marking of a fresh session.
        let terms = match read_terms(&ctx)? {
            Some(t) => t,
            None => match terms_for_first_marking(cmd) {
                Some(t) => {
                    write_terms(&ctx, &t)?;
                    t
                }
                None => continue, // an unrecognized line; git ignores it
            },
        };
        let Some(side) = side_of(cmd, &terms) else {
            continue;
        };
        let term = match side {
            Side::Bad => &terms.bad,
            Side::Good => &terms.good,
        };
        for spec in &cmd_args {
            let id = resolve(&ctx.repo, spec)?;
            write_mark(&ctx, term, side, id)?;
        }
    }

    let terms = current_terms(&ctx)?;
    let no_checkout = ctx.file("BISECT_HEAD").exists();
    auto_next(&ctx, &terms, no_checkout)
}

fn resolve(repo: &gix::Repository, spec: &str) -> Result<ObjectId> {
    let commit = repo.rev_parse_single(spec)?.object()?.peel_to_commit()?;
    Ok(commit.id)
}

/// First line of the commit message, with git's subject folding: the leading
/// paragraph, line breaks collapsed into single spaces.
fn subject(repo: &gix::Repository, id: ObjectId) -> Result<String> {
    let commit = repo.find_object(id)?.try_into_commit()?;
    let raw = commit.message_raw()?.to_str_lossy().into_owned();
    let mut out = String::new();
    for line in raw.lines() {
        let line = line.trim_end();
        if line.is_empty() {
            if out.is_empty() {
                continue;
            }
            break;
        }
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(line);
    }
    Ok(out)
}

// --- the bisection step ------------------------------------------------------

/// git's `bisect_auto_next`: report what is still missing, or take a step.
///
/// With `no_checkout` the chosen commit is recorded in the per-worktree
/// `BISECT_HEAD` ref instead of being checked out, matching `git bisect start
/// --no-checkout`.
fn auto_next(ctx: &Ctx, terms: &Terms, no_checkout: bool) -> Result<ExitCode> {
    let bad = ctx.bad(&terms)?;
    let goods = ctx.goods(&terms)?;

    if bad.is_none() || goods.is_empty() {
        let status = match (bad.is_some(), goods.len()) {
            (false, 0) => format!(
                "status: waiting for both '{}' and '{}' commits",
                terms.good, terms.bad
            ),
            (true, 0) => format!(
                "status: waiting for '{}' commit(s), '{}' commit known",
                terms.good, terms.bad
            ),
            (false, n) => format!(
                "status: waiting for '{}' commit, {n} '{}' commit{} known",
                terms.bad,
                terms.good,
                if n == 1 { "" } else { "s" }
            ),
            // Excluded by the `if` above: both sides known means we take a step.
            (true, _) => unreachable!("both sides are known"),
        };
        println!("{status}");
        ctx.append_log(&format!("# {status}\n"))?;
        return Ok(ExitCode::SUCCESS);
    }

    let bad = bad.expect("checked above");
    take_step(ctx, terms, bad, goods, no_checkout)
}

/// Do the real bisection step: resolve merge bases first (git's
/// `check_merge_bases`), then pick and check out the midpoint. `git bisect next`
/// reaches this directly when only the bad side is known, so `goods` may be empty.
fn take_step(
    ctx: &Ctx,
    terms: &Terms,
    bad: ObjectId,
    goods: Vec<ObjectId>,
    no_checkout: bool,
) -> Result<ExitCode> {
    if !goods.is_empty() {
        if let Some(code) = check_merge_bases(ctx, bad, &goods, terms, no_checkout)? {
            return Ok(code);
        }
    }

    let first_parent = ctx.first_parent_only();
    let candidates = candidate_list(ctx, bad, &goods, first_parent)?;
    let n = candidates.len();
    // `if (skipped_revs.nr) bisect_flags |= FIND_BISECTION_ALL` (bisect.c:1038):
    // with anything skipped the whole list is sorted and then filtered, so a
    // skipped best candidate can be stepped away from.
    let skipped = ctx.skipped()?;
    let (best, reaches, tried) = if skipped.is_empty() {
        let (best, reaches) = find_bisection(ctx, &candidates, first_parent)?;
        (Some(best), reaches, Vec::new())
    } else {
        let (sorted, reaches) = find_bisection_all(ctx, &candidates, first_parent)?;
        let (best, tried) = managed_skipped(&sorted, &skipped, bad);
        (best, reaches, tried)
    };

    let Some(best) = best else {
        // `if (!revs.commits)`: nothing survived the filter, so every remaining
        // candidate was skipped.
        if let Some(code) = error_if_skipped_commits(ctx, &tried, None, terms, &candidates)? {
            return Ok(code);
        }
        println!("{} was both {} and {}", bad.to_hex(), terms.good, terms.bad);
        return Ok(ExitCode::from(1));
    };

    if best == bad {
        // The bad end is all that is left, and skipped commits could still hide
        // the real culprit — git says so instead of naming one.
        if let Some(code) = error_if_skipped_commits(ctx, &tried, Some(bad), terms, &candidates)? {
            return Ok(code);
        }
        return report_first_bad(ctx, bad, terms);
    }

    // `bisect_rev_setup()` prints how many candidates remain *after* the one about
    // to be tested: everything it reaches, minus itself.
    let left = n - reaches - 1;
    let steps = estimate_bisect_steps(n);
    println!(
        "Bisecting: {left} revision{} left to test after this (roughly {steps} step{})",
        if left == 1 { "" } else { "s" },
        if steps == 1 { "" } else { "s" }
    );

    write_ref(&ctx.file("BISECT_EXPECTED_REV"), best)?;
    let hex = best.to_hex().to_string();
    if no_checkout {
        write_ref(&ctx.file("BISECT_HEAD"), best)?;
    } else {
        // Through `run_command()` in git; see `crate::cstdio::run_command`.
        let _child = crate::cstdio::run_command();
        super::checkout::checkout(&["-q".to_string(), hex.clone()])?;
    }
    println!("[{hex}] {}", subject(&ctx.repo, best)?);
    Ok(ExitCode::SUCCESS)
}

/// git's `check_merge_bases`: when a good end is not an ancestor of the bad one,
/// the true merge base(s) must be resolved before the range can be bisected. For
/// each merge base of `bad` against the goods:
///   * if it equals `bad`, the sides are inconsistent — `handle_bad_merge_base`;
///   * if it is already a good, it needs no test;
///   * otherwise it is checked out with `Bisecting: a merge base must be tested`.
///
/// Returns `Some(code)` when the step is complete (a merge base was checked out,
/// exit 0) or refused (`handle_bad_merge_base`); `None` means the range is clear
/// and the caller proceeds to pick the midpoint. Once cleared, `BISECT_ANCESTORS_OK`
/// short-circuits the check on later steps, exactly as git records it.
fn check_merge_bases(
    ctx: &Ctx,
    bad: ObjectId,
    goods: &[ObjectId],
    terms: &Terms,
    no_checkout: bool,
) -> Result<Option<ExitCode>> {
    if ctx.file("BISECT_ANCESTORS_OK").exists() {
        return Ok(None);
    }
    let bases: Vec<ObjectId> = ctx
        .repo
        .merge_bases_many(bad, goods)?
        .into_iter()
        .map(|id| id.detach())
        .collect();
    for mb in bases {
        if mb == bad {
            return Ok(Some(handle_bad_merge_base(ctx, bad, goods, terms)?));
        }
        if goods.contains(&mb) {
            continue;
        }
        // A merge base that is neither the bad rev nor already good has to be
        // tested; git checks it out and stops the step here.
        println!("Bisecting: a merge base must be tested");
        write_ref(&ctx.file("BISECT_EXPECTED_REV"), mb)?;
        let hex = mb.to_hex().to_string();
        if no_checkout {
            write_ref(&ctx.file("BISECT_HEAD"), mb)?;
        } else {
            // Through `run_command()` in git; see `crate::cstdio::run_command`.
            let _child = crate::cstdio::run_command();
            super::checkout::checkout(&["-q".to_string(), hex.clone()])?;
        }
        println!("[{hex}] {}", subject(&ctx.repo, mb)?);
        return Ok(Some(ExitCode::SUCCESS));
    }
    std::fs::write(ctx.file("BISECT_ANCESTORS_OK"), "")?;
    Ok(None)
}

/// git's `handle_bad_merge_base`: the merge base itself is the bad rev. When it is
/// the commit we just asked the user to test (`is_expected_rev`), report that the
/// change lies between it and the goods (exit 3); otherwise the good/bad marks are
/// simply inconsistent (exit 1).
fn handle_bad_merge_base(
    ctx: &Ctx,
    bad: ObjectId,
    goods: &[ObjectId],
    terms: &Terms,
) -> Result<ExitCode> {
    let bad_hex = bad.to_hex().to_string();
    let good_hex = goods
        .iter()
        .map(|g| g.to_hex().to_string())
        .collect::<Vec<_>>()
        .join(" ");
    if is_expected_rev(ctx, bad)? {
        if terms.bad == "bad" && terms.good == "good" {
            eprintln!("The merge base {bad_hex} is bad.");
            eprintln!("This means the bug has been fixed between {bad_hex} and [{good_hex}].");
        } else if terms.bad == "new" && terms.good == "old" {
            eprintln!("The merge base {bad_hex} is new.");
            eprintln!("The property has changed between {bad_hex} and [{good_hex}].");
        } else {
            eprintln!("The merge base {bad_hex} is '{}'.", terms.bad);
            eprintln!(
                "This means the first '{}' commit is between {bad_hex} and [{good_hex}].",
                terms.good
            );
        }
        return Ok(ExitCode::from(3));
    }
    eprintln!(
        "Some '{}' revs are not ancestors of the '{}' rev.",
        terms.good, terms.bad
    );
    eprintln!("git bisect cannot work properly in this case.");
    eprintln!("Maybe you mistook '{}' and '{}' revs?", terms.good, terms.bad);
    Ok(ExitCode::from(1))
}

/// Whether `id` is the commit the last step asked to be tested, per
/// `BISECT_EXPECTED_REV` (git's `is_expected_rev`).
fn is_expected_rev(ctx: &Ctx, id: ObjectId) -> Result<bool> {
    Ok(read_ref(&ctx.file("BISECT_EXPECTED_REV"))? == Some(id))
}

/// The commits still under suspicion — reachable from `bad`, not from any good —
/// in the order git's revision walk yields them (newest first, `list[0] == bad`).
fn candidate_list(
    ctx: &Ctx,
    bad: ObjectId,
    goods: &[ObjectId],
    first_parent: bool,
) -> Result<Vec<ObjectId>> {
    let mut list = Vec::new();
    let mut walk = ctx.repo.rev_walk(Some(bad)).with_hidden(goods.to_vec());
    // `bisect_rev_setup` sets `revs.first_parent_only` from BISECT_FIRST_PARENT,
    // so the candidate set itself is the first-parent chain (bisect.c:1077).
    if first_parent {
        walk = walk.first_parent_only();
    }
    for info in walk.all()? {
        list.push(info?.id);
    }
    if list.is_empty() {
        crate::git_fatal!("no testable commit found between the marked revisions");
    }
    Ok(list)
}

/// `find_bisection()` (bisect.c:240-317): pick the commit that splits the
/// candidate set most evenly, and say how many candidates it reaches.
///
/// The weight of a candidate is how many candidates it can reach, itself
/// included. git computes those by propagating along the ancestry — a commit with
/// one interesting parent inherits its parent's weight plus one, a merge is
/// counted outright — and then walks the list looking for a commit that is
/// *halfway*: `|2 * weight - nr| <= 1`. The first such commit wins immediately;
/// with none (a set too small to have one), the commit whose
/// `min(weight, nr - weight)` is largest does.
///
/// The weights here are computed by reachability over the candidate subgraph,
/// oldest first, which is the same number git's propagation arrives at and is
/// exact for a merge as well as for a chain.
fn find_bisection(ctx: &Ctx, list: &[ObjectId], first_parent: bool) -> Result<(ObjectId, usize)> {
    let nr = list.len();
    let (parents, weights) = bisection_weights(ctx, list, first_parent)?;
    find_bisection_from(list, &parents, &weights, nr)
}

/// `find_bisection()` with `FIND_BISECTION_ALL`, which `bisect_next_all()` turns
/// on as soon as anything has been skipped (bisect.c:1039).
///
/// It suppresses `do_find_bisection()`'s halfway shortcut and ends in
/// `best_bisection_sorted()` instead of `best_bisection()`: the whole candidate
/// list comes back ordered by `min(weight, nr - weight)` descending, ties broken
/// by ascending object id, so `managed_skipped()` can walk it from the best
/// candidate outwards. `reaches` stays the *head's* weight — `find_bisection()`
/// assigns `*reaches = weight(best)` before the skip filter runs, which is why
/// the `Bisecting: N revisions left` line does not change when a skip moves the
/// commit that actually gets checked out.
fn find_bisection_all(
    ctx: &Ctx,
    list: &[ObjectId],
    first_parent: bool,
) -> Result<(Vec<ObjectId>, usize)> {
    let nr = list.len();
    let (_parents, weights) = bisection_weights(ctx, list, first_parent)?;
    let mut order: Vec<usize> = (0..nr).collect();
    let distance = |i: usize| {
        let w = weights[i] as i64;
        w.min(nr as i64 - w)
    };
    // `compare_commit_dist()`: descending distance, then ascending `oidcmp`.
    order.sort_by(|&a, &b| {
        distance(b)
            .cmp(&distance(a))
            .then_with(|| list[a].as_bytes().cmp(list[b].as_bytes()))
    });
    let reaches = order.first().map(|&i| weights[i]).unwrap_or(0);
    Ok((order.into_iter().map(|i| list[i]).collect(), reaches))
}

/// The candidate subgraph: each commit's candidate parents (by list position)
/// and the number of candidates it reaches, itself included — git's `weight()`.
fn bisection_weights(
    ctx: &Ctx,
    list: &[ObjectId],
    first_parent: bool,
) -> Result<(Vec<Vec<usize>>, Vec<usize>)> {
    let nr = list.len();
    let index: std::collections::HashMap<ObjectId, usize> =
        list.iter().enumerate().map(|(i, id)| (*id, i)).collect();

    // Parents that are themselves candidates, by list position. With
    // `FIND_BISECTION_FIRST_PARENT_ONLY` git stops after the first parent in
    // both `count_distance` and the weight propagation (bisect.c:104, 338, 362).
    let mut parents: Vec<Vec<usize>> = Vec::with_capacity(nr);
    for id in list {
        let commit = ctx.repo.find_object(*id)?.try_into_commit()?;
        let mut ps = commit.parent_ids();
        parents.push(if first_parent {
            ps.next()
                .and_then(|p| index.get(&p.detach()).copied())
                .into_iter()
                .collect()
        } else {
            ps.filter_map(|p| index.get(&p.detach()).copied()).collect()
        });
    }

    // `reach[i]` is the set of candidates commit `i` can reach, as a bitset.
    //
    // The list is in commit-date order, which is *not* a topological order — with
    // equal dates a parent can be listed before its child — so the sets are filled
    // by an explicit post-order walk rather than by iterating the list. This is
    // what git's repeated propagation loop achieves by re-scanning until every
    // weight is known.
    let words = nr.div_ceil(64);
    let mut reach = vec![0u64; nr * words];
    let mut done = vec![false; nr];
    for root in 0..nr {
        if done[root] {
            continue;
        }
        let mut stack = vec![(root, false)];
        while let Some((i, expanded)) = stack.pop() {
            if done[i] {
                continue;
            }
            if !expanded {
                stack.push((i, true));
                for &p in &parents[i] {
                    if !done[p] {
                        stack.push((p, false));
                    }
                }
                continue;
            }
            // Every parent is finished by now: union their sets into this one.
            let (before, rest) = reach.split_at_mut(i * words);
            let (mine, after) = rest.split_at_mut(words);
            mine[i / 64] |= 1u64 << (i % 64);
            for &p in &parents[i] {
                let src = if p > i {
                    &after[(p - i - 1) * words..(p - i) * words]
                } else {
                    &before[p * words..(p + 1) * words]
                };
                for (dst, s) in mine.iter_mut().zip(src) {
                    *dst |= *s;
                }
            }
            done[i] = true;
        }
    }
    let weights: Vec<usize> = (0..nr)
        .map(|i| reach[i * words..(i + 1) * words].iter().map(|w| w.count_ones() as usize).sum())
        .collect();
    Ok((parents, weights))
}

/// `do_find_bisection()` plus `best_bisection()`: the single commit git tests
/// next when nothing has been skipped.
fn find_bisection_from(
    list: &[ObjectId],
    parents: &[Vec<usize>],
    weights: &[usize],
    nr: usize,
) -> Result<(ObjectId, usize)> {
    // `do_find_bisection()` (bisect.c:130-217), simulated in its own order because
    // the order decides which of several equally good candidates is returned.
    //
    // git walks the list *reversed* (`find_bisection()` turns it round while
    // counting), seeds every commit that has no interesting parent with weight 1,
    // and then loops over the rest until each one's weight is known — a commit
    // with one interesting parent inherits `parent + 1`, a merge is counted
    // outright. The halfway shortcut is tested only where a weight is *derived*,
    // never on the seeded ones, which is why a three-commit range returns the
    // middle commit rather than the oldest.
    const PENDING_ONE: i64 = -1;
    const PENDING_MERGE: i64 = -2;
    let mut w: Vec<i64> = vec![0; nr];
    let mut counted = 0usize;
    let order: Vec<usize> = (0..nr).rev().collect();
    for &i in &order {
        w[i] = match parents[i].len() {
            0 => {
                counted += 1;
                1
            }
            1 => PENDING_ONE,
            _ => PENDING_MERGE,
        };
    }
    let halfway = |weight: i64| (-1..=1).contains(&(2 * weight - nr as i64));
    while counted < nr {
        let mut progress = false;
        for &i in &order {
            match w[i] {
                PENDING_MERGE => {
                    // `count_distance()`: walk from the merge and count what it
                    // reaches, which is exactly the reachability computed above.
                    w[i] = weights[i] as i64;
                    counted += 1;
                    progress = true;
                    if halfway(w[i]) {
                        return Ok((list[i], weights[i]));
                    }
                }
                PENDING_ONE => {
                    let p = parents[i][0];
                    if w[p] < 0 {
                        continue;
                    }
                    w[i] = w[p] + 1;
                    counted += 1;
                    progress = true;
                    if halfway(w[i]) {
                        return Ok((list[i], w[i] as usize));
                    }
                }
                _ => {}
            }
        }
        if !progress {
            break;
        }
    }

    // Nothing was halfway: the commit whose `min(weight, nr - weight)` is largest,
    // scanned in the same reversed order.
    let mut best = (list[order[0]], weights[order[0]]);
    let mut best_distance = -1i64;
    for &i in &order {
        let weight = weights[i] as i64;
        let distance = weight.min(nr as i64 - weight);
        if distance > best_distance {
            best_distance = distance;
            best = (list[i], weights[i]);
        }
    }
    Ok(best)
}

/// `filter_skipped()` with `show_all = 0` (bisect.c:521-568), over the
/// distance-sorted candidate list.
///
/// Returns `(kept, tried, count, skipped_first)`. The shortcut matters: while the
/// head of the list has *not* been skipped, the first unskipped commit ends the
/// scan and the rest of the list comes back untouched — so `count` is 0 and no
/// `tried` list is built. Only when the head itself is skipped does git filter
/// the whole list, because it then has to pick a replacement away from it.
fn filter_skipped(
    list: &[ObjectId],
    skipped: &[ObjectId],
) -> (Vec<ObjectId>, Vec<ObjectId>, usize, bool) {
    let is_skipped = |id: &ObjectId| skipped.binary_search(id).is_ok();
    let mut kept: Vec<ObjectId> = Vec::new();
    let mut tried: Vec<ObjectId> = Vec::new();
    let mut skipped_first = false;
    if skipped.is_empty() {
        return (list.to_vec(), tried, 0, false);
    }
    for (i, id) in list.iter().enumerate() {
        if is_skipped(id) {
            if !skipped_first {
                skipped_first = true;
            }
            tried.push(*id);
        } else {
            if !skipped_first {
                // `return list`: everything from here on, unfiltered.
                return (list[i..].to_vec(), Vec::new(), 0, false);
            }
            kept.push(*id);
        }
    }
    let count = kept.len();
    (kept, tried, count, skipped_first)
}

/// git's `get_prn()` (bisect.c:579): `rand(3)`'s recurrence, seeded with the
/// count itself rather than carried between calls — so the pick is a pure
/// function of how many candidates survived the skip filter.
fn get_prn(count: u32) -> u32 {
    let count = count.wrapping_mul(1103515245).wrapping_add(12345);
    (count / 65536) % PRN_MODULO
}

/// bisect.c:571.
const PRN_MODULO: u32 = 32768;

/// git's `sqrti()` (bisect.c:588): Newton's method in `float`, stopping once the
/// step is below half a unit, then truncated. Ported in `f32` because the
/// rounding of the intermediate values is what the result depends on.
fn sqrti(val: i32) -> i32 {
    if val == 0 {
        return 0;
    }
    let mut x = val as f32;
    loop {
        let y = (x + val as f32 / x) / 2.0;
        let d = if y > x { y - x } else { x - y };
        x = y;
        if d < 0.5 {
            break;
        }
    }
    x as i32
}

/// git's `skip_away()` (bisect.c:605): step away from the best candidate by a
/// pseudo-random distance, and never land on the `bad` end itself.
fn skip_away(list: &[ObjectId], count: usize, bad: ObjectId) -> Option<ObjectId> {
    let prn = get_prn(count as u32) as i64;
    let index =
        (count as i64 * prn / PRN_MODULO as i64) * sqrti(prn as i32) as i64 / sqrti(PRN_MODULO as i32) as i64;
    for (i, id) in list.iter().enumerate() {
        if i as i64 == index {
            if *id != bad {
                return Some(*id);
            }
            // The `bad` end is never handed back as the next commit to test:
            // git steps one *back* towards the better candidates instead.
            return Some(if i > 0 { list[i - 1] } else { list[0] });
        }
    }
    list.first().copied()
}

/// git's `managed_skipped()` (bisect.c:630): the filter plus, when the best
/// candidate is one of the skipped ones, the step away from it.
fn managed_skipped(
    list: &[ObjectId],
    skipped: &[ObjectId],
    bad: ObjectId,
) -> (Option<ObjectId>, Vec<ObjectId>) {
    if skipped.is_empty() {
        return (list.first().copied(), Vec::new());
    }
    let (kept, tried, count, skipped_first) = filter_skipped(list, skipped);
    if !skipped_first {
        return (kept.first().copied(), tried);
    }
    (skip_away(&kept, count, bad), tried)
}

/// git's `error_if_skipped_commits()` (bisect.c:465): the search cannot go on
/// because every remaining candidate was skipped. Exit 2
/// (`BISECT_ONLY_SKIPPED_LEFT`), and `bisect_skipped_commits()` then appends the
/// same candidate list to `BISECT_LOG` in *revision-walk* order rather than the
/// distance order printed here.
fn error_if_skipped_commits(
    ctx: &Ctx,
    tried: &[ObjectId],
    bad: Option<ObjectId>,
    terms: &Terms,
    candidates: &[ObjectId],
) -> Result<Option<ExitCode>> {
    if tried.is_empty() {
        return Ok(None);
    }
    println!("There are only 'skip'ped commits left to test.");
    println!("The first '{}' commit could be any of:", terms.bad);
    for id in tried {
        println!("{}", id.to_hex());
    }
    if let Some(bad) = bad {
        println!("{}", bad.to_hex());
    }
    println!("We cannot bisect more!");

    ctx.append_log("# only skipped commits left to test\n")?;
    for id in candidates {
        ctx.append_log(&format!(
            "# possible first '{}' commit: [{}] {}\n",
            terms.bad,
            id.to_hex(),
            subject(&ctx.repo, *id)?
        ))?;
    }
    Ok(Some(ExitCode::from(2)))
}

/// git's `estimate_bisect_steps`.
fn estimate_bisect_steps(all: usize) -> usize {
    if all < 3 {
        return 0;
    }
    let n = usize::BITS as usize - 1 - all.leading_zeros() as usize; // floor(log2(all))
    let e = 1usize << n;
    let x = all - e;
    if e < 3 * x {
        n
    } else {
        n - 1
    }
}

/// The bisection is over: name the culprit and show it, as git does.
fn report_first_bad(ctx: &Ctx, bad: ObjectId, terms: &Terms) -> Result<ExitCode> {
    let hex = bad.to_hex().to_string();
    let subj = subject(&ctx.repo, bad)?;
    // Rendered before anything is printed, so an unsupported diff bails cleanly.
    let report = diff_tree_report(&ctx.repo, bad)?;

    ctx.append_log(&format!("# first '{}' commit: [{hex}] {subj}\n", terms.bad))?;
    println!("{hex} is the first '{}' commit", terms.bad);
    std::io::stdout().write_all(&report)?;
    Ok(ExitCode::SUCCESS)
}

// --- `git diff-tree --pretty --stat --summary` --------------------------------

/// One row of the diffstat.
struct StatEntry {
    /// Display path, C-quoted when it needs it (so always ASCII).
    name: String,
    added: u32,
    deleted: u32,
    /// `(old size, new size)` for a binary file, which shows no `+`/`-` graph.
    binary: Option<(u64, u64)>,
    /// The `--summary` line this change contributes, if any.
    summary: Option<String>,
}

/// Render the commit exactly as `git diff-tree --pretty --stat --summary` does.
/// Like diff-tree, a root commit or an empty diff renders nothing at all.
fn diff_tree_report(repo: &gix::Repository, id: ObjectId) -> Result<Vec<u8>> {
    let commit = repo.find_object(id)?.try_into_commit()?;
    let parents: Vec<ObjectId> = commit.parent_ids().map(|p| p.detach()).collect();
    let Some(parent) = parents.first().copied() else {
        return Ok(Vec::new());
    };

    // For a merge, `--cc` still renders `--stat`/`--summary` against the first
    // parent alone (combine-diff.c:1567 "show stat against the first parent even
    // when doing combined diff"), so the same single-parent diff serves both.
    let new_tree = commit.tree()?;
    let old_tree = repo.find_object(parent)?.try_into_commit()?.tree()?;
    let mut changes = repo.diff_tree_to_tree(
        Some(&old_tree),
        Some(&new_tree),
        gix::diff::Options::default(),
    )?;
    if changes.is_empty() {
        return Ok(Vec::new());
    }
    changes.sort_by(|a, b| change_path(a).cmp(change_path(b)));

    let mut files = Vec::with_capacity(changes.len());
    for change in &changes {
        files.push(stat_entry(repo, change)?);
    }

    let mut out: Vec<u8> = Vec::new();
    writeln!(out, "commit {}", commit.id())?;
    // `--pretty` adds a `Merge:` header for a multi-parent commit. bisect's
    // `show_diff_tree()` leaves `rev.abbrev` at git's default (unlike
    // `diff-tree`, which pins it to full oids), so the parents are abbreviated.
    if parents.len() > 1 {
        use gix::prelude::ObjectIdExt;
        let shorts: Vec<String> = parents
            .iter()
            .map(|p| p.attach(repo).shorten_or_id().to_string())
            .collect();
        writeln!(out, "Merge: {}", shorts.join(" "))?;
    }
    let author = commit.author()?;
    out.extend_from_slice(b"Author: ");
    out.extend_from_slice(author.name);
    out.extend_from_slice(b" <");
    out.extend_from_slice(author.email);
    out.extend_from_slice(b">\n");
    let date = author.time()?.format(gix::date::time::format::DEFAULT)?;
    writeln!(out, "Date:   {date}")?;
    out.push(b'\n');
    for line in trim_trailing_newlines(commit.message_raw()?).split(|&b| b == b'\n') {
        out.extend_from_slice(b"    ");
        out.extend_from_slice(line);
        out.push(b'\n');
    }
    out.push(b'\n');
    out.extend_from_slice(render_stat(&files).as_bytes());
    Ok(out)
}

fn change_path(change: &ChangeDetached) -> &[u8] {
    match change {
        ChangeDetached::Addition { location, .. }
        | ChangeDetached::Deletion { location, .. }
        | ChangeDetached::Modification { location, .. }
        | ChangeDetached::Rewrite { location, .. } => location,
    }
}

fn trim_trailing_newlines(mut s: &[u8]) -> &[u8] {
    while let Some(&last) = s.last() {
        if last == b'\n' || last == b'\r' {
            s = &s[..s.len() - 1];
        } else {
            break;
        }
    }
    s
}

/// Turn one tree change into a diffstat row, counting lines with git's own
/// (Myers + indent heuristic) diff so the numbers match.
fn stat_entry(repo: &gix::Repository, change: &ChangeDetached) -> Result<StatEntry> {
    match change {
        ChangeDetached::Addition {
            location,
            entry_mode,
            id,
            ..
        } => {
            let content = content_of(repo, *id, entry_mode.is_commit())?;
            let name = quote_path(location);
            let summary = Some(format!("create mode {:06o} {name}", entry_mode.value()));
            if is_binary(&content) {
                return Ok(StatEntry {
                    name,
                    added: 0,
                    deleted: 0,
                    binary: Some((0, content.len() as u64)),
                    summary,
                });
            }
            Ok(StatEntry {
                name,
                added: count_lines(&[], &content),
                deleted: 0,
                binary: None,
                summary,
            })
        }
        ChangeDetached::Deletion {
            location,
            entry_mode,
            id,
            ..
        } => {
            let content = content_of(repo, *id, entry_mode.is_commit())?;
            let name = quote_path(location);
            let summary = Some(format!("delete mode {:06o} {name}", entry_mode.value()));
            if is_binary(&content) {
                return Ok(StatEntry {
                    name,
                    added: 0,
                    deleted: 0,
                    binary: Some((content.len() as u64, 0)),
                    summary,
                });
            }
            Ok(StatEntry {
                name,
                added: 0,
                deleted: count_lines(&content, &[]),
                binary: None,
                summary,
            })
        }
        ChangeDetached::Modification {
            location,
            previous_entry_mode,
            previous_id,
            entry_mode,
            id,
        } => {
            let name = quote_path(location);
            let summary = (previous_entry_mode.value() != entry_mode.value()).then(|| {
                format!(
                    "mode change {:06o} => {:06o} {name}",
                    previous_entry_mode.value(),
                    entry_mode.value()
                )
            });
            if previous_id == id {
                return Ok(StatEntry {
                    name,
                    added: 0,
                    deleted: 0,
                    binary: None,
                    summary,
                });
            }
            let old = content_of(repo, *previous_id, previous_entry_mode.is_commit())?;
            let new = content_of(repo, *id, entry_mode.is_commit())?;
            if is_binary(&old) || is_binary(&new) {
                return Ok(StatEntry {
                    name,
                    added: 0,
                    deleted: 0,
                    binary: Some((old.len() as u64, new.len() as u64)),
                    summary,
                });
            }
            let input = InternedInput::new(old.as_slice(), new.as_slice());
            let diff = diff_with_slider_heuristics(Algorithm::Myers, &input);
            Ok(StatEntry {
                name,
                added: diff.count_additions(),
                deleted: diff.count_removals(),
                binary: None,
                summary,
            })
        }
        // Never produced: rewrite tracking is off, matching diff-tree's default.
        ChangeDetached::Rewrite { .. } => bail!("rename/copy detection is not supported"),
    }
}

fn count_lines(old: &[u8], new: &[u8]) -> u32 {
    let input = InternedInput::new(old, new);
    let diff = diff_with_slider_heuristics(Algorithm::Myers, &input);
    if old.is_empty() {
        diff.count_additions()
    } else {
        diff.count_removals()
    }
}

/// The bytes to diff: a blob straight from the odb, a submodule as the
/// `Subproject commit <oid>` line git substitutes.
fn content_of(repo: &gix::Repository, id: ObjectId, is_submodule: bool) -> Result<Vec<u8>> {
    if is_submodule {
        Ok(format!("Subproject commit {}\n", id.to_hex()).into_bytes())
    } else {
        Ok(repo.find_object(id)?.detach().data)
    }
}

/// git's binary heuristic: a NUL byte within the first 8000 bytes.
fn is_binary(data: &[u8]) -> bool {
    data.iter().take(8000).any(|&b| b == 0)
}

/// `quote_c_style()`: the name verbatim unless some byte needs escaping, in which
/// case the whole name double-quoted with C escapes. The table and the
/// `core.quotePath` flag it reads live in [`crate::quote`], shared with every
/// other verb that prints a path.
fn quote_path(path: impl AsRef<[u8]>) -> String {
    crate::quote::quoted_name_string(path.as_ref())
}

/// The rows [`super::diffstat::show_stats`] renders.
fn stat_rows(files: &[StatEntry]) -> Vec<diffstat::StatFile> {
    files
        .iter()
        .map(|f| match f.binary {
            Some((old, new)) => diffstat::StatFile {
                print_name: f.name.clone().into_bytes(),
                added: new,
                deleted: old,
                binary: true,
                is_unmerged: false,
            },
            None => diffstat::StatFile::text(
                f.name.clone().into_bytes(),
                u64::from(f.added),
                u64::from(f.deleted),
            ),
        })
        .collect()
}

/// The `--stat` block plus the `--summary` lines, as `show_diff_tree()` prints
/// them. This is `diff-tree`'s geometry — `builtin/bisect.c` never calls
/// `init_diffstat_widths()` — so it is a flat 80 columns and ignores `$COLUMNS`.
fn render_stat(files: &[StatEntry]) -> String {
    let mut out = Vec::new();
    diffstat::show_stats(
        &mut out,
        &stat_rows(files),
        &StatWidths::plumbing(),
        &super::diff_color::DiffColors::disabled(),
    );
    let mut out = String::from_utf8_lossy(&out).into_owned();
    for f in files {
        if let Some(line) = &f.summary {
            out.push_str(&format!(" {line}\n"));
        }
    }
    out
}



#[cfg(test)]
mod tests {
    use super::{bisect_names, terms_for_first_marking};

    fn argv(args: &[&str]) -> Vec<String> {
        args.iter().map(|s| s.to_string()).collect()
    }

    /// Every string here is stock git 2.55.0's own `.git/BISECT_NAMES`, read back
    /// after the named invocation. The leading space and the quoted `--` are the
    /// two details `sq_quote_argv()` contributes and the two this port had wrong:
    /// it wrote the pathspec tail without them, so a `bisect start` that named no
    /// pathspec at all recorded the last *revision* as one.
    #[test]
    fn bisect_names_matches_sq_quote_argv() {
        // `git bisect start HEAD HEAD~2` — both operands parse as revisions, so
        // the scan runs off the end and nothing is recorded.
        assert_eq!(bisect_names(&argv(&["HEAD", "HEAD~2"]), 2), "\n");
        // `git bisect start HEAD -- src` — the separator is part of the tail.
        assert_eq!(bisect_names(&argv(&["HEAD", "--", "src"]), 1), " '--' 'src'\n");
        assert_eq!(bisect_names(&argv(&["--", "src"]), 0), " '--' 'src'\n");
        // `git bisect start no-such-rev` — one unresolvable trailing token is
        // below git's `pathspec_pos < argc - 1` gate, so it records nothing.
        assert_eq!(bisect_names(&argv(&["no-such-rev"]), 0), "\n");
        // …and the same token followed by another one is recorded, both of them.
        assert_eq!(bisect_names(&argv(&["a", "b"]), 0), " 'a' 'b'\n");
        // A path needing shell quoting keeps `sq_quote_buf`'s escaping.
        assert_eq!(bisect_names(&argv(&["--", "it's"]), 0), " '--' 'it'\\''s'\n");
    }

    /// The marking word decides the term pair, and only for the four words git
    /// recognises — `check_and_set_terms()` returns early for `skip`, which is
    /// why `git bisect skip` in a fresh repository writes no `BISECT_TERMS`.
    #[test]
    fn first_marking_word_picks_the_term_pair() {
        for (word, bad, good) in
            [("bad", "bad", "good"), ("good", "bad", "good"), ("new", "new", "old"), ("old", "new", "old")]
        {
            let t = terms_for_first_marking(word).expect("marking word");
            assert_eq!((t.bad.as_str(), t.good.as_str()), (bad, good), "for {word}");
        }
        assert!(terms_for_first_marking("skip").is_none());
        assert!(terms_for_first_marking("start").is_none());
    }

    /// `skip_away()`'s pick is arithmetic, not chance: `get_prn(count)` seeded
    /// with the surviving-candidate count, `sqrti()`'s `float` Newton iteration,
    /// and the index they multiply out to.
    ///
    /// Every row is the output of bisect.c's own `get_prn`/`sqrti` compiled and
    /// run over the same counts, which is what the port has to agree with — the
    /// two `int` truncations and the `float` rounding are all load-bearing
    /// (`sqrti(21381)` is 146, and one off in either direction moves the pick).
    #[test]
    fn skip_away_index_is_gits_arithmetic() {
        // (count, prn, sqrti(prn), index)
        let rows = [
            (0u32, 0u32, 0i32, 0i64),
            (1, 16838, 129, 0),
            (2, 908, 30, 0),
            (3, 17747, 133, 0),
            (4, 1817, 42, 0),
            (5, 18655, 136, 1),
            (7, 19564, 139, 3),
            (11, 21381, 146, 5),
            (14, 6360, 79, 0),
            (100, 12662, 112, 23),
            (32767, 25968, 161, 23097),
        ];
        assert_eq!(super::sqrti(super::PRN_MODULO as i32), 181);
        for (count, prn, root, index) in rows {
            assert_eq!(super::get_prn(count), prn, "get_prn({count})");
            assert_eq!(super::sqrti(prn as i32), root, "sqrti({prn})");
            let got = (count as i64 * prn as i64 / super::PRN_MODULO as i64)
                * super::sqrti(prn as i32) as i64
                / super::sqrti(super::PRN_MODULO as i32) as i64;
            assert_eq!(got, index, "index for count={count}");
        }
    }
}
