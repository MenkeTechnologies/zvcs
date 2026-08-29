//! Configuration that changes what a read prints.
//!
//! [`crate::runner::Case::config`] exists because git's behaviour is a function
//! of its configuration at least as much as of its argv, and the corpus has
//! used it — 260 of the 1,002 keys `git help --config` lists are set by some
//! case. The other 742 are not, and 285 of those are *referenced in this port's
//! own source*: there is code that reads the key and nothing that compares what
//! it does with what git does. A port that reads a setting and acts on it
//! wrongly scores exactly the same as one that honours it.
//!
//! This module takes the part of that gap which is cheapest to be sure about —
//! settings whose whole effect is on a **read**. Every case runs a read-only
//! verb, so the setting either changes the bytes on stdout or it does not, and
//! no state probe has to adjudicate anything.
//!
//! ## Every pair here was measured, and most of the first draft was deleted
//!
//! A configuration case has a failure mode no other case has: it can be
//! **vacuous**. If the value happens to equal git's default, or the shape's
//! output has no element for the setting to change, then both sides print the
//! same bytes whatever the port does, the case passes for ever, and it has
//! measured nothing. That is the same error as scoring an unported command as a
//! skip, and it is invisible in the report — a vacuous case looks exactly like
//! a passing one.
//!
//! So each pair below was checked the way the harness checks a port: run stock
//! git in `templates/<shape>/` with the setting and without it, and compare the
//! bytes. Only the pairs that *changed stock's output* are here. The first
//! draft of this module had 156 cases; the check retired half of them, and the
//! three classes it caught are worth naming because each is easy to repeat:
//!
//!  * **The value was the default.** `color.grep.match=red bold`,
//!    `color.status.untracked=red` and `color.diff.func=cyan` are all git's own
//!    defaults, so setting them changed nothing. Every colour value here is now
//!    [`COLOR`] — `bold ul 202`, which no default uses.
//!  * **The flag did not exist.** `git status` has no `--color` option at all
//!    (it answers `error: unknown option`), in long *and* short form; eleven
//!    cases were comparing a usage error. Status colour comes from
//!    `color.status` / `color.ui`, so those cases carry the umbrella key.
//!  * **The shape had no element to change.** `color.diff.old` needs a removed
//!    line and `Branched`'s tip commit only adds; `diff.context` needs a file
//!    longer than its context; `status.aheadBehind` needs an upstream. Each is
//!    now paired with a shape that has the element.
//!
//! Retired as unreachable rather than assumed absent — every one of these was
//! tried against the real fixtures and could not be made to bite:
//! `color.diff.func` (no fixture yields a hunk header carrying a function
//! name), the `*Moved` / `*Alternative` / `*Dimmed` slots (no fixture renders a
//! moved block under `--color-moved`), `color.decorate.remoteBranch`,
//! `color.grep.context`, `color.branch.plain`, `color.status.localBranch` /
//! `.remoteBranch`, `color.blame.*` (`highlightRecent` is a function of the
//! clock, which [`crate::env::harden`] pins), the `core.autocrlf` / `core.eol` /
//! `core.safecrlf` family (no fixture carries CRLF content or a `text`
//! attribute), `core.ignoreCase`, `core.fileMode`, `core.ignoreStat`,
//! `core.bare`, `core.bigFileThreshold`, `core.excludesFile`,
//! `core.attributesFile`, `diff.indentHeuristic`, `diff.suppressBlankEmpty`,
//! `diff.algorithm`, `diff.interHunkContext`, `diff.ignoreSubmodules`,
//! `status.relativePaths`, `status.branch`, `status.renames`,
//! `status.submoduleSummary`, `log.showRoot`, `log.follow`,
//! `log.initialDecorationSet`, `log.diffMerges`, `grep.fullName`,
//! `grep.patternType`, `grep.extendedRegexp`, `grep.threads`, `blame.date`,
//! `blame.coloring` and `versionsort.suffix`. Several would bite against a
//! fixture that does not exist yet; adding one is the way to bring them back,
//! not deleting the check that found them.
//!
//! Two standing exclusions: nothing here depends on the clock
//! (`log.date=relative` and `blame.highlightRecent` are a function of *now*, so
//! the two sides would disagree whenever a second ticked between them) or on
//! how the local git was built (`grep.patternType=perl` needs PCRE).

use crate::fixture::Shape;
use crate::runner::{Case, ConfigEntry, ConfigScope};

/// The value every colour slot is set to: bold, underlined, and 256-colour
/// index 202. Deliberately a combination no git default uses, so a slot that is
/// honoured looks different from a slot that is not.
const COLOR: &str = "bold ul 202";

pub fn cases(out: &mut Vec<Case>) {
    color_slots(out);
    color_umbrellas(out);
    diff_rendering(out);
    core_semantics(out);
    per_verb_defaults(out);
    scoped(out);
}

/// One read under one setting, delivered through `-c`.
fn under(out: &mut Vec<Case>, cmd: &'static str, args: &[&str], shape: Shape, cfg: (&str, &str)) {
    out.push(Case::new(cmd, args, shape).with_config(&[cfg]));
}

/// One colour slot, on a verb that has a `--color` flag.
fn slot(out: &mut Vec<Case>, cmd: &'static str, args: &[&str], shape: Shape, key: &str) {
    under(out, cmd, args, shape, (key, COLOR));
}

/// One colour slot on `status`, which has no `--color` flag: the umbrella key
/// turns colour on and the slot says what it should look like.
fn status_slot(out: &mut Vec<Case>, args: &[&str], key: &str) {
    out.push(
        Case::new("status", args, Shape::Dirty)
            .with_config(&[("color.status", "always"), (key, COLOR)]),
    );
}

/// The individual colour slots, each paired with a read that paints it.
fn color_slots(out: &mut Vec<Case>) {
    for key in ["color.diff.meta", "color.diff.frag", "color.diff.new", "color.diff.context"] {
        slot(out, "diff", &["diff", "--color=always", "HEAD~1", "HEAD"], Shape::Branched, key);
    }
    // A removed line to paint, and a whitespace error to highlight: both need
    // the shape whose commits are whitespace.
    slot(out, "diff", &["diff", "--color=always", "HEAD~1", "HEAD"], Shape::Whitespace, "color.diff.old");
    slot(
        out,
        "diff",
        &["diff", "--color=always", "--ws-error-highlight=all", "HEAD~1", "HEAD"],
        Shape::Whitespace,
        "color.diff.whitespace",
    );
    // The commit slot belongs to the log header, not to a bare diff.
    slot(out, "log", &["log", "-p", "--color=always", "-1"], Shape::Branched, "color.diff.commit");

    for key in [
        "color.status.header",
        "color.status.added",
        "color.status.changed",
        "color.status.untracked",
        "color.status.branch",
    ] {
        status_slot(out, &["status"], key);
    }
    for key in ["color.status.added", "color.status.changed", "color.status.untracked"] {
        status_slot(out, &["status", "--short", "--branch"], key);
    }

    for key in ["color.branch.current", "color.branch.local"] {
        slot(out, "branch", &["branch", "--color=always", "-a", "-vv"], Shape::Branched, key);
    }
    // The remote and upstream slots need a remote to track.
    for key in ["color.branch.remote", "color.branch.upstream"] {
        slot(out, "branch", &["branch", "--color=always", "-a", "-vv"], Shape::BehindRemote, key);
    }

    for key in ["color.decorate.branch", "color.decorate.tag", "color.decorate.HEAD"] {
        slot(out, "log", &["log", "--color=always", "--decorate", "--oneline", "-3"], Shape::Branched, key);
    }

    for key in [
        "color.grep.filename",
        "color.grep.linenumber",
        "color.grep.match",
        "color.grep.separator",
        "color.grep.selected",
    ] {
        slot(out, "grep", &["grep", "--color=always", "-n", "-C1", "fn"], Shape::Linear, key);
    }
}

/// The umbrella keys, which turn colour on with no flag at all — the path a
/// user's `~/.gitconfig` actually takes.
fn color_umbrellas(out: &mut Vec<Case>) {
    under(out, "diff", &["diff", "HEAD~1", "HEAD"], Shape::Branched, ("color.ui", "always"));
    under(out, "diff", &["diff", "HEAD~1", "HEAD"], Shape::Branched, ("color.diff", "always"));
    under(out, "log", &["log", "--decorate", "--oneline", "-3"], Shape::Branched, ("color.ui", "always"));
    under(out, "status", &["status"], Shape::Dirty, ("color.status", "always"));
    under(out, "branch", &["branch", "-a"], Shape::Branched, ("color.branch", "always"));
    under(out, "grep", &["grep", "-n", "fn"], Shape::Linear, ("color.grep", "always"));
}

/// `diff.*`: the settings that change a patch's shape rather than its colour.
fn diff_rendering(out: &mut Vec<Case>) {
    for (key, value) in
        [("diff.srcPrefix", "old/"), ("diff.dstPrefix", "new/"), ("diff.noPrefix", "true")]
    {
        under(out, "diff", &["diff", "HEAD~1", "HEAD"], Shape::Branched, (key, value));
    }
    // `diff.orderFile` naming a file that is not there. Not an ordering test —
    // no fixture carries an order file whose globs reorder its paths — but the
    // refusal is behaviour of its own: stock reads the file eagerly and dies
    // `fatal: failed to read orderfile '.gitattributes': No such file or
    // directory` at 128 before printing any of the patch. Compared on stderr,
    // because a port that ignores the setting prints the whole diff at 0 and
    // the message is the only thing that separates the two.
    out.push(
        Case::strict("diff", &["diff", "HEAD~1", "HEAD"], Shape::Branched)
            .with_config(&[("diff.orderFile", ".gitattributes")]),
    );
    // The mnemonic prefixes (`i/`, `w/`, `c/`, `o/`) replace `a/`…`b/` only when
    // the operands say which side is which, so a commit-vs-commit read cannot
    // show them and a worktree read can.
    under(out, "diff", &["diff"], Shape::Dirty, ("diff.mnemonicPrefix", "true"));
    // Context width needs a file longer than its context.
    for value in ["1", "0"] {
        under(out, "diff", &["diff", "HEAD~1", "HEAD"], Shape::Whitespace, ("diff.context", value));
    }
    under(
        out,
        "diff",
        &["diff", "--color=always", "HEAD~1", "HEAD"],
        Shape::Whitespace,
        ("diff.wsErrorHighlight", "all"),
    );
    // Rename detection and the widths of the stat it renders.
    for (key, value) in
        [("diff.renames", "false"), ("diff.renames", "copies"), ("diff.renameLimit", "1")]
    {
        under(out, "diff", &["diff", "--name-status", "HEAD~3", "HEAD"], Shape::Renamed, (key, value));
    }
    for (key, value) in [("diff.statNameWidth", "8"), ("diff.statGraphWidth", "6")] {
        under(out, "diff", &["diff", "--stat", "HEAD~2", "HEAD"], Shape::Renamed, (key, value));
    }
    // A submodule's diff has three renderings; `short` is the default, so the
    // other two are the ones that can tell an implementation apart.
    for value in ["log", "diff"] {
        under(out, "diff", &["diff", "HEAD~1", "HEAD"], Shape::Submodule, ("diff.submodule", value));
    }
}

/// `core.*`: how a path and a comment are spelled on the way out.
fn core_semantics(out: &mut Vec<Case>) {
    // The only shape with a path to quote.
    under(out, "ls-files", &["ls-files"], Shape::AwkwardPaths, ("core.quotePath", "false"));
    under(out, "log", &["log", "--oneline", "-3"], Shape::Branched, ("core.abbrev", "16"));

    // What counts as a comment, observed through the verb whose whole job is to
    // strip them. `auto` is absent: it resolves to `#` here, which is the
    // default, so it cannot discriminate.
    for (key, value) in [("core.commentChar", ";"), ("core.commentString", "//")] {
        out.push(
            Case::with_stdin(
                "stripspace",
                &["stripspace", "--strip-comments"],
                Shape::Linear,
                b"# hash comment\n; semi comment\n// slash comment\nreal line\n",
            )
            .with_config(&[(key, value)]),
        );
    }
}

/// The per-verb defaults: the keys a user sets once and then reads the output
/// of for years.
fn per_verb_defaults(out: &mut Vec<Case>) {
    under(out, "status", &["status"], Shape::Dirty, ("status.short", "true"));
    under(out, "status", &["status"], Shape::Dirty, ("status.showUntrackedFiles", "no"));
    under(out, "status", &["status", "--short"], Shape::Dirty, ("status.showUntrackedFiles", "no"));
    under(out, "status", &["status"], Shape::Stashed, ("status.showStash", "true"));
    // Ahead/behind counts need an upstream to be ahead of.
    under(out, "status", &["status"], Shape::BehindRemote, ("status.aheadBehind", "false"));

    for (key, value) in [
        ("log.abbrevCommit", "true"),
        ("log.decorate", "full"),
        ("log.decorate", "short"),
        ("log.date", "iso"),
        ("log.date", "raw"),
        ("log.date", "unix"),
        ("log.date", "short"),
    ] {
        under(out, "log", &["log", "-3"], Shape::Merged, (key, value));
    }

    for (key, value) in [("grep.lineNumber", "true"), ("grep.column", "true")] {
        under(out, "grep", &["grep", "fn"], Shape::Linear, (key, value));
    }
    for (key, value) in
        [("blame.showEmail", "true"), ("blame.blankBoundary", "true"), ("blame.showRoot", "true")]
    {
        under(out, "blame", &["blame", "README.md"], Shape::Branched, (key, value));
    }

    under(out, "branch", &["branch", "-a"], Shape::Branched, ("branch.sort", "-refname"));
    under(out, "branch", &["branch", "-a"], Shape::Branched, ("column.branch", "always"));
    under(out, "tag", &["tag", "-l"], Shape::TagChain, ("tag.sort", "-refname"));
}

/// The same settings delivered from a file rather than from `-c`.
///
/// Not a duplicate of the cases above: `-c` is parsed by
/// `git_config_from_parameters()` before any repository is found, while
/// `.git/config` and the global file go through the file parser and the scope
/// ordering. A port that reads the command-line list and nothing else passes
/// every `-c` case in this module and fails all of these. The pairs naming one
/// key in two scopes are the precedence question, which neither scope alone can
/// ask — and they are readable because the two values render differently, so a
/// port that merges in the wrong order prints the loser.
fn scoped(out: &mut Vec<Case>) {
    out.push(
        Case::new("diff", &["diff", "--color=always", "HEAD~1", "HEAD"], Shape::Branched)
            .with_scoped_config(vec![ConfigEntry::set(ConfigScope::Repo, "color.diff.meta", COLOR)]),
    );
    out.push(
        Case::new("status", &["status"], Shape::Dirty)
            .with_scoped_config(vec![ConfigEntry::set(ConfigScope::Global, "status.short", "true")]),
    );
    out.push(
        Case::new("log", &["log", "-3"], Shape::Merged)
            .with_scoped_config(vec![ConfigEntry::set(ConfigScope::Repo, "log.date", "iso")]),
    );
    out.push(
        Case::new("grep", &["grep", "fn"], Shape::Linear)
            .with_scoped_config(vec![ConfigEntry::set(ConfigScope::Global, "grep.lineNumber", "true")]),
    );
    // Precedence: the repository's answer wins over the global one.
    out.push(
        Case::new("log", &["log", "-2"], Shape::Merged).with_scoped_config(vec![
            ConfigEntry::set(ConfigScope::Global, "log.date", "raw"),
            ConfigEntry::set(ConfigScope::Repo, "log.date", "short"),
        ]),
    );
    out.push(
        Case::new("status", &["status"], Shape::Dirty).with_scoped_config(vec![
            ConfigEntry::set(ConfigScope::Global, "status.showUntrackedFiles", "no"),
            ConfigEntry::set(ConfigScope::Repo, "status.showUntrackedFiles", "all"),
        ]),
    );
    // And `-c` outranks both, which is the third layer of the same question.
    out.push(
        Case::new("status", &["status"], Shape::Dirty)
            .with_config(&[("status.showUntrackedFiles", "no")])
            .with_scoped_config(vec![ConfigEntry::set(
                ConfigScope::Repo,
                "status.showUntrackedFiles",
                "all",
            )]),
    );
}
