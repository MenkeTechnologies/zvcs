//! Differential corpus cases for git's **exclude precedence system** — the part
//! of `dir.c` that decides *which* rule wins when several match one path, as
//! opposed to whether a single rule matches at all.
//!
//! Every case here is compared against stock git for stdout, exit code and
//! post-command repository state.
//!
//! # Why a module of its own
//!
//! `dir.c` does not evaluate patterns; it evaluates *groups* of pattern lists in
//! a fixed order and stops at the first group that answers. Three facts follow
//! from that shape, and none of them is visible to a case that puts one rule in
//! front of one path:
//!
//! * **A source outranks a source.** `gitignore(5)` lists four, highest first:
//!   the command line, then the `.gitignore` files from the path's own directory
//!   up to the toplevel, then `$GIT_DIR/info/exclude`, then `core.excludesFile`.
//!   Inside one source the last matching pattern wins; between sources the
//!   higher one wins outright, so a later negation in a *lower* source cannot
//!   resurrect anything.
//! * **Depth outranks height.** Within the `.gitignore` group the deeper file is
//!   consulted first, so `sub/.gitignore`'s `!*.log` beats the toplevel `*.log`
//!   for every path under `sub/`, at any depth below it.
//! * **An excluded directory is a wall.** Once a leading directory matches, git
//!   never descends, and no pattern about a path *inside* it can take effect —
//!   "It is not possible to re-include a file if a parent directory of that file
//!   is excluded", `gitignore(5)`. This is the trap: the rule is not "last match
//!   wins" but "last match on the shallowest excluded component wins", and a
//!   port that implements a flat matcher gets every other axis right and this
//!   one wrong.
//!
//! A port re-derives all three per verb, because each verb reaches them through
//! a different door — `status --ignored`, `clean -X`, `add`'s refusal,
//! `ls-files -o/-i`, `check-ignore -v`, `grep --untracked`, `stash -u`. The
//! corpus keeps finding that a port implements the door. So the axis here is
//! *one precedence question through many doors*, and the same question is asked
//! of `check-ignore -v` (which names the winning file and line) and of a verb
//! that only shows the outcome, so a port that reports the right source while
//! selecting the wrong set is caught by the pair.
//!
//! # Territory, against the two neighbours that share the subject
//!
//! `info_attrs.rs` owns `check-ignore`'s *flags and framing* — `-q`, `-z`,
//! `--stdin`, `--non-matching`, the `path: pattern` triple — on the shapes with
//! no rules at all. `shape_reach.rs` owns the *single-rule* answers on
//! [`Shape::Attributes`]: one case per rule form (bare glob, negation,
//! directory, root-anchored, `**`, nested file, `info/exclude`) against the path
//! that rule was written for. `pathspec_stdin.rs` owns pathspec magic and
//! `core.precomposeunicode` as they apply to *pathspecs*. `index_plumbing.rs`
//! owns `ls-files`'s `-o`/`-i`/`--directory` selection and already carries
//! `--exclude=*.log`, `--exclude-from=.gitignore` and
//! `--exclude-per-directory=.gitignore`. `add_rm_mv_clean.rs` owns `clean`'s
//! `-d`/`-x`/`-X` sets and `add`'s refusal on `logs/debug.log`, `build/`,
//! `excluded-by-info.txt` and `sub/local-scratch.txt`.
//!
//! Nothing above asks *which source won*. That is what is left, and it is what
//! this module contains: every reachable adjacent pair of sources with opposite
//! senses, the depth ordering inside the `.gitignore` group, the
//! excluded-directory wall from four sides, the pattern spellings that only
//! matter once two rules disagree, and `core.excludesFile` pointed at each class
//! of target.
//!
//! # Which of the five sources this module could reach, and which it could not
//!
//! A case is one argv against a pristine copy and **cannot create a file**, so a
//! rule has to be already in the fixture or arrive through `-x`/`-e`/
//! `--exclude-from=<existing path>` or `-c core.excludesFile=<existing path>`.
//! [`Shape::Attributes`] carries, verbatim:
//!
//! | source | content |
//! |--------|---------|
//! | `.gitignore` | `*.log`, `!important.log`, `build/`, `/notes.tmp`, `**/deep-ignored/`, `*.o` |
//! | `sub/.gitignore` | `!*.log`, `local-*` |
//! | `.git/info/exclude` | `excluded-by-info.txt` |
//! | `core.excludesFile` | unset |
//! | skip-worktree | none — [`Shape::Sparse`] carries those |
//!
//! * **The command line** is fully reachable: `-x`/`--exclude` and `clean -e`
//!   take an arbitrary pattern, so both senses against every other source are
//!   available, and so is last-wins *within* the command-line group.
//! * **The `.gitignore` group** is fully reachable, including its depth
//!   ordering: `sub/.gitignore`'s `!*.log` contradicts the toplevel `*.log`, and
//!   `check-ignore` answers about paths that do not exist, so `sub/x.log` and
//!   `sub/a/b/x.log` are askable without creating anything.
//! * **`core.excludesFile`** is reachable *as a source* by pointing it at a
//!   fixture file whose lines happen to be patterns. Only three files in the
//!   shape have any: `.gitignore`, `sub/.gitignore` and `.git/info/exclude`.
//!   `sub/.gitignore` is the useful one — read as a global excludes file its
//!   `!*.log` contradicts the toplevel `.gitignore`, which is the
//!   `core.excludesFile` ↔ `.gitignore` pair. Every other tracked file
//!   (`.gitattributes`, `.mailmap`, `docs/manual.md`, `src/tabs.rs`, …) parses
//!   as patterns that match nothing, which is worth exactly one case each as a
//!   "reads it, matches nothing" control.
//! * **`$GIT_DIR/info/exclude`** is reachable only in the *ignore* sense. Its
//!   one pattern, `excluded-by-info.txt`, is matched by no other source, so
//!   `info/exclude` against a **`.gitignore` negation** and `info/exclude`
//!   against a **`core.excludesFile` negation** — the two adjacent pairs that
//!   would need a rule contradicting it in one of those files — are **not
//!   reachable** and are not attempted. They would need a file this harness
//!   cannot write. What *is* measured is `info/exclude` losing to the command
//!   line (`-x '!excluded-by-info.txt'`, `clean -e '!excluded-by-info.txt'`),
//!   which pins the same ordering from the other side.
//! * **skip-worktree** is reachable through [`Shape::Sparse`], whose cone
//!   checkout sets the bit on `outside/*`. It is not an exclusion and git treats
//!   it as none — `check-ignore` says nothing about a skip-worktree path — but
//!   it decides what is *on disk* for the walk to find, so the pair "an exclude
//!   pattern over a sparse-excluded cone" is the interaction, and it is here.
//!   `assume-unchanged` is **not** reachable: no shape sets it, and setting it
//!   takes an `update-index` invocation this module cannot chain to a second
//!   command.
//!
//! # What is deliberately not measured
//!
//! * The *file*-parser rules — a `#` comment line, a blank line, trailing spaces
//!   stripped unless written `\ ` — apply to `add_patterns_from_buffer`, which
//!   reads a **file**. `-x` goes through `add_pattern` and skips all of it. So
//!   the command-line cases below pin the *absence* of that stripping (a
//!   trailing space, a leading `#`, a trailing backslash: all literal, none
//!   matching), and the file-side of the same rules is only reachable through
//!   whatever comment and blank lines a fixture file already has —
//!   `docs/manual.md`, which is `# manual`, a blank line, `prose`, is used for
//!   exactly that.
//! * `archive` does **not** consult this machinery at all; it filters on the
//!   `export-ignore` *attribute*. One case pins that a `core.excludesFile` which
//!   changes every other verb's answer changes nothing in the tar stream.
//! * `describe` has no relation to it and is not represented.

use crate::fixture::Shape;
use crate::runner::Case;

/// Append this subsystem's cases to the corpus.
pub fn cases(out: &mut Vec<Case>) {
    source_ordering(out);
    depth_ordering(out);
    excluded_directory_wall(out);
    pattern_spellings(out);
    the_doors(out);
    excludes_file_targets(out);
    skip_worktree(out);
    awkward_and_decomposed(out);
}

/// Shorthand for one case against [`Shape::Attributes`].
fn at(out: &mut Vec<Case>, cmd: &'static str, args: &[&str]) {
    out.push(Case::new(cmd, args, Shape::Attributes));
}

/// Shorthand for one case against [`Shape::Attributes`] run from `cwd`.
fn at_in(out: &mut Vec<Case>, cmd: &'static str, args: &[&str], cwd: &'static str) {
    out.push(Case::new(cmd, args, Shape::Attributes).in_dir(cwd));
}

/// Shorthand for one case against [`Shape::Attributes`] with `core.excludesFile`
/// pointed at a path inside the fixture.
fn at_ef(out: &mut Vec<Case>, cmd: &'static str, args: &[&str], file: &'static str) {
    out.push(Case::new(cmd, args, Shape::Attributes).with_config(&[("core.excludesFile", file)]));
}

// ---------------------------------------------------------------------------
// 1. Source ordering: the same path, two sources, opposite senses
// ---------------------------------------------------------------------------

/// One adjacent pair of sources at a time, each asked in both senses.
///
/// Observed against stock git 2.55.0 on a copy of [`Shape::Attributes`]:
///
/// | invocation | stdout |
/// |------------|--------|
/// | `ls-files -o --exclude-standard` (control) | `important.log`, `tracked-looking.txt` |
/// | `… -x '!*.log'` | control **+ `logs/debug.log`** |
/// | `… -x important.log` | `tracked-looking.txt` only |
/// | `… -x '!excluded-by-info.txt'` | control **+ `excluded-by-info.txt`** |
/// | `… -x '*.log' -x '!*.log'` | control **+ `logs/debug.log`** |
/// | `… -x '!*.log' -x '*.log'` | `tracked-looking.txt` only |
///
/// Read down that table: the command line un-ignores what `.gitignore` ignores
/// (`!*.log` resurrects `logs/debug.log`), ignores what `.gitignore`
/// un-ignores (`important.log` beats `!important.log`), and un-ignores what
/// `.git/info/exclude` ignores. Within the command-line group the **last**
/// pattern wins, and the last one still outranks every file — the final row is
/// the one that separates "command line wins" from "everything is one flat list
/// and the last line of the last file wins", because a flat implementation puts
/// `.gitignore`'s `!important.log` after both `-x` patterns and keeps
/// `important.log` listed.
///
/// The `core.excludesFile` half uses `sub/.gitignore` as the excludes file: read
/// globally its `!*.log` contradicts the toplevel `*.log`, and `.gitignore` is
/// the higher source, so **nothing changes** — `logs/debug.log` stays ignored
/// and `check-ignore -v` still answers `.gitignore:1:*.log`. A port that appends
/// the excludes file last and takes the last match resurrects it.
fn source_ordering(out: &mut Vec<Case>) {
    // Command line vs `.gitignore`, both senses, through `-o` and through `-i`.
    at(out, "ls-files", &["ls-files", "-o", "--exclude-standard", "-x", "!*.log"]);
    at(out, "ls-files", &["ls-files", "-o", "--exclude-standard", "-x", "important.log"]);
    at(out, "ls-files", &["ls-files", "-o", "-i", "--exclude-standard", "-x", "!*.log"]);
    at(out, "ls-files", &["ls-files", "-o", "-i", "--exclude-standard", "-x", "important.log"]);

    // Last wins *inside* the command-line group, and the group still outranks
    // every file below it.
    at(out, "ls-files", &["ls-files", "-o", "--exclude-standard", "-x", "*.log", "-x", "!*.log"]);
    at(out, "ls-files", &["ls-files", "-o", "--exclude-standard", "-x", "!*.log", "-x", "*.log"]);

    // Command line vs `$GIT_DIR/info/exclude`. The only sense reachable — see
    // the module header for why the opposite one is not.
    at(out, "ls-files", &["ls-files", "-o", "--exclude-standard", "-x", "!excluded-by-info.txt"]);
    at(out, "ls-files", &["ls-files", "-o", "-i", "--exclude-standard", "-x", "!excluded-by-info.txt"]);
    // `Would remove` loses `excluded-by-info.txt` from the `-X` set entirely.
    at(out, "clean", &["clean", "-ndX", "-e", "!excluded-by-info.txt"]);

    // Command line vs `.gitignore` through `clean`, which selects rather than
    // lists: `-e tracked-looking.txt` moves an un-ignored path into the `-X`
    // set, and `-e '!important.log'` cannot move one out of it because
    // `.gitignore` already says the same thing.
    at(out, "clean", &["clean", "-ndX", "-e", "tracked-looking.txt"]);
    at(out, "clean", &["clean", "-ndX", "-e", "!*.log", "-e", "*.log"]);

    // `core.excludesFile` vs `.gitignore`: the excludes file says `!*.log`, the
    // toplevel `.gitignore` says `*.log`, and `.gitignore` is higher.
    at_ef(out, "ls-files", &["ls-files", "-o", "--exclude-standard"], "sub/.gitignore");
    at_ef(out, "ls-files", &["ls-files", "-o", "-i", "--exclude-standard"], "sub/.gitignore");
    at_ef(out, "check-ignore", &["check-ignore", "-v", "logs/debug.log"], "sub/.gitignore");
    at_ef(out, "check-ignore", &["check-ignore", "-v", "important.log"], "sub/.gitignore");
    at_ef(out, "clean", &["clean", "-ndX"], "sub/.gitignore");
    at_ef(out, "status", &["status", "--porcelain", "--ignored"], "sub/.gitignore");

    // Command line vs `core.excludesFile`, with `.gitignore` silent about the
    // path: the excludes file is `.git/info/exclude`, so `excluded-by-info.txt`
    // is claimed by the two lowest sources at once and the command line still
    // takes it back.
    out.push(
        Case::new(
            "ls-files",
            &["ls-files", "-o", "--exclude-standard", "-x", "!excluded-by-info.txt"],
            Shape::Attributes,
        )
        .with_config(&[("core.excludesFile", ".git/info/exclude")]),
    );
    at_ef(out, "check-ignore", &["check-ignore", "-v", "excluded-by-info.txt"], ".git/info/exclude");
}

// ---------------------------------------------------------------------------
// 2. Depth: the nested `.gitignore` beats its parent, at every level below it
// ---------------------------------------------------------------------------

/// The `.gitignore` group ordered by depth, asked about paths that do not exist.
///
/// `check-ignore` answers about a **name**, not a file, which is the only way to
/// reach this axis without creating anything: [`Shape::Attributes`] has no `.log`
/// file under `sub/`, and `sub/.gitignore`'s `!*.log` is the single rule in the
/// fixture that contradicts its parent. Observed:
///
/// | invocation | stdout | exit |
/// |------------|--------|------|
/// | `check-ignore -v sub/hypothetical.log` | `sub/.gitignore:1:!*.log\tsub/hypothetical.log` | 0 |
/// | `check-ignore -v sub/sub2/x.log` | `sub/.gitignore:1:!*.log\tsub/sub2/x.log` | 0 |
/// | `check-ignore -v` from `sub`, `hypothetical.log` | `sub/.gitignore:1:!*.log\thypothetical.log` | 0 |
/// | `check-ignore -v` from `sub`, `../logs/hypothetical.log` | `.gitignore:1:*.log\t../logs/hypothetical.log` | 0 |
/// | `check-ignore -v` from `logs`, `debug.log` | `.gitignore:1:*.log\tdebug.log` | 0 |
///
/// Three things are pinned at once. The nested file wins *at every depth below
/// its own directory*, not only in it (`sub/sub2/x.log`). The winning file is
/// named repository-relative while the subject is echoed exactly as typed, even
/// when that is `../logs/…`. And a negated match still exits 0 under `-v`, which
/// is the polarity `check-ignore` inverts twice — a match is 0, and with `-v` a
/// *negative* match is still a match.
fn depth_ordering(out: &mut Vec<Case>) {
    at(out, "check-ignore", &["check-ignore", "-v", "sub/hypothetical.log"]);
    at(out, "check-ignore", &["check-ignore", "-v", "sub/sub2/x.log"]);
    at(out, "check-ignore", &["check-ignore", "sub/hypothetical.log"]);
    // Beside it, the same basename one directory over, where no nested file
    // shadows the toplevel rule.
    at(out, "check-ignore", &["check-ignore", "-v", "logs/hypothetical.log"]);
    // `local-*` has no contradiction anywhere, so it is the control for "the
    // nested file is consulted at all" independent of the negation.
    at(out, "check-ignore", &["check-ignore", "-v", "sub/local-other.txt"]);
    at(out, "check-ignore", &["check-ignore", "-v", "local-toplevel.txt"]);

    // The same questions with the working directory below the toplevel, where
    // the subject is echoed as typed and the pattern file is still named from
    // the root.
    at_in(out, "check-ignore", &["check-ignore", "-v", "hypothetical.log"], "sub");
    at_in(out, "check-ignore", &["check-ignore", "-v", "../logs/hypothetical.log"], "sub");
    at_in(out, "check-ignore", &["check-ignore", "-v", "-n", "local-scratch.txt"], "sub");
    at_in(out, "check-ignore", &["check-ignore", "-v", "debug.log"], "logs");
    at_in(out, "check-ignore", &["check-ignore", "-v", "output.o"], "build");
    at_in(out, "check-ignore", &["check-ignore", "-v", "../notes.tmp"], "sub");
    at_in(out, "check-ignore", &["check-ignore", "-v", "notes.tmp"], "sub");

    // The verbs that only show the outcome, run from the same directory, so a
    // port that reports the right winning file but selects the wrong set fails
    // beside the `check-ignore` case that agreed.
    at_in(out, "ls-files", &["ls-files", "-o", "-i", "--exclude-standard"], "sub");
    at_in(out, "clean", &["clean", "-ndX"], "sub");
    at_in(out, "status", &["status", "--long", "--ignored"], "sub");
}

// ---------------------------------------------------------------------------
// 3. The excluded directory is a wall
// ---------------------------------------------------------------------------

/// "It is not possible to re-include a file if a parent directory of that file
/// is excluded" — `gitignore(5)` — from four sides.
///
/// `build/` is excluded by the toplevel `.gitignore`, and the command line is
/// the *highest* source, so `-x '!build/output.o'` is the strongest re-inclusion
/// this fixture can express. Observed:
///
/// | invocation | `build/output.o` listed? |
/// |------------|--------------------------|
/// | `ls-files -o --exclude-standard -x '!build/output.o'` | **no** |
/// | `ls-files -o --exclude-standard -x '!build/'` | **no** (`*.o` still matches) |
/// | `ls-files -o --exclude-standard -x '!build/' -x '!*.o'` | **yes** |
/// | `ls-files -o --directory --exclude-standard -x '!build/'` | `build/` collapsed, yes |
///
/// The first row is the trap. A flat matcher that takes the last match over all
/// sources sees `!build/output.o` as the final word and lists the file; git
/// never asks, because `build` matched first and the walk stopped there. The
/// third row is what it takes to actually get in: the directory *and* the file
/// both have to be re-included, which is only possible because `*.o` and
/// `build/` are two separate rules.
///
/// `add` states the same thing in its refusal. `git add -n build/output.o`
/// reports the path it refuses as **`build`**, not `build/output.o` — the walk
/// collapsed to the excluded directory before it reached the file — and
/// `git add -n sub/deep-ignored/thing.txt` reports `sub/deep-ignored`. Both go
/// to stderr with exit 1, so both are [`Case::strict`].
fn excluded_directory_wall(out: &mut Vec<Case>) {
    at(out, "ls-files", &["ls-files", "-o", "--exclude-standard", "-x", "!build/output.o"]);
    at(out, "ls-files", &["ls-files", "-o", "--exclude-standard", "-x", "!build/"]);
    at(out, "ls-files", &["ls-files", "-o", "--exclude-standard", "-x", "!build/", "-x", "!*.o"]);
    at(out, "ls-files", &["ls-files", "-o", "--directory", "--exclude-standard", "-x", "!build/"]);
    at(out, "ls-files", &["ls-files", "-o", "--exclude-standard", "-x", "!sub/deep-ignored/thing.txt"]);
    at(out, "ls-files", &["ls-files", "-o", "--exclude-standard", "-x", "!sub/deep-ignored/"]);
    at(out, "ls-files", &["ls-files", "-o", "--exclude-standard", "-x", "!**/deep-ignored/", "-x", "!*"]);
    at(out, "clean", &["clean", "-ndX", "-e", "!build/output.o"]);
    at(out, "clean", &["clean", "-ndX", "-e", "!build/", "-e", "!*.o"]);

    // The wall as `check-ignore` reports it: the *directory's* rule is the
    // answer for every path beneath, at any depth, and the toplevel
    // `!important.log` cannot reach inside `build/`.
    at(out, "check-ignore", &["check-ignore", "-v", "build/deeper/file.txt"]);
    at(out, "check-ignore", &["check-ignore", "-v", "build/important.log"]);
    at(out, "check-ignore", &["check-ignore", "-v", "sub/deep-ignored/nested/deep.log"]);

    // `--ignored=traditional -uall` is the one porcelain that walks *into* an
    // ignored directory, so it is the control showing the file is really there.
    at(out, "status", &["status", "--porcelain", "--ignored=traditional", "-uall"]);

    // The refusal, which names the excluded *directory* for a file argument.
    out.push(Case::strict("add", &["add", "-n", "build/output.o"], Shape::Attributes));
    out.push(Case::strict("add", &["add", "-n", "sub/deep-ignored/thing.txt"], Shape::Attributes));
    out.push(Case::strict(
        "add",
        &["add", "-n", "build/output.o", "sub/deep-ignored/thing.txt"],
        Shape::Attributes,
    ));
    at(out, "add", &["add", "-f", "-n", "build/output.o"]);
    // The same refusal with the advice turned off: the two `hint:` lines go, the
    // list of paths and the exit code stay.
    out.push(
        Case::strict("add", &["add", "-n", "build/output.o"], Shape::Attributes)
            .with_config(&[("advice.addIgnoredFile", "false")]),
    );
}

// ---------------------------------------------------------------------------
// 4. Pattern spellings that only matter once two rules disagree
// ---------------------------------------------------------------------------

/// The pattern grammar, delivered through the command line so the spelling is
/// the only variable, and aimed at the two paths the fixture leaves
/// **un-ignored** — `important.log` (un-ignored by a `.gitignore` negation) and
/// `tracked-looking.txt` (matched by nothing).
///
/// Baseline is `ls-files -o --exclude-standard`, which prints exactly those two.
/// Observed, one `-x` at a time:
///
/// | pattern | effect |
/// |---------|--------|
/// | `/tracked-looking.txt` | anchored at the toplevel — **matches** |
/// | `tracked-looking.txt/` | directory-only — **no match** on a file |
/// | `looking.txt` | no separator, but a basename match is whole-component — **no match** |
/// | `**/tracked-looking.txt` | leading `**` — **matches** |
/// | `*.tx?` | `?` — **matches** |
/// | `[it]*` | class, matches both — output empty |
/// | `**` | matches everything — output empty |
/// | `/important.log` | anchored, and outranks `.gitignore`'s `!important.log` — **matches** |
/// | `important.log/` | directory-only — **no match** |
/// | `tracked-looking.txt ` (trailing space) | **no match**: `-x` does not strip it |
/// | `tracked-looking.txt\` (trailing backslash) | **no match** |
/// | `#tracked-looking.txt` | **no match**: literal, and `-x` has no comment syntax |
/// | `\!important.log` | **no match**: an escaped `!` is a literal `!` |
/// | `` (the empty pattern) | **no match**, exit 0, no diagnostic |
/// | `logs/**/*.log` | `**` in the middle — **no match**, nothing lives two levels under `logs/` |
///
/// The trailing-space and leading-`#` rows are the ones worth stating plainly:
/// those are `gitignore(5)` rules for a **file**, applied by
/// `add_patterns_from_buffer`. `-x` goes through `add_pattern`, which does none
/// of it, so a port that funnels command-line patterns through its file parser
/// strips the space, drops the `#` line, and diverges on both. The `\!` row is
/// the reverse: that escape *is* handled, by the wildmatch pass rather than the
/// parser, so it survives the command-line route.
fn pattern_spellings(out: &mut Vec<Case>) {
    for pattern in [
        "/tracked-looking.txt",
        "tracked-looking.txt/",
        "looking.txt",
        "**/tracked-looking.txt",
        "*.tx?",
        "[it]*",
        "**",
        "/important.log",
        "important.log/",
        "tracked-looking.txt ",
        "tracked-looking.txt\\",
        "#tracked-looking.txt",
        "\\!important.log",
        "",
        "logs/**/*.log",
    ] {
        out.push(Case::new(
            "ls-files",
            &["ls-files", "-o", "--exclude-standard", "-x", pattern],
            Shape::Attributes,
        ));
    }

    // The same two spellings aimed at a *directory* instead of a file. Observed:
    // `-x sub/` and `-x sub` give byte-identical output, and so do `-e logs/` and
    // `-e logs` — the directory-only flag is a no-op once the subject really is a
    // directory. That is the other half of the `tracked-looking.txt/` row above,
    // where the same flag is the whole difference, and the pair is here so a port
    // that treats the trailing slash as "strip it and match anything" passes the
    // file row and this one, while one that treats it as "require a directory
    // *component* match" fails these two alone.
    at(out, "ls-files", &["ls-files", "-o", "-i", "--exclude-standard", "-x", "sub/"]);
    at(out, "ls-files", &["ls-files", "-o", "-i", "--exclude-standard", "-x", "sub"]);
    at(out, "clean", &["clean", "-ndX", "-e", "logs/"]);
    at(out, "clean", &["clean", "-ndX", "-e", "logs"]);

    // The file-side of the comment and blank-line rules, reached through the one
    // fixture file that has both: `docs/manual.md` is `# manual`, a blank line,
    // `prose`. As an excludes file it must contribute exactly one pattern and
    // no diagnostic.
    at_ef(out, "ls-files", &["ls-files", "-o", "--exclude-standard"], "docs/manual.md");
    at_ef(out, "check-ignore", &["check-ignore", "-v", "-n", "prose"], "docs/manual.md");
    at_ef(out, "check-ignore", &["check-ignore", "-v", "-n", "manual"], "docs/manual.md");
    at(out, "ls-files", &["ls-files", "-o", "--exclude-standard", "--exclude-from=docs/manual.md"]);
}

// ---------------------------------------------------------------------------
// 5. The verbs: one machinery, many doors
// ---------------------------------------------------------------------------

/// Each verb reaches `dir.c` through its own option, and each one is asked the
/// same precedence question the module already asked `ls-files`.
///
/// `grep` is the sharpest of them, because its two spellings answer *opposite*
/// defaults. Observed on [`Shape::Attributes`]:
///
/// | invocation | files reported |
/// |------------|----------------|
/// | `grep --untracked -l ignored` | `.gitignore`, `important.log`, `tracked-looking.txt` |
/// | `grep --untracked --no-exclude-standard -l ignored` | those **plus** the six ignored paths and `logs/keep.log` |
/// | `grep --no-index -l ignored` | the wide set (no exclusion by default) |
/// | `grep --no-index --exclude-standard -l ignored` | the narrow set |
///
/// So `--untracked` excludes unless told not to, and `--no-index` includes
/// unless told to exclude — the same machinery wired to opposite defaults in one
/// command. A port that has one `exclude_standard` boolean gets one of the four
/// rows wrong whichever way it initialises it.
///
/// `status --ignored` is the other door with modes rather than a flag:
/// `matching` reports `build/` (the directory matched a rule) while
/// `traditional -uall` reports `build/output.o` (the file inside it), and `no`
/// reports neither. A pathspec narrows the walk without changing which rule
/// wins, which is why `-- build` and `-- sub` are here beside the unrestricted
/// forms.
///
/// `stash -u` and `stash -a` are the destructive door: `-u` takes the untracked
/// files and leaves the ignored ones on disk, `-a` takes both. Nothing in their
/// stdout says which — the whole difference is in the post-command worktree,
/// which is what the harness compares.
fn the_doors(out: &mut Vec<Case>) {
    // grep: four rows of the table above.
    at(out, "grep", &["grep", "--untracked", "-l", "ignored"]);
    at(out, "grep", &["grep", "--untracked", "--no-exclude-standard", "-l", "ignored"]);
    at(out, "grep", &["grep", "--no-index", "-l", "ignored"]);
    at(out, "grep", &["grep", "--no-index", "--exclude-standard", "-l", "ignored"]);
    at_ef(out, "grep", &["grep", "--untracked", "-l", "ignored"], "sub/.gitignore");
    at_in(out, "grep", &["grep", "--untracked", "--no-exclude-standard", "-l", "ignored"], "sub");

    // status: the three `--ignored` modes crossed with `-uall` and a pathspec.
    at(out, "status", &["status", "--porcelain", "--ignored=no", "-uall"]);
    at(out, "status", &["status", "--porcelain", "--ignored", "--", "build"]);
    at(out, "status", &["status", "--porcelain", "--ignored=matching", "--", "sub"]);
    // The one combination status refuses outright:
    // `fatal: Unsupported combination of ignored and untracked-files arguments`.
    out.push(Case::strict(
        "status",
        &["status", "--porcelain", "--ignored=matching", "-uno"],
        Shape::Attributes,
    ));
    at(out, "status", &["status", "--long", "--ignored=matching"]);

    // clean: `-e` is the same command-line source `ls-files` spells `-x`, and
    // `-X` is the only selector that asks the machinery for the *complement*.
    at(out, "clean", &["clean", "-ndX", "-e", "!*"]);
    at(out, "clean", &["clean", "-ndX", "-e", "**/deep-ignored/", "-e", "!build/"]);
    at(out, "clean", &["clean", "-nd", "-e", "important.log"]);
    at_ef(out, "clean", &["clean", "-nd"], "sub/.gitignore");

    // ls-files: `-X`/`--exclude-from`, which is named on the command line and is
    // **not** command-line *precedence*. `dir.c:add_patterns_from_file()` appends
    // to `EXC_FILE` — the same group as `core.excludesFile` and `info/exclude` —
    // while `-x` appends to `EXC_CMDL`. So the two spellings of "a pattern from
    // the command line" sit on opposite sides of the `.gitignore` group, and the
    // pair below is what shows it: observed on stock,
    // `-o --exclude-standard -x '!*.log'` lists `logs/debug.log` while
    // `-o --exclude-standard -X sub/.gitignore` (whose first line *is* `!*.log`)
    // does not. Below that, the option that *replaces* the per-directory
    // filename: `--exclude-per-directory=.gitattributes` after
    // `--exclude-standard` leaves `info/exclude` in force and drops every
    // `.gitignore` in the tree, because the last of the two to be parsed sets the
    // name.
    at(out, "ls-files", &["ls-files", "-o", "-X", "sub/.gitignore"]);
    at(out, "ls-files", &["ls-files", "-o", "--exclude-standard", "-X", "sub/.gitignore"]);
    at(out, "ls-files", &["ls-files", "-o", "-i", "--exclude-standard", "-X", "sub/.gitignore"]);
    at(out, "ls-files", &["ls-files", "-o", "--exclude-standard", "--exclude-from=.git/info/exclude"]);
    at(out, "ls-files", &["ls-files", "-o", "--exclude-standard", "--exclude-per-directory=.gitattributes"]);
    at(out, "ls-files", &["ls-files", "-o", "-i", "--exclude-standard", "--exclude-per-directory=.gitattributes"]);

    // check-ignore's own doors: `--no-index` drops the index veto that keeps a
    // tracked path out of the answer, `--index` keeps it, and both have to agree
    // with the porcelain above about which *rule* won.
    at(out, "check-ignore", &["check-ignore", "-v", "--no-index", "important.log"]);
    at(out, "check-ignore", &["check-ignore", "-v", "--no-index", "-n", "important.log"]);
    at(out, "check-ignore", &["check-ignore", "-v", "--no-index", ".gitignore"]);
    at_ef(out, "check-ignore", &["check-ignore", "-v", "--no-index", "logs/keep.log"], "sub/.gitignore");

    // stash: the exclusion decides what is *removed* from the worktree, and only
    // the post-command state says which.
    at(out, "stash", &["stash", "push", "-u", "-m", "precedence untracked"]);
    at(out, "stash", &["stash", "push", "-a", "-m", "precedence all"]);

    // archive, which does *not* consult any of it: an excludes file that changes
    // every other verb's answer changes nothing in the stream. Byte-identical to
    // the plain `archive --format=tar HEAD` shape_reach.rs already carries, which
    // is why only the configured half is here.
    at_ef(out, "archive", &["archive", "--format=tar", "HEAD"], "sub/.gitignore");
}

// ---------------------------------------------------------------------------
// 6. What `core.excludesFile` may point at
// ---------------------------------------------------------------------------

/// The four classes of target, and the two error paths `--exclude-from` shares
/// with it.
///
/// Observed against stock:
///
/// | value | behaviour |
/// |-------|-----------|
/// | a file with patterns | read, patterns applied at the lowest precedence |
/// | a path that does not exist | **silent**: no warning, no change |
/// | the empty string | **silent**: treated as unset |
/// | a **directory** | `fatal: cannot use src as an exclude file`, exit 128 |
///
/// The directory case is the discriminating one — a port that opens the path and
/// treats a read error as "no patterns" agrees with the first three rows and
/// silently continues on the fourth. `--exclude-from` produces the identical
/// message for both a directory and a missing file, which is the asymmetry worth
/// pinning: the same non-existent path is fatal to `--exclude-from` and silent
/// to `core.excludesFile`.
fn excludes_file_targets(out: &mut Vec<Case>) {
    // A file with patterns, seen by the verb that names the winning source.
    at_ef(out, "check-ignore", &["check-ignore", "-v", "notes.tmp"], ".gitignore");
    at_ef(out, "check-ignore", &["check-ignore", "-v", "sub/local-scratch.txt"], ".gitignore");
    at_ef(out, "ls-files", &["ls-files", "-o", "--exclude-standard"], ".gitignore");

    // Files whose lines parse as patterns that match nothing: the "reads it,
    // finds nothing" control, which separates "ignored the setting" from
    // "honoured it".
    for file in [".gitattributes", ".mailmap", "src/tabs.rs"] {
        out.push(
            Case::new("ls-files", &["ls-files", "-o", "-i", "--exclude-standard"], Shape::Attributes)
                .with_config(&[("core.excludesFile", file)]),
        );
    }
    at_ef(out, "check-ignore", &["check-ignore", "-v", "-n", "tracked-looking.txt"], ".gitattributes");

    // Silent non-targets.
    at_ef(out, "status", &["status", "--porcelain", "--ignored"], "nosuch/file");
    at_ef(out, "status", &["status", "--porcelain", "--ignored"], "");
    at_ef(out, "check-ignore", &["check-ignore", "-v", "logs/debug.log"], "nosuch/file");

    // The fatal one, plus the two `--exclude-from` refusals that share the
    // message. Each is deliberate error path, so each compares stderr.
    out.push(
        Case::strict("status", &["status", "--porcelain", "--ignored"], Shape::Attributes)
            .with_config(&[("core.excludesFile", "src")]),
    );
    out.push(
        Case::strict("clean", &["clean", "-ndX"], Shape::Attributes)
            .with_config(&[("core.excludesFile", "sub")]),
    );
    out.push(Case::strict(
        "ls-files",
        &["ls-files", "-o", "--exclude-from=nosuch.txt"],
        Shape::Attributes,
    ));
    out.push(Case::strict("ls-files", &["ls-files", "-o", "--exclude-from=src"], Shape::Attributes));
}

// ---------------------------------------------------------------------------
// 7. skip-worktree, which is not an exclusion but decides what the walk finds
// ---------------------------------------------------------------------------

/// [`Shape::Sparse`]'s cone checkout sets `SKIP_WORKTREE` on everything under
/// `outside/`, so `outside/drop.txt` is in the index and **not on disk**, while
/// `outside/stray.txt` is on disk and untracked.
///
/// Observed against stock:
///
/// | invocation | result |
/// |------------|--------|
/// | `check-ignore -v --no-index outside/drop.txt` | no output, exit 1 |
/// | `ls-files -o --exclude-standard -x 'outside/'` | empty |
/// | `ls-files -o -i --exclude-standard -x 'outside/**'` | `outside/stray.txt` |
/// | `clean -ndX -e 'outside/'` | `Would remove outside/stray.txt` |
///
/// The first row is the statement: a skip-worktree bit is **not** an exclusion,
/// and `check-ignore` says nothing about it. The last row is the interaction — a
/// command-line pattern turns the one file that *is* on disk in the sparse-
/// excluded cone into an ignored file, and `-X` then deletes it. A port that
/// conflates "outside the cone" with "ignored" answers the first row with a
/// match and the third with an empty list.
fn skip_worktree(out: &mut Vec<Case>) {
    out.push(Case::new("check-ignore", &["check-ignore", "-v", "--no-index", "outside/drop.txt"], Shape::Sparse));
    out.push(Case::new("check-ignore", &["check-ignore", "-v", "-n", "--no-index", "outside/drop.txt"], Shape::Sparse));
    out.push(Case::new("ls-files", &["ls-files", "-o", "--exclude-standard", "-x", "outside/"], Shape::Sparse));
    out.push(Case::new("ls-files", &["ls-files", "-o", "-i", "--exclude-standard", "-x", "outside/**"], Shape::Sparse));
    out.push(Case::new("clean", &["clean", "-ndX", "-e", "outside/"], Shape::Sparse));
    out.push(Case::new("clean", &["clean", "-ndx", "-e", "stray.txt"], Shape::Sparse));
    out.push(Case::new("status", &["status", "--porcelain", "--ignored=traditional", "-uall"], Shape::Sparse));
    out.push(Case::new("grep", &["grep", "--untracked", "--no-exclude-standard", "-l", "cone"], Shape::Sparse));
}

// ---------------------------------------------------------------------------
// 8. Names the matcher has to normalise before it can compare
// ---------------------------------------------------------------------------

/// Exclude **patterns** — not pathspecs — against the two shapes whose names
/// need work before a comparison means anything.
///
/// [`Shape::AwkwardPaths`] is entirely tracked and carries no ignore file, so
/// `-x` is the only source and `-i -c` is the only selector that reports
/// anything: it lists the *tracked* paths a pattern claims. That makes the
/// output a direct readout of the matcher, with the fixture's quoting rules
/// layered on top — `quote"name.txt` comes back as `"quote\"name.txt"` even
/// under `core.quotePath=false`, because the double quote forces quoting on its
/// own, while `üñïçødé.txt` comes back raw.
///
/// [`Shape::DecomposedPaths`] carries `e` + U+0301 on disk. With
/// `core.precomposeunicode` at its Darwin default, `-x` given the **composed**
/// `é-new.txt` matches the decomposed file on disk; with the setting off it does
/// not, and `ls-files -o` then reports *both* names — the untracked one and the
/// tracked one, which no longer matches its own index entry. That pair is the
/// exclude-pattern half of the normalisation, distinct from the pathspec half
/// `pathspec_stdin.rs` owns.
fn awkward_and_decomposed(out: &mut Vec<Case>) {
    for pattern in ["*\"*", "nested/deep/", "with space.txt", "[abc]*", "*.txt"] {
        out.push(Case::new(
            "ls-files",
            &["ls-files", "-i", "-c", "--exclude-standard", "-x", pattern],
            Shape::AwkwardPaths,
        ));
    }
    out.push(
        Case::new(
            "ls-files",
            &["ls-files", "-i", "-c", "--exclude-standard", "-x", "*.txt"],
            Shape::AwkwardPaths,
        )
        .with_config(&[("core.quotePath", "false")]),
    );
    out.push(Case::new(
        "ls-files",
        &["ls-files", "-i", "-c", "-z", "--exclude-standard", "-x", "with space.txt"],
        Shape::AwkwardPaths,
    ));
    out.push(Case::new(
        "check-ignore",
        &["check-ignore", "-v", "-n", "--no-index", "with space.txt"],
        Shape::AwkwardPaths,
    ));

    // Composed pattern, decomposed name.
    out.push(Case::new(
        "ls-files",
        &["ls-files", "-o", "--exclude-standard", "-x", "\u{e9}-new.txt"],
        Shape::DecomposedPaths,
    ));
    out.push(Case::new(
        "ls-files",
        &["ls-files", "-o", "--exclude-standard", "-x", "e\u{301}-new.txt"],
        Shape::DecomposedPaths,
    ));
    out.push(Case::new(
        "ls-files",
        &["ls-files", "-o", "--exclude-standard", "-x", "*-new.txt"],
        Shape::DecomposedPaths,
    ));
    out.push(
        Case::new(
            "ls-files",
            &["ls-files", "-o", "--exclude-standard", "-x", "\u{e9}-new.txt"],
            Shape::DecomposedPaths,
        )
        .with_config(&[("core.precomposeunicode", "false")]),
    );
    out.push(
        Case::new(
            "ls-files",
            &["ls-files", "-o", "--exclude-standard", "-x", "e\u{301}-new.txt"],
            Shape::DecomposedPaths,
        )
        .with_config(&[("core.precomposeunicode", "false")]),
    );
    out.push(Case::new(
        "check-ignore",
        &["check-ignore", "-v", "-n", "\u{e9}-new.txt"],
        Shape::DecomposedPaths,
    ));
}
