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
//!
//! The step selection reproduces git's `find_bisection()` exactly, including the
//! `halfway()` short-circuit that decides which of two equally-good midpoints is
//! taken, so the chosen commit, the `Bisecting: N revisions left to test after
//! this (roughly M steps)` line and the `[<oid>] <subject>` line are byte-identical.
//! The terminal report reproduces `git diff-tree --pretty --stat --summary`,
//! including git's diffstat column scaling and truncation.
//!
//! Honest limitations — each bails with a precise message rather than guessing:
//!   * Merge commits inside the bisect range. git's weight propagation for
//!     multi-parent commits is not reproduced here, so a non-linear range would
//!     pick a different midpoint; that is refused instead.
//!   * A good revision that is not an ancestor of the bad one (git's
//!     `Bisecting: a merge base must be tested` path).
//!   * `skip`, `run`, `replay`, `visualize`/`view`, `next`, and `help`.
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
        eprint!("fatal: need a command\n\n{USAGE}");
        return Ok(ExitCode::from(129));
    };
    let rest = &args[1..];

    match sub {
        "start" => start(rest),
        "terms" => terms_cmd(rest),
        "log" => log_cmd(),
        "reset" => reset_cmd(rest),
        "skip" => bail!(
            "`bisect skip` is not supported: git picks a replacement commit at random from the \
             remaining candidates, which this port does not reproduce"
        ),
        "run" => bail!("`bisect run` is not supported (it drives an external command per step)"),
        "replay" => bail!("`bisect replay` is not supported"),
        "visualize" | "view" => {
            bail!("`bisect visualize` is not supported (it shells out to gitk/git log)")
        }
        "next" => bail!("`bisect next` is not supported"),
        "help" => bail!("`bisect help` is not supported"),
        // Anything else is a marking word — `bad`/`good`, `new`/`old`, or a
        // custom term a stock-git session recorded — or a genuine typo.
        other => {
            let ctx = Ctx::open()?;
            let is_marking = match read_terms(&ctx)? {
                Some(t) => other == t.bad || other == t.good,
                None => terms_for_first_marking(other).is_some(),
            };
            if is_marking {
                mark(other, rest)
            } else {
                unknown_command(other)
            }
        }
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

    fn in_progress(&self) -> bool {
        self.file("BISECT_START").exists()
    }

    /// The bad-side tip, if one has been marked.
    fn bad(&self) -> Result<Option<ObjectId>> {
        read_ref(&self.refs_dir().join("bad"))
    }

    /// Every marked good-side commit, sorted for deterministic iteration.
    fn goods(&self) -> Result<Vec<ObjectId>> {
        let dir = self.refs_dir();
        let Ok(entries) = std::fs::read_dir(&dir) else {
            return Ok(Vec::new());
        };
        let mut out = Vec::new();
        for entry in entries {
            let entry = entry?;
            let file_name = entry.file_name();
            let Some(name) = file_name.to_str() else {
                continue;
            };
            if !name.starts_with("good-") {
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

// --- subcommand: terms -------------------------------------------------------

fn terms_cmd(args: &[String]) -> Result<ExitCode> {
    let ctx = Ctx::open()?;
    let Some(terms) = read_terms(&ctx)? else {
        eprintln!("error: no terms defined");
        return Ok(ExitCode::from(1));
    };
    match args.len() {
        0 => {
            println!("Your current terms are '{}' for the old state", terms.good);
            println!("and '{}' for the new state.", terms.bad);
        }
        1 => match args[0].as_str() {
            "--term-good" | "--term-old" => println!("{}", terms.good),
            "--term-bad" | "--term-new" => println!("{}", terms.bad),
            other => bail!("unsupported flag {other:?} (ported: --term-good, --term-old, --term-bad, --term-new)"),
        },
        _ => bail!("`bisect terms` takes at most one flag"),
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
    for a in args {
        if a.starts_with('-') {
            bail!("unsupported flag {a:?} (`bisect reset` takes an optional <commit>)");
        }
    }
    if args.len() > 1 {
        bail!("`bisect reset` takes at most one <commit>");
    }

    let ctx = Ctx::open()?;
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

    let branch_ref = format!("refs/heads/{target}");
    let target_is_branch = ctx.repo.try_find_reference(branch_ref.as_str())?.is_some();

    if !was_detached && target_is_branch && old_branch.as_deref() == Some(target) {
        eprintln!("Already on '{target}'");
        return Ok(());
    }

    super::checkout::checkout(&["-q".to_string(), target.to_string()])?;

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
    let mut pathspecs: Vec<String> = Vec::new();

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
            pathspecs = args[i + 1..].to_vec();
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
            match resolve(&ctx.repo, arg) {
                Ok(id) => resolved.push(id),
                Err(_) if has_double_dash => {
                    eprintln!("fatal: '{arg}' does not appear to be a valid revision");
                    return Ok(ExitCode::from(128));
                }
                // An unresolvable token with no `--` present starts the pathspec.
                Err(_) => {
                    pathspecs = args[i..].to_vec();
                    break;
                }
            }
        }
        i += 1;
    }

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
    let bisect_names = if pathspecs.is_empty() {
        String::new()
    } else {
        pathspecs
            .iter()
            .map(|p| sq_quote(p))
            .collect::<Vec<_>>()
            .join(" ")
    };
    std::fs::write(ctx.file("BISECT_NAMES"), format!("{bisect_names}\n"))?;
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
            (&terms.bad, ctx.refs_dir().join("bad"))
        } else {
            (
                &terms.good,
                ctx.refs_dir().join(format!("good-{}", id.to_hex())),
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

/// The label `BISECT_START` records: the branch name, or the full oid when HEAD
/// is detached.
fn head_label(repo: &gix::Repository) -> Result<String> {
    let head = repo.head()?;
    if head.is_unborn() {
        bail!("cannot bisect: HEAD does not point at a commit yet");
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
    if !ctx.in_progress() {
        eprintln!("You need to start by \"git bisect start\"");
        return Ok(ExitCode::from(1));
    }

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

    for a in args {
        if a.starts_with('-') {
            bail!("unsupported flag {a:?} (ported: `bisect {word} [<rev>...]`)");
        }
    }
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
    let bad = ctx.bad()?;
    let goods = ctx.goods()?;
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
            Side::Bad => (&terms.bad, ctx.refs_dir().join("bad")),
            Side::Good => (
                &terms.good,
                ctx.refs_dir().join(format!("good-{}", id.to_hex())),
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
    let bad = ctx.bad()?;
    let goods = ctx.goods()?;

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
    check_good_are_ancestors_of_bad(ctx, bad, &goods, terms)?;

    let chain = candidate_chain(ctx, bad, &goods)?;
    let n = chain.len();
    let reaches = choose_weight(n);
    // `chain` is newest-first; a weight of `w` is the w-th commit from the old end.
    let best = chain[n - reaches];

    if best == bad {
        return report_first_bad(ctx, bad, terms);
    }

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
        super::checkout::checkout(&["-q".to_string(), hex.clone()])?;
    }
    println!("[{hex}] {}", subject(&ctx.repo, best)?);
    Ok(ExitCode::SUCCESS)
}

/// git refuses to bisect a range whose good ends are not ancestors of the bad
/// one without first testing the merge base; that path is not ported.
fn check_good_are_ancestors_of_bad(
    ctx: &Ctx,
    bad: ObjectId,
    goods: &[ObjectId],
    terms: &Terms,
) -> Result<()> {
    if ctx.file("BISECT_ANCESTORS_OK").exists() {
        return Ok(());
    }
    for good in goods {
        let base = ctx.repo.merge_base(bad, *good)?.detach();
        if base != *good {
            bail!(
                "the '{}' revision {} is not an ancestor of the '{}' revision {}; testing a merge \
                 base first is not supported",
                terms.good,
                good.to_hex(),
                terms.bad,
                bad.to_hex()
            );
        }
    }
    std::fs::write(ctx.file("BISECT_ANCESTORS_OK"), "")?;
    Ok(())
}

/// The commits still under suspicion — reachable from `bad`, not from any good —
/// ordered newest first, `chain[0] == bad`.
///
/// Only linear ranges are accepted: git's weight propagation across merges is not
/// reproduced here, and guessing it would pick a different midpoint.
fn candidate_chain(ctx: &Ctx, bad: ObjectId, goods: &[ObjectId]) -> Result<Vec<ObjectId>> {
    let mut set = std::collections::HashSet::new();
    for info in ctx
        .repo
        .rev_walk(Some(bad))
        .with_hidden(goods.to_vec())
        .all()?
    {
        set.insert(info?.id);
    }
    if set.is_empty() {
        bail!("no testable commit found between the marked revisions");
    }

    let mut chain = vec![bad];
    let mut cur = bad;
    loop {
        let commit = ctx.repo.find_object(cur)?.try_into_commit()?;
        let parents: Vec<ObjectId> = commit.parent_ids().map(|p| p.detach()).collect();
        if parents.len() > 1 {
            bail!(
                "merge commit {} is inside the bisect range; only linear ranges are supported",
                cur.to_hex()
            );
        }
        match parents.first() {
            Some(p) if set.contains(p) => {
                chain.push(*p);
                cur = *p;
            }
            _ => break,
        }
    }
    if chain.len() != set.len() {
        bail!(
            "the bisect range is not linear ({} candidates reachable, {} on the first-parent \
             chain); only linear ranges are supported",
            set.len(),
            chain.len()
        );
    }
    Ok(chain)
}

/// The weight (number of candidates reachable from it, itself included) of the
/// commit git's `find_bisection()` picks out of `n` linear candidates.
///
/// git assigns weight 1 to the oldest candidate up front, then walks the rest in
/// ancestor order and returns the first one that is "halfway" —
/// `|2 * weight - n| <= 1` — before ever running the best-distance scan. With no
/// halfway commit (`n <= 2`) the scan runs and picks the oldest candidate.
fn choose_weight(n: usize) -> usize {
    if n <= 2 {
        return 1;
    }
    for w in 2..=n {
        let d = 2 * w as i64 - n as i64;
        if (-1..=1).contains(&d) {
            return w;
        }
    }
    1
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
    if parents.len() > 1 {
        bail!("the first bad commit is a merge; combined diffs (--cc) are not supported");
    }
    let Some(parent) = parents.first().copied() else {
        return Ok(Vec::new());
    };

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

/// C-style path quoting matching git's default `core.quotePath=true`.
fn quote_path(path: impl AsRef<[u8]>) -> String {
    let bytes = path.as_ref();
    let needs = bytes
        .iter()
        .any(|&b| b < 0x20 || b == 0x7f || b == b'"' || b == b'\\' || b >= 0x80);
    if !needs {
        return String::from_utf8_lossy(bytes).into_owned();
    }
    let mut out = String::from("\"");
    for &b in bytes {
        match b {
            b'"' => out.push_str("\\\""),
            b'\\' => out.push_str("\\\\"),
            0x07 => out.push_str("\\a"),
            0x08 => out.push_str("\\b"),
            0x09 => out.push_str("\\t"),
            0x0a => out.push_str("\\n"),
            0x0b => out.push_str("\\v"),
            0x0c => out.push_str("\\f"),
            0x0d => out.push_str("\\r"),
            b if b < 0x20 || b == 0x7f || b >= 0x80 => out.push_str(&format!("\\{b:03o}")),
            b => out.push(b as char),
        }
    }
    out.push('"');
    out
}

/// Render the `--stat` block plus the `--summary` lines, reproducing git's
/// `show_stats()` column arithmetic for the default 80-column, non-tty width.
fn render_stat(files: &[StatEntry]) -> String {
    let mut max_len = 0usize;
    let mut max_change = 0u32;
    let mut any_binary = false;
    for f in files {
        max_len = max_len.max(f.name.len());
        if f.binary.is_some() {
            any_binary = true;
            continue;
        }
        max_change = max_change.max(f.added + f.deleted);
    }

    let mut number_width = decimal_width(max_change);
    if any_binary {
        number_width = number_width.max(3);
    }

    // Give the graph at least 6 columns, then spend what is left on the name.
    let width: isize = 80;
    let mut name_width = max_len as isize;
    let mut graph_width = max_change as isize;
    let number_width_i = number_width as isize;
    if name_width + number_width_i + 6 + graph_width > width {
        let cap = width * 3 / 8 - number_width_i - 6;
        if graph_width > cap {
            graph_width = cap.max(6);
        }
        if name_width > width - number_width_i - 6 - graph_width {
            name_width = width - number_width_i - 6 - graph_width;
        } else {
            graph_width = width - number_width_i - 6 - name_width;
        }
    }
    let name_width = name_width.max(0) as usize;
    let graph_width = graph_width.max(0) as usize;

    let mut out = String::new();
    let (mut total_added, mut total_deleted) = (0u64, 0u64);

    for f in files {
        let name = print_name(&f.name, name_width);
        if let Some((old, new)) = f.binary {
            out.push_str(&format!(
                " {name:<name_width$} |{:>w$}",
                "Bin",
                w = number_width + 1
            ));
            if old != 0 || new != 0 {
                out.push_str(&format!(" {old} -> {new} bytes"));
            }
            out.push('\n');
            continue;
        }

        total_added += u64::from(f.added);
        total_deleted += u64::from(f.deleted);
        let total = f.added + f.deleted;
        let (mut add, mut del) = (f.added as usize, f.deleted as usize);
        if max_change > 0 && graph_width <= max_change as usize {
            let mut t = scale_linear(add + del, graph_width, max_change as usize);
            if t < 2 && add > 0 && del > 0 {
                t = 2;
            }
            if add < del {
                add = scale_linear(add, graph_width, max_change as usize);
                del = t - add;
            } else {
                del = scale_linear(del, graph_width, max_change as usize);
                add = t - del;
            }
        }
        out.push_str(&format!(
            " {name:<name_width$} | {total:>number_width$}{}{}{}\n",
            if total != 0 { " " } else { "" },
            "+".repeat(add),
            "-".repeat(del),
        ));
    }

    // git's print_stat_summary: the zero side is still named when both are zero.
    let n = files.len();
    out.push_str(&format!(
        " {n} file{} changed",
        if n == 1 { "" } else { "s" }
    ));
    if total_added != 0 || total_deleted == 0 {
        out.push_str(&format!(
            ", {total_added} insertion{}(+)",
            if total_added == 1 { "" } else { "s" }
        ));
    }
    if total_deleted != 0 || total_added == 0 {
        out.push_str(&format!(
            ", {total_deleted} deletion{}(-)",
            if total_deleted == 1 { "" } else { "s" }
        ));
    }
    out.push('\n');

    for f in files {
        if let Some(line) = &f.summary {
            out.push_str(&format!(" {line}\n"));
        }
    }
    out
}

/// git's `fill_print_name` truncation: keep the tail, cut back to a `/` boundary,
/// and mark it with a leading `...`. Names are ASCII here (see `quote_path`).
fn print_name(name: &str, name_width: usize) -> String {
    if name.len() <= name_width || name_width < 4 {
        return name.to_owned();
    }
    let tail = &name[name.len() - (name_width - 3)..];
    let tail = match tail.find('/') {
        Some(i) => &tail[i..],
        None => tail,
    };
    format!("...{tail}")
}

/// git's `scale_linear`: any non-zero change keeps at least one column.
fn scale_linear(it: usize, width: usize, max_change: usize) -> usize {
    if it == 0 || width == 0 || max_change == 0 {
        return 0;
    }
    1 + (it * (width - 1)) / max_change
}

fn decimal_width(mut n: u32) -> usize {
    let mut w = 1;
    while n >= 10 {
        n /= 10;
        w += 1;
    }
    w
}
