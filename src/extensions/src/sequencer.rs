//! `.git/sequencer` — the on-disk state a multi-commit `cherry-pick`/`revert`
//! leaves behind so `--continue`, `--abort`, `--skip`, `git status` and a later
//! `git commit` can pick the sequence back up.
//!
//! This is the non-rebase half of git's `sequencer.c`. `rebase -i` shares the
//! same machinery but keeps its state under `.git/rebase-merge` instead
//! (`get_dir()` / `get_todo_path()` switch on `is_rebase_i`), and that directory
//! is owned by [`crate::porcelain::rebase`]; nothing here touches it.
//!
//! ## Single pick versus a sequence
//!
//! `sequencer_pick_revisions()` splits on the command line, not on the number of
//! commits it ends up replaying:
//!
//! ```text
//! if (opts->revs->cmdline.nr == 1 && … no_walk && !flags)
//!         res = single_pick(r, cmit, opts);   /* no sequencer directory at all */
//! ```
//!
//! One plain `<commit>` operand takes `single_pick`, which writes only
//! `CHERRY_PICK_HEAD`/`REVERT_HEAD` — git's own comment says this is what makes
//! it "possible to cherry-pick in the middle of a cherry-pick sequence".
//! Anything else — two operands, a range, a `^exclusion` — creates the directory
//! and runs `pick_commits()`.
//!
//! ## What is written, and when
//!
//! Starting a sequence, in `sequencer_pick_revisions()`'s order:
//!
//! | file | written by | contents |
//! |------|-----------|----------|
//! | `sequencer/` | `create_seq_dir()` | the directory itself; refuses if one is live |
//! | `sequencer/head` | `save_head()` | pre-sequence `HEAD`, hex + `\n` |
//! | `sequencer/opts` | `save_opts()` | config file of the non-default options; omitted entirely when there are none |
//! | `sequencer/abort-safety` | `update_abort_safety_file()` | current `HEAD`, hex + `\n` |
//! | `sequencer/todo` | `save_todo()`, per item | the instructions **from the current one onward** |
//!
//! `save_todo()` runs at the top of every `pick_commits()` iteration and
//! `update_abort_safety_file()` at the bottom of every `do_pick_commit()`
//! (its `leave:` label, so a *failed* pick updates it too). A sequence that runs
//! to the end finishes with `sequencer_remove_state()`, which removes the whole
//! directory — which is why a clean multi-commit pick leaves no trace of it.
//!
//! ## Why `abort-safety` is separate from `head`
//!
//! `head` is where `--abort` rewinds *to*; `abort-safety` is what `--abort`
//! checks *against*. If the user committed by hand mid-sequence, `HEAD` no longer
//! matches `abort-safety` and [`rollback_is_safe`] fails, so git warns and
//! declines to rewind rather than throwing that commit away.

use std::path::{Path, PathBuf};

use anyhow::Result;
use gix::hash::ObjectId;
use gix::revision::walk::Sorting;
use gix::traverse::commit::simple::CommitTimeOrder;

/// Which of the two sequencer commands a todo list belongs to.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Action {
    Pick,
    Revert,
}

impl Action {
    /// The `todo_command_info[]` string this action writes into `sequencer/todo`.
    pub fn todo_word(self) -> &'static str {
        match self {
            Action::Pick => "pick",
            Action::Revert => "revert",
        }
    }

    /// `action_name()` — the word git puts in its own diagnostics.
    pub fn name(self) -> &'static str {
        match self {
            Action::Pick => "cherry-pick",
            Action::Revert => "revert",
        }
    }
}

/// One `sequencer/todo` instruction: `<command> <abbrev> <subject>`.
///
/// `walk_revs_populate_todo()` builds the line from `short_commit_name()` and
/// the commit subject, and keeps the full commit alongside it. The abbreviation
/// is what lands on disk; `oid` is carried so a live sequence never has to
/// resolve its own abbreviations back.
#[derive(Clone, Debug)]
pub struct TodoItem {
    pub action: Action,
    pub oid: ObjectId,
    pub abbrev: String,
    pub subject: String,
}

impl TodoItem {
    /// The line `save_todo()` writes, without its newline.
    fn line(&self) -> String {
        format!("{} {} {}", self.action.todo_word(), self.abbrev, self.subject)
    }
}

/// What a `cherry-pick`/`revert` command line selects, or the refusal it earns.
///
/// The three failure shapes are kept apart because git prints them from three
/// different places, and the order it reports them in is observable: a bad
/// revision (`setup_revisions`, 128) outranks an unknown option
/// (`run_sequencer`, 129), which outranks a non-commit operand
/// (`sequencer_pick_revisions`, 128).
pub enum Revs {
    /// The commits to replay, in replay order, and whether git's sequencer runs.
    ///
    /// `sequencer` is `sequencer_pick_revisions()`'s split: `false` selects
    /// `single_pick()`, which writes no `.git/sequencer` at all.
    Picks {
        commits: Vec<ObjectId>,
        sequencer: bool,
    },
    /// `setup_revisions()` could not resolve this operand, which it reports as
    /// `fatal: bad revision '<spec>'` with status 128 the moment it reaches it.
    BadRevision(String),
    /// A pending object that is not commit-ish, named as it was written on the
    /// command line. `sequencer_pick_revisions()` reports
    /// `error: <name>: can't cherry-pick a <kind>` — that wording is fixed, so a
    /// `revert` says "cherry-pick" too and only the `fatal: <action> failed`
    /// tail differs — with status 128.
    NotACommit {
        name: String,
        kind: gix::object::Kind,
    },
    /// A dash operand git's option table did not know. `setup_revisions()` keeps
    /// it and `run_sequencer()` reports it as the usage block with status 129
    /// once the whole list is scanned, so a bad revision anywhere outranks it.
    UnknownOption,
}

/// git's `setup_revisions()` + `prepare_revs()` as `builtin/revert.c` drives
/// them: turn the operand list into the commits to replay, in replay order.
///
/// `run_sequencer()` hands the operand list to `setup_revisions()` having set
/// `revs->no_walk = 1` and `revs->unsorted_input = 1`, so a list of plain
/// `<commit>` operands is taken as typed, in order, with no walk at all. What
/// turns the walk on is an *uninteresting* pending object, because
/// `add_pending_object_with_path()` clears `no_walk` for one — and `^<commit>`,
/// the left side of `<a>..<b>` and the merge bases of `<a>...<b>` all queue one.
/// So the spelling of the operands, not their number, decides between a list and
/// a walk, and a single `<a>..<b>` walks while `<a> <b>` does not.
///
/// `prepare_revs()` then reverses a *pick* that walks:
///
/// ```text
/// /*
///  * picking (but not reverting) ranges (but not individual revisions)
///  * should be done in reverse
///  */
/// if (opts->action == REPLAY_PICK && !opts->revs->no_walk)
///         opts->revs->reverse ^= 1;
/// ```
///
/// which is why `git cherry-pick <a>..<b>` replays oldest first — the only order
/// in which the replayed commits apply to each other — while `git revert
/// <a>..<b>` backs out newest first, and neither reverses a no-walk list.
///
/// A leading `-` operand is `run_sequencer()`'s `@{-1}` rewrite. It is applied to
/// `argv[1]` alone, so a `-` anywhere else stays an unrecognized option.
///
/// The ranges are split here and everything else goes through `gix`'s rev-spec
/// parser, which covers `^<commit>` and the `<a>^!`/`<a>^@` parent spellings.
/// Two `handle_revision_arg` spellings stay unserved and are reported as
/// refusals rather than approximated: `<a>^-[<n>]` (`gix` does not parse it, so
/// it is a bad revision here while stock picks `<a>^..<a>`), and the
/// `--not`/`--all` pseudo-revisions, which reach this port as unknown options.
pub fn prepare_revs<S: AsRef<str>>(
    repo: &gix::Repository,
    specs: &[S],
    action: Action,
) -> Result<Revs> {
    let mut include: Vec<ObjectId> = Vec::new();
    let mut exclude: Vec<ObjectId> = Vec::new();
    let mut walked = false;
    let mut unknown_option = false;
    // `single_pick()`'s test is on the command line, not on the selection:
    // `cmdline.nr == 1 && whence == REV_CMD_REV && no_walk && !flags`. Only a
    // plain `<commit>` operand carries `REV_CMD_REV`, so a `^`, a range or a
    // parent spelling disqualifies it however few commits it ends up naming.
    let mut plain_operands = 0usize;

    for (n, spec) in specs.iter().enumerate() {
        let spec = spec.as_ref();
        let spec = if n == 0 && spec == "-" { "@{-1}" } else { spec };
        // A dash-prefixed operand is not a revision: git keeps it as an
        // unrecognized option and only reports it once the whole list is
        // scanned, so a later bad revision still wins.
        if spec.starts_with('-') {
            unknown_option = true;
            continue;
        }

        // `<a>...<b>`: both tips, with their merge bases excluded. Checked ahead
        // of `..` because the two spellings share a prefix.
        if let Some((lhs, rhs)) = spec.split_once("...") {
            let (left, right) = match (peel_side(repo, lhs), peel_side(repo, rhs)) {
                (Side::Commit(l), Side::Commit(r)) => (l, r),
                (Side::NotACommit(name, kind), _) | (_, Side::NotACommit(name, kind)) => {
                    return Ok(Revs::NotACommit { name, kind })
                }
                _ => return Ok(Revs::BadRevision(spec.to_owned())),
            };
            for base in repo.merge_bases_many(left, &[right])? {
                exclude.push(base.detach());
                walked = true;
            }
            include.push(left);
            include.push(right);
            continue;
        }

        // `<a>..<b>`: everything `<b>` reaches that `<a>` does not.
        if let Some((lhs, rhs)) = spec.split_once("..") {
            match (peel_side(repo, lhs), peel_side(repo, rhs)) {
                (Side::Commit(from), Side::Commit(to)) => {
                    exclude.push(from);
                    include.push(to);
                    walked = true;
                }
                (Side::NotACommit(name, kind), _) | (_, Side::NotACommit(name, kind)) => {
                    return Ok(Revs::NotACommit { name, kind })
                }
                _ => return Ok(Revs::BadRevision(spec.to_owned())),
            }
            continue;
        }

        let Ok(parsed) = repo.rev_parse(spec) else {
            return Ok(Revs::BadRevision(spec.to_owned()));
        };
        match *parsed {
            gix::revision::plumbing::Spec::Include(id) => match peel_id(repo, id) {
                Side::Commit(id) => {
                    include.push(id);
                    plain_operands += 1;
                }
                Side::NotACommit(_, kind) => {
                    return Ok(Revs::NotACommit { name: spec.to_owned(), kind })
                }
                Side::Unresolved => return Ok(Revs::BadRevision(spec.to_owned())),
            },
            gix::revision::plumbing::Spec::Exclude(id) => match peel_id(repo, id) {
                Side::Commit(id) => {
                    exclude.push(id);
                    walked = true;
                }
                Side::NotACommit(_, kind) => {
                    return Ok(Revs::NotACommit { name: spec.to_owned(), kind })
                }
                Side::Unresolved => return Ok(Revs::BadRevision(spec.to_owned())),
            },
            // `<a>^!`: `<a>` itself, with everything its parents reach excluded.
            gix::revision::plumbing::Spec::ExcludeParents(id) => {
                let Side::Commit(id) = peel_id(repo, id) else {
                    return Ok(Revs::BadRevision(spec.to_owned()));
                };
                include.push(id);
                for parent in repo.find_commit(id)?.parent_ids() {
                    exclude.push(parent.detach());
                    walked = true;
                }
            }
            // `<a>^@`: every parent of `<a>`, but not `<a>`.
            gix::revision::plumbing::Spec::IncludeOnlyParents(id) => {
                let Side::Commit(id) = peel_id(repo, id) else {
                    return Ok(Revs::BadRevision(spec.to_owned()));
                };
                include.extend(repo.find_commit(id)?.parent_ids().map(|p| p.detach()));
            }
            // The range spellings are handled above; `gix` can only produce these
            // for an operand that reached neither branch, which it cannot.
            gix::revision::plumbing::Spec::Range { .. } | gix::revision::plumbing::Spec::Merge { .. } => {
                return Ok(Revs::BadRevision(spec.to_owned()))
            }
        }
    }

    if unknown_option {
        return Ok(Revs::UnknownOption);
    }

    if !walked {
        // `add_pending_object()` sets `SEEN`, so an operand named twice is
        // replayed once — but the operand count above was read before this, as
        // git reads `cmdline.nr`.
        let sequencer = !(specs.len() == 1 && plain_operands == 1);
        let mut seen = std::collections::HashSet::new();
        include.retain(|id| seen.insert(*id));
        return Ok(Revs::Picks { commits: include, sequencer });
    }

    // `revs->unsorted_input` governs the *no-walk* list only; a real walk comes
    // out of `limit_list()` in commit-date order, newest first, which is what
    // decides how two commits with no ancestry between them interleave. Leaving
    // `gix`'s graph-order default in place put `git cherry-pick <a>..<b> <side>`
    // in an order stock never produces.
    let mut commits = Vec::new();
    let walk = repo
        .rev_walk(include)
        .with_hidden(exclude)
        .sorting(Sorting::ByCommitTime(CommitTimeOrder::NewestFirst));
    for info in walk.all()? {
        commits.push(info?.id);
    }
    if action == Action::Pick {
        commits.reverse();
    }
    // A walked selection always runs through the sequencer, even at length 1.
    Ok(Revs::Picks { commits, sequencer: true })
}

/// What one revision word resolved to.
enum Side {
    Commit(ObjectId),
    /// The object exists but nothing commit-ish is behind it; the name is kept
    /// because git's diagnostic quotes the operand as written.
    NotACommit(String, gix::object::Kind),
    Unresolved,
}

/// One side of a range: git defaults an omitted side to `HEAD`.
fn peel_side(repo: &gix::Repository, name: &str) -> Side {
    let name = if name.is_empty() { "HEAD" } else { name };
    let Ok(id) = repo.rev_parse_single(name) else {
        return Side::Unresolved;
    };
    match peel_id(repo, id.detach()) {
        Side::NotACommit(_, kind) => Side::NotACommit(name.to_owned(), kind),
        other => other,
    }
}

/// `lookup_commit_reference_gently()`: peel tags through to the commit, and
/// report the object's own type when there is none.
fn peel_id(repo: &gix::Repository, id: ObjectId) -> Side {
    let Ok(object) = repo.find_object(id) else {
        return Side::Unresolved;
    };
    let kind = object.kind;
    match object.peel_to_commit() {
        Ok(commit) => Side::Commit(commit.id),
        Err(_) => Side::NotACommit(id.to_string(), kind),
    }
}

/// `git_path_seq_dir()`.
pub fn dir(git_dir: &Path) -> PathBuf {
    git_dir.join("sequencer")
}

/// `git_path_todo_file()`.
pub fn todo_path(git_dir: &Path) -> PathBuf {
    dir(git_dir).join("todo")
}

/// `git_path_opts_file()`.
pub fn opts_path(git_dir: &Path) -> PathBuf {
    dir(git_dir).join("opts")
}

/// `git_path_head_file()` — the pre-sequence `HEAD` that `--abort` rewinds to.
pub fn head_path(git_dir: &Path) -> PathBuf {
    dir(git_dir).join("head")
}

/// `git_path_abort_safety_file()`.
pub fn abort_safety_path(git_dir: &Path) -> PathBuf {
    dir(git_dir).join("abort-safety")
}

/// `sequencer_get_last_command()`: which command the live todo list belongs to,
/// or `None` when there is no todo file or its first instruction is neither
/// `pick` nor `revert`.
///
/// git skips leading whitespace and then requires the command word to be
/// followed by a space or tab, so a bare `pick\n` does not count.
pub fn get_last_command(git_dir: &Path) -> Option<Action> {
    let buf = std::fs::read(todo_path(git_dir)).ok()?;
    let bol = buf
        .iter()
        .position(|b| !matches!(b, b' ' | b'\t' | b'\r' | b'\n'))
        .map(|i| &buf[i..])
        .unwrap_or(&[]);
    for action in [Action::Pick, Action::Revert] {
        let word = action.todo_word().as_bytes();
        if bol.starts_with(word) && matches!(bol.get(word.len()), Some(b' ' | b'\t')) {
            return Some(action);
        }
    }
    None
}

/// `have_finished_the_last_pick()`: the todo list is down to a single line, so
/// the pick that just concluded was its last. A missing todo file is `false` —
/// git treats "no sequence at all" as "nothing to finish".
pub fn have_finished_the_last_pick(git_dir: &Path) -> bool {
    let Ok(buf) = std::fs::read(todo_path(git_dir)) else {
        return false;
    };
    match buf.iter().position(|&b| b == b'\n') {
        None => true,
        Some(eol) => eol + 1 >= buf.len(),
    }
}

/// `create_seq_dir()`: refuse when a sequence is already live, otherwise create
/// the directory.
///
/// The refusal names whichever command owns the live todo list — not the one
/// being started — and its advice offers `--skip` only when a pick is actually
/// stopped (`CHERRY_PICK_HEAD`/`REVERT_HEAD` present). The caller turns the
/// error into `fatal: <action> failed` and status 128.
pub fn create(git_dir: &Path) -> Result<std::result::Result<(), ()>> {
    if let Some(live) = get_last_command(git_dir) {
        let advise_skip =
            git_dir.join("REVERT_HEAD").exists() || git_dir.join("CHERRY_PICK_HEAD").exists();
        eprintln!("error: {} is already in progress", live.name());
        crate::advice::Advice::SequencerInUse.advise_plain(&format!(
            "try \"git {} (--continue | {}--abort | --quit)\"",
            live.name(),
            if advise_skip { "--skip | " } else { "" }
        ));
        return Ok(Err(()));
    }
    std::fs::create_dir_all(dir(git_dir))?;
    Ok(Ok(()))
}

/// `save_head()`: the pre-sequence `HEAD`, hex plus a newline
/// (`write_message(..., append_eol = 1)`).
pub fn save_head(git_dir: &Path, head: ObjectId) -> Result<()> {
    std::fs::write(head_path(git_dir), format!("{head}\n"))?;
    Ok(())
}

/// `save_todo()`: the instructions from `current` onward.
///
/// The current item stays in the file — only `rebase -i` advances past it, and
/// that is the `is_rebase_i(opts) && !reschedule` branch this port never takes.
/// So a stopped sequence's `todo` always begins with the instruction that
/// stopped it, which is what `--continue` re-reads.
pub fn save_todo(git_dir: &Path, items: &[TodoItem], current: usize) -> Result<()> {
    let mut buf = String::new();
    for item in &items[current.min(items.len())..] {
        buf.push_str(&item.line());
        buf.push('\n');
    }
    std::fs::write(todo_path(git_dir), buf)?;
    Ok(())
}

/// `read_populate_todo()` → `todo_list_parse_insn_buffer()`, restricted to the
/// two instructions a cherry-pick/revert sequence can contain.
///
/// Each abbreviation is resolved against the repository, as git's parser does
/// while it builds the list; an instruction that no longer names a commit is the
/// same `invalid line` failure git reports.
pub fn read_todo(repo: &gix::Repository, git_dir: &Path) -> Result<Vec<TodoItem>> {
    let text = std::fs::read_to_string(todo_path(git_dir))?;
    let mut items = Vec::new();
    for (n, line) in text.lines().enumerate() {
        let trimmed = line.trim_start();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let (word, rest) = trimmed.split_once(|c: char| c == ' ' || c == '\t').unwrap_or((trimmed, ""));
        let action = match word {
            "pick" | "p" => Action::Pick,
            "revert" => Action::Revert,
            _ => anyhow::bail!("invalid line {}: {line}", n + 1),
        };
        let rest = rest.trim_start();
        let (abbrev, subject) = rest.split_once(' ').unwrap_or((rest, ""));
        let Ok(oid) = repo
            .rev_parse_single(abbrev)
            .map_err(anyhow::Error::from)
            .and_then(|id| Ok(id.object()?.peel_to_commit()?.id))
        else {
            anyhow::bail!("invalid line {}: {line}", n + 1);
        };
        items.push(TodoItem {
            action,
            oid,
            abbrev: abbrev.to_string(),
            subject: subject.to_string(),
        });
    }
    Ok(items)
}

/// `update_abort_safety_file()`: record the `HEAD` `--abort` will later verify
/// against, or an empty file when there is no `HEAD` to read.
///
/// git's own guard — "do nothing on a single-pick" — is the directory test, so
/// this is a no-op unless a sequence is live.
pub fn update_abort_safety_file(repo: &gix::Repository) -> Result<()> {
    let git_dir = repo.git_dir();
    if !dir(git_dir).exists() {
        return Ok(());
    }
    let body = match repo.head_id() {
        Ok(id) => format!("{}\n", id.detach()),
        Err(_) => "\n".to_string(),
    };
    std::fs::write(abort_safety_path(git_dir), body)?;
    Ok(())
}

/// `rollback_is_safe()`: `HEAD` still is what the last pick left behind, so
/// rewinding to `sequencer/head` cannot lose work the user committed by hand.
///
/// A missing `abort-safety` file compares against the null id, exactly as git's
/// `ENOENT` branch does — so it is only "safe" when `HEAD` is unreadable too.
pub fn rollback_is_safe(repo: &gix::Repository) -> bool {
    let expected = std::fs::read_to_string(abort_safety_path(repo.git_dir()))
        .ok()
        .and_then(|text| ObjectId::from_hex(text.trim().as_bytes()).ok());
    let actual = repo.head_id().ok().map(|id| id.detach());
    expected == actual
}

/// `sequencer_remove_state()`: the sequence is over, the directory goes.
pub fn remove_state(git_dir: &Path) {
    let _ = std::fs::remove_dir_all(dir(git_dir));
}

/// The option state `save_opts()` persists, in its declaration order.
///
/// Every field is "was it given", not "what did it resolve to": `save_opts`
/// writes a key only for a non-default value, which is why a plain
/// `git cherry-pick A B` leaves no `sequencer/opts` file at all.
#[derive(Default)]
pub struct SavedOpts<'a> {
    pub no_commit: bool,
    /// `opts->edit`, a tri-state: `None` is git's `-1` ("not given").
    pub edit: Option<bool>,
    pub allow_empty: bool,
    pub allow_empty_message: bool,
    pub drop_redundant_commits: bool,
    pub keep_redundant_commits: bool,
    pub signoff: bool,
    pub record_origin: bool,
    pub allow_ff: bool,
    /// `-m <n>`; `0` is git's "not given".
    pub mainline: u32,
    pub strategy: Option<&'a str>,
    pub gpg_sign: Option<&'a str>,
    pub xopts: &'a [&'a str],
    /// `opts->allow_rerere_auto`: `None` unspecified, else autoupdate on/off.
    pub allow_rerere_auto: Option<bool>,
    /// `opts->explicit_cleanup`: the resolved mode name, or `None`.
    pub default_msg_cleanup: Option<&'a str>,
}

/// `save_opts()`: write the non-default options as a config file.
///
/// Nothing is written when every option is at its default — git's
/// `repo_config_set_in_file_gently` is simply never called, so the file does not
/// come into existence and `read_populate_opts()` later skips it.
pub fn save_opts(git_dir: &Path, o: &SavedOpts<'_>) -> Result<()> {
    let mut body = String::new();
    let mut set = |key: &str, value: &str| {
        if body.is_empty() {
            body.push_str("[options]\n");
        }
        body.push('\t');
        body.push_str(key);
        body.push_str(" = ");
        body.push_str(&config_value(value));
        body.push('\n');
    };

    if o.no_commit {
        set("no-commit", "true");
    }
    if let Some(edit) = o.edit {
        set("edit", if edit { "true" } else { "false" });
    }
    if o.allow_empty {
        set("allow-empty", "true");
    }
    if o.allow_empty_message {
        set("allow-empty-message", "true");
    }
    if o.drop_redundant_commits {
        set("drop-redundant-commits", "true");
    }
    if o.keep_redundant_commits {
        set("keep-redundant-commits", "true");
    }
    if o.signoff {
        set("signoff", "true");
    }
    if o.record_origin {
        set("record-origin", "true");
    }
    if o.allow_ff {
        set("allow-ff", "true");
    }
    if o.mainline != 0 {
        set("mainline", &o.mainline.to_string());
    }
    if let Some(strategy) = o.strategy {
        set("strategy", strategy);
    }
    if let Some(key) = o.gpg_sign {
        set("gpg-sign", key);
    }
    for x in o.xopts {
        set("strategy-option", x);
    }
    if let Some(auto) = o.allow_rerere_auto {
        set("allow-rerere-auto", if auto { "true" } else { "false" });
    }
    if let Some(mode) = o.default_msg_cleanup {
        set("default-msg-cleanup", mode);
    }

    if body.is_empty() {
        return Ok(());
    }
    std::fs::write(opts_path(git_dir), body)?;
    Ok(())
}

/// git's `store_write_pair()` (config.c) value rendering: quote when a leading
/// or trailing space or a comment character would otherwise be lost, and escape
/// newline, tab, `"` and `\` inside.
fn config_value(value: &str) -> String {
    let quote = value.starts_with(' ')
        || value.ends_with(' ')
        || value.contains(';')
        || value.contains('#');
    let mut out = String::with_capacity(value.len() + 2);
    if quote {
        out.push('"');
    }
    for c in value.chars() {
        match c {
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '"' | '\\' => {
                out.push('\\');
                out.push(c);
            }
            _ => out.push(c),
        }
    }
    if quote {
        out.push('"');
    }
    out
}

/// The options a stopped sequence recorded, as `read_populate_opts()` reads them
/// back for `--continue`.
///
/// Only the keys [`save_opts`] can have written are recognised; anything else in
/// the file is ignored rather than fatal, matching `populate_opts_cb`'s
/// "unknown key" tolerance for the values this port does not model.
#[derive(Default, Debug)]
pub struct LoadedOpts {
    pub no_commit: bool,
    pub edit: Option<bool>,
    pub allow_empty: bool,
    pub allow_empty_message: bool,
    pub drop_redundant_commits: bool,
    pub keep_redundant_commits: bool,
    pub signoff: bool,
    pub record_origin: bool,
    pub allow_ff: bool,
    pub mainline: u32,
    pub strategy: Option<String>,
    pub gpg_sign: Option<String>,
    pub xopts: Vec<String>,
    pub allow_rerere_auto: Option<bool>,
    pub default_msg_cleanup: Option<String>,
}

/// `read_populate_opts()`: load `sequencer/opts`, or the defaults when the file
/// was never written.
pub fn read_opts(git_dir: &Path) -> LoadedOpts {
    let mut o = LoadedOpts::default();
    let Ok(text) = std::fs::read_to_string(opts_path(git_dir)) else {
        return o;
    };
    for line in text.lines() {
        let line = line.trim();
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim();
        let value = unquote_config_value(value.trim());
        let yes = value == "true";
        match key {
            "no-commit" => o.no_commit = yes,
            "edit" => o.edit = Some(yes),
            "allow-empty" => o.allow_empty = yes,
            "allow-empty-message" => o.allow_empty_message = yes,
            "drop-redundant-commits" => o.drop_redundant_commits = yes,
            "keep-redundant-commits" => o.keep_redundant_commits = yes,
            "signoff" => o.signoff = yes,
            "record-origin" => o.record_origin = yes,
            "allow-ff" => o.allow_ff = yes,
            "mainline" => o.mainline = value.parse().unwrap_or(0),
            "strategy" => o.strategy = Some(value),
            "gpg-sign" => o.gpg_sign = Some(value),
            "strategy-option" => o.xopts.push(value),
            "allow-rerere-auto" => o.allow_rerere_auto = Some(yes),
            "default-msg-cleanup" => o.default_msg_cleanup = Some(value),
            _ => {}
        }
    }
    o
}

/// Undo [`config_value`]'s quoting for the values this module writes.
fn unquote_config_value(raw: &str) -> String {
    let body = match raw.strip_prefix('"').and_then(|r| r.strip_suffix('"')) {
        Some(inner) => inner,
        None => raw,
    };
    let mut out = String::with_capacity(body.len());
    let mut chars = body.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some(other) => out.push(other),
            None => out.push('\\'),
        }
    }
    out
}

/// `sequencer_post_commit_cleanup()`: drop the pseudo-refs a stopped pick left
/// behind and, once the todo list is down to its final entry, the sequencer
/// directory with it.
///
/// `AUTO_MERGE` goes unconditionally; the directory only goes when a pick was
/// actually in progress *and* it was the last one, which is what lets
/// `git reset --merge` sit in the middle of `--skip` without destroying the
/// remaining todo.
pub fn post_commit_cleanup(repo: &gix::Repository) -> Result<()> {
    let git_dir = repo.git_dir();
    let mut need_cleanup = delete_state_ref(repo, "CHERRY_PICK_HEAD");
    need_cleanup |= delete_state_ref(repo, "REVERT_HEAD");
    delete_state_ref(repo, "AUTO_MERGE");
    if !need_cleanup || !have_finished_the_last_pick(git_dir) {
        return Ok(());
    }
    remove_state(git_dir);
    Ok(())
}

/// `refs_delete_ref(…, REF_NO_DEREF)` for a root-level pseudo-ref, reporting
/// whether it had existed.
///
/// These are normally a loose file holding a raw object id — that is what the
/// files ref backend produces for a name outside `refs/`, and what this port
/// writes — so the file is removed first and the ref store consulted only after.
pub fn delete_state_ref(repo: &gix::Repository, name: &str) -> bool {
    let mut removed = std::fs::remove_file(repo.git_dir().join(name)).is_ok();
    if let Ok(reference) = repo.find_reference(name) {
        let current = reference.target().into_owned();
        removed |= repo
            .edit_reference(gix::refs::transaction::RefEdit {
                change: gix::refs::transaction::Change::Delete {
                    expected: gix::refs::transaction::PreviousValue::MustExistAndMatch(current),
                    log: gix::refs::transaction::RefLog::AndReference,
                    message: Default::default(),
                },
                name: reference.name().to_owned(),
                deref: false,
            })
            .is_ok();
    }
    removed
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmpdir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("zvcs-seq-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("sequencer")).expect("create fixture dir");
        dir
    }

    fn item(action: Action, abbrev: &str, subject: &str) -> TodoItem {
        TodoItem {
            action,
            oid: ObjectId::empty_tree(gix::hash::Kind::Sha1),
            abbrev: abbrev.to_string(),
            subject: subject.to_string(),
        }
    }

    /// `save_todo()` keeps the *current* instruction, so a stopped sequence's
    /// todo starts with the pick that stopped it. Dropping it would make
    /// `--continue` skip a commit outright.
    #[test]
    fn save_todo_keeps_the_current_instruction() {
        let git_dir = tmpdir("todo");
        let items = vec![
            item(Action::Pick, "aaaaaaaaaa", "one"),
            item(Action::Pick, "bbbbbbbbbb", "two"),
            item(Action::Pick, "cccccccccc", "three"),
        ];

        save_todo(&git_dir, &items, 1).expect("write todo");
        assert_eq!(
            std::fs::read_to_string(todo_path(&git_dir)).expect("read todo"),
            "pick bbbbbbbbbb two\npick cccccccccc three\n"
        );
        assert_eq!(get_last_command(&git_dir), Some(Action::Pick));
        assert!(!have_finished_the_last_pick(&git_dir));

        // The last instruction on its own is what `have_finished_the_last_pick`
        // reports true for — the signal `git reset` uses to decide whether the
        // sequencer directory may go.
        save_todo(&git_dir, &items, 2).expect("write todo");
        assert!(have_finished_the_last_pick(&git_dir));

        let _ = std::fs::remove_dir_all(&git_dir);
    }

    /// A `revert` todo reports its own action, and a first line that is neither
    /// instruction reports none — `create_seq_dir()` only refuses for the two it
    /// recognises.
    #[test]
    fn last_command_reads_the_first_instruction() {
        let git_dir = tmpdir("last");
        save_todo(&git_dir, &[item(Action::Revert, "aaaaaaaaaa", "x")], 0).expect("write todo");
        assert_eq!(get_last_command(&git_dir), Some(Action::Revert));

        std::fs::write(todo_path(&git_dir), "pick\n").expect("write todo");
        assert_eq!(get_last_command(&git_dir), None, "a bare `pick` needs its space");

        std::fs::write(todo_path(&git_dir), "# nothing\n").expect("write todo");
        assert_eq!(get_last_command(&git_dir), None);

        let _ = std::fs::remove_dir_all(&git_dir);
    }

    /// `save_opts()` writes only what was given, and writes nothing at all for a
    /// plain sequence — which is why stock leaves no `sequencer/opts` for
    /// `git cherry-pick A B`.
    #[test]
    fn opts_round_trip() {
        let git_dir = tmpdir("opts");
        save_opts(&git_dir, &SavedOpts::default()).expect("write opts");
        assert!(!opts_path(&git_dir).exists(), "defaults write no file");

        let xopts = ["diff-algorithm=patience", "ours"];
        let saved = SavedOpts {
            no_commit: true,
            edit: Some(false),
            signoff: true,
            record_origin: true,
            mainline: 2,
            strategy: Some("recursive"),
            xopts: &xopts,
            default_msg_cleanup: Some("whitespace"),
            ..Default::default()
        };
        save_opts(&git_dir, &saved).expect("write opts");
        assert_eq!(
            std::fs::read_to_string(opts_path(&git_dir)).expect("read opts"),
            "[options]\n\tno-commit = true\n\tedit = false\n\tsignoff = true\n\
             \trecord-origin = true\n\tmainline = 2\n\tstrategy = recursive\n\
             \tstrategy-option = diff-algorithm=patience\n\tstrategy-option = ours\n\
             \tdefault-msg-cleanup = whitespace\n"
        );

        let loaded = read_opts(&git_dir);
        assert!(loaded.no_commit);
        assert_eq!(loaded.edit, Some(false));
        assert!(loaded.signoff);
        assert!(loaded.record_origin);
        assert_eq!(loaded.mainline, 2);
        assert_eq!(loaded.strategy.as_deref(), Some("recursive"));
        assert_eq!(loaded.xopts, vec!["diff-algorithm=patience", "ours"]);
        assert_eq!(loaded.default_msg_cleanup.as_deref(), Some("whitespace"));

        let _ = std::fs::remove_dir_all(&git_dir);
    }

    /// A value carrying a comment character has to survive the round trip, or
    /// `-X` options would silently change meaning across a `--continue`.
    #[test]
    fn config_values_quote_comment_characters() {
        assert_eq!(config_value("recursive"), "recursive");
        assert_eq!(config_value("a#b"), "\"a#b\"");
        assert_eq!(config_value(" pad "), "\" pad \"");
        assert_eq!(config_value("q\"uote"), "q\\\"uote");
        assert_eq!(unquote_config_value("\"a#b\""), "a#b");
        assert_eq!(unquote_config_value("q\\\"uote"), "q\"uote");
    }
}
