//! Differential corpus cases for the boundary where git **hands control to a
//! program outside itself** — and, more to the point, for what git does with
//! what comes back.
//!
//! A tool launcher is easy to reproduce badly and hard to reproduce well. Its
//! stdout is usually the tool's own, so a port that spawns *anything* prints the
//! right bytes; what a port gets wrong is invisible there. Whether the tool ran
//! at all. How many times. Which of two configured names selected it. Whether
//! its exit status was honoured or swallowed. Which of the seven arguments git
//! promises it actually received. Every case in this file is chosen so one of
//! those becomes an observable — through the tool's own output when the tool is
//! `echo` of a fixed string, through the file the tool leaves in the worktree
//! where the state probe reads its bytes, or through the exit code when the
//! whole question is "did the launcher stop".
//!
//! # How territory is divided, and what is new here
//!
//! Six modules were already in this area before this one existed. What each
//! owns, and the sentence that says what it left:
//!
//! * **`misc_commands.rs`** owns `difftool --extcmd` with `true`/`false` as the
//!   command, the option-parse refusals, `difftool--helper`'s seven-positional
//!   form, and all of `instaweb` and the argument-error half of `web--browse`.
//!   Its commands succeed or fail and print nothing, so nothing there can
//!   distinguish *one* launch from *five* — and it states that
//!   `GIT_DIFFTOOL_EXTCMD`/`GIT_DIFF_TOOL` are unreachable from a case, which
//!   stopped being true when [`Case::with_env`] arrived: neither is one of
//!   [`crate::env::harden`]'s pins.
//! * **`archive_export.rs`** owns `difftool` driven by `difftool.<tool>.cmd`
//!   through four selection routes (`--tool=`, `diff.tool`, `--gui` +
//!   `diff.guitool`, `difftool.prompt=false`), the global
//!   `difftool.trustExitCode` over an `--extcmd`, and `mergetool`'s
//!   `writeToTemp`, `prompt`, `merge.tool`/`merge.guitool` and `-O<orderfile>`.
//! * **`merge_family.rs`** owns the `mergetool` baseline on
//!   [`Shape::Conflicted`]: take-local / take-remote, `--tool-help`,
//!   `keepBackup=false`, the empty `Linear` advice path.
//! * **`helpers_credentials.rs`** owns the credential *front end* — the four
//!   in-fixture helper shapes (answers / declines / fails / logs), the
//!   `fill`→`get` verb mapping, `credential.useHttpPath`, the empty-value list
//!   reset, `credential-store --file=`, and four `credential.<url>.*` sections
//!   (exact host, other host, scheme mismatch, a seeded username).
//! * **`hooks_identity.rs`** owns `git hook run` on [`Shape::HooksFail`] —
//!   every installed hook's exit status, `--to-stdin=`, `--ignore-missing`,
//!   `--allow-unknown-hook-name`, and `core.hooksPath` — and
//!   **`informational.rs`** owns the succeeding half of the same verb on
//!   [`Shape::Hooked`]: `pre-commit`, `commit-msg`, arguments after `--`,
//!   `--to-stdin=`, `hook list`. Both dispatch from the fixture root.
//! * **`attributes_filters.rs`** owns `GIT_EXTERNAL_DIFF` and the per-driver
//!   `diff.<driver>.command` on `diff` and `log`.
//!
//! What none of them has, and what this file is:
//!
//! * **How many times the tool ran, and on which paths.** `true` and `false`
//!   are silent. `sh -c "echo TOOL"` is not: one line per path git handed it,
//!   so a pathspec, `--cached` and a revision range each change the *count* and
//!   not merely the exit code.
//! * **The three trust-exit-code spellings, told apart.** `--trust-exit-code`
//!   and `--no-trust-exit-code` on the command line, `difftool.trustExitCode`
//!   in configuration — and `difftool.<tool>.trustExitCode`, which looks like
//!   the other two and is **not read at all** by `difftool`. Measured on stock
//!   2.55.0: a `difftool.p.cmd=exit 3` tool exits 0 under
//!   `difftool.p.trustExitCode=true` and 128 under `difftool.trustExitCode=true`.
//!   A port that implements the per-tool key diverges on exactly one of the two.
//! * **`mergetool` on [`Shape::Rerere`]**, which no case had ever pointed it at.
//!   That shape is mid-merge with three unmerged paths and a replayed rerere
//!   resolution, and it is the only fixture that can show where mergetool's file
//!   list *comes from*: with `MERGE_RR` present, `git-mergetool.sh` asks
//!   `git rerere remaining` rather than `diff --diff-filter=U`. Measured on
//!   stock 2.55.0 — the default run offers `fresh.txt` alone and leaves
//!   `rr.txt`/`other.txt` unmerged, and the same run under `rerere.enabled=false`
//!   prints `No files need merging` and resolves *nothing*, because a disabled
//!   `rerere remaining` answers with an empty list rather than with the three
//!   paths `status` reports.
//! * **`credential`'s remaining control words and matching rules**: a helper
//!   that says `quit=1` (fatal, exit 128 — a port that treats it as a decline
//!   runs the next helper and answers), a helper whose output is not
//!   `key=value`, `credential.interactive=false`, and the `credential.<url>.*`
//!   forms the neighbour does not reach — a `*` wildcard host, a bare `*`, a
//!   path component, a username component, and a port.
//! * **`git hook run` from a subdirectory**, and a hook invoked with arguments
//!   that make it rewrite a *tracked* file. [`Shape::Hooked`] exists because a
//!   hook's path and working directory were once resolved against a cwd that had
//!   already moved, and `hook run` is the one verb that dispatches a hook with no
//!   verb's control flow in front of it — yet every existing `hook run` case, in
//!   both modules that emit one, runs from the fixture root.
//! * **`diff.external`**, which appears nowhere else in the corpus. It is the
//!   *unconditional* external diff — no attribute, no environment — and it is
//!   gated per verb in a way a port can get wrong six ways: applied by `diff`,
//!   not by `show` or `log -p` without `--ext-diff`, never by `format-patch`,
//!   never by `diff-tree -p` even *with* `--ext-diff`, and never by any of
//!   `--stat`/`--raw`/`--numstat`/`--name-status`.
//! * **`GIT_SSH_COMMAND` with the argv recorded into the worktree**, which is
//!   what caught the divergence documented in [`ssh_command`].
//! * **`web--browse` with a browser that actually runs.** The neighbour's ten
//!   cases are all argument errors; `browser.<tool>.cmd=echo BROWSE` is the URL
//!   list git decided to pass, on stdout.
//!
//! # The determinism rule this module lives under
//!
//! A tool is admissible here only if its output is a function of git's own
//! inputs. `true`, `false`, `exit 3`, `echo <fixed string>`, `cat` of a path git
//! supplies, and a shell function that writes its `"$@"` to a fixed worktree
//! path are; anything that reads the clock, a pid, a temp directory or the
//! environment is not. Every case below was run twice against stock 2.55.0 in
//! two fresh copies of its shape and the two runs compared before it was written
//! down.
//!
//! ## What that rule excludes, and why — stated rather than quietly skipped
//!
//! * **`mergetool.keepTemporaries` with a tool that fails.** That is the only
//!   combination in which the temporaries survive, and their names carry the
//!   launcher's pid: measured on stock 2.55.0, a failing tool under
//!   `keepTemporaries=true` leaves `conflict_BASE_31607.txt`,
//!   `conflict_LOCAL_31607.txt` and `conflict_REMOTE_31607.txt` untracked, and
//!   31607 is different on the next run and on the other side. The reachable
//!   half is here — `keepTemporaries=true` with a tool that *succeeds*, where
//!   the contract is that nothing survives — and the failing half is not.
//! * **`mergetool --prompt` with the harness's closed stdin.** The declined
//!   prompt leaves the temporaries behind — the cleanup is on the resolved path
//!   — so the same pid-named files land in the worktree. Written, run, and taken
//!   out: two fresh copies of [`Shape::Conflicted`] gave
//!   `conflict_{BACKUP,BASE,LOCAL,REMOTE}_88949.txt` and then `…_90325.txt`, and
//!   the harness independently scored the case `NONDETERMINISTIC` before it was
//!   removed. `archive_export.rs` pins `mergetool.prompt=false`; the declining
//!   direction has no measurable form. The *difftool* prompt does not have this
//!   problem — it writes nothing — and is measured below.
//! * **`mergetool.hideResolved`.** It removes the already-merged hunks from
//!   `$LOCAL` and `$REMOTE` before the tool sees them, so it needs a file that
//!   is *both* auto-merged in one hunk and conflicted in another. No fixture has
//!   one: `Conflicted`'s `conflict.txt` is a one-line add/add, and `Rerere`'s
//!   `rr.txt` and `other.txt` conflict in every hunk that differs at all.
//!   Verified on stock 2.55.0 — `mergetool -y --tool=<cat $LOCAL>` stages
//!   `29f4f56…` with `hideResolved` on and off alike. A case would measure the
//!   flag's parsing and nothing else, so none is written.
//! * **`difftool --dir-diff`'s temporary trees.** Named after the system temp
//!   directory, outside the fixture, invisible to the state probe;
//!   `archive_export.rs` already records this and its `--dir-diff` cases are
//!   exit-code cases for the same reason.
//! * **`core.askPass` and `SSH_ASKPASS`.** `harden` pins `GIT_ASKPASS=true` and
//!   `git_prompt()` takes the first of `GIT_ASKPASS`, `core.askPass`,
//!   `SSH_ASKPASS` that is set, so neither of the other two can ever be
//!   consulted. `helpers_credentials.rs` states this and pins it with two cases
//!   that assert `core.askPass` changes nothing; this module does not repeat it.
//! * **`core.editor`, `core.pager` and `sequence.editor` as *behaviour*.**
//!   `harden` pins `GIT_EDITOR`, `GIT_SEQUENCE_EDITOR` and `GIT_PAGER`, and each
//!   outranks its configuration key. What is left reachable is the *precedence*,
//!   and `git var` is where it shows: three cases below set the losing key and
//!   assert the pinned value comes back. `sequence.editor` is not among them —
//!   `history_rewrite.rs` already pins that one through `rebase -i`.
//! * **`credential-cache` with a daemon, and any URL a resolver could answer.**
//!   Inherited constraints, stated in full by `helpers_credentials.rs`. Every
//!   host here is under `example.invalid` (RFC 6761: never resolves) and the two
//!   `ssh://` cases were timed on both sides before being written down — 0.13s
//!   for the binary under test, no `ssh` process, no lookup.
//! * **`instaweb`.** `misc_commands.rs` covers every form of it that does not
//!   start a server, and there is no form that starts one and stays comparable.

use crate::fixture::Shape;
use crate::runner::Case;

/// Append this subsystem's cases to the corpus.
pub fn cases(out: &mut Vec<Case>) {
    difftool_invocations(out);
    difftool_exit_status(out);
    difftool_selection(out);
    mergetool_rerere(out);
    mergetool_misc(out);
    credential_control_words(out);
    credential_url_matching(out);
    hook_run_placement(out);
    external_diff_gates(out);
    ssh_command(out);
    editor_pager_pins(out);
    web_browse(out);
}

// ---------------------------------------------------------------------------
// Tools
// ---------------------------------------------------------------------------
//
// Each is a literal. Nothing below assembles a command from a fixture path, an
// environment variable or a case parameter, for the reason `archive_export.rs`
// gives: a command that names one side's copy is not the same command on the
// other side.

/// Prints one fixed line per invocation, so the tool's *invocation count* is on
/// stdout. The single most useful tool in this file: `true` cannot tell one
/// launch from five, and five is what a port that ignores a pathspec produces.
const TOOL_ECHO: &str = "--extcmd=sh -c \"echo TOOL\"";
/// Exits 3 — a status neither 0 nor 1, so a port that maps every failure onto 1
/// is visible wherever the status is propagated rather than interpreted.
const TOOL_EXIT3: &str = "--extcmd=sh -c \"exit 3\"";
/// The configured spelling of the same, for the `difftool.<tool>.cmd` route.
const CFG_EXIT3: (&str, &str) = ("difftool.p.cmd", "exit 3");
/// A configured tool that echoes, for the routes that select by name.
const CFG_ECHO: (&str, &str) = ("difftool.p.cmd", "echo RAN");

/// The mergetool used throughout this file: it resolves by taking one side,
/// spelled exactly as `merge_family.rs` and `archive_export.rs` spell it so the
/// three modules describe the same tool the same way. `trustExitCode` beside it
/// is what makes the launcher non-interactive.
const MT_LOCAL: &[(&str, &str)] = &[
    ("mergetool.p.cmd", "cat \"$LOCAL\" > \"$MERGED\""),
    ("mergetool.p.trustExitCode", "true"),
];
/// The pre-image side. On [`Shape::Rerere`]'s `rr.txt` there *is* a merge base,
/// which [`Shape::Conflicted`]'s add/add path does not have — so this is the
/// only place in the corpus where `$BASE` is a file rather than `/dev/null`.
const MT_BASE: &[(&str, &str)] = &[
    ("mergetool.p.cmd", "cat \"$BASE\" > \"$MERGED\""),
    ("mergetool.p.trustExitCode", "true"),
];
/// A tool that refuses, trusted, so the refusal is the launcher's verdict.
const MT_FAIL: &[(&str, &str)] =
    &[("mergetool.p.cmd", "false"), ("mergetool.p.trustExitCode", "true")];

// ---------------------------------------------------------------------------
// difftool: how many times did the tool run, and over what
// ---------------------------------------------------------------------------

/// `difftool` with a tool that says so, once per path.
///
/// Everything else about `difftool` can be reproduced by a port that launches
/// the tool once, or not at all, or once per *file in the tree* — `true` and
/// `false` are silent and their exit code is the same either way. `echo TOOL`
/// makes the launcher's own decision the output: the number of `TOOL` lines is
/// the number of paths git selected, and it moves with `--cached`, with a
/// pathspec, with a revision range and with the shape.
///
/// Measured on stock 2.55.0, counting `TOOL` lines in a fresh copy of each
/// shape: `Dirty` bare 2, `--cached` 1, `-- README.md` 1; `Branched HEAD~1 HEAD`
/// 1; `AwkwardPaths HEAD~1 HEAD` 4; `Renamed HEAD~1 HEAD` 1;
/// `Symlinks HEAD~1 HEAD` 1; `IntentToAdd` bare 4 and `--cached` 2;
/// `Conflicted` bare **0**.
///
/// That last one is a measurement rather than a gap: an unmerged path is not
/// handed to an external diff at all, so a port that offers one prints a `TOOL`
/// line stock does not.
///
/// `-y` throughout, and deliberately: it is the short spelling of `--no-prompt`
/// and it appears nowhere else in the corpus, which spells the flag
/// `--no-prompt` in every difftool case it already had.
fn difftool_invocations(out: &mut Vec<Case>) {
    let dt = |out: &mut Vec<Case>, args: &[&str], shape| {
        out.push(Case::new("difftool", args, shape));
    };

    // The count, and the three things that change it.
    dt(out, &["difftool", "-y", TOOL_ECHO], Shape::Dirty);
    dt(out, &["difftool", "-y", TOOL_ECHO, "--", "README.md"], Shape::Dirty);
    dt(out, &["difftool", "-y", TOOL_ECHO, "--cached"], Shape::Dirty);
    dt(out, &["difftool", "-y", TOOL_ECHO, "HEAD~1", "HEAD"], Shape::Branched);
    dt(out, &["difftool", "-y", TOOL_ECHO, "main", "feature"], Shape::Branched);
    // Shapes whose selection is of a different kind: quote-worthy names, a
    // rename pair, a typechange to a symlink, and a mid-merge tree — where the
    // answer is that the unmerged path is offered to nobody.
    dt(out, &["difftool", "-y", TOOL_ECHO, "HEAD~1", "HEAD"], Shape::AwkwardPaths);
    dt(out, &["difftool", "-y", TOOL_ECHO, "HEAD~1", "HEAD"], Shape::Renamed);
    dt(out, &["difftool", "-y", TOOL_ECHO, "HEAD~1", "HEAD"], Shape::Symlinks);
    dt(out, &["difftool", "-y", TOOL_ECHO], Shape::Conflicted);
    // Intent-to-add: `diff` shows an ITA path as a worktree addition and
    // `--cached` hides it, so the two counts must differ by the ITA entries —
    // 4 against 2 on stock.
    dt(out, &["difftool", "-y", TOOL_ECHO], Shape::IntentToAdd);
    dt(out, &["difftool", "-y", TOOL_ECHO, "--cached"], Shape::IntentToAdd);
    // `--no-index`, which no difftool case in the corpus has. Two worktree paths
    // with no repository involvement, and stock exits 1 because they differ.
    dt(out, &["difftool", "-y", "--no-index", TOOL_ECHO, "README.md", "top.txt"], Shape::Hooked);

    // The prompt path with the harness's closed stdin. `misc_commands.rs` has
    // this argv with a *silent* tool, where "the prompt was declined" and "the
    // tool ran and printed nothing" are the same bytes. With an echoing tool
    // they are not: stock reads EOF, declines both launches, and no `TOOL` line
    // appears at all.
    dt(out, &["difftool", TOOL_ECHO], Shape::Dirty);
    // The environment spelling of `-y`, which `misc_commands.rs` records as
    // unreachable from a case and is not: `GIT_DIFFTOOL_NO_PROMPT` is not one of
    // `harden`'s pins. Stock 2.55.0 launches the tool for both paths.
    out.push(
        Case::new("difftool", &["difftool", "--tool=p"], Shape::Dirty)
            .with_config(&[CFG_ECHO])
            .with_env(&[("GIT_DIFFTOOL_NO_PROMPT", "true")]),
    );
}

// ---------------------------------------------------------------------------
// difftool: whose exit code is honoured
// ---------------------------------------------------------------------------

/// The three spellings of "trust the tool's exit code", one of which is not a
/// spelling of it at all.
///
/// `difftool` reads `--trust-exit-code`/`--no-trust-exit-code` and the
/// configuration key `difftool.trustExitCode`. It does **not** read
/// `difftool.<tool>.trustExitCode` — that key belongs to `mergetool` — and the
/// resemblance is the trap. Measured on stock 2.55.0 with a `exit 3` tool:
///
/// | setting | exit |
/// |---|---|
/// | `--trust-exit-code` | 128, `external diff died` |
/// | `--no-trust-exit-code` | 0 |
/// | `-c difftool.trustExitCode=true` | 128 |
/// | `-c difftool.trustExitCode=false` | 0 |
/// | `-c difftool.p.trustExitCode=true` | **0** |
///
/// A port that honours the per-tool key diverges on the last row alone, which no
/// existing case can reach: `archive_export.rs` sets the global key and
/// `merge_family.rs` sets the per-tool key on the *other* command.
///
/// The tool exits **3** rather than 1 throughout. `false` is exit 1, and 1 is
/// also what a diff-found status looks like, so a port that confuses "the tool
/// disagreed" with "the tool failed" is indistinguishable under `false` and
/// separable under `exit 3`.
fn difftool_exit_status(out: &mut Vec<Case>) {
    // Command line, both directions.
    out.push(Case::new("difftool", &["difftool", "-y", "--trust-exit-code", TOOL_EXIT3], Shape::Dirty));
    out.push(Case::new("difftool", &["difftool", "-y", "--no-trust-exit-code", TOOL_EXIT3], Shape::Dirty));
    // The flag with a tool that succeeds: trusting an exit code of 0 must not
    // itself be a failure.
    out.push(Case::new("difftool", &["difftool", "-y", "--trust-exit-code", TOOL_ECHO], Shape::Dirty));

    // Configuration, over a *named* tool rather than an `--extcmd` — the route
    // `archive_export.rs` does not take with this key.
    for (key, value) in [("difftool.trustExitCode", "true"), ("difftool.trustExitCode", "false")] {
        out.push(
            Case::new("difftool", &["difftool", "-y", "--tool=p"], Shape::Dirty)
                .with_config(&[CFG_EXIT3, (key, value)]),
        );
    }
    // The key that looks like the two above and is never read.
    out.push(
        Case::new("difftool", &["difftool", "-y", "--tool=p"], Shape::Dirty)
            .with_config(&[CFG_EXIT3, ("difftool.p.trustExitCode", "true")]),
    );
    // The command line must beat the configuration in both directions.
    out.push(
        Case::new("difftool", &["difftool", "-y", "--no-trust-exit-code", "--tool=p"], Shape::Dirty)
            .with_config(&[CFG_EXIT3, ("difftool.trustExitCode", "true")]),
    );
    out.push(
        Case::new("difftool", &["difftool", "-y", "--trust-exit-code", "--tool=p"], Shape::Dirty)
            .with_config(&[CFG_EXIT3, ("difftool.trustExitCode", "false")]),
    );
    // A tool that dies part-way: with the walk stopped at the first path, the
    // count of `TOOL` lines that reached stdout before the abort is the
    // observable, and it must be the same on both sides.
    out.push(
        Case::new("difftool", &["difftool", "-y", "--trust-exit-code", "--tool=p"], Shape::Dirty)
            .with_config(&[("difftool.p.cmd", "echo TOOL; exit 3")]),
    );
}

// ---------------------------------------------------------------------------
// difftool: which of the configured names won
// ---------------------------------------------------------------------------

/// Tool selection through the two routes a `Case` was thought not to have, plus
/// the two GUI fallbacks.
///
/// `GIT_DIFF_TOOL` and `GIT_DIFFTOOL_EXTCMD` are read by
/// `git-difftool--helper.sh` and are how `git difftool` passes its own decision
/// down to the helper it installs as `GIT_EXTERNAL_DIFF`. Setting one directly
/// is therefore setting the *result* of the selection git is about to perform,
/// and what is measured is whether the port's own selection overrides it the way
/// stock's does. Neither is one of `harden`'s pins.
///
/// The two GUI rows are the fallbacks `archive_export.rs`'s `--gui` case does
/// not reach, because it configures `diff.guitool` and so never asks what
/// happens when there isn't one. Measured on stock 2.55.0: `--gui` with only
/// `diff.tool` set falls back to it and launches, and `difftool.guiDefault=true`
/// selects `diff.guitool` with no `--gui` on the command line at all.
fn difftool_selection(out: &mut Vec<Case>) {
    // The environment routes.
    out.push(
        Case::new("difftool", &["difftool", "-y"], Shape::Dirty)
            .with_config(&[CFG_ECHO])
            .with_env(&[("GIT_DIFF_TOOL", "p")]),
    );
    out.push(
        Case::new("difftool", &["difftool", "-y"], Shape::Dirty)
            .with_env(&[("GIT_DIFFTOOL_EXTCMD", "sh -c \"echo FROM-ENV\"")]),
    );
    // The command line's own `--extcmd` against the environment's: git exports
    // its own value over the inherited one, so `FROM-ENV` must not appear.
    out.push(
        Case::new("difftool", &["difftool", "-y", TOOL_ECHO], Shape::Dirty)
            .with_env(&[("GIT_DIFFTOOL_EXTCMD", "sh -c \"echo FROM-ENV\"")]),
    );
    // `--tool=` against an inherited `GIT_DIFF_TOOL` naming a different tool.
    out.push(
        Case::new("difftool", &["difftool", "-y", "--tool=p"], Shape::Dirty)
            .with_config(&[("difftool.p.cmd", "echo RAN-P"), ("difftool.q.cmd", "echo RAN-Q")])
            .with_env(&[("GIT_DIFF_TOOL", "q")]),
    );

    // The GUI fallbacks.
    out.push(
        Case::new("difftool", &["difftool", "-y", "--gui"], Shape::Dirty)
            .with_config(&[CFG_ECHO, ("diff.tool", "p")]),
    );
    out.push(
        Case::new("difftool", &["difftool", "-y"], Shape::Dirty)
            .with_config(&[
                CFG_ECHO,
                ("diff.guitool", "p"),
                ("difftool.guiDefault", "true"),
            ]),
    );
    // `--no-gui` must undo it, so `diff.tool` is what runs and `diff.guitool` is
    // not — two tools that print different strings, so which one ran is on
    // stdout rather than merely inferable.
    out.push(
        Case::new("difftool", &["difftool", "-y", "--no-gui"], Shape::Dirty)
            .with_config(&[
                ("difftool.p.cmd", "echo RAN-CLI"),
                ("difftool.q.cmd", "echo RAN-GUI"),
                ("diff.tool", "p"),
                ("diff.guitool", "q"),
                ("difftool.guiDefault", "true"),
            ]),
    );
}

// ---------------------------------------------------------------------------
// mergetool on a shape it has never been pointed at
// ---------------------------------------------------------------------------

/// `mergetool` over [`Shape::Rerere`], where the list of files to merge does not
/// come from the index.
///
/// `git-mergetool.sh` chooses its subject list two ways: if `$GIT_DIR/MERGE_RR`
/// exists it runs `git rerere remaining`, and only otherwise does it fall back to
/// `diff --name-only --diff-filter=U`. Every existing mergetool case runs on
/// [`Shape::Conflicted`], which has no `MERGE_RR`, so the first branch has never
/// been entered and a port that only ever asks the index scores the same as one
/// that asks rerere.
///
/// This shape enters it, and the two branches give visibly different answers.
/// Measured on stock 2.55.0 against a fresh copy — `status --porcelain` reports
/// three unmerged paths (`AA fresh.txt`, `UU other.txt`, `UU rr.txt`):
///
/// * bare `mergetool -y --tool=p` prints `Merging:` / `fresh.txt` and stages
///   `fresh.txt` alone, because `rerere remaining` excludes the two paths whose
///   recorded resolution has already been replayed into the worktree.
/// * the same run under `-c rerere.enabled=false` prints `No files need
///   merging`, stages nothing and leaves all three paths unmerged — because
///   `rerere remaining` with rerere off is an empty list, not a full one.
///   A port that reads the index here resolves three files and diverges in
///   stdout, in `ls-files --stage` and in the untracked `*.orig` files at once.
/// * an explicit pathspec bypasses the list entirely: `-- rr.txt` merges `rr.txt`
///   and leaves `fresh.txt` and `other.txt` alone.
///
/// `$BASE` is the other thing this shape has and `Conflicted` does not. That
/// one's conflict is an add/add, so `$BASE` is `/dev/null` and a tool reading it
/// writes an empty file; `rr.txt` descends from `rerere: base`, so
/// `cat "$BASE" > "$MERGED"` stages a blob that exists in history and the state
/// probe can tell it from both sides' versions.
fn mergetool_rerere(out: &mut Vec<Case>) {
    let mt = |out: &mut Vec<Case>, cfg: &[(&str, &str)], args: &[&str]| {
        out.push(Case::new("mergetool", args, Shape::Rerere).with_config(cfg));
    };
    // The `rerere remaining` list, and what it leaves out.
    mt(out, MT_LOCAL, &["mergetool", "-y", "--tool=p"]);
    // The same list, emptied by turning rerere off. The whole point is that this
    // resolves *less* than the line above, not more.
    out.push(
        Case::new("mergetool", &["mergetool", "-y", "--tool=p"], Shape::Rerere)
            .with_config(&[MT_LOCAL[0], MT_LOCAL[1], ("rerere.enabled", "false")]),
    );
    // Explicit pathspecs, which are not filtered through `rerere remaining` at
    // all. `rr.txt` and `other.txt` are the two paths the bare run skips.
    mt(out, MT_LOCAL, &["mergetool", "-y", "--tool=p", "--", "rr.txt"]);
    mt(out, MT_LOCAL, &["mergetool", "-y", "--tool=p", "--", "other.txt"]);
    mt(out, MT_LOCAL, &["mergetool", "-y", "--tool=p", "--", "rr.txt", "other.txt"]);
    // A path that is not unmerged, and one that does not exist.
    mt(out, MT_LOCAL, &["mergetool", "-y", "--tool=p", "--", "README.md"]);
    mt(out, MT_LOCAL, &["mergetool", "-y", "--tool=p", "--", "no-such-file.txt"]);

    // The merge base, which only this shape has.
    out.push(
        Case::new("mergetool", &["mergetool", "-y", "--tool=p", "--", "rr.txt"], Shape::Rerere)
            .with_config(MT_BASE),
    );
    // `writeToTemp` moves `$LOCAL`/`$BASE`/`$REMOTE` out of the worktree, so a
    // port that resolves them as siblings of the merged path writes nothing.
    out.push(
        Case::new("mergetool", &["mergetool", "-y", "--tool=p", "--", "rr.txt"], Shape::Rerere)
            .with_config(&[MT_LOCAL[0], MT_LOCAL[1], ("mergetool.writeToTemp", "true")]),
    );
    // A tool that fails on a shape with more than one candidate: stock stops at
    // the first failure, so the second path must stay unmerged too.
    out.push(
        Case::strict("mergetool", &["mergetool", "-y", "--tool=p", "--", "rr.txt", "other.txt"], Shape::Rerere)
            .with_config(MT_FAIL),
    );
}

/// The `mergetool` knobs the other two modules leave, on the shape they use.
///
/// * **`-y`**, the short spelling of `--no-prompt`. `archive_export.rs` and
///   `merge_family.rs` both write `--no-prompt`, so the short form has never
///   been parsed by a case.
/// * **`mergetool.keepTemporaries=true` over a tool that succeeds**, where the
///   contract is that the temporaries are removed anyway — measured twice, and
///   the worktree afterwards holds `conflict.txt.orig` and nothing else.
/// * **`mergetool.keepBackup=true`**, the default direction. `merge_family.rs`
///   pins `false`, where the `.orig` file must *not* survive; nothing pinned the
///   direction where it must.
///
/// `--prompt` with the harness's closed stdin was written, run, and taken out.
/// Stock asks, reads EOF and gives up — and leaves the four temporaries behind,
/// because the cleanup is on the resolved path. Measured twice in two fresh
/// copies: `conflict_{BACKUP,BASE,LOCAL,REMOTE}_88949.txt`, then `…_90325.txt`.
/// That number is the pid, so the untracked-file probe would report a difference
/// on every run whatever the port did; the harness scored the case
/// `NONDETERMINISTIC` before it was removed.
fn mergetool_misc(out: &mut Vec<Case>) {
    out.push(
        Case::new("mergetool", &["mergetool", "-y", "--tool=p"], Shape::Conflicted)
            .with_config(MT_LOCAL),
    );
    out.push(
        Case::new("mergetool", &["mergetool", "-y", "--tool=p"], Shape::Conflicted)
            .with_config(&[MT_LOCAL[0], MT_LOCAL[1], ("mergetool.keepTemporaries", "true")]),
    );
    // `keepBackup` in its *default* direction on a shape where the `.orig` file
    // is the only trace: `merge_family.rs` pins `false`, nothing pins `true`.
    out.push(
        Case::new("mergetool", &["mergetool", "-y", "--tool=p"], Shape::Conflicted)
            .with_config(&[MT_LOCAL[0], MT_LOCAL[1], ("mergetool.keepBackup", "true")]),
    );
}

// ---------------------------------------------------------------------------
// Credential descriptions fed on stdin
// ---------------------------------------------------------------------------
//
// One-line `&'static [u8]` literals, flush left, for the reason
// `helpers_credentials.rs` states: a `\`-continued Rust string swallows the next
// line's leading whitespace and silently rewrites the payload. Each is
// checkable against the `stdin[<len>B/<hash>]` segment `--list-cases` prints.

/// The minimum git accepts, and the request every matching-rule case asks.
const REQ: &[u8] = b"protocol=https\nhost=example.invalid\n\n";
/// The same host under a subdomain, so a `*.` wildcard section has something to
/// match that an exact-host section does not.
const REQ_SUB: &[u8] = b"protocol=https\nhost=sub.example.invalid\n\n";
/// Carries a path, for the sections that name one.
const REQ_PATH: &[u8] = b"protocol=https\nhost=example.invalid\npath=a/b.git\n\n";
/// Carries a *different* path, so a path-scoped section has to decline.
const REQ_OTHER_PATH: &[u8] = b"protocol=https\nhost=example.invalid\npath=other.git\n\n";
/// Carries a username, for the sections that name one.
const REQ_USER: &[u8] = b"protocol=https\nhost=example.invalid\nusername=bob\n\n";
/// A username no section names.
const REQ_OTHER_USER: &[u8] = b"protocol=https\nhost=example.invalid\nusername=eve\n\n";
/// A non-default port, which is part of the `host` field rather than a field of
/// its own — so a section that omits it must not match.
const REQ_PORT: &[u8] = b"protocol=https\nhost=example.invalid:8443\n\n";
/// **Already complete**: username *and* password supplied by the caller. What
/// makes this interesting is what git does not do with it; see
/// [`credential_control_words`].
const REQ_COMPLETE: &[u8] =
    b"protocol=https\nhost=example.invalid\nusername=bob\npassword=s3cret\n\n";

// ---------------------------------------------------------------------------
// In-fixture credential helpers
// ---------------------------------------------------------------------------

/// Answers, so a matching section is visible as `username=u` on stdout and a
/// non-matching one as the empty answer the pinned `GIT_ASKPASS=true` produces.
const H_ANSWER: &str = "!f() { echo username=u; echo password=p; }; f";
/// A second, distinguishable answer, for the cases that ask *which* of two
/// helpers was reached.
const H_SECOND: &str = "!g() { echo username=second; echo password=sp; }; g";
/// Writes a line that is not `key=value`. Stock warns and carries on.
const H_GARBAGE: &str = "!f() { echo garbage-line; }; f";
/// Says `quit=1`, git's control word for "stop asking anyone".
const H_QUIT: &str = "!f() { echo quit=1; }; f";
/// Answers, and adds a key git has no field for.
const H_UNKNOWN_KEY: &str = "!f() { echo username=u; echo password=p; echo unknown=z; }; f";
/// Answers with a username and no password, so the prompt still has to run for
/// the half that is missing.
const H_USER_ONLY: &str = "!f() { echo username=u; }; f";
/// Records the operation and the whole request into a worktree file the state
/// probe compares byte for byte. Spelled as `helpers_credentials.rs` spells it,
/// so the two modules' logs are the same format.
const H_LOG: &str = "!f() { echo \"op=$1\" > cred-log; cat >> cred-log; }; f";

// ---------------------------------------------------------------------------
// credential: the control words, and the requests git answers by itself
// ---------------------------------------------------------------------------

/// What a helper can say back besides a field, and what git does when the
/// caller has already said everything.
///
/// `helpers_credentials.rs` covers the three helper *outcomes* — answers,
/// declines, exits non-zero — and treats the last two as identical, which they
/// are. It does not cover the two things a helper can say that are neither:
///
/// * **`quit=1`.** Measured on stock 2.55.0:
///   `fatal: credential helper '<cmd>' told us to quit`, exit 128, and the
///   second helper in the list never runs. A port that treats an unknown key as
///   noise falls through to the next helper and prints `username=second` at exit
///   0 — a difference in stdout *and* exit code from one word.
/// * **a line that is not `key=value`.** Stock warns
///   (`warning: invalid credential line: garbage-line`), ignores it, and carries
///   on to the prompt: exit 0, both fields empty. Strict, because the warning is
///   the entire behaviour — nothing else distinguishes this from a decline.
///
/// And two things about the *request* rather than the answer:
///
/// * **A complete request short-circuits.** Given `username` and `password`
///   both, `credential fill` echoes them back and **never runs the helper at
///   all** — verified on stock 2.55.0 with a helper that writes to stderr: no
///   output. This is measured with `H_LOG`, so "the helper did not run" is the
///   *absence* of `cred-log` from the worktree, which the state probe reports.
///   The failure mode it guards is not cosmetic: a port that runs the helper
///   anyway has handed a password it already had to an external program stock
///   never starts.
/// * **`credential.interactive=false`** turns the prompt from a source of empty
///   fields into a refusal: `fatal: unable to get password from user`, exit 128,
///   where the same case without it exits 0. `helpers_credentials.rs` records
///   the prompt path as reachable-and-empty; this is the other half of it.
fn credential_control_words(out: &mut Vec<Case>) {
    let c = |args: &[&str], cfg: &[(&str, &str)], input, out: &mut Vec<Case>| {
        out.push(Case::with_stdin("credential", args, Shape::Linear, input).with_config(cfg));
    };

    // `quit=1`, alone and in front of a helper that would have answered.
    c(&["credential", "fill"], &[("credential.helper", H_QUIT)], REQ, out);
    c(
        &["credential", "fill"],
        &[("credential.helper", H_QUIT), ("credential.helper", H_SECOND)],
        REQ,
        out,
    );
    // The reverse order: the first helper answers, so `quit` is never reached.
    c(
        &["credential", "fill"],
        &[("credential.helper", H_SECOND), ("credential.helper", H_QUIT)],
        REQ,
        out,
    );
    // `quit=1` on the write side, where there is no answer to abandon.
    c(&["credential", "approve"], &[("credential.helper", H_QUIT)], REQ_COMPLETE, out);

    // Garbage, and an extra key git has no field for. The first is strict
    // because its warning is all there is; the second must be silently ignored.
    out.push(
        Case {
            compare_stderr: true,
            ..Case::with_stdin("credential", &["credential", "fill"], Shape::Linear, REQ)
        }
        .with_config(&[("credential.helper", H_GARBAGE)]),
    );
    // Garbage from the *second* helper, after the first declined: the warning
    // fires and the loop still ends at the prompt.
    out.push(
        Case::with_stdin("credential", &["credential", "fill"], Shape::Linear, REQ)
            .with_config(&[("credential.helper", H_GARBAGE), ("credential.helper", H_SECOND)]),
    );
    c(&["credential", "fill"], &[("credential.helper", H_UNKNOWN_KEY)], REQ, out);
    // Half an answer: the username comes from the helper and the password from
    // the prompt, which is pinned to `true` and supplies nothing.
    c(&["credential", "fill"], &[("credential.helper", H_USER_ONLY)], REQ, out);

    // The short circuit, measured by the log file that must not appear.
    c(&["credential", "fill"], &[("credential.helper", H_LOG)], REQ_COMPLETE, out);
    // The control: the same helper, one field short, does run.
    c(&["credential", "fill"], &[("credential.helper", H_LOG)], REQ_USER, out);

    // `credential.interactive`, in both directions and against a helper that
    // answers — where it must change nothing, because no prompt is reached.
    out.push(
        Case {
            compare_stderr: true,
            ..Case::with_stdin("credential", &["credential", "fill"], Shape::Linear, REQ)
        }
        .with_config(&[("credential.interactive", "false")]),
    );
    c(&["credential", "fill"], &[("credential.interactive", "true")], REQ, out);
    c(
        &["credential", "fill"],
        &[("credential.interactive", "false"), ("credential.helper", H_ANSWER)],
        REQ,
        out,
    );
    // Only the password is missing: `interactive=false` must still refuse.
    out.push(
        Case {
            compare_stderr: true,
            ..Case::with_stdin("credential", &["credential", "fill"], Shape::Linear, REQ_USER)
        }
        .with_config(&[("credential.interactive", "false")]),
    );
}

// ---------------------------------------------------------------------------
// credential.<url>.*: which section describes this request
// ---------------------------------------------------------------------------

/// The URL-matching rules `helpers_credentials.rs` does not reach.
///
/// It pins four: an exact host that matches, a different host that does not, an
/// `http://` section against an `https://` request, and a section that seeds a
/// username. That leaves the parts of `urlmatch.c` where a port is most likely
/// to have written `starts_with` and moved on. Each row below was measured on
/// stock 2.55.0 and the answer is either `username=u` (the section's helper ran)
/// or `username=` (it did not):
///
/// | section | request | answer |
/// |---|---|---|
/// | `https://*.example.invalid` | `sub.example.invalid` | `u` |
/// | `https://*.example.invalid` | `example.invalid` | *empty* |
/// | `https://*` | `example.invalid` | *empty* |
/// | `https://example.invalid/a/b.git` | `path=a/b.git` | `u` |
/// | `https://example.invalid/a/b.git` | `path=other.git` | *empty* |
/// | `https://bob@example.invalid` | `username=bob` | `u` |
/// | `https://bob@example.invalid` | `username=eve` | *empty* |
/// | `https://example.invalid:8443` | `host=example.invalid:8443` | `u` |
/// | `https://example.invalid` | `host=example.invalid:8443` | *empty* |
///
/// Two of those are worth stating outright because they are the ones an
/// approximation gets backwards. The path row matches **even though
/// `credential.useHttpPath` is off**: that setting decides whether the path is
/// *forwarded to the helper*, not whether configuration may match on it — and
/// stock's answer for the matching row drops the `path=` line from stdout while
/// still having used it to select the section. And a bare `https://*` matches
/// nothing at all: the wildcard stands for one host *component*, so it cannot
/// stand for the whole host.
///
/// The last two rows are the ordering questions. A generic `credential.helper`
/// and a URL-scoped one are one list in configuration order, so the generic one
/// answers first; and an *empty* URL-scoped value resets that list the same way
/// an empty generic one does, which is how a section can subtract a helper it
/// did not add.
fn credential_url_matching(out: &mut Vec<Case>) {
    let m = |section: &str, input, out: &mut Vec<Case>| {
        out.push(
            Case::with_stdin("credential", &["credential", "fill"], Shape::Linear, input)
                .with_config(&[(section, H_ANSWER)]),
        );
    };

    // Wildcards: one component, never the whole host.
    m("credential.https://*.example.invalid.helper", REQ_SUB, out);
    m("credential.https://*.example.invalid.helper", REQ, out);
    m("credential.https://*.helper", REQ, out);
    // A path component, with `useHttpPath` left at its default off.
    m("credential.https://example.invalid/a/b.git.helper", REQ_PATH, out);
    m("credential.https://example.invalid/a/b.git.helper", REQ_OTHER_PATH, out);
    // The same section with the path *also* forwarded, so the log shows both
    // halves of the distinction at once: the section matched on the path, and
    // whether the helper was told about it is a separate setting.
    out.push(
        Case::with_stdin("credential", &["credential", "fill"], Shape::Linear, REQ_PATH)
            .with_config(&[
                ("credential.https://example.invalid/a/b.git.helper", H_LOG),
                ("credential.https://example.invalid/a/b.git.useHttpPath", "true"),
            ]),
    );
    // A username component.
    m("credential.https://bob@example.invalid.helper", REQ_USER, out);
    m("credential.https://bob@example.invalid.helper", REQ_OTHER_USER, out);
    m("credential.https://bob@example.invalid.helper", REQ, out);
    // A port, which lives inside `host`.
    m("credential.https://example.invalid:8443.helper", REQ_PORT, out);
    m("credential.https://example.invalid.helper", REQ_PORT, out);
    m("credential.https://example.invalid:8443.helper", REQ, out);

    // Ordering and the reset.
    out.push(
        Case::with_stdin("credential", &["credential", "fill"], Shape::Linear, REQ).with_config(&[
            ("credential.helper", H_ANSWER),
            ("credential.https://example.invalid.helper", H_SECOND),
        ]),
    );
    out.push(
        Case::with_stdin("credential", &["credential", "fill"], Shape::Linear, REQ).with_config(&[
            ("credential.helper", H_ANSWER),
            ("credential.https://example.invalid.helper", ""),
        ]),
    );
    // The same reset from a section that does *not* match: the list must
    // survive, so the generic helper still answers.
    out.push(
        Case::with_stdin("credential", &["credential", "fill"], Shape::Linear, REQ).with_config(&[
            ("credential.helper", H_ANSWER),
            ("credential.https://other.invalid.helper", ""),
        ]),
    );
    // A section-scoped `username` against a request that already names a
    // different one: the request wins, and the helper is not consulted for a
    // field it was not asked about.
    out.push(
        Case::with_stdin("credential", &["credential", "fill"], Shape::Linear, REQ_OTHER_USER)
            .with_config(&[
                ("credential.https://example.invalid.username", "bob"),
                ("credential.helper", H_LOG),
            ]),
    );
}

// ---------------------------------------------------------------------------
// hook run: where the hook runs, and what it is handed
// ---------------------------------------------------------------------------

/// `git hook run` from a directory that is not the repository root, and with
/// arguments that make the hook rewrite a tracked file.
///
/// `hooks_identity.rs` runs every `hook run` case on [`Shape::HooksFail`] from
/// the fixture root, so what it measures is each hook's exit status. Two things
/// it cannot measure follow from that:
///
/// * **A hook runs with its working directory at the top level, whatever the
///   caller's was.** That is the exact defect [`Shape::Hooked`] was built for —
///   a hook's path and cwd resolved against a directory that had already moved —
///   and `hook run` is the shortest way to reach it, with no verb's control flow
///   in between. `Hooked`'s `pre-commit` writes `hook-ran.txt` in whatever
///   directory it starts in, so *where the file lands* is the answer. Measured
///   on stock 2.55.0: run from `sub/`, `hook-ran.txt` appears at the repository
///   root and not in `sub/`, byte-identical to the run from the root.
/// * **The arguments actually reach the hook.** `Hooked`'s `commit-msg` appends
///   `hooked-trailer` to the file named by `$1`. Pointing it at a *tracked* path
///   puts the result under the state probe's content comparison: `top.txt` is
///   modified in the worktree, and a port that dropped the argument leaves the
///   file alone. `informational.rs` already emits the no-argument form, where
///   the hook's `$1` is empty; these are the forms that name a path.
///
/// `--ignore-missing` over a hook that *is* installed is the third: the flag
/// tolerates absence, and must not also suppress presence.
fn hook_run_placement(out: &mut Vec<Case>) {
    // The subdirectory runs. `informational.rs` already emits the same argv
    // from the fixture root, which is the control these are read against: the
    // three must produce byte-identical worktrees.
    out.push(Case::new("hook", &["hook", "run", "pre-commit"], Shape::Hooked).in_dir("sub"));
    out.push(Case::new("hook", &["hook", "run", "pre-commit"], Shape::Hooked).in_dir("src"));
    // `core.hooksPath` is resolved against the repository root too, not against
    // the caller's directory — so the relative path that works from the root has
    // to work from `sub/` unchanged.
    out.push(
        Case::new("hook", &["hook", "run", "pre-commit"], Shape::Hooked)
            .in_dir("sub")
            .with_config(&[("core.hooksPath", ".git/hooks")]),
    );

    // Arguments through to a hook that writes where it is told.
    out.push(Case::new("hook", &["hook", "run", "commit-msg", "--", "top.txt"], Shape::Hooked));
    out.push(
        Case::new("hook", &["hook", "run", "commit-msg", "--", "sub/nested.txt"], Shape::Hooked),
    );
    // The same, dispatched from the subdirectory: the argument is relative to
    // the *hook's* directory, which is the top level, so `top.txt` still names
    // the tracked file at the root.
    out.push(
        Case::new("hook", &["hook", "run", "commit-msg", "--", "top.txt"], Shape::Hooked)
            .in_dir("sub"),
    );
    // `--ignore-missing` over a hook that exists must still run it.
    out.push(Case::new("hook", &["hook", "run", "--ignore-missing", "pre-commit"], Shape::Hooked));
    // The hook this shape does not install, without the flag that tolerates
    // absence — `informational.rs` has the `--ignore-missing` half.
    out.push(Case::strict("hook", &["hook", "run", "post-commit"], Shape::Hooked));
}

// ---------------------------------------------------------------------------
// diff.external: the unconditional external diff, and where it is not applied
// ---------------------------------------------------------------------------

/// `diff.external`, which appears nowhere else in the corpus, and the five verbs
/// that decide for themselves whether to honour it.
///
/// `attributes_filters.rs` owns the other two spellings — a per-driver
/// `diff.<driver>.command`, which applies only to the paths an attribute names,
/// and `GIT_EXTERNAL_DIFF` in the environment. `diff.external` is the third: a
/// configuration key with no attribute behind it, so it applies to every path in
/// every shape, and it is the one a port is likeliest to have skipped precisely
/// because the other two exist.
///
/// The gating is not uniform and each row was measured on stock 2.55.0 against
/// [`Shape::Branched`] with `diff.external=sh -c "echo EXT"`:
///
/// | invocation | external? |
/// |---|---|
/// | `diff HEAD~1 HEAD` | yes — output is `EXT` |
/// | `diff --no-ext-diff HEAD~1 HEAD` | no — ordinary patch |
/// | `show HEAD` | no |
/// | `show --ext-diff HEAD` | yes |
/// | `log -p -1` | no |
/// | `log -p --ext-diff -1` | yes |
/// | `format-patch -1 --stdout` | **no, even so** |
/// | `diff-tree -p --ext-diff HEAD` | **no, even so** |
/// | `diff --stat` / `--raw` / `--numstat` / `--name-status` | no |
///
/// The two bold rows are the ones a port gets wrong by being *consistent*:
/// `format-patch` turns the external diff off unconditionally because a patch
/// that a program rewrote is not a patch anyone can apply, and `diff-tree` is
/// plumbing and never turns it on. A port that routes every textual diff through
/// one gate produces `EXT` in a mailbox.
///
/// `diff.external=false` is the death path, and it is where the two keys' error
/// messages have to agree: `fatal: external diff died, stopping at <path>`,
/// exit 128, and *no* partial output before it.
///
/// The last pair is precedence. `GIT_EXTERNAL_DIFF` beats `diff.external`:
/// verified by setting the key to a command that echoes and the variable to a
/// name that does not exist, and getting
/// `error: cannot run sh_bad: No such file or directory` — so the variable was
/// the one tried.
fn external_diff_gates(out: &mut Vec<Case>) {
    const EXT: (&str, &str) = ("diff.external", "sh -c \"echo EXT\"");
    const DIES: (&str, &str) = ("diff.external", "false");

    let e = |cmd: &'static str, args: &[&str], shape, out: &mut Vec<Case>| {
        out.push(Case::new(cmd, args, shape).with_config(&[EXT]));
    };

    // Applied.
    e("diff", &["diff", "HEAD~1", "HEAD"], Shape::Branched, out);
    e("diff", &["diff", "HEAD"], Shape::Dirty, out);
    e("diff", &["diff", "--cached"], Shape::Dirty, out);
    e("show", &["show", "--ext-diff", "HEAD"], Shape::Branched, out);
    e("log", &["log", "-p", "--ext-diff", "-1"], Shape::Branched, out);
    // Not applied.
    e("diff", &["diff", "--no-ext-diff", "HEAD~1", "HEAD"], Shape::Branched, out);
    e("show", &["show", "HEAD"], Shape::Branched, out);
    e("log", &["log", "-p", "-1"], Shape::Branched, out);
    e("format-patch", &["format-patch", "-1", "--stdout"], Shape::Branched, out);
    e("format-patch", &["format-patch", "-1", "--stdout", "--ext-diff"], Shape::Branched, out);
    e("diff-tree", &["diff-tree", "-p", "HEAD"], Shape::Branched, out);
    e("diff-tree", &["diff-tree", "-p", "--ext-diff", "HEAD"], Shape::Branched, out);
    // The summary formats, which have no patch for a program to replace.
    for flag in ["--stat", "--raw", "--numstat", "--name-status", "--shortstat", "--summary"] {
        e("diff", &["diff", flag, "HEAD~1", "HEAD"], Shape::Branched, out);
    }
    // A shape where the paths are of other kinds: a rename pair, a typechange to
    // a symlink, an empty blob, quote-worthy names.
    e("diff", &["diff", "HEAD~1", "HEAD"], Shape::Renamed, out);
    e("diff", &["diff", "HEAD~1", "HEAD"], Shape::Symlinks, out);
    e("diff", &["diff", "HEAD~1", "HEAD"], Shape::AwkwardPaths, out);
    // Binary content, where the external command replaces the `Binary files
    // differ` line git would otherwise print.
    e("diff", &["diff", "HEAD~1", "HEAD"], Shape::Patches, out);

    // The command that dies.
    out.push(
        Case::strict("diff", &["diff", "HEAD~1", "HEAD"], Shape::Branched).with_config(&[DIES]),
    );
    out.push(
        Case::strict("diff", &["diff", "--no-ext-diff", "HEAD~1", "HEAD"], Shape::Branched)
            .with_config(&[DIES]),
    );
    // A command that prints and *then* dies: whatever reached stdout before the
    // abort has to be the same on both sides.
    out.push(
        Case::new("diff", &["diff", "HEAD~1", "HEAD"], Shape::Branched)
            .with_config(&[("diff.external", "sh -c \"echo EXT; exit 3\"")]),
    );

    // Precedence against the environment spelling.
    out.push(
        Case::new("diff", &["diff", "HEAD~1", "HEAD"], Shape::Branched)
            .with_config(&[EXT])
            .with_env(&[("GIT_EXTERNAL_DIFF", "sh -c \"echo XD\"")]),
    );
    // …and against the per-driver key, on the shape that has an attribute to
    // hang it on. `docs/manual.md` and `README.md` are `diff=markdown` there, so
    // the driver's command owns those paths and `diff.external` owns the rest.
    out.push(
        Case::new("diff", &["diff", "HEAD~3", "HEAD"], Shape::Attributes).with_config(&[
            EXT,
            ("diff.markdown.command", "sh -c \"echo DRIVER\""),
        ]),
    );
}

// ---------------------------------------------------------------------------
// GIT_SSH_COMMAND: the argv git hands the transport
// ---------------------------------------------------------------------------

/// The SSH transport's command, recorded rather than run.
///
/// Nothing in the corpus sets `GIT_SSH_COMMAND` or `core.sshCommand`, and the
/// obvious reason not to is the one `helpers_credentials.rs` states: a case must
/// never reach a resolver, and a port that *ignores* the setting runs the real
/// `ssh`, which does. That risk is measured rather than assumed here — both
/// sides were timed on the recording case below before it was written down, and
/// the binary under test returned in 0.13 s having spawned no `ssh`.
///
/// The recording is the point. Git's rule for the SSH command is that the
/// **remote command is one argument**: `ssh <host> "git-upload-pack '<path>'"`,
/// with the path quoted *inside* that single argument because the far end runs it
/// through a shell. Splitting it into words changes what the remote shell is
/// asked to do, and there is no way to see that from stdout, from the exit code,
/// or from an error message — every failure to connect reads
/// `fatal: Could not read from remote repository.` regardless.
///
/// So the command is a shell function that writes its own `"$@"` into a fixed
/// worktree path, one argument per line, which the state probe compares byte for
/// byte. Measured on stock 2.55.0, `ls-remote ssh://user@example.invalid/x.git`
/// produced exactly:
///
/// ```text
/// -o
/// SendEnv=GIT_PROTOCOL
/// user@example.invalid
/// git-upload-pack '/x.git'
/// ```
///
/// The binary under test writes the file and leaves it **empty** — the shell
/// form is run with no arguments at all — and on the plain-path form of the same
/// probe it passes three arguments where stock passes two, splitting
/// `git-upload-pack '/x.git'` into `git-upload-pack` and `'/x.git'` with the
/// quotes kept as literal characters.
///
/// Every URL is under `example.invalid` (RFC 6761: never resolves), and the
/// three commands are `true`, `false` and a shell function — none of them
/// reaches a network, and none of them is `ssh`.
fn ssh_command(out: &mut Vec<Case>) {
    /// Writes the transport's whole argv into the worktree, one argument per
    /// line. Git appends `"$@"` when it wraps a command with arguments in a
    /// shell, which is what makes the function form work — the same mechanism
    /// `credential.helper=!f() { … }; f` relies on.
    const RECORD: &str = "f() { printf \"%s\\n\" \"$@\" > ssh-argv.txt; }; f";
    const URL: &str = "ssh://user@example.invalid/x.git";

    // The recording, through the environment and through the configuration key.
    out.push(
        Case::new("ls-remote", &["ls-remote", URL], Shape::Linear)
            .with_env(&[("GIT_SSH_COMMAND", RECORD)]),
    );
    out.push(
        Case::new("ls-remote", &["ls-remote", URL], Shape::Linear)
            .with_config(&[("core.sshCommand", RECORD)]),
    );
    // A different URL spelling: the scp-like `host:path` form, whose remote
    // command carries a *relative* path — `git-upload-pack 'x.git'` against the
    // URL form's `git-upload-pack '/x.git'`.
    out.push(
        Case::new("ls-remote", &["ls-remote", "user@example.invalid:x.git"], Shape::Linear)
            .with_env(&[("GIT_SSH_COMMAND", RECORD)]),
    );
    // `--upload-pack` replaces the remote program inside that single argument:
    // stock sends `custom-pack '/x.git'`, so the substitution and the quoting
    // are measured together.
    out.push(
        Case::new("ls-remote", &["ls-remote", "--upload-pack=custom-pack", URL], Shape::Linear)
            .with_env(&[("GIT_SSH_COMMAND", RECORD)]),
    );
    // A push rather than a fetch, so the remote program is `git-receive-pack`.
    out.push(
        Case::new("push", &["push", URL, "main"], Shape::Linear)
            .with_env(&[("GIT_SSH_COMMAND", RECORD)]),
    );

    // The environment must beat the configuration key.
    out.push(
        Case::new("ls-remote", &["ls-remote", URL], Shape::Linear)
            .with_config(&[("core.sshCommand", "true")])
            .with_env(&[("GIT_SSH_COMMAND", RECORD)]),
    );

    // Commands that record nothing: the transport still fails, at the same exit
    // code, with no file left behind and nothing spawned that could resolve a
    // name.
    for cmd in ["true", "false"] {
        out.push(
            Case::new("ls-remote", &["ls-remote", URL], Shape::Linear)
                .with_env(&[("GIT_SSH_COMMAND", cmd)]),
        );
    }
    out.push(
        Case::new("ls-remote", &["ls-remote", URL], Shape::Linear)
            .with_config(&[("core.sshCommand", "true")]),
    );
    // `ssh.variant` tells git which command-line dialect to speak, and it is
    // the one input to the argv above that is not the URL. Four distinct
    // answers on stock 2.55.0, all with the same one-argument remote command:
    // `ssh`/`auto` prepend `-o SendEnv=GIT_PROTOCOL`, `tortoiseplink` prepends
    // `-batch`, and `simple`/`plink`/`putty` prepend nothing.
    for variant in ["simple", "ssh", "plink", "putty", "tortoiseplink", "auto"] {
        out.push(
            Case::new("ls-remote", &["ls-remote", URL], Shape::Linear)
                .with_config(&[("ssh.variant", variant)])
                .with_env(&[("GIT_SSH_COMMAND", RECORD)]),
        );
    }
}

// ---------------------------------------------------------------------------
// The editor and pager pins, measured by what they make unreachable
// ---------------------------------------------------------------------------

/// `core.editor` and `core.pager` are unreachable, and `git var` is where that
/// is a fact rather than an assumption.
///
/// [`crate::env::harden`] pins `GIT_EDITOR=true` and `GIT_PAGER=cat`, and each
/// outranks its configuration key in git's own lookup order
/// (`editor.c:git_editor`, `pager.c:git_pager`). So no case in this corpus can
/// make `core.editor` or `core.pager` change a byte of behaviour, and a port
/// that ignores both keys entirely is indistinguishable from one that honours
/// them — *unless* the precedence itself is asked about, which is what these do.
///
/// `git var GIT_EDITOR` resolves the editor without running it, so the answer is
/// the name that won. Measured on stock 2.55.0: `-c core.editor=my-editor var
/// GIT_EDITOR` prints `true`, and `-c core.pager=my-pager var GIT_PAGER` prints
/// `cat`. A port that reads the configuration key first prints `my-editor` and
/// diverges here and nowhere else.
///
/// `sequence.editor` is not among them: `history_rewrite.rs` already pins that
/// precedence through `rebase -i -c sequence.editor=false`.
fn editor_pager_pins(out: &mut Vec<Case>) {
    out.push(
        Case::new("var", &["var", "GIT_EDITOR"], Shape::Linear)
            .with_config(&[("core.editor", "my-editor")]),
    );
    out.push(
        Case::new("var", &["var", "GIT_PAGER"], Shape::Linear)
            .with_config(&[("core.pager", "my-pager")]),
    );
    out.push(
        Case::new("var", &["var", "-l"], Shape::Linear)
            .with_config(&[("core.editor", "my-editor"), ("core.pager", "my-pager")]),
    );
    // `pager.<cmd>` is the third key the pin makes inert, and it is inert in a
    // second way: `GIT_PAGER=cat` means paginating and not paginating produce
    // the same bytes, so `--no-pager` and `--paginate` must agree with each
    // other and with the setting.
    out.push(
        Case::new("log", &["log", "--oneline", "-2"], Shape::Branched)
            .with_config(&[("pager.log", "false")]),
    );
    out.push(
        Case::new("log", &["log", "--oneline", "-2"], Shape::Branched)
            .with_config(&[("pager.log", "my-pager")])
            .with_globals(&[&["--no-pager"]]),
    );
    out.push(
        Case::new("log", &["log", "--oneline", "-2"], Shape::Branched)
            .with_config(&[("core.pager", "my-pager")])
            .with_globals(&[&["--paginate"]]),
    );
}

// ---------------------------------------------------------------------------
// web--browse with a browser that runs
// ---------------------------------------------------------------------------

/// `web--browse` driven by `browser.<tool>.cmd`, which is the only way to see
/// what it decided to open.
///
/// `misc_commands.rs` has ten cases here and every one of them is an argument
/// error — deliberately, because its comment records that stock resolves a real
/// URL by really fetching it. That leaves the command's actual job unmeasured: a
/// port that parsed the flags and launched nothing scores 10/10.
///
/// A configured browser closes it. `browser.p.cmd=echo BROWSE` makes the URL
/// list git assembled the process's stdout, and every route to selecting `p` —
/// `-b`, `--browser=`, `web.browser` — has to arrive at the same line. Measured
/// on stock 2.55.0: `BROWSE https://example.invalid/x`, and with two URLs, one
/// line carrying both.
///
/// Nothing here can reach the network: `example.invalid` never resolves, and the
/// only program run is `echo`, `true` or `false`.
fn web_browse(out: &mut Vec<Case>) {
    const URL: &str = "https://example.invalid/x";
    const ECHO: (&str, &str) = ("browser.p.cmd", "echo BROWSE");

    out.push(
        Case::new("web--browse", &["web--browse", "-b", "p", URL], Shape::Linear)
            .with_config(&[ECHO]),
    );
    out.push(
        Case::new("web--browse", &["web--browse", "--browser=p", URL], Shape::Linear)
            .with_config(&[ECHO]),
    );
    out.push(
        Case::new("web--browse", &["web--browse", URL], Shape::Linear)
            .with_config(&[ECHO, ("web.browser", "p")]),
    );
    // Two URLs in one launch, and a URL that is a local path — the two operand
    // shapes the command distinguishes.
    out.push(
        Case::new(
            "web--browse",
            &["web--browse", "-b", "p", "https://a.invalid", "https://b.invalid"],
            Shape::Linear,
        )
        .with_config(&[ECHO]),
    );
    out.push(
        Case::new("web--browse", &["web--browse", "-b", "p", "README.md"], Shape::Linear)
            .with_config(&[ECHO]),
    );
    // The command line must beat `web.browser`, so `BROWSE-CLI` is what runs.
    out.push(
        Case::new("web--browse", &["web--browse", "-b", "p", URL], Shape::Linear).with_config(&[
            ("browser.p.cmd", "echo BROWSE-CLI"),
            ("browser.q.cmd", "echo BROWSE-CFG"),
            ("web.browser", "q"),
        ]),
    );
    // A browser that fails: the status is the launcher's.
    out.push(
        Case::new("web--browse", &["web--browse", "-b", "p", URL], Shape::Linear)
            .with_config(&[("browser.p.cmd", "false")]),
    );
    // `browser.<tool>.path` without a `cmd` does not make `p` a known browser:
    // stock says `Unknown browser 'p'.` and exits 1. Strict, because that
    // message is the whole distinction from the `cmd` case above.
    out.push(
        Case::strict("web--browse", &["web--browse", "-b", "p", URL], Shape::Linear)
            .with_config(&[("browser.p.path", "true")]),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runner::ConfigEntry;

    /// Every command this module hands to git is a literal, and names nothing
    /// outside the fixture.
    ///
    /// The whole file is a list of programs git is asked to run, which is
    /// exactly the shape of case that can stop being a measurement and start
    /// being a machine probe: a tool command that named an absolute path would
    /// run one side's copy from the other side's run, and one that interpolated
    /// an environment variable would let the harness's own environment into the
    /// compared output. Neither is caught by the runner — it checks the
    /// *environment* a case sets, not the command text inside a configuration
    /// value — so the check belongs here, next to the values it is about.
    ///
    /// The permitted `$` names are the four git itself exports for a merge tool
    /// (`$LOCAL`, `$REMOTE`, `$BASE`, `$MERGED`) and the `$@`/`$1` a shell
    /// function reads its own arguments from. Anything else is a variable the
    /// case did not put there.
    #[test]
    fn every_tool_command_is_a_fixture_local_literal() {
        const TOOL_KEYS: &[&str] = &[
            ".cmd",
            "diff.external",
            "core.sshCommand",
            "credential.helper",
            "core.editor",
            "core.pager",
            "diff.markdown.command",
        ];
        const ALLOWED_VARS: &[&str] = &["$LOCAL", "$REMOTE", "$BASE", "$MERGED", "$@", "$1"];

        let mut cases = Vec::new();
        super::cases(&mut cases);
        assert!(!cases.is_empty(), "this module emits no cases at all");

        let is_tool = |e: &ConfigEntry| {
            e.key.as_deref().is_some_and(|k| {
                TOOL_KEYS.iter().any(|suffix| k.ends_with(suffix) || k == *suffix)
            })
        };

        for case in &cases {
            // Configured commands.
            for entry in case.config.iter().filter(|e| is_tool(e)) {
                check(&case.id(), entry.key.as_deref().unwrap_or(""), &entry.value, ALLOWED_VARS);
            }
            // The same text delivered through the environment: `GIT_SSH_COMMAND`,
            // `GIT_EXTERNAL_DIFF`, `GIT_DIFFTOOL_EXTCMD`, `GIT_DIFF_TOOL`.
            for (key, value) in &case.env {
                if key.ends_with("COMMAND") || key.ends_with("DIFF") || key.ends_with("EXTCMD") {
                    check(&case.id(), key, value, ALLOWED_VARS);
                }
            }
            // And on the command line, where `--extcmd=` carries one.
            for arg in &case.args {
                if let Some(cmd) = arg.strip_prefix("--extcmd=") {
                    check(&case.id(), "--extcmd", cmd, ALLOWED_VARS);
                }
            }
        }

        fn check(id: &str, key: &str, value: &str, allowed: &[&str]) {
            assert!(
                !value.split_whitespace().any(|word| word.starts_with('/')),
                "{id}: {key} names an absolute path, which is one side's copy only: {value}"
            );
            let mut rest = value.to_string();
            for var in allowed {
                rest = rest.replace(var, "");
            }
            assert!(
                !rest.contains('$'),
                "{id}: {key} interpolates a variable the case did not supply: {value}"
            );
        }
    }

    /// The shapes this module reaches, asserted rather than assumed.
    ///
    /// Three of its groups exist *because* of the shape they run on —
    /// `mergetool` needs [`Shape::Rerere`]'s `MERGE_RR`, `hook run` needs
    /// [`Shape::Hooked`]'s subdirectory, and the `$BASE` case needs a conflict
    /// with a merge base. If a later edit moved any of them onto a shape that
    /// merely looks similar, every case would still pass and would measure
    /// nothing; that is the failure this pins.
    #[test]
    fn the_shapes_the_measurements_depend_on_are_still_drawn() {
        let mut cases = Vec::new();
        super::cases(&mut cases);
        for (cmd, shape) in [
            ("mergetool", Shape::Rerere),
            ("hook", Shape::Hooked),
            ("difftool", Shape::IntentToAdd),
            ("ls-remote", Shape::Linear),
        ] {
            assert!(
                cases.iter().any(|c| c.cmd == cmd && c.shape == shape),
                "no {cmd} case runs on {}, so the measurement it exists for is gone",
                shape.name()
            );
        }
        // `hook run` from a subdirectory is the whole point of the hook group.
        assert!(
            cases.iter().any(|c| c.cmd == "hook" && c.cwd == Some("sub")),
            "no hook case runs from a subdirectory"
        );
    }
}

