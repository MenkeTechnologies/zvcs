//! The interactive hunk selector — a port of git 2.55.0's `add-patch.c`.
//!
//! This is the engine behind `git add -p`, `git checkout -p`, `git restore -p`,
//! `git reset -p` and `git stash -p` (and, once wired up, `git commit -p`). It
//! generates a diff, splits it into per-file / per-hunk records, drives git's
//! one-key prompt loop over them, and applies the selected subset.
//!
//! ### Structure
//!
//! The port keeps `add-patch.c`'s data model verbatim, because every command in
//! the prompt loop is defined in terms of it:
//!
//! ```text
//!   plain / colored  — the raw diff text; every hunk is a byte range into it
//!   FileDiff         — one `diff --git` section: a `head` pseudo-hunk (the
//!                      header) plus the real hunks, and the deleted/added/
//!                      mode_change/binary flags parsed out of the header
//!   Hunk             — start/end into `plain`, the parsed `@@` header, the
//!                      running `delta` an edit introduced, `splittable_into`
//!                      (how many hunks `s` would produce), and the y/n decision
//! ```
//!
//! Hunk headers are *regenerated* on output rather than copied (`render_hunk`),
//! because skipping a hunk shifts every later hunk's new-side offset by `delta`.
//! Splitting (`split_hunk`) walks the hunk's lines and cuts at each first context
//! line after a run of `+`/`-` lines, redistributing the line counts; merging
//! (`merge_hunks`) is its inverse, needed when adjacent selected hunks overlap
//! after an edit. Both are line-for-line ports.
//!
//! ### Sub-processes
//!
//! git shells out to `git diff-files` / `git diff-index` for the diff and to
//! `git apply` for the result, with `GIT_INDEX_FILE` pointing at the index it
//! wants touched. This port does the same, re-executing *this* binary (so the
//! child is zvcs' own `diff-files` / `diff-index` / `apply`, not stock git).
//! That reuses the already-ported apply engine — the placement search, the
//! `--cached` index writer, the `-R` reversal — instead of growing a second one.
//!
//! ### Deviations (never faked, always noted)
//!
//! ```text
//!   * `P` (page the hunk) spawns the pager for that one hunk and waits, rather
//!     than installing it over fd 1 for the rest of the process; the rendered
//!     bytes are the same.
//!   * The single-key reader (`interactive.singleKey`) puts stdin in
//!     non-canonical mode and reads one byte, as git does, but does not query
//!     the terminfo escape-sequence table for multi-byte keys: an ESC-prefixed
//!     sequence is drained non-blockingly instead of waiting 500ms per byte.
//!     Both end up as an unknown command in this prompt loop.
//!   * git refreshes and rewrites the index before a non-`index_only` mode runs
//!     (`repo_refresh_and_write_index`) and again after applying. Neither is
//!     reproduced: both only rewrite the stat cache, which is invisible to the
//!     object/ref/index logical state this port is measured on.
//! ```

use anyhow::Result;
use std::io::{IsTerminal, Read, Write};
use std::process::{Command, ExitCode, Stdio};

use super::color;

// ---------------------------------------------------------------------------
// git's built-in color constants (color.h)
// ---------------------------------------------------------------------------

const GIT_COLOR_RESET: &str = "\x1b[m";
const GIT_COLOR_BOLD: &str = "\x1b[1m";
const GIT_COLOR_RED: &str = "\x1b[31m";
const GIT_COLOR_GREEN: &str = "\x1b[32m";
const GIT_COLOR_CYAN: &str = "\x1b[36m";
const GIT_COLOR_BOLD_RED: &str = "\x1b[1;31m";
const GIT_COLOR_BOLD_BLUE: &str = "\x1b[1;34m";
/// `GIT_COLOR_NORMAL` — "paint nothing", git's default for `diff.context`.
const GIT_COLOR_NORMAL: &str = "";

// ---------------------------------------------------------------------------
// patch modes (add-patch.c's `patch_mode_*` table)
// ---------------------------------------------------------------------------

/// One row of git's `patch_mode` table: which diff produces the hunks, which
/// `git apply` invocation consumes them, and the prompt / help wording.
struct PatchMode {
    /// The plumbing diff command and its fixed leading arguments.
    diff_cmd: &'static [&'static str],
    /// Extra arguments for the `git apply` that commits the selection.
    apply_args: &'static [&'static str],
    /// Extra arguments for the `git apply --check` run behind `e`.
    apply_check_args: &'static [&'static str],
    /// The diff is shown reversed, so `render_hunk` shifts the *old* offset by
    /// `delta` and the hunk editor swaps the meaning of `+` and `-`.
    is_reverse: bool,
    /// The mode only ever touches the index. git uses this to skip the
    /// `repo_refresh_and_write_index()` it otherwise runs before the selector;
    /// this port never does that refresh (it only rewrites the stat cache), so
    /// the flag is carried for table fidelity and read by nothing.
    #[allow(dead_code)]
    index_only: bool,
    /// Apply to index *and* worktree, with git's fall-back dance when only one
    /// of the two accepts the patch.
    apply_for_checkout: bool,
    /// Prompts for `PROMPT_MODE_CHANGE`, `PROMPT_DELETION`, `PROMPT_ADDITION`,
    /// `PROMPT_HUNK`.
    ///
    /// Rendered with `(decision, keys)` — the `(was: y)` marker and the
    /// context-dependent extra keys — always in that order, because git's
    /// `printf` passes both whatever the string does with them. A mode whose
    /// string has only one `%s` therefore shows the marker and silently drops
    /// the key list; [`PATCH_MODE_SPLIT`] is git's one such mode.
    prompt_mode: [&'static str; 4],
    /// The trailing line of the manual-edit instructions.
    edit_hunk_hint: &'static str,
    /// The always-applicable half of the `?` help.
    help_patch_text: &'static str,
}

/// Prompt slots, in git's `enum prompt_mode_type` order.
const PROMPT_MODE_CHANGE: usize = 0;
const PROMPT_DELETION: usize = 1;
const PROMPT_ADDITION: usize = 2;
const PROMPT_HUNK: usize = 3;

static PATCH_MODE_ADD: PatchMode = PatchMode {
    diff_cmd: &["diff-files"],
    apply_args: &["--cached"],
    apply_check_args: &["--cached"],
    is_reverse: false,
    index_only: false,
    apply_for_checkout: false,
    prompt_mode: [
        "Stage mode change%s [y,n,q,a,d%s,?]? ",
        "Stage deletion%s [y,n,q,a,d%s,?]? ",
        "Stage addition%s [y,n,q,a,d%s,?]? ",
        "Stage this hunk%s [y,n,q,a,d%s,?]? ",
    ],
    edit_hunk_hint: "If the patch applies cleanly, the edited hunk \
                     will immediately be marked for staging.",
    help_patch_text: "y - stage this hunk\n\
                      n - do not stage this hunk\n\
                      q - quit; do not stage this hunk or any of the remaining ones\n\
                      a - stage this hunk and all later hunks in the file\n\
                      d - do not stage this hunk or any of the later hunks in the file\n",
};

static PATCH_MODE_STASH: PatchMode = PatchMode {
    diff_cmd: &["diff-index", "HEAD"],
    apply_args: &["--cached"],
    apply_check_args: &["--cached"],
    is_reverse: false,
    index_only: false,
    apply_for_checkout: false,
    prompt_mode: [
        "Stash mode change%s [y,n,q,a,d%s,?]? ",
        "Stash deletion%s [y,n,q,a,d%s,?]? ",
        "Stash addition%s [y,n,q,a,d%s,?]? ",
        "Stash this hunk%s [y,n,q,a,d%s,?]? ",
    ],
    edit_hunk_hint: "If the patch applies cleanly, the edited hunk \
                     will immediately be marked for stashing.",
    help_patch_text: "y - stash this hunk\n\
                      n - do not stash this hunk\n\
                      q - quit; do not stash this hunk or any of the remaining ones\n\
                      a - stash this hunk and all later hunks in the file\n\
                      d - do not stash this hunk or any of the later hunks in the file\n",
};

static PATCH_MODE_RESET_HEAD: PatchMode = PatchMode {
    diff_cmd: &["diff-index", "--cached"],
    apply_args: &["-R", "--cached"],
    apply_check_args: &["-R", "--cached"],
    is_reverse: true,
    index_only: true,
    apply_for_checkout: false,
    prompt_mode: [
        "Unstage mode change%s [y,n,q,a,d%s,?]? ",
        "Unstage deletion%s [y,n,q,a,d%s,?]? ",
        "Unstage addition%s [y,n,q,a,d%s,?]? ",
        "Unstage this hunk%s [y,n,q,a,d%s,?]? ",
    ],
    edit_hunk_hint: "If the patch applies cleanly, the edited hunk \
                     will immediately be marked for unstaging.",
    help_patch_text: "y - unstage this hunk\n\
                      n - do not unstage this hunk\n\
                      q - quit; do not unstage this hunk or any of the remaining ones\n\
                      a - unstage this hunk and all later hunks in the file\n\
                      d - do not unstage this hunk or any of the later hunks in the file\n",
};

static PATCH_MODE_RESET_NOTHEAD: PatchMode = PatchMode {
    diff_cmd: &["diff-index", "-R", "--cached"],
    apply_args: &["--cached"],
    apply_check_args: &["--cached"],
    is_reverse: false,
    index_only: true,
    apply_for_checkout: false,
    prompt_mode: [
        "Apply mode change to index%s [y,n,q,a,d%s,?]? ",
        "Apply deletion to index%s [y,n,q,a,d%s,?]? ",
        "Apply addition to index%s [y,n,q,a,d%s,?]? ",
        "Apply this hunk to index%s [y,n,q,a,d%s,?]? ",
    ],
    edit_hunk_hint: "If the patch applies cleanly, the edited hunk \
                     will immediately be marked for applying.",
    help_patch_text: "y - apply this hunk to index\n\
                      n - do not apply this hunk to index\n\
                      q - quit; do not apply this hunk or any of the remaining ones\n\
                      a - apply this hunk and all later hunks in the file\n\
                      d - do not apply this hunk or any of the later hunks in the file\n",
};

static PATCH_MODE_CHECKOUT_INDEX: PatchMode = PatchMode {
    diff_cmd: &["diff-files"],
    apply_args: &["-R"],
    apply_check_args: &["-R"],
    is_reverse: true,
    index_only: false,
    apply_for_checkout: false,
    prompt_mode: [
        "Discard mode change from worktree%s [y,n,q,a,d%s,?]? ",
        "Discard deletion from worktree%s [y,n,q,a,d%s,?]? ",
        "Discard addition from worktree%s [y,n,q,a,d%s,?]? ",
        "Discard this hunk from worktree%s [y,n,q,a,d%s,?]? ",
    ],
    edit_hunk_hint: "If the patch applies cleanly, the edited hunk \
                     will immediately be marked for discarding.",
    help_patch_text: "y - discard this hunk from worktree\n\
                      n - do not discard this hunk from worktree\n\
                      q - quit; do not discard this hunk or any of the remaining ones\n\
                      a - discard this hunk and all later hunks in the file\n\
                      d - do not discard this hunk or any of the later hunks in the file\n",
};

static PATCH_MODE_CHECKOUT_HEAD: PatchMode = PatchMode {
    diff_cmd: &["diff-index"],
    apply_args: &[],
    apply_check_args: &["-R"],
    is_reverse: true,
    index_only: false,
    apply_for_checkout: true,
    prompt_mode: [
        "Discard mode change from index and worktree%s [y,n,q,a,d%s,?]? ",
        "Discard deletion from index and worktree%s [y,n,q,a,d%s,?]? ",
        "Discard addition from index and worktree%s [y,n,q,a,d%s,?]? ",
        "Discard this hunk from index and worktree%s [y,n,q,a,d%s,?]? ",
    ],
    edit_hunk_hint: "If the patch applies cleanly, the edited hunk \
                     will immediately be marked for discarding.",
    help_patch_text: "y - discard this hunk from index and worktree\n\
                      n - do not discard this hunk from index and worktree\n\
                      q - quit; do not discard this hunk or any of the remaining ones\n\
                      a - discard this hunk and all later hunks in the file\n\
                      d - do not discard this hunk or any of the later hunks in the file\n",
};

static PATCH_MODE_CHECKOUT_NOTHEAD: PatchMode = PatchMode {
    diff_cmd: &["diff-index", "-R"],
    apply_args: &[],
    apply_check_args: &[],
    is_reverse: false,
    index_only: false,
    apply_for_checkout: true,
    prompt_mode: [
        "Apply mode change to index and worktree%s [y,n,q,a,d%s,?]? ",
        "Apply deletion to index and worktree%s [y,n,q,a,d%s,?]? ",
        "Apply addition to index and worktree%s [y,n,q,a,d%s,?]? ",
        "Apply this hunk to index and worktree%s [y,n,q,a,d%s,?]? ",
    ],
    edit_hunk_hint: "If the patch applies cleanly, the edited hunk \
                     will immediately be marked for applying.",
    help_patch_text: "y - apply this hunk to index and worktree\n\
                      n - do not apply this hunk to index and worktree\n\
                      q - quit; do not apply this hunk or any of the remaining ones\n\
                      a - apply this hunk and all later hunks in the file\n\
                      d - do not apply this hunk or any of the later hunks in the file\n",
};

static PATCH_MODE_WORKTREE_HEAD: PatchMode = PatchMode {
    diff_cmd: &["diff-index"],
    apply_args: &["-R"],
    apply_check_args: &["-R"],
    is_reverse: true,
    index_only: false,
    apply_for_checkout: false,
    prompt_mode: [
        "Discard mode change from worktree%s [y,n,q,a,d%s,?]? ",
        "Discard deletion from worktree%s [y,n,q,a,d%s,?]? ",
        "Discard addition from worktree%s [y,n,q,a,d%s,?]? ",
        "Discard this hunk from worktree%s [y,n,q,a,d%s,?]? ",
    ],
    edit_hunk_hint: "If the patch applies cleanly, the edited hunk \
                     will immediately be marked for discarding.",
    help_patch_text: "y - discard this hunk from worktree\n\
                      n - do not discard this hunk from worktree\n\
                      q - quit; do not discard this hunk or any of the remaining ones\n\
                      a - discard this hunk and all later hunks in the file\n\
                      d - do not discard this hunk or any of the later hunks in the file\n",
};

static PATCH_MODE_WORKTREE_NOTHEAD: PatchMode = PatchMode {
    diff_cmd: &["diff-index", "-R"],
    apply_args: &[],
    apply_check_args: &[],
    is_reverse: false,
    index_only: false,
    apply_for_checkout: false,
    prompt_mode: [
        "Apply mode change to worktree%s [y,n,q,a,d%s,?]? ",
        "Apply deletion to worktree%s [y,n,q,a,d%s,?]? ",
        "Apply addition to worktree%s [y,n,q,a,d%s,?]? ",
        "Apply this hunk to worktree%s [y,n,q,a,d%s,?]? ",
    ],
    edit_hunk_hint: "If the patch applies cleanly, the edited hunk \
                     will immediately be marked for applying.",
    help_patch_text: "y - apply this hunk to worktree\n\
                      n - do not apply this hunk to worktree\n\
                      q - quit; do not apply this hunk or any of the remaining ones\n\
                      a - apply this hunk and all later hunks in the file\n\
                      d - do not apply this hunk or any of the later hunks in the file\n",
};

/// The mode `run_add_p_index` builds on its stack, for `git history split`.
///
/// Two things separate it from [`PATCH_MODE_ADD`], which it otherwise copies.
/// Its diff is `diff-tree -r <parent tree> <commit>` rather than `diff-files`
/// (the tree oid is a run-time value, supplied through [`State::diff_extra`]).
/// And its prompts carry a single `%s` where every other mode carries two, so
/// the `(was: …)` marker is all that is substituted and the key list never
/// reaches the screen — `[y,n,q,a,d,?]` even though `p`/`P` are live. That is
/// git's own output, not a simplification: its `printf` passes both arguments
/// unconditionally and C drops the one the format has no slot for.
static PATCH_MODE_SPLIT: PatchMode = PatchMode {
    diff_cmd: &["diff-tree", "-r"],
    apply_args: &["--cached"],
    apply_check_args: &["--cached"],
    is_reverse: false,
    index_only: true,
    apply_for_checkout: false,
    prompt_mode: [
        "Stage mode change [y,n,q,a,d%s,?]? ",
        "Stage deletion [y,n,q,a,d%s,?]? ",
        "Stage addition [y,n,q,a,d%s,?]? ",
        "Stage this hunk [y,n,q,a,d%s,?]? ",
    ],
    edit_hunk_hint: "If the patch applies cleanly, the edited hunk \
                     will immediately be marked for staging.",
    help_patch_text: "y - stage this hunk\n\
                      n - do not stage this hunk\n\
                      q - quit; do not stage this hunk or any of the remaining ones\n\
                      a - stage this hunk and all later hunks in the file\n\
                      d - do not stage this hunk or any of the later hunks in the file\n",
};

/// The remainder of the `?` help — lines shown only when the corresponding key
/// is currently available (git filters them against the prompt's key list).
const HELP_PATCH_REMAINDER: &str = "\
j - go to the next undecided hunk, roll over at the bottom\n\
J - go to the next hunk, roll over at the bottom\n\
k - go to the previous undecided hunk, roll over at the top\n\
K - go to the previous hunk, roll over at the top\n\
g - select a hunk to go to\n\
/ - search for a hunk matching the given regex\n\
s - split the current hunk into smaller hunks\n\
e - manually edit the current hunk\n\
p - print the current hunk\n\
P - print the current hunk using the pager\n\
> - go to the next file, roll over at the bottom\n\
< - go to the previous file, roll over at the top\n\
? - print help\n\
HUNKS SUMMARY - Hunks: %d, USE: %d, SKIP: %d\n";

/// Which family of `patch_mode_*` rows a caller wants; the exact row also
/// depends on the revision (see [`select_mode`]), matching `run_add_p`.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Mode {
    /// `git add -p` / `git commit -p`.
    Add,
    /// `git stash -p`. No caller yet: `stash` refuses `-p` because the selector
    /// would have to stage into a scratch index (`GIT_INDEX_FILE`), which this
    /// port does not honour — see the comment at `stash.rs`'s `-p` arm.
    #[allow(dead_code)]
    Stash,
    /// `git reset -p`.
    Reset,
    /// `git checkout -p`.
    Checkout,
    /// `git restore -p` (git's `ADD_P_WORKTREE`).
    Worktree,
}

/// git's `struct interactive_options` — the `-U`/`--inter-hunk-context`/
/// `--[no-]auto-advance` values the caller parsed, plus `ADD_P_DISALLOW_EDIT`.
#[derive(Clone, Copy)]
pub(crate) struct Options {
    /// `-U`/`--unified <n>`; `-1` means "not given" (fall back to `diff.context`).
    pub(crate) context: i32,
    /// `--inter-hunk-context <n>`; `-1` means "not given".
    pub(crate) interhunk: i32,
    /// `--[no-]auto-advance`: when off, the file list is navigable with `<`/`>`
    /// and nothing is applied until every file has been visited.
    pub(crate) auto_advance: bool,
    /// git's `ADD_P_DISALLOW_EDIT`: hide the `e` command (set by `git stash -p`,
    /// whose second apply pass cannot cope with a hand-edited hunk).
    pub(crate) disallow_edit: bool,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            context: -1,
            interhunk: -1,
            auto_advance: true,
            disallow_edit: false,
        }
    }
}

// ---------------------------------------------------------------------------
// resolved configuration (add-patch.c's `interactive_config_init`)
// ---------------------------------------------------------------------------

/// The colors, context widths and input mode the prompt loop runs with.
pub(super) struct Config {
    /// Diff context width handed to the child diff, or `-1` for its default.
    pub(super) context: i32,
    /// Inter-hunk context handed to the child diff, or `-1`.
    pub(super) interhunk: i32,
    /// See [`Options::auto_advance`].
    pub(super) auto_advance: bool,
    /// Whether the *hunk* text should be colored (`color.diff` / `color.ui`).
    use_color_diff: bool,
    /// `color.interactive.header` — the `Split into N hunks.` notice.
    pub(super) header_color: String,
    /// `color.interactive.help` — the `?` help text.
    pub(super) help_color: String,
    /// `color.interactive.prompt` — the `(1/3) Stage this hunk...` line.
    pub(super) prompt_color: String,
    /// `color.interactive.error` — every `err()` diagnostic.
    pub(super) error_color: String,
    /// The reset sequence for the four slots above, or "" when color is off.
    pub(super) reset_color_interactive: String,
    /// `color.diff.frag` — regenerated `@@` headers.
    fraginfo_color: String,
    /// `color.diff.context` (falling back to `color.diff.plain`).
    context_color: String,
    /// `color.diff.old`.
    file_old_color: String,
    /// `color.diff.new`.
    file_new_color: String,
    /// The reset sequence for the diff slots, or "" when diff color is off.
    reset_color_diff: String,
    /// `interactive.diffFilter` — a shell command the *colored* diff is piped
    /// through before it is sliced into hunks. It must preserve the line count.
    diff_filter: Option<String>,
    /// `diff.algorithm`, forwarded to the child diff.
    diff_algorithm: Option<String>,
    /// `interactive.singleKey` — read one keystroke instead of a line.
    single_key: bool,
}

impl Config {
    /// Port of `interactive_config_init`: config first, then the command line.
    pub(super) fn init(repo: &gix::Repository, opts: &Options) -> Result<Self> {
        let snap = repo.config_snapshot();
        let use_color_interactive = color::want_color_stdout(repo, "interactive");
        let use_color_diff = color::want_color_stdout(repo, "diff");

        let mut cfg = Self {
            context: -1,
            interhunk: -1,
            auto_advance: opts.auto_advance,
            use_color_diff,
            header_color: init_color(&snap, use_color_interactive, "interactive.header", GIT_COLOR_BOLD),
            help_color: init_color(&snap, use_color_interactive, "interactive.help", GIT_COLOR_BOLD_RED),
            prompt_color: init_color(&snap, use_color_interactive, "interactive.prompt", GIT_COLOR_BOLD_BLUE),
            error_color: init_color(&snap, use_color_interactive, "interactive.error", GIT_COLOR_BOLD_RED),
            reset_color_interactive: if use_color_interactive { GIT_COLOR_RESET.into() } else { String::new() },
            fraginfo_color: init_color(&snap, use_color_diff, "diff.frag", GIT_COLOR_CYAN),
            context_color: String::new(),
            file_old_color: init_color(&snap, use_color_diff, "diff.old", GIT_COLOR_RED),
            file_new_color: init_color(&snap, use_color_diff, "diff.new", GIT_COLOR_GREEN),
            reset_color_diff: if use_color_diff { GIT_COLOR_RESET.into() } else { String::new() },
            diff_filter: snap.string("interactive.diffFilter").map(|v| v.to_string()),
            diff_algorithm: snap.string("diff.algorithm").map(|v| v.to_string()),
            single_key: snap.boolean("interactive.singleKey").unwrap_or(false),
        };

        // git resolves `color.diff.context` and, only when that key is missing
        // or unparseable, `color.diff.plain` — the historical spelling of the
        // same slot. The sentinel default is how git detects "not resolved".
        const FALL_BACK: &str = "fall back";
        cfg.context_color = init_color(&snap, use_color_diff, "diff.context", FALL_BACK);
        if cfg.context_color == FALL_BACK {
            cfg.context_color = init_color(&snap, use_color_diff, "diff.plain", GIT_COLOR_NORMAL);
        }

        if let Some(n) = snap.integer("diff.context") {
            if n < 0 {
                crate::git_fatal!("diff.context cannot be negative");
            }
            cfg.context = n as i32;
        }
        if let Some(n) = snap.integer("diff.interHunkContext") {
            if n < 0 {
                crate::git_fatal!("diff.interHunkContext cannot be negative");
            }
            cfg.interhunk = n as i32;
        }

        if opts.context != -1 {
            if opts.context < 0 {
                crate::git_fatal!("--unified cannot be negative");
            }
            cfg.context = opts.context;
        }
        if opts.interhunk != -1 {
            if opts.interhunk < 0 {
                crate::git_fatal!("--inter-hunk-context cannot be negative");
            }
            cfg.interhunk = opts.interhunk;
        }
        Ok(cfg)
    }
}

/// Port of `init_color`: an unset key — or one whose value git's `color_parse`
/// rejects — falls back to the built-in default, and color-off yields "".
fn init_color(snap: &gix::config::Snapshot<'_>, want: bool, section_and_slot: &str, default: &str) -> String {
    if !want {
        return String::new();
    }
    match snap.string(&format!("color.{section_and_slot}")) {
        Some(v) => color::parse_color_spec(&v.to_string()).unwrap_or_else(|| default.to_string()),
        None => default.to_string(),
    }
}

// ---------------------------------------------------------------------------
// the parsed diff
// ---------------------------------------------------------------------------

/// A parsed `@@ -a,b +c,d @@<extra>` line.
#[derive(Clone, Copy, Default)]
struct HunkHeader {
    old_offset: u64,
    old_count: u64,
    new_offset: u64,
    new_count: u64,
    /// Byte range of the text after the second `@@` (the function signature),
    /// newline included, in `plain`.
    extra_start: usize,
    extra_end: usize,
    /// The same range in `colored`.
    colored_extra_start: usize,
    colored_extra_end: usize,
    /// The colored hunk header could not be located, so it is echoed verbatim
    /// instead of being regenerated.
    suppress_colored_line_range: bool,
}

/// The user's decision about one hunk.
#[derive(Clone, Copy, PartialEq, Eq, Default)]
enum Use {
    #[default]
    Undecided,
    Skip,
    Use,
}

/// One hunk: a byte range into `plain` (and `colored`), its header, and state.
#[derive(Clone, Copy, Default)]
struct Hunk {
    start: usize,
    end: usize,
    colored_start: usize,
    colored_end: usize,
    /// How many hunks `s` would produce from this one.
    splittable_into: usize,
    /// Net new-side line shift this hunk introduces relative to its header,
    /// non-zero only after a manual edit.
    delta: i64,
    use_: Use,
    header: HunkHeader,
}

/// One `diff --git` section.
#[derive(Default)]
struct FileDiff {
    /// The header lines, treated as a pseudo-hunk so they can be rendered and
    /// (for a pure mode change) partially skipped.
    head: Hunk,
    hunk: Vec<Hunk>,
    deleted: bool,
    added: bool,
    mode_change: bool,
    binary: bool,
}

/// git's `struct add_p_state`.
struct State<'a> {
    repo: &'a gix::Repository,
    cfg: Config,
    mode: &'static PatchMode,
    revision: Option<String>,
    /// Scratch buffer, shared exactly as git shares `s->buf` (the `/` search
    /// deliberately appends to whatever the prompt left there).
    buf: Vec<u8>,
    answer: Vec<u8>,
    plain: Vec<u8>,
    colored: Vec<u8>,
    files: Vec<FileDiff>,
    /// Set once the single-key reader has fallen back to line input, so the
    /// warning is printed at most once (git's `warning_displayed`).
    single_key_warned: bool,
    /// Arguments spliced in directly after `mode.diff_cmd`.
    ///
    /// `run_add_p_index` finishes its `diff_cmd` at run time with the parent
    /// tree's oid; a `&'static` table cannot hold that, so it arrives here and
    /// `parse_diff` appends it in the same position.
    diff_extra: Vec<String>,
    /// git's `s->index_file`: the index every child of this selector operates
    /// on, exported to each of them as `GIT_INDEX_FILE` by
    /// `setup_child_process`. `None` leaves the children on the repository's own
    /// index, which is what `run_add_p` does.
    index_file: Option<std::path::PathBuf>,
}

// ---------------------------------------------------------------------------
// small byte helpers (strbuf equivalents)
// ---------------------------------------------------------------------------

/// git's `find_next_line`: the offset just past the next newline, or the end.
fn find_next_line(buf: &[u8], offset: usize) -> usize {
    match buf[offset..].iter().position(|&c| c == b'\n') {
        Some(i) => offset + i + 1,
        None => buf.len(),
    }
}

/// git's `normalize_marker`: an empty context line may omit its leading space.
fn normalize_marker(buf: &[u8], i: usize) -> u8 {
    let c = buf.get(i).copied().unwrap_or(0);
    if c == b'\n' || (c == b'\r' && buf.get(i + 1) == Some(&b'\n')) {
        b' '
    } else {
        c
    }
}

/// git's `strbuf_complete_line`.
fn complete_line(buf: &mut Vec<u8>) {
    if !buf.is_empty() && *buf.last().unwrap() != b'\n' {
        buf.push(b'\n');
    }
}

/// Substitute the two `%s` of a prompt-mode format string, in order.
fn fmt2(fmt: &str, a: &str, b: &str) -> String {
    let mut out = String::with_capacity(fmt.len() + a.len() + b.len());
    let mut rest = fmt;
    for arg in [a, b] {
        match rest.find("%s") {
            Some(i) => {
                out.push_str(&rest[..i]);
                out.push_str(arg);
                rest = &rest[i + 2..];
            }
            None => break,
        }
    }
    out.push_str(rest);
    out
}

/// git's `color_fprintf`: paint only when the slot is non-empty.
pub(super) fn color_print(color: &str, text: &str) {
    let mut out = std::io::stdout();
    if color.is_empty() {
        let _ = out.write_all(text.as_bytes());
    } else {
        let _ = write!(out, "{color}{text}{GIT_COLOR_RESET}");
    }
}

/// git's `color_fprintf_ln`.
pub(super) fn color_println(color: &str, text: &str) {
    color_print(color, text);
    let _ = std::io::stdout().write_all(b"\n");
}

// ---------------------------------------------------------------------------
// child processes
// ---------------------------------------------------------------------------

/// Re-execute this binary as `git <args>`, optionally feeding it `input` and
/// capturing its stdout. stderr is inherited, as git's `pipe_command` leaves it.
///
/// `dir` overrides the child's working directory. The diff children inherit the
/// caller's (so relative pathspecs resolve the way the user typed them); the
/// `apply` children are run at the worktree root, because a reassembled patch is
/// always a `diff --git` patch and those name paths relative to the top level —
/// git's `patch->is_toplevel_relative`, which makes its own `apply` ignore the
/// prefix for exactly these patches.
pub(super) fn run_git(
    args: &[String],
    input: Option<&[u8]>,
    capture: bool,
    dir: Option<&std::path::Path>,
) -> std::io::Result<(bool, Vec<u8>)> {
    run_git_in_index(args, input, capture, dir, None)
}

/// [`run_git`] with git's `setup_child_process` environment: `GIT_INDEX_FILE`
/// pointing at the index this selector is staging into.
///
/// git sets it on *every* child it spawns, the diff children included, so a
/// selector running against a scratch index never reads or writes the
/// repository's own. `None` leaves the environment alone.
fn run_git_in_index(
    args: &[String],
    input: Option<&[u8]>,
    capture: bool,
    dir: Option<&std::path::Path>,
    index_file: Option<&std::path::Path>,
) -> std::io::Result<(bool, Vec<u8>)> {
    let exe = std::env::current_exe()?;
    let mut cmd = Command::new(exe);
    cmd.args(args);
    if let Some(d) = dir {
        cmd.current_dir(d);
    }
    if let Some(index) = index_file {
        cmd.env("GIT_INDEX_FILE", index);
    }
    cmd.stdin(if input.is_some() { Stdio::piped() } else { Stdio::null() });
    cmd.stdout(if capture { Stdio::piped() } else { Stdio::inherit() });
    let mut child = cmd.spawn()?;
    if let Some(data) = input {
        let mut stdin = child.stdin.take().expect("stdin piped");
        let _ = stdin.write_all(data);
        drop(stdin);
    }
    let mut out = Vec::new();
    if capture {
        let mut stdout = child.stdout.take().expect("stdout piped");
        stdout.read_to_end(&mut out)?;
    }
    let status = child.wait()?;
    Ok((status.success(), out))
}

/// Run `command`, feeding it `input` and capturing its stdout — git's
/// `use_shell` child for `interactive.diffFilter` (`filter_cp.use_shell = 1`
/// with the filter string as the whole argv). `None` on spawn/IO failure (the
/// exit status is ignored, as git's `pipe_command` ignores it here).
fn run_shell(command: &str, input: &[u8]) -> Option<Vec<u8>> {
    let mut child = crate::external::prepare_shell_cmd_str(command, crate::external::NO_ARGS)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .ok()?;
    let mut stdin = child.stdin.take()?;
    let data = input.to_vec();
    // Write on a helper thread: a filter that streams (rather than slurping)
    // would otherwise deadlock on a full pipe while we wait to write it all.
    let writer = std::thread::spawn(move || {
        let _ = stdin.write_all(&data);
    });
    let mut out = Vec::new();
    child.stdout.take()?.read_to_end(&mut out).ok()?;
    let _ = writer.join();
    let _ = child.wait();
    Some(out)
}

// ---------------------------------------------------------------------------
// diff parsing
// ---------------------------------------------------------------------------

/// git's `parse_range`: `<offset>[,<count>]`, returning the rest of the line.
fn parse_range(p: &[u8]) -> Option<(u64, u64, usize)> {
    let mut i = 0;
    while i < p.len() && p[i].is_ascii_digit() {
        i += 1;
    }
    if i == 0 {
        return None;
    }
    let offset: u64 = std::str::from_utf8(&p[..i]).ok()?.parse().ok()?;
    if p.get(i) != Some(&b',') {
        return Some((offset, 1, i));
    }
    let start = i + 1;
    let mut j = start;
    while j < p.len() && p[j].is_ascii_digit() {
        j += 1;
    }
    if j == start {
        return None;
    }
    let count: u64 = std::str::from_utf8(&p[start..j]).ok()?.parse().ok()?;
    Some((offset, count, j))
}

impl State<'_> {
    /// The worktree root, where the `apply` children run (see [`run_git`]).
    fn workdir(&self) -> Option<&std::path::Path> {
        self.repo.workdir()
    }

    /// git's `err()`: the diagnostic goes to *stdout*, painted with
    /// `color.interactive.error`, with a trailing newline from `puts`.
    fn err(&self, msg: &str) {
        let mut out = std::io::stdout();
        let _ = write!(out, "{}{}{}\n", self.cfg.error_color, msg, self.cfg.reset_color_interactive);
        let _ = out.flush();
    }

    /// git's `parse_hunk_header`. Rewrites `hunk.start` to skip the header line.
    fn parse_hunk_header(&self, hunk: &mut Hunk) -> Result<()> {
        let line = hunk.start;
        let eol = match self.plain[line..].iter().position(|&c| c == b'\n') {
            Some(i) => line + i,
            None => self.plain.len(),
        };
        let mut p = line;
        let bad = || {
            anyhow::anyhow!(
                "could not parse hunk header '{}'",
                String::from_utf8_lossy(&self.plain[line..eol])
            )
        };
        if !self.plain[p..].starts_with(b"@@ -") {
            return Err(bad());
        }
        p += 4;
        let (old_offset, old_count, used) = parse_range(&self.plain[p..]).ok_or_else(bad)?;
        p += used;
        if !self.plain[p..].starts_with(b" +") {
            return Err(bad());
        }
        p += 2;
        let (new_offset, new_count, used) = parse_range(&self.plain[p..]).ok_or_else(bad)?;
        p += used;
        if !self.plain[p..].starts_with(b" @@") {
            return Err(bad());
        }
        p += 3;

        hunk.header.old_offset = old_offset;
        hunk.header.old_count = old_count;
        hunk.header.new_offset = new_offset;
        hunk.header.new_count = new_count;
        hunk.start = eol + usize::from(self.plain.get(eol) == Some(&b'\n'));
        hunk.header.extra_start = p;
        hunk.header.extra_end = hunk.start;

        if self.colored.is_empty() {
            hunk.header.colored_extra_start = 0;
            hunk.header.colored_extra_end = 0;
            return Ok(());
        }

        // Locate the same trailing text in the colored rendering.
        let cline = hunk.colored_start;
        let ceol = match self.colored[cline..].iter().position(|&c| c == b'\n') {
            Some(i) => cline + i,
            None => self.colored.len(),
        };
        let window = &self.colored[cline..ceol];
        match find_sub(window, b"@@ -").and_then(|a| find_sub(&window[a + 4..], b" @@").map(|b| a + 4 + b)) {
            Some(rel) => hunk.header.colored_extra_start = cline + rel + 3,
            None => {
                hunk.header.colored_extra_start = hunk.colored_start;
                hunk.header.suppress_colored_line_range = true;
            }
        }
        hunk.colored_start = ceol + usize::from(self.colored.get(ceol) == Some(&b'\n'));
        hunk.header.colored_extra_end = hunk.colored_start;
        Ok(())
    }

    /// git's `parse_diff`: run the mode's diff command and slice it into
    /// [`FileDiff`]s and [`Hunk`]s.
    fn parse_diff(&mut self, pathspecs: &[String]) -> Result<()> {
        let mut args: Vec<String> = self.mode.diff_cmd.iter().map(|s| s.to_string()).collect();
        // The run-time tail of `diff_cmd` — `run_add_p_index`'s parent tree oid.
        args.extend(self.diff_extra.iter().cloned());
        if self.cfg.context != -1 {
            args.push(format!("--unified={}", self.cfg.context));
        }
        if self.cfg.interhunk != -1 {
            args.push(format!("--inter-hunk-context={}", self.cfg.interhunk));
        }
        if let Some(algo) = &self.cfg.diff_algorithm {
            args.push(format!("--diff-algorithm={algo}"));
        }
        if let Some(rev) = &self.revision {
            // An unborn HEAD has no commit to diff against: use the empty tree,
            // exactly as git does.
            let unborn = rev == "HEAD" && self.repo.head_id().is_err();
            if unborn {
                args.push(gix::ObjectId::empty_tree(self.repo.object_hash()).to_string());
            } else {
                args.push(rev.clone());
            }
        }
        let color_arg_index = args.len();
        args.push("--no-color".into());
        args.push("--ignore-submodules=dirty".into());
        args.push("-p".into());
        args.push("--".into());
        args.extend(pathspecs.iter().cloned());

        let (ok, plain) = run_git_in_index(&args, None, true, None, self.index_file.as_deref())?;
        if !ok {
            crate::git_fatal!("could not parse diff");
        }
        self.plain = plain;
        if self.plain.is_empty() {
            return Ok(());
        }
        complete_line(&mut self.plain);

        // The colored rendering, re-run with `--color` in place of `--no-color`
        // exactly as git overwrites that one argv slot. When the diff should not
        // be painted the buffer stays empty and every later `colored` branch is
        // skipped, which is also git's state on a pipe.
        if self.cfg.use_color_diff {
            let mut cargs = args.clone();
            cargs[color_arg_index] = "--color".into();
            let (ok, colored) =
                run_git_in_index(&cargs, None, true, None, self.index_file.as_deref())?;
            if !ok {
                crate::git_fatal!("could not parse colored diff");
            }
            self.colored = colored;

            // `interactive.diffFilter`: a shell command the colored diff is
            // piped through (diff-highlight, delta, …). It must keep a
            // one-to-one line correspondence — the parse below checks that.
            if let Some(filter) = self.cfg.diff_filter.clone() {
                match run_shell(&filter, &self.colored) {
                    Some(out) => self.colored = out,
                    None => crate::git_fatal!("failed to run '{filter}'"),
                }
            }
            complete_line(&mut self.colored);
        }

        let has_color = !self.colored.is_empty();
        let mut colored_p = 0usize;
        let colored_pend = self.colored.len();
        let mut marker = 0u8;
        let mut p = 0usize;
        let pend = self.plain.len();
        // Which file / hunk the parser is currently filling in.
        let mut file_idx: usize = 0;
        // `None` selects the file's `head` pseudo-hunk.
        let mut hunk_idx: Option<usize> = None;

        while p != pend {
            let eol = match self.plain[p..].iter().position(|&c| c == b'\n') {
                Some(i) => p + i,
                None => pend,
            };
            let ch = normalize_marker(&self.plain, p);
            let mut mode_change_line = false;

            if self.plain[p..].starts_with(b"diff ") || self.plain[p..].starts_with(b"* Unmerged path ") {
                // A `+`/`-` last line means the previous hunk had no trailing
                // context, so it still counts as one splittable unit.
                if let Some(h) = current_hunk_mut(&mut self.files, file_idx, hunk_idx) {
                    if marker == b'-' || marker == b'+' {
                        h.splittable_into += 1;
                    }
                }
                self.files.push(FileDiff::default());
                file_idx = self.files.len() - 1;
                hunk_idx = None;
                let head = &mut self.files[file_idx].head;
                head.start = p;
                if has_color {
                    head.colored_start = colored_p;
                }
                marker = 0;
            } else if p == 0 {
                crate::git_fatal!(
                    "diff starts with unexpected line:\n{}",
                    String::from_utf8_lossy(&self.plain[p..eol])
                );
            } else if self.files[file_idx].deleted {
                // Keep the rest of a deletion in a single "hunk".
            } else if self.plain[p..].starts_with(b"@@ ")
                || (hunk_idx.is_none() && self.plain[p..].starts_with(b"deleted file"))
            {
                let deleted_line = self.plain[p..].starts_with(b"deleted file");
                if marker == b'-' || marker == b'+' {
                    if let Some(h) = current_hunk_mut(&mut self.files, file_idx, hunk_idx) {
                        h.splittable_into += 1;
                    }
                }
                let mut h = Hunk { start: p, ..Hunk::default() };
                if has_color {
                    h.colored_start = colored_p;
                }
                self.files[file_idx].hunk.push(h);
                hunk_idx = Some(self.files[file_idx].hunk.len() - 1);
                if deleted_line {
                    self.files[file_idx].deleted = true;
                } else {
                    let idx = hunk_idx.unwrap();
                    h = self.files[file_idx].hunk[idx];
                    self.parse_hunk_header(&mut h)?;
                    self.files[file_idx].hunk[idx] = h;
                }
                marker = ch;
            } else if hunk_idx.is_none() && self.plain[p..].starts_with(b"new file") {
                self.files[file_idx].added = true;
            } else if hunk_idx.is_none()
                && self.plain[p..].starts_with(b"old mode ")
                && is_octal(&self.plain[p + 9..eol])
            {
                mode_change_line = true;
                // The mode-change pseudo-hunk is part of the header "hunk", so
                // `hunk_idx` deliberately stays on the head.
                self.files[file_idx].mode_change = true;
                let mut h = Hunk { start: p, ..Hunk::default() };
                if has_color {
                    h.colored_start = colored_p;
                }
                self.files[file_idx].hunk.push(h);
            } else if hunk_idx.is_none()
                && self.plain[p..].starts_with(b"new mode ")
                && is_octal(&self.plain[p + 9..eol])
            {
                // Extends the pseudo-hunk to cover the `new mode` line too.
                mode_change_line = true;
            } else if hunk_idx.is_none() && self.plain[p..].starts_with(b"Binary files ") {
                self.files[file_idx].binary = true;
            }

            if marker == b'-' || marker == b'+' {
                if ch == b' ' {
                    if let Some(h) = current_hunk_mut(&mut self.files, file_idx, hunk_idx) {
                        h.splittable_into += 1;
                    }
                }
            }
            if marker != 0 && ch != b'\\' {
                marker = ch;
            }

            p = if eol == pend { pend } else { eol + 1 };
            let hunk_end = p;
            if let Some(h) = current_hunk_mut(&mut self.files, file_idx, hunk_idx) {
                h.end = hunk_end;
            }

            let mut colored_end = 0usize;
            if has_color {
                match self.colored[colored_p..colored_pend].iter().position(|&c| c == b'\n') {
                    Some(i) => colored_p += i + 1,
                    None => {
                        if p != pend || colored_p == colored_pend {
                            return Err(mismatched_output());
                        }
                        colored_p = colored_pend;
                    }
                }
                colored_end = colored_p;
                if let Some(h) = current_hunk_mut(&mut self.files, file_idx, hunk_idx) {
                    h.colored_end = colored_end;
                }
            }

            if mode_change_line {
                let f = &mut self.files[file_idx];
                f.hunk[0].end = hunk_end;
                if has_color {
                    f.hunk[0].colored_end = colored_end;
                }
            }
        }

        if let Some(h) = current_hunk_mut(&mut self.files, file_idx, hunk_idx) {
            if marker == b'-' || marker == b'+' {
                h.splittable_into += 1;
            }
        }
        if has_color && colored_p != colored_pend {
            return Err(mismatched_output());
        }
        Ok(())
    }
}

/// git's `mismatched_output` diagnostic for a misbehaving `interactive.diffFilter`:
/// an `error()` followed by an `advise()`, in that order. Both are written here,
/// so the returned error carries an empty message that [`run`] does not re-print.
fn mismatched_output() -> anyhow::Error {
    eprintln!("error: mismatched output from interactive.diffFilter");
    crate::advice::print_hint(
        "Your filter must maintain a one-to-one correspondence\n\
         between its input and output lines.",
    );
    anyhow::anyhow!("")
}

/// The hunk the parser is currently appending to: the file's `head` pseudo-hunk
/// when `hunk_idx` is `None`, else the indexed hunk.
fn current_hunk_mut(files: &mut [FileDiff], file_idx: usize, hunk_idx: Option<usize>) -> Option<&mut Hunk> {
    let f = files.get_mut(file_idx)?;
    match hunk_idx {
        None => Some(&mut f.head),
        Some(i) => f.hunk.get_mut(i),
    }
}

/// git's `is_octal`.
fn is_octal(p: &[u8]) -> bool {
    !p.is_empty() && p.iter().all(|&c| (b'0'..=b'7').contains(&c))
}

/// `memmem` for byte slices.
fn find_sub(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.len() > haystack.len() {
        return None;
    }
    (0..=haystack.len() - needle.len()).find(|&i| &haystack[i..i + needle.len()] == needle)
}

// ---------------------------------------------------------------------------
// rendering
// ---------------------------------------------------------------------------

impl State<'_> {
    /// git's `render_hunk`: regenerate the `@@` header (shifted by `delta`) and
    /// append the hunk body.
    fn render_hunk(&self, hunk: &Hunk, delta: i64, colored: bool, out: &mut Vec<u8>) {
        let header = &hunk.header;
        if header.old_offset != 0 || header.new_offset != 0 {
            let p;
            let len;
            let mut old_offset = header.old_offset as i64;
            let mut new_offset = header.new_offset as i64;

            if !colored {
                p = header.extra_start;
                len = header.extra_end - header.extra_start;
            } else if header.suppress_colored_line_range {
                out.extend_from_slice(&self.colored[header.colored_extra_start..header.colored_extra_end]);
                out.extend_from_slice(&self.colored[hunk.colored_start..hunk.colored_end]);
                return;
            } else {
                out.extend_from_slice(self.cfg.fraginfo_color.as_bytes());
                p = header.colored_extra_start;
                len = header.colored_extra_end - header.colored_extra_start;
            }

            if self.mode.is_reverse {
                old_offset -= delta;
            } else {
                new_offset += delta;
            }

            out.extend_from_slice(format!("@@ -{old_offset}").as_bytes());
            if header.old_count != 1 {
                out.extend_from_slice(format!(",{}", header.old_count).as_bytes());
            }
            out.extend_from_slice(format!(" +{new_offset}").as_bytes());
            if header.new_count != 1 {
                out.extend_from_slice(format!(",{}", header.new_count).as_bytes());
            }
            out.extend_from_slice(b" @@");

            if len != 0 {
                let src = if colored { &self.colored } else { &self.plain };
                out.extend_from_slice(&src[p..p + len]);
            } else if colored {
                out.extend_from_slice(self.cfg.reset_color_diff.as_bytes());
                out.push(b'\n');
            } else {
                out.push(b'\n');
            }
        }

        if colored {
            out.extend_from_slice(&self.colored[hunk.colored_start..hunk.colored_end]);
        } else {
            out.extend_from_slice(&self.plain[hunk.start..hunk.end]);
        }
    }

    /// git's `render_diff_header`: cut the `old mode`/`new mode` pseudo-hunk out
    /// of the header when the user declined the mode change.
    fn render_diff_header(&self, file_idx: usize, colored: bool, out: &mut Vec<u8>) {
        let f = &self.files[file_idx];
        let skip_mode_change = f.mode_change && f.hunk[0].use_ != Use::Use;
        let head = &f.head;
        if !skip_mode_change {
            self.render_hunk(head, 0, colored, out);
            return;
        }
        let first = &f.hunk[0];
        if colored {
            out.extend_from_slice(&self.colored[head.colored_start..first.colored_start]);
            out.extend_from_slice(&self.colored[first.colored_end..head.colored_end]);
        } else {
            out.extend_from_slice(&self.plain[head.start..first.start]);
            out.extend_from_slice(&self.plain[first.end..head.end]);
        }
    }

    /// git's `merge_hunks`: coalesce selected hunks that overlap after an edit.
    /// Returns non-zero when `merged` should be rendered instead of `hunk[i]`,
    /// mirroring the C caller's truthiness test (which also takes the `-1`
    /// error return as "merged").
    fn merge_hunks(&mut self, file_idx: usize, hunk_index: &mut usize, use_all: bool, merged: &mut Hunk) -> i32 {
        let start_index = *hunk_index;
        let mut i = start_index;
        let hunk_nr = self.files[file_idx].hunk.len();
        if !use_all && self.files[file_idx].hunk[i].use_ != Use::Use {
            return 0;
        }
        *merged = self.files[file_idx].hunk[i];
        merged.colored_start = 0;
        merged.colored_end = 0;

        while i + 1 < hunk_nr {
            let hunk = self.files[file_idx].hunk[i + 1];
            let next = hunk.header;
            let header = merged.header;
            if (!use_all && hunk.use_ != Use::Use)
                || header.new_offset as i64 >= next.new_offset as i64 + merged.delta
                || ((header.new_offset + header.new_count) as i64) < next.new_offset as i64 + merged.delta
            {
                break;
            }
            i += 1;

            let delta;
            if merged.start < hunk.start && merged.end > hunk.start {
                merged.end = hunk.end;
                merged.colored_end = hunk.colored_end;
                delta = 0;
            } else {
                // One of the hunks was edited and lives at the tail of `plain`;
                // splice the two line ranges together through a scratch copy.
                let overlapping_line_count = (header.new_offset + header.new_count) as i64
                    - merged.delta
                    - next.new_offset as i64;
                let mut overlap_end = hunk.start;
                let mut overlap_start = overlap_end;
                for j in 0..overlapping_line_count.max(0) as usize {
                    let overlap_next = find_next_line(&self.plain, overlap_end);
                    if overlap_next > hunk.end {
                        // git BUGs here; treat it as a failed merge instead.
                        return -1;
                    }
                    if normalize_marker(&self.plain, overlap_end) != b' ' {
                        eprintln!(
                            "error: expected context line #{} in\n{}",
                            j + 1,
                            String::from_utf8_lossy(&self.plain[hunk.start..hunk.end])
                        );
                        return -1;
                    }
                    overlap_start = overlap_end;
                    overlap_end = overlap_next;
                }
                let len = overlap_end - overlap_start;
                if len > merged.end - merged.start
                    || self.plain[merged.end - len..merged.end] != self.plain[overlap_start..overlap_end]
                {
                    eprintln!(
                        "error: hunks do not overlap:\n{}\n\tdoes not end with:\n{}",
                        String::from_utf8_lossy(&self.plain[merged.start..merged.end]),
                        String::from_utf8_lossy(&self.plain[overlap_start..overlap_end])
                    );
                    return -1;
                }
                if merged.end != self.plain.len() {
                    let start = self.plain.len();
                    let slice = self.plain[merged.start..merged.end].to_vec();
                    self.plain.extend_from_slice(&slice);
                    merged.start = start;
                    merged.end = self.plain.len();
                }
                let tail = self.plain[overlap_end..hunk.end].to_vec();
                self.plain.extend_from_slice(&tail);
                merged.end = self.plain.len();
                merged.splittable_into += hunk.splittable_into;
                delta = merged.delta;
                merged.delta += hunk.delta;
            }

            merged.header.old_count = next.old_offset + next.old_count - header.old_offset;
            merged.header.new_count =
                (next.new_offset as i64 + delta + next.new_count as i64 - header.new_offset as i64) as u64;
        }

        if i == start_index {
            return 0;
        }
        *hunk_index = i;
        1
    }

    /// git's `reassemble_patch`: the header plus every selected hunk, with each
    /// header's new-side offset shifted by the hunks dropped before it.
    fn reassemble_patch(&mut self, file_idx: usize, use_all: bool) -> Vec<u8> {
        let mut out = Vec::new();
        let save_len = self.plain.len();
        let mut delta: i64 = 0;
        self.render_diff_header(file_idx, false, &mut out);

        let mut i = usize::from(self.files[file_idx].mode_change);
        while i < self.files[file_idx].hunk.len() {
            let hunk = self.files[file_idx].hunk[i];
            if !use_all && hunk.use_ != Use::Use {
                delta += hunk.header.old_count as i64 - hunk.header.new_count as i64;
            } else {
                let mut merged = Hunk::default();
                let chosen = if self.merge_hunks(file_idx, &mut i, use_all, &mut merged) != 0 {
                    merged
                } else {
                    hunk
                };
                self.render_hunk(&chosen, delta, false, &mut out);
                self.plain.truncate(save_len);
                delta += chosen.delta;
            }
            i += 1;
        }
        out
    }

    /// git's `split_hunk`: cut one hunk at every first context line following a
    /// run of `+`/`-` lines, redistributing the old/new line counts.
    fn split_hunk(&mut self, file_idx: usize, hunk_index: usize) {
        let colored = !self.colored.is_empty();
        let orig = self.files[file_idx].hunk[hunk_index];
        if orig.splittable_into < 2 {
            return;
        }
        let mut splittable_into = orig.splittable_into;
        let end = orig.end;
        let colored_end = orig.colored_end;
        let mut remaining = orig.header;

        let mut parts: Vec<Hunk> = vec![Hunk::default(); orig.splittable_into];
        // The first split inherits the original's range and header; the rest
        // start zeroed, exactly as git's `memset(hunk + 1, 0, ...)` leaves them.
        parts[0] = Hunk {
            splittable_into: 1,
            use_: Use::Undecided,
            ..orig
        };
        parts[0].header.old_count = 0;
        parts[0].header.new_count = 0;

        let mut h = 0usize;
        let mut first = true;
        let mut current = orig.start;
        let mut colored_current = if colored { orig.colored_start } else { 0 };
        let mut marker = 0u8;
        let mut context_line_count: u64 = 0;

        while splittable_into > 1 {
            let mut ch = normalize_marker(&self.plain, current);
            if ch == 0 {
                break; // git BUGs on a buffer overrun; stop instead.
            }

            // First context line after a run of +/- lines: the next split hunk
            // starts here.
            if (marker == b'-' || marker == b'+') && ch == b' ' {
                first = false;
                parts[h + 1].start = current;
                if colored {
                    parts[h + 1].colored_start = colored_current;
                }
                context_line_count = 0;
            }

            // Still inside the current run, or the very first line.
            let mut advance = marker != b' ' || (ch != b'-' && ch != b'+');
            if !advance && first {
                // The leading context of the hunk belongs to the first split.
                parts[h].header.old_count = context_line_count;
                parts[h].header.new_count = context_line_count;
                context_line_count = 0;
                first = false;
                advance = true;
            }

            if advance {
                if ch == b'\\' {
                    // A `\ No newline` comment attaches to the previous line.
                    ch = if marker != 0 { marker } else { b' ' };
                }
                match ch {
                    b' ' => context_line_count += 1,
                    b'-' => parts[h].header.old_count += 1,
                    b'+' => parts[h].header.new_count += 1,
                    _ => break,
                }
                marker = ch;
                current = find_next_line(&self.plain, current);
                if colored {
                    colored_current = find_next_line(&self.colored, colored_current);
                }
                continue;
            }

            // A new hunk starts here; the context line is shared with the
            // previous one.
            remaining.old_offset += parts[h].header.old_count;
            remaining.old_count -= parts[h].header.old_count;
            remaining.new_offset += parts[h].header.new_count;
            remaining.new_count -= parts[h].header.new_count;

            parts[h + 1].header.old_offset = parts[h].header.old_offset + parts[h].header.old_count;
            parts[h + 1].header.new_offset = parts[h].header.new_offset + parts[h].header.new_count;

            parts[h].header.old_count += context_line_count;
            parts[h].header.new_count += context_line_count;
            parts[h].end = current;
            if colored {
                parts[h].colored_end = colored_current;
            }

            h += 1;
            parts[h].splittable_into = 1;
            parts[h].use_ = Use::Undecided;
            parts[h].header.old_count = context_line_count;
            parts[h].header.new_count = context_line_count;
            context_line_count = 0;

            splittable_into -= 1;
            marker = ch;
        }

        // The last split simply gets the rest.
        parts[h].header.old_count = remaining.old_count;
        parts[h].header.new_count = remaining.new_count;
        parts[h].end = end;
        if colored {
            parts[h].colored_end = colored_end;
        }

        self.files[file_idx].hunk.splice(hunk_index..hunk_index + 1, parts);
    }

    /// git's `recolor_hunk`: paint an edited hunk from its plain text, since no
    /// colored rendering of it exists.
    fn recolor_hunk(&mut self, hunk: &mut Hunk) {
        if self.colored.is_empty() {
            return;
        }
        hunk.colored_start = self.colored.len();
        let mut current = hunk.start;
        while current < hunk.end {
            let mut eol = current;
            while eol < hunk.end && self.plain[eol] != b'\n' {
                eol += 1;
            }
            let next = eol + usize::from(eol < hunk.end);
            if eol > current && self.plain[eol - 1] == b'\r' {
                eol -= 1;
            }
            let color = match self.plain[current] {
                b'-' => self.cfg.file_old_color.clone(),
                b'+' => self.cfg.file_new_color.clone(),
                _ => self.cfg.context_color.clone(),
            };
            self.colored.extend_from_slice(color.as_bytes());
            let body = self.plain[current..eol].to_vec();
            self.colored.extend_from_slice(&body);
            let reset = self.cfg.reset_color_diff.clone();
            self.colored.extend_from_slice(reset.as_bytes());
            if next > eol {
                let tail = self.plain[eol..next].to_vec();
                self.colored.extend_from_slice(&tail);
            }
            current = next;
        }
        hunk.colored_end = self.colored.len();
    }
}

// ---------------------------------------------------------------------------
// manual hunk editing
// ---------------------------------------------------------------------------

impl State<'_> {
    /// git's `edit_hunk_manually`. Returns `Ok(false)` when the user emptied the
    /// buffer (edit abandoned), `Ok(true)` when an edited hunk was recorded.
    fn edit_hunk_manually(&mut self, hunk: &mut Hunk) -> Result<bool> {
        let comment = comment_line_str(self.repo);
        let mut buf: Vec<u8> = Vec::new();
        add_commented_lines(
            &mut buf,
            "Manual hunk edit mode -- see bottom for a quick guide.\n",
            &comment,
        );
        self.render_hunk(hunk, 0, false, &mut buf);
        let (rm, del) = if self.mode.is_reverse { ('+', '-') } else { ('-', '+') };
        add_commented_lines(
            &mut buf,
            &format!(
                "---\n\
                 To remove '{rm}' lines, make them ' ' lines (context).\n\
                 To remove '{del}' lines, delete them.\n\
                 Lines starting with {comment} will be removed.\n"
            ),
            &comment,
        );
        add_commented_lines(&mut buf, &format!("{}\n", self.mode.edit_hunk_hint), &comment);
        add_commented_lines(
            &mut buf,
            "If it does not apply cleanly, you will be given an opportunity to\n\
             edit again.  If all lines of the hunk are removed, then the edit is\n\
             aborted and the hunk is left unchanged.\n",
            &comment,
        );

        let path = self.repo.git_dir().join("addp-hunk-edit.diff");
        std::fs::write(&path, &buf)?;
        launch_editor(self.repo, &path)?;
        let edited = std::fs::read(&path)?;

        // Strip the commented lines.
        hunk.start = self.plain.len();
        let mut i = 0usize;
        while i < edited.len() {
            let next = find_next_line(&edited, i);
            if !edited[i..].starts_with(comment.as_bytes()) {
                let line = edited[i..next].to_vec();
                self.plain.extend_from_slice(&line);
            }
            i = next;
        }
        hunk.end = self.plain.len();
        if hunk.end == hunk.start {
            return Ok(false);
        }
        self.recolor_hunk(hunk);
        if self.plain[hunk.start] == b'@' {
            self.parse_hunk_header(hunk)
                .map_err(|_| anyhow::anyhow!("could not parse hunk header"))?;
        }
        Ok(true)
    }

    /// git's `recount_edited_hunk`: recompute the line counts and the delta the
    /// edit introduced.
    fn recount_edited_hunk(&self, hunk: &mut Hunk, orig_old_count: u64, orig_new_count: u64) -> i64 {
        hunk.splittable_into = 0;
        hunk.header.old_count = 0;
        hunk.header.new_count = 0;
        let mut marker = b' ';
        let mut i = hunk.start;
        while i < hunk.end {
            match normalize_marker(&self.plain, i) {
                b'-' => {
                    hunk.header.old_count += 1;
                    if marker == b' ' {
                        hunk.splittable_into += 1;
                    }
                    marker = b'-';
                }
                b'+' => {
                    hunk.header.new_count += 1;
                    if marker == b' ' {
                        hunk.splittable_into += 1;
                    }
                    marker = b'+';
                }
                b' ' => {
                    hunk.header.old_count += 1;
                    hunk.header.new_count += 1;
                    marker = b' ';
                }
                _ => {}
            }
            i = find_next_line(&self.plain, i);
        }
        orig_old_count as i64 - orig_new_count as i64 - hunk.header.old_count as i64
            + hunk.header.new_count as i64
    }

    /// git's `run_apply_check`.
    fn run_apply_check(&mut self, file_idx: usize) -> bool {
        let patch = self.reassemble_patch(file_idx, true);
        let mut args = vec!["apply".to_string(), "--check".to_string()];
        args.extend(self.mode.apply_check_args.iter().map(|s| s.to_string()));
        matches!(
            run_git_in_index(&args, Some(&patch), false, self.workdir(), self.index_file.as_deref()),
            Ok((true, _))
        )
    }

    /// git's `edit_hunk_loop`: edit, validate, re-offer. `Ok(true)` means the
    /// edited hunk is in place and should be marked used.
    fn edit_hunk_loop(&mut self, file_idx: usize, hunk_index: Option<usize>) -> bool {
        let plain_len = self.plain.len();
        let colored_len = self.colored.len();
        let backup = *self.hunk_at(file_idx, hunk_index);
        loop {
            let mut hunk = *self.hunk_at(file_idx, hunk_index);
            match self.edit_hunk_manually(&mut hunk) {
                Ok(false) => {
                    *self.hunk_at_mut(file_idx, hunk_index) = backup;
                    return false;
                }
                Ok(true) => {
                    let d = self.recount_edited_hunk(
                        &mut hunk,
                        backup.header.old_count,
                        backup.header.new_count,
                    );
                    hunk.delta += d;
                    *self.hunk_at_mut(file_idx, hunk_index) = hunk;
                    if self.run_apply_check(file_idx) {
                        return true;
                    }
                }
                Err(e) => {
                    eprintln!("error: {e}");
                }
            }
            // Drop the edit (it was appended to `plain`) and offer another go.
            self.plain.truncate(plain_len);
            self.colored.truncate(colored_len);
            *self.hunk_at_mut(file_idx, hunk_index) = backup;
            if !self.prompt_yesno(
                "Your edited hunk does not apply. Edit again (saying \"no\" discards!) [y/n]? ",
            ) {
                return false;
            }
        }
    }

    /// The addressed hunk, or the file's `head` pseudo-hunk.
    fn hunk_at(&self, file_idx: usize, hunk_index: Option<usize>) -> &Hunk {
        match hunk_index {
            None => &self.files[file_idx].head,
            Some(i) => &self.files[file_idx].hunk[i],
        }
    }

    /// Mutable [`Self::hunk_at`].
    fn hunk_at_mut(&mut self, file_idx: usize, hunk_index: Option<usize>) -> &mut Hunk {
        match hunk_index {
            None => &mut self.files[file_idx].head,
            Some(i) => &mut self.files[file_idx].hunk[i],
        }
    }

    /// git's `prompt_yesno`: loop until the answer starts with `y` or `n`.
    /// EOF answers "no".
    fn prompt_yesno(&mut self, prompt: &str) -> bool {
        loop {
            color_print(&self.cfg.prompt_color.clone(), prompt);
            let _ = std::io::stdout().flush();
            if self.read_single_character().is_none() {
                return false;
            }
            match self.answer.first().map(|c| c.to_ascii_lowercase()) {
                Some(b'n') => return false,
                Some(b'y') => return true,
                _ => {}
            }
        }
    }
}

/// `core.commentString` / `core.commentChar`, defaulting to `#`.
fn comment_line_str(repo: &gix::Repository) -> String {
    let snap = repo.config_snapshot();
    snap.string("core.commentString")
        .or_else(|| snap.string("core.commentChar"))
        .map(|v| v.to_string())
        .filter(|v| !v.is_empty() && v != "auto")
        .unwrap_or_else(|| "#".to_string())
}

/// git's `strbuf_add_commented_lines`: prefix every line, adding a space unless
/// the line already starts with a newline or a tab.
fn add_commented_lines(out: &mut Vec<u8>, text: &str, prefix: &str) {
    let buf = text.as_bytes();
    let mut i = 0usize;
    while i < buf.len() {
        let next = find_next_line(buf, i);
        out.extend_from_slice(prefix.as_bytes());
        if buf[i] != b'\n' && buf[i] != b'\t' {
            out.push(b' ');
        }
        out.extend_from_slice(&buf[i..next]);
        i = next;
    }
    complete_line(out);
}

/// git's `git_editor()` chain, run through the shell so `core.editor = "code -w"`
/// works.
fn launch_editor(repo: &gix::Repository, path: &std::path::Path) -> Result<()> {
    let dumb = std::env::var("TERM").map(|t| t == "dumb").unwrap_or(true);
    let snap = repo.config_snapshot();
    let editor = std::env::var("GIT_EDITOR")
        .ok()
        .filter(|v| !v.is_empty())
        .or_else(|| snap.string("core.editor").map(|v| v.to_string()))
        .or_else(|| {
            if dumb {
                None
            } else {
                std::env::var("VISUAL").ok().filter(|v| !v.is_empty())
            }
        })
        .or_else(|| std::env::var("EDITOR").ok().filter(|v| !v.is_empty()))
        .or_else(|| if dumb { None } else { Some("vi".to_string()) });
    let Some(editor) = editor else {
        crate::git_fatal!("terminal is dumb, but EDITOR unset");
    };
    if editor == ":" {
        return Ok(());
    }
    let status = crate::external::prepare_shell_cmd_str(&editor, [path]).status()?;
    if !status.success() {
        crate::git_fatal!("There was a problem with the editor '{editor}'.");
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// applying
// ---------------------------------------------------------------------------

impl State<'_> {
    /// git's `apply_for_checkout`: index and worktree must both accept the
    /// patch, else fall back to the worktree alone (with a prompt) or just show
    /// the diff.
    fn apply_for_checkout(&mut self, diff: &[u8], is_reverse: bool) {
        let rev: &[&str] = if is_reverse { &["-R"] } else { &[] };
        let argv = |extra: &[&str]| -> Vec<String> {
            let mut v = vec!["apply".to_string()];
            v.extend(extra.iter().map(|s| s.to_string()));
            v.extend(rev.iter().map(|s| s.to_string()));
            v
        };
        let dir = self.repo.workdir().map(|p| p.to_owned());
        let index = self.index_file.clone();
        let check = |extra: &[&str]| {
            matches!(
                run_git_in_index(&argv(extra), Some(diff), false, dir.as_deref(), index.as_deref()),
                Ok((true, _))
            )
        };
        let applies_index = check(&["--cached", "--check"]);
        let applies_worktree = check(&["--check"]);

        if applies_worktree && applies_index {
            let _ =
                run_git_in_index(&argv(&["--cached"]), Some(diff), false, dir.as_deref(), index.as_deref());
            let _ = run_git_in_index(&argv(&[]), Some(diff), false, dir.as_deref(), index.as_deref());
            return;
        }
        if !applies_index {
            self.err("The selected hunks do not apply to the index!");
            if self.prompt_yesno("Apply them to the worktree anyway? ") {
                let _ = run_git_in_index(&argv(&[]), Some(diff), false, dir.as_deref(), index.as_deref());
                return;
            }
            self.err("Nothing was applied.\n");
        } else {
            // As a last resort, show the diff to the user.
            let _ = std::io::stdout().write_all(diff);
        }
    }

    /// git's `apply_patch`: feed the reassembled selection to `git apply`.
    fn apply_patch(&mut self, file_idx: usize) {
        let f = &self.files[file_idx];
        let any = f.hunk.iter().any(|h| h.use_ == Use::Use);
        if !(any || (f.hunk.is_empty() && f.head.use_ == Use::Use)) {
            return;
        }
        let patch = self.reassemble_patch(file_idx, false);
        if self.mode.apply_for_checkout {
            let reverse = self.mode.is_reverse;
            self.apply_for_checkout(&patch, reverse);
        } else {
            let mut args = vec!["apply".to_string()];
            args.extend(self.mode.apply_args.iter().map(|s| s.to_string()));
            let dir = self.repo.workdir().map(|p| p.to_owned());
            let index = self.index_file.as_deref();
            if !matches!(
                run_git_in_index(&args, Some(&patch), false, dir.as_deref(), index),
                Ok((true, _))
            ) {
                eprintln!("error: 'git apply' failed");
            }
        }
    }
}

// ---------------------------------------------------------------------------
// the `g` hunk picker
// ---------------------------------------------------------------------------

/// Width of the `-a,b +c,d` column in the `g` listing.
const SUMMARY_HEADER_WIDTH: usize = 20;
/// Maximum width of one `g` listing line.
const SUMMARY_LINE_WIDTH: usize = 80;
/// How many hunks one `g` page lists.
const DISPLAY_HUNKS_LINES: usize = 20;

impl State<'_> {
    /// git's `summarize_hunk`: the line range, padded, then the hunk's first
    /// non-context line.
    fn summarize_hunk(&self, hunk: &Hunk, out: &mut Vec<u8>) {
        let len = out.len();
        let h = &hunk.header;
        out.extend_from_slice(
            format!(
                " -{},{} +{},{} ",
                h.old_offset, h.old_count, h.new_offset, h.new_count
            )
            .as_bytes(),
        );
        if out.len() - len < SUMMARY_HEADER_WIDTH {
            out.resize(len + SUMMARY_HEADER_WIDTH, b' ');
        }
        let mut i = hunk.start;
        while i < hunk.end && self.plain[i] == b' ' {
            i = find_next_line(&self.plain, i);
        }
        if i < hunk.end {
            let next = find_next_line(&self.plain, i);
            out.extend_from_slice(&self.plain[i..next]);
        }
        if out.len() - len > SUMMARY_LINE_WIDTH {
            out.truncate(len + SUMMARY_LINE_WIDTH);
        }
        complete_line(out);
    }

    /// git's `display_hunks`: one page of the `g` listing.
    fn display_hunks(&mut self, file_idx: usize, start_index: usize) -> usize {
        let hunk_nr = self.files[file_idx].hunk.len();
        let end_index = (start_index + DISPLAY_HUNKS_LINES).min(hunk_nr);
        let mut i = start_index;
        while i < end_index {
            let hunk = self.files[file_idx].hunk[i];
            i += 1;
            let mut buf = Vec::new();
            let marker = match hunk.use_ {
                Use::Use => '+',
                Use::Skip => '-',
                Use::Undecided => ' ',
            };
            buf.extend_from_slice(format!("{marker}{i:2}: ").as_bytes());
            self.summarize_hunk(&hunk, &mut buf);
            let _ = std::io::stdout().write_all(&buf);
            self.buf = buf;
        }
        end_index
    }
}

// ---------------------------------------------------------------------------
// input
// ---------------------------------------------------------------------------

impl State<'_> {
    /// git's `read_single_character`. `None` is EOF. In single-key mode the
    /// keystroke is echoed, since the terminal did not (and the fall-back line
    /// reader echoes too — git prints unconditionally).
    fn read_single_character(&mut self) -> Option<()> {
        if self.cfg.single_key {
            let res = self.read_key_without_echo();
            let echo = match res {
                Some(()) => String::from_utf8_lossy(&self.answer).into_owned(),
                None => String::new(),
            };
            println!("{echo}");
            return res;
        }
        self.read_line_interactively()
    }

    /// git's `git_read_line_interactively`: a line with its trailing newline
    /// (and CR) removed. `None` is EOF.
    fn read_line_interactively(&mut self) -> Option<()> {
        let _ = std::io::stdout().flush();
        let mut line = String::new();
        match std::io::stdin().read_line(&mut line) {
            Ok(0) | Err(_) => None,
            Ok(_) => {
                // `strbuf_getline_lf` drops the LF, `strbuf_trim_trailing_newline`
                // then drops one CR — exactly one of each, never a run.
                if line.ends_with('\n') {
                    line.pop();
                    if line.ends_with('\r') {
                        line.pop();
                    }
                }
                self.answer = line.into_bytes();
                Some(())
            }
        }
    }

    /// git's `read_key_without_echo`, restricted to stdin (`SAVE_TERM_STDIN`).
    /// When the terminal cannot be put in non-canonical mode — a pipe, or any
    /// platform without termios — it warns once and reads a whole line instead,
    /// exactly as git does. `None` is EOF.
    fn read_key_without_echo(&mut self) -> Option<()> {
        if self.single_key_warned {
            return self.read_line_interactively();
        }
        let Some(saved) = enable_non_canonical() else {
            eprintln!(
                "warning: reading single keystrokes not supported on this platform; \
                 reading line instead"
            );
            self.single_key_warned = true;
            return self.read_line_interactively();
        };
        let _ = std::io::stdout().flush();
        let mut byte = [0u8; 1];
        let read = std::io::stdin().read(&mut byte);
        let out = match read {
            Ok(1) => {
                self.answer.clear();
                if byte[0] == 0x1b {
                    // An escape sequence: git renders the ESC as `^[` and keeps
                    // reading. Drain whatever already arrived; a partial
                    // sequence is an unknown command either way.
                    self.answer.extend_from_slice(b"^[");
                    let mut extra = [0u8; 32];
                    if let Ok(n) = read_nonblocking(&mut extra) {
                        self.answer.extend_from_slice(&extra[..n]);
                    }
                } else {
                    self.answer.push(byte[0]);
                }
                Some(())
            }
            _ => None,
        };
        restore_term(&saved);
        out
    }
}

/// Put stdin in non-canonical, no-echo mode, returning the previous settings.
/// `None` when stdin is not a terminal (git's `enable_non_canonical` failure).
fn enable_non_canonical() -> Option<libc::termios> {
    if !std::io::stdin().is_terminal() {
        return None;
    }
    // SAFETY: `tcgetattr`/`tcsetattr` on fd 0, which we own; the struct is
    // fully initialized by `tcgetattr` before it is read.
    unsafe {
        let mut old: libc::termios = std::mem::zeroed();
        if libc::tcgetattr(libc::STDIN_FILENO, &mut old) < 0 {
            return None;
        }
        let mut raw = old;
        raw.c_lflag &= !(libc::ICANON | libc::ECHO);
        raw.c_cc[libc::VMIN] = 1;
        raw.c_cc[libc::VTIME] = 0;
        if libc::tcsetattr(libc::STDIN_FILENO, libc::TCSAFLUSH, &raw) < 0 {
            return None;
        }
        Some(old)
    }
}

/// git's `restore_term`.
fn restore_term(saved: &libc::termios) {
    // SAFETY: restoring the settings `enable_non_canonical` captured on fd 0.
    unsafe {
        libc::tcsetattr(libc::STDIN_FILENO, libc::TCSAFLUSH, saved);
    }
}

/// Read whatever is already buffered on stdin without blocking.
fn read_nonblocking(buf: &mut [u8]) -> std::io::Result<usize> {
    // SAFETY: toggling O_NONBLOCK on fd 0 around a single read, restored after.
    unsafe {
        let flags = libc::fcntl(libc::STDIN_FILENO, libc::F_GETFL);
        if flags < 0 {
            return Ok(0);
        }
        libc::fcntl(libc::STDIN_FILENO, libc::F_SETFL, flags | libc::O_NONBLOCK);
        let n = libc::read(libc::STDIN_FILENO, buf.as_mut_ptr().cast(), buf.len());
        libc::fcntl(libc::STDIN_FILENO, libc::F_SETFL, flags);
        Ok(if n > 0 { n as usize } else { 0 })
    }
}

/// Page one rendered hunk: git installs the pager over fd 1 and waits for it;
/// this feeds the same bytes to the same pager as a one-shot child.
fn page_bytes(repo: &gix::Repository, data: &[u8]) {
    let cfg = repo.config_snapshot();
    let program = crate::pager::resolve_pager(Some(&cfg));
    if program.is_empty() || program == "cat" {
        let _ = std::io::stdout().write_all(data);
        return;
    }
    let mut cmd = crate::external::prepare_shell_cmd_str(&program, crate::external::NO_ARGS);
    cmd.stdin(Stdio::piped());
    if std::env::var_os("LESS").is_none() {
        cmd.env("LESS", "FRX");
    }
    if std::env::var_os("LV").is_none() {
        cmd.env("LV", "-c");
    }
    let Ok(mut child) = cmd.spawn() else {
        let _ = std::io::stdout().write_all(data);
        return;
    };
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(data);
    }
    let _ = child.wait();
}

// ---------------------------------------------------------------------------
// the prompt loop
// ---------------------------------------------------------------------------

/// git's `dec_mod`.
fn dec_mod(a: usize, m: usize) -> usize {
    if a > 0 {
        a - 1
    } else {
        m - 1
    }
}

/// git's `inc_mod`.
fn inc_mod(a: usize, m: usize) -> usize {
    if a + 1 < m {
        a + 1
    } else {
        0
    }
}

impl State<'_> {
    /// git's `patch_update_file`: the whole per-file prompt loop. Returns the
    /// index of the next file to visit; `files.len()` means "quit".
    fn patch_update_file(&mut self, idx: usize, disallow_edit: bool) -> usize {
        let mut hunk_index = 0usize;
        let mut rendered_hunk_index: i64 = -1;
        let colored = !self.colored.is_empty();
        let mut use_pager = false;
        let mut all_decided = false;
        let mut patch_update_resp = idx;

        // Empty added files have no hunks.
        if self.files[idx].hunk.is_empty() && !self.files[idx].added {
            return patch_update_resp + 1;
        }

        let mut header = Vec::new();
        self.render_diff_header(idx, colored, &mut header);
        let _ = std::io::stdout().write_all(&header);

        loop {
            let hunk_nr = self.files[idx].hunk.len();
            let mut allow_goto_previous_hunk = false;
            let mut allow_goto_previous_undecided = false;
            let mut allow_goto_next_hunk = false;
            let mut allow_goto_next_undecided = false;
            let mut allow_search_and_goto = false;
            let mut allow_split = false;
            let mut allow_edit = false;
            let mut allow_goto_previous_file = false;
            let mut allow_goto_next_file = false;

            if hunk_index >= hunk_nr {
                hunk_index = 0;
            }
            let cur: Option<usize> = if hunk_nr != 0 { Some(hunk_index) } else { None };
            let mut undecided_previous: i64 = -1;
            let mut undecided_next: i64 = -1;

            if hunk_nr != 0 {
                let mut i = dec_mod(hunk_index, hunk_nr);
                while i != hunk_index {
                    if self.files[idx].hunk[i].use_ == Use::Undecided {
                        undecided_previous = i as i64;
                        break;
                    }
                    i = dec_mod(i, hunk_nr);
                }
                let mut i = inc_mod(hunk_index, hunk_nr);
                while i != hunk_index {
                    if self.files[idx].hunk[i].use_ == Use::Undecided {
                        undecided_next = i as i64;
                        break;
                    }
                    i = inc_mod(i, hunk_nr);
                }
            }

            if undecided_previous < 0 && undecided_next < 0 && self.hunk_at(idx, cur).use_ != Use::Undecided {
                if !self.cfg.auto_advance {
                    all_decided = true;
                } else {
                    patch_update_resp += 1;
                    break;
                }
            }

            self.buf.clear();
            if hunk_nr != 0 {
                if rendered_hunk_index != hunk_index as i64 {
                    let hunk = self.files[idx].hunk[hunk_index];
                    let mut out = Vec::new();
                    self.render_hunk(&hunk, 0, colored, &mut out);
                    if use_pager {
                        page_bytes(self.repo, &out);
                        use_pager = false;
                    } else {
                        let _ = std::io::stdout().write_all(&out);
                    }
                    rendered_hunk_index = hunk_index as i64;
                }

                self.buf.clear();
                if undecided_previous >= 0 {
                    allow_goto_previous_undecided = true;
                    self.buf.extend_from_slice(b",k");
                }
                if hunk_nr > 1 {
                    allow_goto_previous_hunk = true;
                    self.buf.extend_from_slice(b",K");
                }
                if undecided_next >= 0 {
                    allow_goto_next_undecided = true;
                    self.buf.extend_from_slice(b",j");
                }
                if hunk_nr > 1 {
                    allow_goto_next_hunk = true;
                    self.buf.extend_from_slice(b",J");
                }
                if hunk_nr > 1 {
                    allow_search_and_goto = true;
                    self.buf.extend_from_slice(b",g,/");
                }
                if self.files[idx].hunk[hunk_index].splittable_into > 1 {
                    allow_split = true;
                    self.buf.extend_from_slice(b",s");
                }
                if !disallow_edit
                    && hunk_index + 1 > usize::from(self.files[idx].mode_change)
                    && !self.files[idx].deleted
                {
                    allow_edit = true;
                    self.buf.extend_from_slice(b",e");
                }
                if !self.cfg.auto_advance && self.files.len() > 1 {
                    allow_goto_next_file = true;
                    self.buf.extend_from_slice(b",>");
                    allow_goto_previous_file = true;
                    self.buf.extend_from_slice(b",<");
                }
                self.buf.extend_from_slice(b",p,P");
            }

            let prompt_mode_type = if self.files[idx].deleted {
                PROMPT_DELETION
            } else if self.files[idx].added {
                PROMPT_ADDITION
            } else if self.files[idx].mode_change && hunk_index == 0 {
                PROMPT_MODE_CHANGE
            } else {
                PROMPT_HUNK
            };

            let decision = match self.hunk_at(idx, cur).use_ {
                Use::Use => " (was: y)",
                Use::Skip => " (was: n)",
                Use::Undecided => "",
            };
            let keys = String::from_utf8_lossy(&self.buf).into_owned();
            {
                let mut out = std::io::stdout();
                let _ = write!(
                    out,
                    "{}({}/{}) ",
                    self.cfg.prompt_color,
                    hunk_index + 1,
                    if hunk_nr != 0 { hunk_nr } else { 1 }
                );
                let _ = out.write_all(
                    fmt2(self.mode.prompt_mode[prompt_mode_type], decision, &keys).as_bytes(),
                );
                if !self.cfg.reset_color_interactive.is_empty() {
                    let _ = out.write_all(self.cfg.reset_color_interactive.as_bytes());
                }
                let _ = out.flush();
            }

            if self.read_single_character().is_none() {
                patch_update_resp = self.files.len();
                break;
            }
            if self.answer.is_empty() {
                continue;
            }
            let raw0 = self.answer[0];
            let ch = raw0.to_ascii_lowercase();
            let answer_text = String::from_utf8_lossy(&self.answer).into_owned();

            if self.answer.len() != 1 && ch != b'g' && ch != b'/' {
                self.err(&format!("Only one letter is expected, got '{answer_text}'"));
                continue;
            }

            let mut soft_increment = false;
            if ch == b'y' {
                self.hunk_at_mut(idx, cur).use_ = Use::Use;
                soft_increment = true;
            } else if ch == b'n' {
                self.hunk_at_mut(idx, cur).use_ = Use::Skip;
                soft_increment = true;
            } else if ch == b'a' || ch == b'd' {
                let decision = if ch == b'a' { Use::Use } else { Use::Skip };
                if hunk_nr != 0 {
                    while hunk_index < hunk_nr {
                        if self.files[idx].hunk[hunk_index].use_ == Use::Undecided {
                            self.files[idx].hunk[hunk_index].use_ = decision;
                        }
                        hunk_index += 1;
                    }
                    hunk_index = self.files[idx]
                        .hunk
                        .iter()
                        .position(|h| h.use_ == Use::Undecided)
                        .unwrap_or(0);
                } else if self.files[idx].head.use_ == Use::Undecided {
                    self.files[idx].head.use_ = decision;
                }
            } else if ch == b'q' {
                patch_update_resp = self.files.len();
                break;
            } else if !self.cfg.auto_advance && raw0 == b'>' {
                if allow_goto_next_file {
                    patch_update_resp = if patch_update_resp == self.files.len() - 1 {
                        0
                    } else {
                        patch_update_resp + 1
                    };
                    break;
                }
                self.err("No next file");
                continue;
            } else if !self.cfg.auto_advance && raw0 == b'<' {
                if allow_goto_previous_file {
                    patch_update_resp = if patch_update_resp == 0 {
                        self.files.len() - 1
                    } else {
                        patch_update_resp - 1
                    };
                    break;
                }
                self.err("No previous file");
                continue;
            } else if raw0 == b'K' {
                if allow_goto_previous_hunk {
                    hunk_index = dec_mod(hunk_index, hunk_nr);
                } else {
                    self.err("No other hunk");
                }
            } else if raw0 == b'J' {
                if allow_goto_next_hunk {
                    hunk_index += 1;
                } else {
                    self.err("No other hunk");
                }
            } else if raw0 == b'k' {
                if allow_goto_previous_undecided {
                    hunk_index = undecided_previous as usize;
                } else {
                    self.err("No other undecided hunk");
                }
            } else if raw0 == b'j' {
                if allow_goto_next_undecided {
                    hunk_index = undecided_next as usize;
                } else {
                    self.err("No other undecided hunk");
                }
            } else if raw0 == b'g' {
                if !allow_search_and_goto {
                    self.err("No other hunks to goto");
                    continue;
                }
                self.answer.remove(0);
                trim(&mut self.answer);
                let mut i = (hunk_index as i64 - (DISPLAY_HUNKS_LINES / 2) as i64)
                    .max(usize::from(self.files[idx].mode_change) as i64)
                    as usize;
                while self.answer.is_empty() {
                    i = self.display_hunks(idx, i);
                    print!(
                        "{}",
                        if i < hunk_nr {
                            "go to which hunk (<ret> to see more)? "
                        } else {
                            "go to which hunk? "
                        }
                    );
                    let _ = std::io::stdout().flush();
                    if self.read_line_interactively().is_none() {
                        break;
                    }
                }
                trim(&mut self.answer);
                let text = String::from_utf8_lossy(&self.answer).into_owned();
                match text.parse::<u64>() {
                    Ok(n) if n > 0 && n <= hunk_nr as u64 => hunk_index = n as usize - 1,
                    Ok(_) => {
                        let plural = if hunk_nr == 1 { "hunk" } else { "hunks" };
                        self.err(&format!("Sorry, only {hunk_nr} {plural} available."));
                    }
                    Err(_) => self.err(&format!("Invalid number: '{text}'")),
                }
            } else if raw0 == b'/' {
                if !allow_search_and_goto {
                    self.err("No other hunks to search");
                    continue;
                }
                self.answer.remove(0);
                trim_trailing_newline(&mut self.answer);
                if self.answer.is_empty() {
                    print!("search for regex? ");
                    let _ = std::io::stdout().flush();
                    if self.read_line_interactively().is_none() {
                        break;
                    }
                    trim_trailing_newline(&mut self.answer);
                    if self.answer.is_empty() {
                        continue;
                    }
                }
                let pattern = String::from_utf8_lossy(&self.answer).into_owned();
                let regex = match regex::bytes::RegexBuilder::new(&pattern)
                    .unicode(false)
                    .multi_line(true)
                    .build()
                {
                    Ok(r) => r,
                    Err(e) => {
                        self.err(&format!("Malformed search regexp {pattern}: {e}"));
                        continue;
                    }
                };
                let mut i = hunk_index;
                loop {
                    let hunk = self.files[idx].hunk[i];
                    let mut out = std::mem::take(&mut self.buf);
                    self.render_hunk(&hunk, 0, false, &mut out);
                    let hit = regex.is_match(&out);
                    self.buf = out;
                    if hit {
                        break;
                    }
                    i += 1;
                    if i == hunk_nr {
                        i = 0;
                    }
                    if i != hunk_index {
                        continue;
                    }
                    self.err("No hunk matches the given pattern");
                    break;
                }
                hunk_index = i;
            } else if raw0 == b's' {
                let splittable_into = self.files[idx].hunk[hunk_index].splittable_into;
                if !allow_split {
                    self.err("Sorry, cannot split this hunk");
                } else {
                    self.split_hunk(idx, hunk_index);
                    color_println(
                        &self.cfg.header_color.clone(),
                        &format!("Split into {splittable_into} hunks."),
                    );
                    rendered_hunk_index = -1;
                }
            } else if raw0 == b'e' {
                if !allow_edit {
                    self.err("Sorry, cannot edit this hunk");
                } else if self.edit_hunk_loop(idx, cur) {
                    self.hunk_at_mut(idx, cur).use_ = Use::Use;
                    soft_increment = true;
                }
            } else if ch == b'p' {
                rendered_hunk_index = -1;
                use_pager = raw0 == b'P';
            } else if raw0 == b'?' {
                let help_color = self.cfg.help_color.clone();
                color_print(&help_color, self.mode.help_patch_text);
                let keys = String::from_utf8_lossy(&self.buf).into_owned();
                for line in HELP_PATCH_REMAINDER.lines() {
                    if all_decided && line.starts_with("HUNKS SUMMARY") {
                        let total = hunk_nr;
                        let used = self.files[idx].hunk.iter().filter(|h| h.use_ == Use::Use).count();
                        let skipped = self.files[idx].hunk.iter().filter(|h| h.use_ == Use::Skip).count();
                        // git formats the whole remaining help string here, so
                        // the line's own trailing newline survives *and*
                        // `color_fprintf_ln` adds one — hence the blank line.
                        color_println(
                            &help_color,
                            &format!("HUNKS SUMMARY - Hunks: {total}, USE: {used}, SKIP: {skipped}\n"),
                        );
                    }
                    let first = line.as_bytes()[0];
                    if first != b'?' && !keys.as_bytes().contains(&first) {
                        continue;
                    }
                    color_println(&help_color, line);
                }
            } else {
                self.err(&format!("Unknown command '{answer_text}' (use '?' for help)"));
            }

            if soft_increment {
                hunk_index = if undecided_next < 0 {
                    hunk_nr
                } else {
                    undecided_next as usize
                };
            }
        }

        if self.cfg.auto_advance {
            self.apply_patch(idx);
        }
        println!();
        patch_update_resp
    }
}

/// git's `strbuf_trim`.
fn trim(buf: &mut Vec<u8>) {
    while buf.first().is_some_and(|c| c.is_ascii_whitespace()) {
        buf.remove(0);
    }
    while buf.last().is_some_and(|c| c.is_ascii_whitespace()) {
        buf.pop();
    }
}

/// git's `strbuf_trim_trailing_newline`.
fn trim_trailing_newline(buf: &mut Vec<u8>) {
    if buf.last() == Some(&b'\n') {
        buf.pop();
        if buf.last() == Some(&b'\r') {
            buf.pop();
        }
    }
}

// ---------------------------------------------------------------------------
// entry point
// ---------------------------------------------------------------------------

/// git's `run_add_p`'s mode table lookup.
fn select_mode(mode: Mode, revision: Option<&str>) -> &'static PatchMode {
    match mode {
        Mode::Stash => &PATCH_MODE_STASH,
        Mode::Reset => match revision {
            None | Some("HEAD") => &PATCH_MODE_RESET_HEAD,
            Some(_) => &PATCH_MODE_RESET_NOTHEAD,
        },
        Mode::Checkout => match revision {
            None => &PATCH_MODE_CHECKOUT_INDEX,
            Some("HEAD") => &PATCH_MODE_CHECKOUT_HEAD,
            Some(_) => &PATCH_MODE_CHECKOUT_NOTHEAD,
        },
        Mode::Worktree => match revision {
            None => &PATCH_MODE_CHECKOUT_INDEX,
            Some("HEAD") => &PATCH_MODE_WORKTREE_HEAD,
            Some(_) => &PATCH_MODE_WORKTREE_NOTHEAD,
        },
        Mode::Add => &PATCH_MODE_ADD,
    }
}

/// Run the interactive hunk selector — git's `run_add_p`.
///
/// `revision` is the tree-ish the diff is taken against (`None` for the index),
/// and `pathspecs` limits it. Returns git's exit code: 0, or 255 for the `-1`
/// its `cmd_*` returns on a failed parse.
pub(crate) fn run(
    repo: &gix::Repository,
    mode: Mode,
    revision: Option<&str>,
    opts: Options,
    pathspecs: &[String],
) -> Result<ExitCode> {
    Ok(ExitCode::from(run_status(repo, mode, revision, opts, pathspecs)?))
}

/// [`run`] as its raw status byte, for callers that must branch on failure
/// rather than propagate it: `git commit`'s `interactive_add()` wraps
/// `run_add_p()` in `!!` and turns a non-zero result into
/// `die(_("interactive add failed"))`.
pub(crate) fn run_status(
    repo: &gix::Repository,
    mode: Mode,
    revision: Option<&str>,
    opts: Options,
    pathspecs: &[String],
) -> Result<u8> {
    let cfg = match Config::init(repo, &opts) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("fatal: {e}");
            return Ok(128);
        }
    };
    let state = State {
        repo,
        cfg,
        mode: select_mode(mode, revision),
        revision: revision.map(str::to_string),
        buf: Vec::new(),
        answer: Vec::new(),
        plain: Vec::new(),
        colored: Vec::new(),
        files: Vec::new(),
        single_key_warned: false,
        diff_extra: Vec::new(),
        index_file: None,
    };
    run_common(state, opts, pathspecs)
}

/// git's `run_add_p_index`: the same selector over a caller-supplied index file,
/// diffing `revision`'s parent tree against `revision` itself.
///
/// `git history split` is the only caller. The index at `index_file` is what the
/// `apply --cached` children stage into (via `GIT_INDEX_FILE`), so the selection
/// lands there and the repository's own index is never touched; reading the tree
/// back out of it is the caller's job.
pub(crate) fn run_index(
    repo: &gix::Repository,
    index_file: &std::path::Path,
    revision: &str,
    opts: Options,
    pathspecs: &[String],
) -> Result<u8> {
    let cfg = match Config::init(repo, &opts) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("fatal: {e}");
            return Ok(128);
        }
    };
    let mut state = State {
        repo,
        cfg,
        mode: &PATCH_MODE_SPLIT,
        revision: Some(revision.to_string()),
        buf: Vec::new(),
        answer: Vec::new(),
        plain: Vec::new(),
        colored: Vec::new(),
        files: Vec::new(),
        single_key_warned: false,
        diff_extra: Vec::new(),
        index_file: Some(index_file.to_path_buf()),
    };

    // `lookup_commit_reference_by_name(revision)`, then the parent's tree — or
    // the empty tree for a root commit — as the left side of the diff.
    let commit = repo
        .rev_parse_single(revision)
        .ok()
        .and_then(|id| id.object().ok())
        .and_then(|obj| obj.peel_to_commit().ok());
    let Some(commit) = commit else {
        state.err("Revision does not refer to a commit");
        return Ok(1);
    };
    let parent_tree = match commit.parent_ids().next() {
        Some(p) => repo.find_commit(p.detach())?.tree_id()?.detach(),
        None => repo.object_hash().empty_tree(),
    };
    state.diff_extra = vec![parent_tree.to_string()];

    run_common(state, opts, pathspecs)
}

/// git's `run_add_p_common`: parse the diff, walk the files, then report the
/// two whole-run diagnostics.
fn run_common(mut state: State<'_>, opts: Options, pathspecs: &[String]) -> Result<u8> {
    if let Err(e) = state.parse_diff(pathspecs) {
        // An empty message means the diagnostic was already written where git's
        // own `error()` writes it (see `mismatched_output`).
        let msg = e.to_string();
        if !msg.is_empty() {
            eprintln!("error: {msg}");
        }
        // Every caller wraps `run_add_p` in `!!`, so a failure is exit 1.
        return Ok(1);
    }

    let mut binary_count = 0usize;
    let mut i = 0usize;
    while i < state.files.len() {
        if state.files[i].binary && state.files[i].hunk.is_empty() {
            binary_count += 1;
            i += 1;
            continue;
        }
        i = state.patch_update_file(i, opts.disallow_edit);
        if i == state.files.len() {
            break;
        }
    }

    if !state.cfg.auto_advance {
        for i in 0..state.files.len() {
            state.apply_patch(i);
        }
    }

    if state.files.is_empty() {
        state.err("No changes.");
    } else if binary_count == state.files.len() {
        state.err("Only binary files changed.");
    }

    Ok(0)
}
