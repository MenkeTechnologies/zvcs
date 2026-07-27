//! git's `advice.*` hint gating (git's `advice.c`).
//!
//! Every optional hint git prints (the `hint:` lines that suggest a next step)
//! is controlled by an `advice.<slot>` boolean that defaults to true; setting it
//! false suppresses just that hint. git reads these via `advice_enabled()`; this
//! is the shared gate so every zvcs hint site honors the same switch identically
//! rather than advertising `advice.<slot>` while ignoring it.
//!
//! Three behaviors of `advice.c` are reproduced here and must stay together,
//! because each one is observable:
//!
//! * `GIT_ADVICE=0` in the environment squelches *every* hint regardless of
//!   configuration (`git_env_bool(GIT_ADVICE_ENVIRONMENT, 1)` in
//!   `advice_enabled()`), as `git help config` documents under `advice.*`.
//! * `advice.pushUpdateRejected` is additionally gated on the older
//!   `advice.pushNonFastForward` name — `advice_enabled()` special-cases the
//!   pair so either one set to false disables the whole push-rejection family.
//! * `advise_if_enabled()` appends `Disable this message with "git config set
//!   advice.<slot> false"` **only when the slot is unconfigured**. Setting the
//!   slot to true keeps the hint and drops that trailer.

use gix::Repository;

/// One `advice.*` slot. Only slots this codebase actually reaches are listed:
/// the enum is the set of hints zvcs can print, so every variant maps to a live
/// hint site, and [`Advice::key`] is the literal the config lookup uses.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Advice {
    /// Directions in the long `git status` output, the `git commit` message
    /// template and the branch-switch help — `wt_status.hints`.
    StatusHints,
    /// `git tag -a <new> <existing-tag>` created a tag pointing at a tag.
    NestedTag,
    /// A command stopped because the index still carries unmerged entries.
    ResolveConflict,
    /// A sequencer operation refused to run over a dirty index
    /// (`error_dirty_index`).
    CommitBeforeMerge,
    /// `git rm` refused because the work tree or index would lose changes.
    RmHints,
    /// Umbrella slot for every push rejection; false disables the whole family.
    PushUpdateRejected,
    /// Historical alias `advice_enabled()` ANDs into `PushUpdateRejected`.
    PushNonFastForward,
    /// The rejected ref is the branch that is currently checked out.
    PushNonFFCurrent,
    /// The rejected ref was pushed by refspec/matching, not as the current branch.
    PushNonFFMatching,
    /// The remote ref points at an object this repository does not have.
    PushFetchFirst,
    /// Git is blocked on an editor the user may not have noticed.
    WaitingForEditor,
    /// A ref name was rejected by `check-ref-format`'s rules.
    RefSyntax,
    /// `--set-upstream-to` named an upstream that does not exist.
    SetUpstreamFailure,
    /// `git branch -d` refused a branch that is not fully merged.
    ForceDeleteBranch,
    /// A DWIM branch name matched more than one remote.
    CheckoutAmbiguousRemoteBranchName,
    /// A hookdir script was skipped because it is not executable.
    IgnoredHook,
    /// `die_expecting_a_branch()`: `git switch` was given something that is not
    /// a branch, and `--detach` would have done what the user meant.
    SuggestDetachingHead,
    /// A full-length hex object name is also the name of a ref, which is almost
    /// always a ref created by mistake.
    ObjectNameWarning,
    /// A pathspec selected only paths outside the sparse-checkout definition, so
    /// the command left the index alone (`advise_on_updating_sparse_paths`).
    UpdateSparsePath,
}

impl Advice {
    /// The full configuration key this slot reads, e.g. `advice.statusHints`.
    /// This is the string handed to the config lookup — the literal *is* the
    /// read, not a label for it.
    pub const fn key(self) -> &'static str {
        match self {
            Advice::StatusHints => "advice.statusHints",
            Advice::NestedTag => "advice.nestedTag",
            Advice::ResolveConflict => "advice.resolveConflict",
            Advice::CommitBeforeMerge => "advice.commitBeforeMerge",
            Advice::RmHints => "advice.rmHints",
            Advice::PushUpdateRejected => "advice.pushUpdateRejected",
            Advice::PushNonFastForward => "advice.pushNonFastForward",
            Advice::PushNonFFCurrent => "advice.pushNonFFCurrent",
            Advice::PushNonFFMatching => "advice.pushNonFFMatching",
            Advice::PushFetchFirst => "advice.pushFetchFirst",
            Advice::WaitingForEditor => "advice.waitingForEditor",
            Advice::RefSyntax => "advice.refSyntax",
            Advice::SetUpstreamFailure => "advice.setUpstreamFailure",
            Advice::ForceDeleteBranch => "advice.forceDeleteBranch",
            Advice::CheckoutAmbiguousRemoteBranchName => {
                "advice.checkoutAmbiguousRemoteBranchName"
            }
            Advice::IgnoredHook => "advice.ignoredHook",
            Advice::SuggestDetachingHead => "advice.suggestDetachingHead",
            Advice::ObjectNameWarning => "advice.objectNameWarning",
            Advice::UpdateSparsePath => "advice.updateSparsePath",
        }
    }

    /// Whether this hint should be shown, discovering the repository for its
    /// configuration. Outside a repository only `GIT_ADVICE` applies, matching
    /// git, which reads advice settings from whatever config it managed to load.
    pub fn enabled(self) -> bool {
        match gix::discover(".") {
            Ok(repo) => self.enabled_in(&repo),
            Err(_) => globally_enabled(),
        }
    }

    /// Whether this hint should be shown for an already-open repository.
    pub fn enabled_in(self, repo: &Repository) -> bool {
        if !globally_enabled() {
            return false;
        }
        if repo.config_snapshot().boolean(self.key()) == Some(false) {
            return false;
        }
        // `advice_enabled()` folds the deprecated alias into the umbrella slot,
        // so `advice.pushNonFastForward=false` still turns the family off.
        if self == Advice::PushUpdateRejected {
            return Advice::PushNonFastForward.enabled_in(repo);
        }
        true
    }

    /// True when the user has never set this slot, which is what makes
    /// `advise_if_enabled()` add its `Disable this message with …` trailer.
    fn unconfigured_in(self, repo: &Repository) -> bool {
        repo.config_snapshot().boolean(self.key()).is_none()
    }

    /// `advise()`: print `body` to stderr with every line prefixed `hint: `
    /// (a blank line becomes a bare `hint:`), if this slot is enabled.
    /// Returns whether anything was printed.
    pub fn advise(self, body: &str) -> bool {
        match gix::discover(".") {
            Ok(repo) => self.advise_in(&repo, body),
            Err(_) => {
                if !globally_enabled() {
                    return false;
                }
                print_hint(body);
                true
            }
        }
    }

    /// `advise()` with repository discovery: the hint alone, with no
    /// `Disable this message with …` trailer, for the sites git spells as an
    /// `advice_enabled()` check followed by a plain `advise()`.
    pub fn advise_plain(self, body: &str) -> bool {
        match gix::discover(".") {
            Ok(repo) => self.advise_plain_in(&repo, body),
            Err(_) => {
                if !globally_enabled() {
                    return false;
                }
                print_hint(body);
                true
            }
        }
    }

    /// `advise()` against an open repository: the hint alone, with no
    /// `Disable this message with …` trailer. This is what git's hand-rolled
    /// advice sites use — `advise_pull_before_push()` and friends check
    /// `advice_enabled()` themselves and then call plain `advise()`.
    pub fn advise_plain_in(self, repo: &Repository, body: &str) -> bool {
        if !self.enabled_in(repo) {
            return false;
        }
        print_hint(body);
        true
    }

    /// `advise_if_enabled()`: [`Advice::advise`] against an open repository,
    /// followed by the `Disable this message with …` line git appends when the
    /// slot has never been configured.
    pub fn advise_in(self, repo: &Repository, body: &str) -> bool {
        if !self.enabled_in(repo) {
            return false;
        }
        print_hint(body);
        if self.unconfigured_in(repo) {
            print_hint(&format!(
                "Disable this message with \"git config set {} false\"",
                self.key()
            ));
        }
        true
    }
}

/// `parse_remote_branch()`'s ambiguous-DWIM block (`builtin/checkout.c`), shared
/// by `git checkout` and `git switch` — `cmdname` is the *only* thing git
/// interpolates. The remote in the prose is the literal `origin` even when the
/// matches are on other remotes: git is naming the conventional default, not the
/// remotes it found. git checks `advice_enabled()` here and then calls plain
/// `advise()`, so no `Disable this message with …` trailer is printed.
pub fn ambiguous_remote_branch_name(repo: &Repository, cmdname: &str) {
    Advice::CheckoutAmbiguousRemoteBranchName.advise_plain_in(
        repo,
        &format!(
            "If you meant to check out a remote tracking branch on, e.g. 'origin',\n\
             you can do so by fully qualifying the name with the --track option:\n\
             \n\
             \x20   git {cmdname} --track origin/<name>\n\
             \n\
             If you'd like to always have checkouts of an ambiguous <name> prefer\n\
             one remote, e.g. the 'origin' remote, consider setting\n\
             checkout.defaultRemote=origin in your config."
        ),
    );
}

/// Port of `advise_on_updating_sparse_paths()` (`advice.c`): the report a command
/// prints when a pathspec matched only paths outside the sparse-checkout
/// definition, so nothing was updated in the index.
///
/// The three-line preamble and the path list are plain `stderr` writes that are
/// *not* gated on any `advice.*` slot — only the closing suggestion is, through
/// `advise_if_enabled(ADVICE_UPDATE_SPARSE_PATH, …)`, which is why its
/// `Disable this message with …` trailer appears while the slot is unconfigured.
/// Nothing is printed at all for an empty list.
pub fn on_updating_sparse_paths(repo: &Repository, paths: &[String]) {
    if paths.is_empty() {
        return;
    }
    eprintln!(
        "The following paths and/or pathspecs matched paths that exist\n\
         outside of your sparse-checkout definition, so will not be\n\
         updated in the index:"
    );
    for p in paths {
        eprintln!("{p}");
    }
    Advice::UpdateSparsePath.advise_in(
        repo,
        "If you intend to update such entries, try one of the following:\n\
         * Use the --sparse option.\n\
         * Disable or modify the sparsity rules.",
    );
}

/// `git_env_bool(GIT_ADVICE_ENVIRONMENT, 1)`: `GIT_ADVICE` set to a false value
/// squelches every hint, for tools that drive git as a subprocess.
fn globally_enabled() -> bool {
    match std::env::var("GIT_ADVICE") {
        Ok(v) => !matches!(v.trim(), "0" | "false" | "no" | "off" | ""),
        Err(_) => true,
    }
}

/// git's `vadvise()` line framing: `hint: ` before each line, and a bare
/// `hint:` for an empty one (no trailing space), all on stderr. Each whole line —
/// the `hint:` prefix included — is painted with `color.advice.hint` (default
/// yellow) when `color.advice` allows it; git closes the span before the newline.
pub(crate) fn print_hint(body: &str) {
    let color = hint_color();
    let reset = if color.is_empty() { "" } else { "\x1b[m" };
    for line in body.split('\n') {
        if line.is_empty() {
            eprintln!("{color}hint:{reset}");
        } else {
            eprintln!("{color}hint: {line}{reset}");
        }
    }
}

/// git's `advise_get_color(ADVICE_COLOR_HINT)`: the `color.advice.hint` sequence,
/// or the empty string when `color.advice` (which, unlike the stdout slots, has no
/// `color.ui` fallback and is `auto` against stderr) says not to color.
fn hint_color() -> String {
    let repo = gix::discover(".").ok();
    if !crate::porcelain::color::want_color_stderr(repo.as_ref(), "advice") {
        return String::new();
    }
    match repo {
        Some(r) => crate::porcelain::color::slot(&r.config_snapshot(), "color.advice.hint", "yellow"),
        None => "\x1b[33m".to_string(),
    }
}

/// Whether the `advice.<slot>` hint should be shown: true unless the user set
/// `advice.<slot> = false` or `GIT_ADVICE` is false. Outside a repository (or
/// when config can't be read) hints show, matching git's default. Call sites
/// that name a slot [`Advice`] carries should prefer [`Advice::enabled`], which
/// also honors the `pushUpdateRejected` alias.
pub fn enabled(slot: &str) -> bool {
    if !globally_enabled() {
        return false;
    }
    match gix::discover(".") {
        Ok(repo) => repo.config_snapshot().boolean(&format!("advice.{slot}")) != Some(false),
        Err(_) => true,
    }
}
