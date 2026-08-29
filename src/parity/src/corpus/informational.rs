//! Differential corpus cases for the informational subsystem.
//!
//! Populated per-command; every case here is compared against stock git for
//! stdout, exit code and post-command repository state.
//!
//! This module owns the surface that **tells a script what git is**: the
//! commands whose output is parsed by tooling rather than read by a human.
//! `var`, `check-ref-format`, `column`, `help`, `version`, `bugreport`,
//! `diagnose`, `web--browse`, `sh-i18n--envsubst`, `check-mailmap`,
//! `interpret-trailers`, `stripspace`, `hook` and `for-each-repo`, plus the
//! **pre-verb** options `--list-cmds=<group>` and `--exec-path` that `git.c`
//! answers before any subcommand is dispatched.
//!
//! The stake is concrete for zvcs: it ships a zsh completion generated from its
//! own dispatch table, and `--list-cmds=<group>` and `help -a` are the two
//! questions a completion script asks. A port that answers those differently
//! breaks completion for every user without any command ever failing.
//!
//! # Filing
//!
//! A case's `cmd` is its reporting bucket, not an assertion about `argv[1]`.
//! Three groups here are filed under a bucket other than their first token, and
//! each is deliberate:
//!
//!   * `--list-cmds=<group>` and `--exec-path` are filed under `help`. They are
//!     pre-verb options with no subcommand at all, and `help.c:list_cmds()` is
//!     the function that answers `--list-cmds`, so `help` is where a divergence
//!     in the command listing belongs.
//!   * `--version` and `-v` are filed under `version`, beside `git version`.
//!   * `branch`/`tag`/`status`/`clean` invocations that exist only to exercise
//!     `column.ui` and the per-command `column.<cmd>` keys are filed under
//!     `column` — the columnation code is the thing under test, and filing them
//!     under their verb would bury them in hundreds of unrelated cases.
//!
//! # Outputs established as NOT comparable, and why
//!
//! Several commands in this area are traps: they print the machine, the clock,
//! or the installed binary's own build, and a case over one of them can never
//! pass for any correct implementation. They are named here rather than
//! silently omitted, because "structurally unmeasurable" and "untested" are
//! different facts.
//!
//!   * **`git --html-path`, `git --man-path`, `git --info-path`.** Each prints
//!     an installation path. `runner::mask_paths` masks exactly three strings —
//!     the fixture root, the home directory, and the side's *own* exec-path —
//!     and none of them covers a documentation or manpage prefix. Measured on
//!     this machine: stock answers `/opt/homebrew/opt/git/share/man`, the port
//!     answers `<HOME>/.zvcs/man`. The two can never be equal, whatever the port
//!     does, so no case here invokes them.
//!   * **`git --exec-path` is comparable only weakly, and is included on that
//!     basis.** `runner::exec_path_of` derives each side's mask token by running
//!     `<binary> --exec-path`, so the case's own output is what defines the
//!     token and both sides render `<EXEC-PATH>`. What it still measures is
//!     narrow but real: that the option is recognised, exits 0, and prints one
//!     non-empty line — a side that printed nothing would leave its mask needle
//!     empty and the substitution would not happen.
//!   * **`version --build-options`** describes a C toolchain (`libcurl`, `zlib`,
//!     `gettext`, `feature:` lines) that the port does not have and cannot
//!     fabricate. It is already a known `gits-disagree` between the two stock
//!     oracles; `plumbing_refs.rs` owns the one case and this module adds none.
//!   * **`diagnose`'s success path.** Its stdout is `version --build-options`
//!     followed by `Available space on '<path>': 57012.46 GiB` — free disk
//!     space, which moves *between two runs of the same binary*, measured
//!     drifting from 57012.46 to 57011.45 GiB inside one minute. It is not even
//!     self-deterministic, so every `diagnose` case here is an argument-surface
//!     refusal that exits before the collector starts.
//!   * **`bugreport`'s report file.** `runner::probe_worktree_content` compares
//!     every worktree file under 64 KiB byte for byte, untracked ones included,
//!     and the report embeds `strftime` output and the same build-options block.
//!     A report written into the worktree therefore fails on state for all time.
//!     What *is* comparable is the announcement line and the filename shape, so
//!     the cases here write into `.git` with `-o .git`: `collect_worktree` stops
//!     at a `.git` directory and prints `<git directory>` without recursing, so
//!     the file is created, named, and reported on stdout without its bytes
//!     entering the digest.
//!   * **`help --web` / `help.format=web` with no browser configured.** Stock
//!     falls back through `web--browse` to the platform browser and *launches
//!     it* — verified: it printed the rendered page and exited 0. A case that
//!     opens a browser window four times per run is not a measurement. Every web
//!     and man case here pins a viewer that does nothing
//!     (`browser.<name>.cmd=true`, `man.<name>.cmd=true`) or points the lookup
//!     at a path that does not exist, so the only thing measured is the routing.
//!   * **`git --list-cmds=others`** enumerates `git-*` executables on `$PATH`
//!     minus those in the side's exec-path, so its answer names whatever is
//!     installed on the machine. It is included because both sides see the same
//!     `$PATH` under `env::harden` and were measured agreeing, but a divergence
//!     there should be read as "the two exec-path exclusion sets differ", not as
//!     a missing command.
//!
//! # The environment pins, and what a `var` case can therefore measure
//!
//! `env::harden` fixes 25 variables, and twelve of them are inputs to `git var`:
//! `GIT_AUTHOR_NAME`/`_EMAIL`, `GIT_COMMITTER_NAME`/`_EMAIL`,
//! `GIT_AUTHOR_DATE`/`GIT_COMMITTER_DATE` (so `GIT_AUTHOR_IDENT` and
//! `GIT_COMMITTER_IDENT` are `zvcs parity <parity@example.invalid> 1700000000
//! +0000` unless configuration overrides the name and email), `GIT_EDITOR=true`,
//! `GIT_SEQUENCE_EDITOR=true`, `GIT_PAGER=cat`,
//! `GIT_CONFIG_GLOBAL=GIT_CONFIG_SYSTEM=/dev/null`, and `GIT_CONFIG_NOSYSTEM=1`.
//! `runner::apply_case_env` asserts that a case may not re-point any of them, so
//! a case here never pretends to vary a pinned value. What it measures instead
//! is the *reading* of the pin:
//!
//!   * `-c core.editor=…  var GIT_EDITOR` must still answer `true`, because
//!     `builtin/var.c:editor()` consults `GIT_EDITOR` before `core.editor`. A
//!     port that reads the config first passes every un-pinned test and fails
//!     this one.
//!   * `var GIT_CONFIG_SYSTEM` exits 1 with no output, because
//!     `GIT_CONFIG_NOSYSTEM=1` suppresses the system file entirely — the pin is
//!     the whole reason that variable has an *absent* answer to give.
//!   * The two variables `harden` does **not** pin and `var` still reads —
//!     `XDG_CONFIG_HOME` and `GIT_ATTR_NOSYSTEM` — are set by two cases through
//!     [`crate::runner::Case::with_env`], which is additive and symmetric.
//!     `XDG_CONFIG_HOME={repo}/xdg` moves `GIT_ATTR_GLOBAL` to
//!     `<REPO>/xdg/git/attributes`, inside the masked prefix; `GIT_ATTR_NOSYSTEM=1`
//!     empties `GIT_ATTR_SYSTEM`.
//!
//! # Fixture constraints
//!
//! Most of this module is `Shape::Linear`: these commands answer from argv,
//! configuration and the environment, and a richer history would only slow the
//! run. The exceptions are the shapes that supply the *premise* a case needs and
//! that no argv can build: `Attributes` for the `.mailmap` `check-mailmap` reads,
//! `Branched` for the branch and tag lists `column.ui` has to lay out, `Dirty`
//! for the untracked set `column.clean` lays out, `Hooked` and `HooksFail` for
//! the hooks `hook run` invokes, and `Submodule`/`Worktree` for the second
//! repository `for-each-repo` has to walk into.

use crate::fixture::Shape;
use crate::runner::Case;

/// Append this subsystem's cases to the corpus.
pub fn cases(out: &mut Vec<Case>) {
    pre_verb_options(out);
    version_reports(out);
    var_variables(out);
    ref_name_grammar(out);
    columnation(out);
    help_listings(out);
    report_writers(out);
    browser_launch(out);
    envsubst(out);
    mailmap_lookup(out);
    trailers(out);
    whitespace_filter(out);
    hook_dispatch(out);
    repo_walk(out);
}

/// The command listing a completion script parses, and the pre-verb options
/// that answer before dispatch.
///
/// `git.c:handle_options()` answers `--list-cmds=<groups>` by calling
/// `help.c:list_cmds()`, which splits the argument on commas and appends one
/// group per token. Every group is a different selector over the same two
/// tables — `commands[]` in `git.c` for the builtins and the `git-*` files on
/// `$PATH` for the rest — so getting one right says nothing about the others:
///
///   * `builtins` is `commands[]` verbatim; `main` is that plus the scripted
///     commands found in the exec-path; `others` is `$PATH` minus `main`.
///   * `nohelpers` filters out the `--` and `-` helper names, so on a normal
///     install it is empty and the interesting fact is that it is empty rather
///     than an error.
///   * `alias` reads `alias.*` from configuration; `config` lists the
///     configuration variables git knows, both empty in a fixture that sets
///     none — and both non-empty the moment a case adds one, which is the pair
///     below.
///   * `list-mainporcelain`, `list-complete`, `list-guide`, `parseopt` and
///     `deprecated` read the `command-list.txt` categories compiled into the
///     binary. `parseopt` is the one `git-completion.bash` uses to decide
///     whether a verb can be completed from `--git-completion-helper`, so an
///     empty answer there silently degrades every flag completion.
///
/// An unknown group is a `fatal:` at 128 and is the contract, not a bug.
fn pre_verb_options(out: &mut Vec<Case>) {
    for group in [
        "builtins",
        "main",
        "others",
        "nohelpers",
        "alias",
        "config",
        "deprecated",
        "list-mainporcelain",
        "list-complete",
        "list-guide",
        "parseopt",
    ] {
        out.push(Case::new("help", &[&format!("--list-cmds={group}")], Shape::Linear));
    }
    // Two groups at once: the argument is a comma-separated *list*, and a parser
    // that treats it as one enum value answers `unsupported command listing
    // type` for a spelling git accepts.
    out.push(Case::new("help", &["--list-cmds=main,others"], Shape::Linear));
    // `alias` and `config` are the two groups whose answer is a function of the
    // repository rather than of the binary, so an empty answer above proves
    // nothing on its own.
    out.push(
        Case::new("help", &["--list-cmds=alias"], Shape::Linear)
            .with_config(&[("alias.parityco", "checkout"), ("alias.paritysw", "switch")]),
    );
    // The refusal is the contract: an unknown group name must not be silently
    // ignored, or a completion script asking for a group this git does not have
    // would get a listing for the groups it does and complete the wrong words.
    out.push(Case::strict("help", &["--list-cmds=no-such-group"], Shape::Linear));
    // Comparable only in the narrow sense the module doc states: both sides
    // render `<EXEC-PATH>`, so this measures recognition and a non-empty answer.
    out.push(Case::new("help", &["--exec-path"], Shape::Linear));
    // As a *setter* it is a different code path — `--exec-path=<path>` puts the
    // value in `GIT_EXEC_PATH` and continues to the verb, which must still run.
    out.push(Case::new("help", &["--exec-path=nosuchexecdir", "rev-parse", "--git-dir"], Shape::Linear));
}

/// `git version` and the two pre-verb spellings of it.
///
/// The version string is the one thing in this module both binaries are
/// contracted to agree on exactly — the port reports `git version 2.55.0`
/// because that is the git it implements — and it is what every `git --version |
/// cut` in a shell script reads. `version --build-options` is deliberately
/// absent; see the module doc.
fn version_reports(out: &mut Vec<Case>) {
    out.push(Case::new("version", &["--version"], Shape::Linear));
    out.push(Case::new("version", &["-v"], Shape::Linear));
    // Trailing operands are accepted and ignored, which is not obvious and is
    // exactly the sort of thing a rewritten option scan tightens by accident.
    out.push(Case::new("version", &["version", "ignored-operand"], Shape::Linear));
    // `--version` is answered before the repository is discovered, so it works
    // where every other command would fail.
    out.push(Case::new("version", &["--version"], Shape::Damaged));
}

/// `git var`: the eleven names `builtin/var.c` answers, and the layered lookup
/// behind each one.
///
/// `var` is not a config reader. Every name in `git_vars[]` has its own resolver
/// — `editor()`, `sequence_editor()`, `pager()`, `default_branch()`,
/// `git_attr_val_system()` — and each consults the environment, then
/// configuration, then a compiled-in default, in an order the port has to
/// reproduce per variable rather than once. The pins described in the module doc
/// are what make that order observable at all: with `GIT_EDITOR=true` fixed,
/// `-c core.editor=vi` must change nothing.
///
/// `plumbing_objects.rs` owns the bare name lookups and the identity cases; this
/// group adds the resolvers' *second* input and their refusals.
fn var_variables(out: &mut Vec<Case>) {
    // `-l` is not a list of names, it is the whole configuration followed by
    // every variable — the one invocation whose output a script parses as a
    // block. It answers differently once configuration exists, which is what
    // separates "prints the table" from "prints the table it computed".
    out.push(
        Case::new("var", &["var", "-l"], Shape::Linear)
            .with_config(&[("user.name", "Cfg Name"), ("user.email", "cfg@example.invalid")]),
    );
    out.push(Case::new("var", &["var", "-l"], Shape::Branched));

    // `GIT_DEFAULT_BRANCH` runs the name through `check_refname_format`, so an
    // `init.defaultBranch` git would refuse to create is refused here too. This
    // is the only `var` name with a validator behind it.
    out.push(Case::new("var", &["-c", "init.defaultBranch=bad..name", "var", "GIT_DEFAULT_BRANCH"], Shape::Linear));
    out.push(Case::new("var", &["-c", "init.defaultBranch=", "var", "GIT_DEFAULT_BRANCH"], Shape::Linear));

    // The empty setting is not the unset one: an empty `core.pager` means "no
    // pager", and a resolver that treats empty as absent falls back to the
    // compiled default instead.
    out.push(Case::new("var", &["-c", "core.pager=", "var", "GIT_PAGER"], Shape::Linear));
    out.push(Case::new("var", &["-c", "core.editor=", "var", "GIT_EDITOR"], Shape::Linear));
    out.push(Case::new("var", &["-c", "sequence.editor=", "var", "GIT_SEQUENCE_EDITOR"], Shape::Linear));
    // `GIT_SEQUENCE_EDITOR` falls back to `core.editor` and then to `GIT_EDITOR`,
    // three levels rather than two — the one place `var`'s resolvers chain.
    out.push(Case::new("var", &["-c", "core.editor=core-ed", "var", "GIT_SEQUENCE_EDITOR"], Shape::Linear));

    // Identity assembly, which is `fmt_ident()` and not string concatenation:
    // angle brackets in a name and a missing half are the two inputs that decide
    // between an assembled ident and a refusal.
    out.push(Case::new("var", &["-c", "user.name=Has <Angle>", "-c", "user.email=a@b", "var", "GIT_AUTHOR_IDENT"], Shape::Linear));
    out.push(Case::new("var", &["-c", "user.email=only@example.invalid", "var", "GIT_AUTHOR_IDENT"], Shape::Linear));
    out.push(Case::new("var", &["-c", "user.useConfigOnly=true", "-c", "user.name=N", "var", "GIT_AUTHOR_IDENT"], Shape::Linear));

    // The attribute paths. `core.attributesFile` overrides the XDG location for
    // `GIT_ATTR_GLOBAL`; `GIT_ATTR_SYSTEM` is unaffected by it, which is the
    // pair that catches a resolver wired to the wrong key.
    out.push(Case::new("var", &["-c", "core.attributesFile=my-attrs", "var", "GIT_ATTR_GLOBAL"], Shape::Linear));

    // The two environment inputs `env::harden` does not pin. Both are additive
    // and symmetric — see the module doc — and both move an answer no
    // configuration key can reach.
    out.push(
        Case::new("var", &["var", "GIT_ATTR_GLOBAL"], Shape::Linear)
            .with_env(&[("XDG_CONFIG_HOME", "{repo}/xdg")]),
    );
    out.push(
        Case::new("var", &["var", "GIT_ATTR_SYSTEM"], Shape::Linear)
            .with_env(&[("GIT_ATTR_NOSYSTEM", "1")]),
    );

    // Names are matched exactly, not case-insensitively and not by prefix. The
    // refusal is the contract: a script that reads an answer for a name this git
    // does not have would act on a value nobody computed.
    out.push(Case::strict("var", &["var", "git_author_ident"], Shape::Linear));
}

/// `git check-ref-format`: every rejection rule in `refs.c:check_refname_format`,
/// one case per rule.
///
/// This is the validator every script that builds a ref name calls before
/// creating it, and its whole contract is the exit code — 0 for a legal name, 1
/// for an illegal one, and nothing on stdout unless `--normalize` was asked for.
/// The rules are a list in `check_refname_component()` and a port that
/// implements nine of the ten scores 90% on a corpus that tests one name and
/// 100% on a corpus that tests none, so the cases below are one per class:
///
///   `..`, `~`, `^`, `:`, `?`, `*`, `[`, a backslash, a space, an ASCII control
///   byte, a component starting with `.`, a component ending `.lock`, a trailing
///   `.`, a leading or trailing `/`, an empty component (`//`), the
///   two-character sequence `@{`, and the whole name being exactly `@`.
///
/// `plumbing_refs.rs` owns a first pass at these; this group adds the classes it
/// does not reach and the flag interactions, which are where the rules change
/// rather than merely repeat:
///
///   * `--allow-onelevel` permits a name with no `/`, and nothing else — the
///     `.lock` rule still applies to it.
///   * `--refspec-pattern` permits **one** `*` in **one** component; two stars,
///     or a star sharing a component with other text at the top level, are still
///     refused. A bare `*` is legal only with `--allow-onelevel` as well.
///   * `--normalize` collapses runs of `/` and strips leading ones, then prints
///     the result — the only mode with stdout to compare. It normalizes; it does
///     not resolve, so `..` stays a rejection rather than becoming a parent
///     directory.
///   * `--branch` is a different function entirely (`strbuf_check_branch_ref`
///     plus `interpret_branch_name`), which is why it cannot be combined with
///     the other three and why `@{-1}` is in scope for it alone.
fn ref_name_grammar(out: &mut Vec<Case>) {
    // One case per illegal character class. All exit 1 with no output; the
    // assertion is that they exit 1 rather than 0.
    for name in [
        "refs/heads/a^b",
        "refs/heads/a:b",
        "refs/heads/a?b",
        "refs/heads/a[b",
        "refs/heads/a*b",
        "refs/heads/a\u{1}b",
        "/refs/heads/x",
        "refs/heads/x/",
        "refs/heads//x",
        "refs/heads/x@{1}",
        "refs/heads/x/y.lock",
        "@{",
        "",
    ] {
        out.push(Case::new("check-ref-format", &["check-ref-format", name], Shape::Linear));
    }

    // `--normalize`: the only mode that prints. The last of the three is what
    // separates normalizing from resolving — `..` stays a rejection.
    out.push(Case::new("check-ref-format", &["check-ref-format", "--normalize", "/refs/heads/x"], Shape::Linear));
    out.push(Case::new("check-ref-format", &["check-ref-format", "--normalize", "///refs///heads///x"], Shape::Linear));
    out.push(Case::new("check-ref-format", &["check-ref-format", "--normalize", "refs/heads/x/../y"], Shape::Linear));

    // `--refspec-pattern`: where the star is allowed and where it is not.
    out.push(Case::new("check-ref-format", &["check-ref-format", "--refspec-pattern", "refs/heads/*/x"], Shape::Linear));
    out.push(Case::new("check-ref-format", &["check-ref-format", "--refspec-pattern", "refs/*/*"], Shape::Linear));
    out.push(Case::new("check-ref-format", &["check-ref-format", "--refspec-pattern", "*"], Shape::Linear));
    out.push(Case::new("check-ref-format", &["check-ref-format", "--allow-onelevel", "--refspec-pattern", "*"], Shape::Linear));

    // `--allow-onelevel` relaxes exactly one rule.
    out.push(Case::new("check-ref-format", &["check-ref-format", "--allow-onelevel", "x.lock"], Shape::Linear));

    // `--branch` is `interpret_branch_name`, not the format checker: it takes a
    // *branch expression*, resolves `@{-N}` against the reflog, and dies at 128
    // where the checker would merely exit 1.
    out.push(Case::new("check-ref-format", &["check-ref-format", "--branch", "x..y"], Shape::Linear));
    out.push(Case::new("check-ref-format", &["check-ref-format", "--branch", "@{-2}"], Shape::Linear));
    out.push(Case::new("check-ref-format", &["check-ref-format", "--branch", "@{-1}"], Shape::Branched));
    // The combinations git rejects outright, at 129 with the usage block: the
    // refusal is the contract, because accepting one of these would run the
    // wrong validator over a name a script is about to create.
    out.push(Case::strict("check-ref-format", &["check-ref-format", "--normalize", "--branch", "main"], Shape::Linear));
    out.push(Case::strict("check-ref-format", &["check-ref-format", "--branch", "--allow-onelevel", "main"], Shape::Linear));
    // `--` does not turn an option into the operand: the checker takes exactly
    // one name and `--normalize` after `--` is still parsed as a second one.
    out.push(Case::strict("check-ref-format", &["check-ref-format", "--", "--normalize"], Shape::Linear));
}

/// Eight short words, one per line — enough rows that a layout decision is
/// visible and short enough that the whole grid fits one screen width.
const COLUMN_WORDS: &[u8] = b"alpha\nbeta\ngamma\ndelta\nepsilon\nzeta\neta\ntheta\n";

/// `git column` and the `column.*` keys its callers read.
///
/// `column.c` takes one integer of mode bits and a layout, and the two halves
/// fail independently. The bits are parsed by `parse_column_option()` from a
/// comma-separated list where `always`/`auto`/`never` set the *enable* bits and
/// `plain`/`column`/`row`/`dense`/`nodense` set the *layout*; `--raw-mode=<n>`
/// hands the same integer over directly, which is how `builtin/branch.c` and
/// friends pass what they parsed to a `git column` child. A port that accepts
/// the spellings but computes a different integer produces a listing that is
/// laid out wrongly and never fails.
///
/// `worktree_index.rs` and `pathspec_stdin.rs` own the single-spelling cases;
/// this group adds the spellings they miss, the layout parameters, and — the
/// half nothing covered — the *callers*: `branch`, `tag`, `status` and `clean`
/// each read `column.<verb>` falling back to `column.ui`, and each passes the
/// result to the same layout engine.
fn columnation(out: &mut Vec<Case>) {
    let li = Shape::Linear;

    // Mode spellings not otherwise reached, each fed the same input so the
    // difference in the output is the difference in the parsed bits.
    for mode in ["never", "auto", "nodense", "column,dense", "always,column"] {
        out.push(Case::with_stdin("column", &["column", &format!("--mode={mode}"), "--width=40"], li, COLUMN_WORDS));
    }
    // `--raw-mode` is the integer form the porcelain uses internally: bit 0..1
    // is the enable state and bit 2 is dense. Sweeping it is the only way to
    // check the bit assignment rather than the spelling table.
    for raw in ["1", "2", "3", "7"] {
        out.push(Case::with_stdin("column", &["column", &format!("--raw-mode={raw}"), "--width=40"], li, COLUMN_WORDS));
    }

    // Layout parameters. Width 0 and width 1 are the degenerate ends — one
    // column, or a column narrower than the widest entry — and the padding and
    // indent are what a caller sets to nest a listing under a heading.
    out.push(Case::with_stdin("column", &["column", "--mode=column", "--width=0"], li, COLUMN_WORDS));
    out.push(Case::with_stdin("column", &["column", "--mode=column", "--width=40", "--padding=0"], li, COLUMN_WORDS));
    out.push(Case::with_stdin("column", &["column", "--mode=row", "--width=40", "--nl=<>"], li, COLUMN_WORDS));

    // `--command=<verb>` makes `git column` read `column.<verb>` for its
    // default, which is how a scripted porcelain inherits the user's setting.
    for verb in ["branch", "clean"] {
        out.push(Case::with_stdin(
            "column",
            &["-c", &format!("column.{verb}=always,column"), "column", &format!("--command={verb}"), "--width=40"],
            li,
            COLUMN_WORDS,
        ));
    }

    // The callers. Each of these is a real listing laid out by the same engine,
    // and the four verbs read four different keys with one shared fallback.
    out.push(Case::new("column", &["-c", "column.ui=always", "branch"], Shape::Branched));
    out.push(Case::new("column", &["-c", "column.branch=always,column", "branch"], Shape::Branched));
    out.push(Case::new("column", &["branch", "--column=always"], Shape::Branched));
    // `--column` on the command line beats `column.ui` in configuration, and
    // `column.<verb>` beats `column.ui` in configuration — the two precedence
    // edges, one per case.
    out.push(Case::new("column", &["-c", "column.ui=never", "branch", "--column=always"], Shape::Branched));
    out.push(Case::new("column", &["-c", "column.ui=always", "-c", "column.branch=never", "branch"], Shape::Branched));
    out.push(Case::new("column", &["-c", "column.ui=always", "tag"], Shape::Branched));
    out.push(Case::new("column", &["-c", "column.ui=always", "status", "--short"], Shape::Dirty));
    out.push(Case::new("column", &["-c", "column.ui=always", "clean", "-n"], Shape::Dirty));
    out.push(Case::new("column", &["-c", "column.clean=always", "clean", "-n"], Shape::Dirty));
    // An unparsable mode has to be a `fatal:` before the listing is produced,
    // not a silently ignored setting: the refusal is what tells the user their
    // configuration is wrong instead of quietly laying the list out one per row.
    out.push(Case::strict("column", &["-c", "column.ui=bogus", "branch"], Shape::Branched));
}

/// `git help`: the listings completion parses, and the four output formats.
///
/// `help -a` heads its output with `available git commands in '<exec-path>'`
/// and then prints the command list in columns under category headings; that
/// text is what `git-completion.bash`'s `__git_list_all_commands` reads when
/// `--list-cmds` is unavailable, so it is a parsing surface and not prose.
/// `--config` and `--config-for-completion` are read the same way for
/// `git config` completion, and `-g`/`--guides` for `git help <guide>`.
///
/// The format selectors are the other half. `help.c:cmd_help()` dispatches on
/// `help.format`, and each of `man`, `info` and `web` hands off to a different
/// external program. As the module doc records, the default fallbacks *launch* a
/// pager, a browser or `man`, so every case here pins a viewer that does
/// nothing — `man.<name>.cmd=true` and `browser.<name>.cmd=true` — or points the
/// lookup at a path that is not there. What that leaves measurable is exactly
/// the routing decision, which is the part the port has to reproduce.
fn help_listings(out: &mut Vec<Case>) {
    let li = Shape::Linear;

    // The listing forms. `-a` and `--all` are the same option and both are
    // pinned, because a hand-written option table can easily carry one and not
    // the other, and a completion script written against the short spelling
    // would then get a usage block instead of a command list.
    out.push(Case::new("help", &["help", "-a"], li));
    out.push(Case::new("help", &["help", "--all", "--verbose"], li));
    // With an alias configured, `--all` grows an "aliases" section and
    // `--no-aliases` must drop it again. Without a configured alias neither
    // spelling has anything to include or exclude, so the pair below is the
    // only thing that separates them.
    out.push(Case::new("help", &["-c", "alias.parityco=checkout", "help", "--all"], li));
    out.push(Case::new("help", &["-c", "alias.parityco=checkout", "help", "--no-aliases", "--all"], li));

    // The formats, each routed to a viewer that does nothing.
    out.push(Case::new("help", &["-c", "help.format=man", "-c", "man.viewer=noop", "-c", "man.noop.cmd=true", "help", "status"], li));
    out.push(Case::new("help", &["-c", "help.format=web", "-c", "web.browser=noop", "-c", "browser.noop.cmd=true", "help", "status"], li));
    // `help.htmlPath` pointed at nothing: the one web path that refuses instead
    // of launching, and the refusal is the contract — a browser opened on a
    // missing file is worse than a `fatal:`.
    out.push(Case::strict("help", &["-c", "help.htmlPath=nosuchdocdir", "-c", "help.format=web", "help", "status"], li));
    // An unknown `help.format` must be rejected rather than falling through to
    // whichever format the implementation happens to try first.
    out.push(Case::strict("help", &["-c", "help.format=bogus", "help", "status"], li));

    // `help.autoCorrect`, which is not part of `help` at all — it is read by
    // `git.c:cmd_main()` when dispatch misses, and decides whether a mistyped
    // verb becomes a suggestion, a refusal, or a *different command being run*.
    // `immediate` and a numeric delay both run the corrected command, so a port
    // that mishandles the key changes what a typo executes.
    for value in ["never", "0", "immediate", "prompt"] {
        out.push(Case::new("help", &["-c", &format!("help.autoCorrect={value}"), "stauts"], li));
    }
    // A value that is neither a keyword nor a number: stock parses it as a
    // numeric config value and dies, which is the contract for a bad setting.
    out.push(Case::strict("help", &["-c", "help.autoCorrect=bogus", "stauts"], li));
}

/// `bugreport` and `diagnose`: the argument surface, and the one part of the
/// output that is a function of argv rather than of the machine.
///
/// Both commands exist to hand a maintainer a file, and the file's *contents*
/// are the machine and the clock — see the module doc for why neither is
/// comparable. What a user of these commands still depends on is the naming: the
/// suffix defaults to `strftime("%Y-%m-%d-%H%M")`, `-s <suffix>` replaces it,
/// `--no-suffix` removes it, and `-o <dir>` chooses the directory, creating it if
/// necessary. The announcement line on stdout names the file it wrote, so with a
/// fixed suffix the whole line is comparable.
///
/// `-o .git` is what makes that measurable rather than merely stated:
/// `runner::collect_worktree` prints `<git directory>` for a `.git` directory
/// and does not descend, so the report is created and named and reported without
/// its timestamped bytes entering the state digest.
fn report_writers(out: &mut Vec<Case>) {
    let li = Shape::Linear;

    out.push(Case::new("bugreport", &["bugreport", "-o", ".git", "-s", "fixed"], li));
    out.push(Case::new("bugreport", &["bugreport", "--output-directory", ".git", "--suffix", "fixed"], li));
    out.push(Case::new("bugreport", &["bugreport", "-o", ".git", "--no-suffix"], li));
    // A suffix carrying a `/` is a path, not a name: stock does not sanitise it,
    // and the announcement line shows where it decided the file goes.
    out.push(Case::new("bugreport", &["bugreport", "-o", ".git", "-s", "sub/fixed"], li));
    // An output directory whose parent is a regular file cannot be created: the
    // refusal is the contract, and it is the only `-o` case that writes nothing.
    out.push(Case::strict("bugreport", &["bugreport", "-o", "README.md/sub", "-s", "fixed"], li));
    // `--diagnose` takes an optional mode, and an unknown one must be refused
    // before the archive is opened.
    out.push(Case::strict("bugreport", &["bugreport", "--diagnose=nope", "-s", "fixed"], li));

    // `diagnose` prints free disk space, so only the refusals are comparable.
    out.push(Case::strict("diagnose", &["diagnose", "--mode="], li));
    out.push(Case::strict("diagnose", &["diagnose", "--mode=Stats"], li));
}

/// `web--browse`: which browser is chosen, without ever launching one.
///
/// `git-web--browse.sh` resolves a browser from `--browser`/`--tool`, then
/// `web.browser`, then a compiled list of known browsers, and consults
/// `browser.<name>.path` and `browser.<name>.cmd` for how to run it. Left to the
/// fallback it opens the platform browser, which is why every case here either
/// names a browser that does not exist — so the resolver reports the miss — or
/// pins `browser.<name>.cmd` to `true`, so resolution succeeds and the process
/// that runs is a no-op.
fn browser_launch(out: &mut Vec<Case>) {
    let li = Shape::Linear;
    const URL: &str = "https://example.invalid/";

    out.push(Case::new("web--browse", &["web--browse", "--browser=nosuchbrowser", URL], li));
    out.push(Case::new("web--browse", &["web--browse", "--tool=nosuchbrowser", URL], li));
    out.push(Case::new("web--browse", &["-c", "browser.noop.cmd=true", "web--browse", "--browser=noop", URL], li));
    out.push(Case::new("web--browse", &["-c", "browser.noop.path=/nonexistent", "web--browse", "--browser=noop", URL], li));
    out.push(Case::new("web--browse", &["-c", "web.browser=noop", "-c", "browser.noop.cmd=true", "web--browse", URL], li));
    // `--browser` and `--tool` with no value: the option wants an argument and
    // the next token is the URL, so the resolver is handed a "browser" named
    // `https://example.invalid/`.
    out.push(Case::new("web--browse", &["web--browse", "--browser", URL], li));
}

/// `sh-i18n--envsubst`: the two modes of git's cut-down `envsubst`.
///
/// `--variables <template>` prints the variable *names* the template mentions,
/// one per line; without it, the template is expanded against the environment
/// and the input on stdin is copied through. Only `$NAME` and `${NAME}` are
/// recognised, and the name grammar is a leading letter or underscore followed
/// by alphanumerics — so `$1`, `$9x` and a bare `${` are literal text, which is
/// the boundary a rewritten scanner moves.
///
/// Every name used here is either pinned by `env::harden` (`LANG=C`, `TZ=UTC`)
/// or guaranteed absent by `Command::env_clear`, so both sides expand the same
/// values.
fn envsubst(out: &mut Vec<Case>) {
    let li = Shape::Linear;
    let e = "sh-i18n--envsubst";

    out.push(Case::new(e, &[e, "--variables", "${LANG} $TZ ${TERM}"], li));
    out.push(Case::new(e, &[e, "--variables", "${"], li));
    out.push(Case::new(e, &[e, "--variables", "$$LANG"], li));
    out.push(Case::new(e, &[e, "--variables", "$1 $_ $A_B $9x"], li));
    out.push(Case::new(e, &[e, "--variables", "${UNSET_ONE}${UNSET_TWO}"], li));
    // Substitution mode: the template names which variables may be expanded and
    // stdin supplies the text. A name in the text but not in the template is
    // left alone, which is the whole point of the template argument.
    out.push(Case::with_stdin(e, &[e, "$LANG"], li, b"$LANG-$TZ\n${LANG}\nno vars\n"));
    out.push(Case::with_stdin(e, &[e, "$LANG $TZ"], li, b"$LANG-$TZ\n"));
}

/// Four idents on stdin: two the fixture's `.mailmap` rewrites, one it rewrites
/// the email of only, and one it does not know.
const MAILMAP_IDENTS: &[u8] = b"Old Name <old@example.invalid>\n\
Alias Name <alias@example.invalid>\n\
Typo Name <typo@example.invalid>\n\
Unknown <nobody@example.invalid>\n";

/// `check-mailmap`: the lookup rules, on a shape that has a `.mailmap`.
///
/// `mailmap.c` keys entries on the pair (name, email), and the rules that follow
/// from that are not obvious from the file format:
///
///   * The email is matched case-insensitively; the name is matched exactly
///     where an entry supplies one and ignored where it does not. So
///     `<OLD@EXAMPLE.INVALID>` and `Anything <old@example.invalid>` both rewrite
///     through the fixture's first entry, which has no old-name half.
///   * An entry that *does* carry an old name — `<canonical@…> Typo Name
///     <typo@…>` — only fires for that name, so a bare `<typo@…>` is left alone.
///     That pair is the one that separates a real lookup from an email-only map.
///   * A name-only entry (`Solo Name <solo@…>`) replaces the name and keeps the
///     email.
///   * A contact with no angle brackets is not an ident: it is wrapped as
///     `<contact>` and passed through — the one rule here whose case lives in
///     `shape_reach.rs` rather than below.
///
/// `shape_reach.rs` owns the direct lookups; this group adds the matching rules
/// above, the `--stdin` path, and the two command-line spellings of the mailmap
/// source, which are a different code path from the `mailmap.file`/`mailmap.blob`
/// configuration keys the existing cases use.
fn mailmap_lookup(out: &mut Vec<Case>) {
    let at = Shape::Attributes;
    let cm = "check-mailmap";

    out.push(Case::new(cm, &[cm, "<OLD@EXAMPLE.INVALID>"], at));
    out.push(Case::new(cm, &[cm, "Anything At All <old@example.invalid>"], at));
    out.push(Case::new(cm, &[cm, "<typo@example.invalid>"], at));
    out.push(Case::new(cm, &[cm, "alias name <alias@example.invalid>"], at));
    out.push(Case::new(cm, &[cm, "<solo@example.invalid>"], at));
    out.push(Case::new(cm, &[cm, "A <old@example.invalid>", "B <alias@example.invalid>", "C <typo@example.invalid>"], at));

    // `--stdin` reads *in addition to* the operands, and the operands come
    // first — an implementation that reads stdin first reverses the output.
    out.push(Case::with_stdin(cm, &[cm, "--stdin"], at, MAILMAP_IDENTS));
    out.push(Case::with_stdin(cm, &[cm, "--stdin", "Solo Name <solo@example.invalid>"], at, MAILMAP_IDENTS));
    out.push(Case::with_stdin(cm, &[cm, "--no-stdin", "Old Name <old@example.invalid>"], at, MAILMAP_IDENTS));

    // The command-line mailmap sources. A missing file or blob is not an error:
    // it is simply no additional entries, and the repository's own `.mailmap`
    // still applies — which is why the two "no-such" cases must still rewrite.
    out.push(Case::new(cm, &[cm, "--mailmap-file", ".mailmap", "Old Name <old@example.invalid>"], at));
    out.push(Case::new(cm, &[cm, "--mailmap-blob", "HEAD:.mailmap", "Old Name <old@example.invalid>"], at));
}

/// A commit message with a body and an existing trailer block: the input every
/// `interpret-trailers` decision is made against.
const TRAILER_MESSAGE: &[u8] = b"subject line\n\nbody text\n\n\
Signed-off-by: A U Thor <author@example.com>\n\
Acked-by: R Viewer <reviewer@example.invalid>\n";

/// `interpret-trailers`: the option and configuration surface `mail_series.rs`
/// does not reach.
///
/// The command is a rule engine, not a text filter: for each trailer it decides
/// *where* (`--where`, `trailer.where`, `trailer.<token>.where`), *whether* when
/// one already exists (`--if-exists`), and *whether* when none does
/// (`--if-missing`) — three independent axes, each settable at three levels,
/// with the command line beating the token-specific key beating the global key.
///
/// Two spellings and one configuration key here are the ones a rewritten parser
/// tends to lose: `--trailer=<tok>=<value>` as a single `=`-joined token,
/// the `--no-<axis>` forms that reset an axis to its default, and
/// `trailer.<token>.command` — the deprecated predecessor of `.cmd`, which
/// substitutes `$ARG` inside a shell command and is still honoured.
fn trailers(out: &mut Vec<Case>) {
    let li = Shape::Linear;
    let it = "interpret-trailers";
    let msg = TRAILER_MESSAGE;

    // `--trailer=X=y` — the option and its value joined by `=`, with a second
    // `=` inside the value. A splitter that takes the last `=` gets this wrong.
    out.push(Case::with_stdin(it, &[it, "--trailer=X=y"], li, msg));

    // The remaining `--if-exists`/`--if-missing` values, against a message that
    // already has an `Acked-by:`.
    out.push(Case::with_stdin(it, &[it, "--if-exists=add", "--trailer", "Acked-by: R Viewer <reviewer@example.invalid>"], li, msg));
    out.push(Case::with_stdin(it, &[it, "--where=end", "--trailer", "X: y"], li, msg));

    // The `--no-` resets. Each undoes a preceding setting rather than being an
    // unknown option, and each is easy to implement as a no-op by accident.
    out.push(Case::with_stdin(it, &[it, "--where=start", "--no-where", "--trailer", "X: y"], li, msg));
    out.push(Case::with_stdin(it, &[it, "--if-exists=doNothing", "--no-if-exists", "--trailer", "Acked-by: R Viewer <reviewer@example.invalid>"], li, msg));
    out.push(Case::with_stdin(it, &[it, "--if-missing=doNothing", "--no-if-missing", "--trailer", "X: y"], li, msg));

    // No `--trailer` at all: the command still parses, reformats and re-emits
    // the block, which is what `--trim-empty` and `--only-trailers` act on.
    out.push(Case::with_stdin(it, &[it, "--only-trailers", "--no-divider"], li, msg));

    // Token-level configuration. `.key` renames the token, `.where` and
    // `.ifmissing` set that token's axes, and the token-level setting has to
    // beat the global one.
    out.push(Case::with_stdin(it, &["-c", "trailer.x.key=X: ", "-c", "trailer.x.where=start", it, "--trailer", "x: v"], li, msg));
    out.push(Case::with_stdin(it, &["-c", "trailer.x.key=X: ", "-c", "trailer.x.ifmissing=doNothing", it, "--trailer", "x: v"], li, msg));
    // `trailer.<token>.command` runs a shell command with `$ARG` bound to the
    // value, and is applied once with an empty `$ARG` at parse time and once per
    // supplied value — so a correct implementation emits two trailers here.
    // `/bin/echo` is used rather than a script so the case has no fixture
    // dependency and no output that varies.
    out.push(Case::with_stdin(it, &["-c", "trailer.x.key=X: ", "-c", "trailer.x.command=echo cmd-$ARG", it, "--trailer", "x: v"], li, msg));
    // A separator set that does not contain `:` — the token/value split is
    // configurable, and `Signed-off-by:` in the input then stops being a trailer.
    out.push(Case::with_stdin(it, &["-c", "trailer.separators=%", it, "--trailer", "X%y"], li, msg));
    // An unrecognised `trailer.ifexists` is not fatal; it falls back to the
    // default, which is the opposite of how the command-line spelling behaves.
    out.push(Case::with_stdin(it, &["-c", "trailer.ifexists=bogus", it, "--trailer", "X: y"], li, msg));
    // `--in-place` needs a file operand; with stdin it must refuse rather than
    // silently write somewhere.
    out.push(Case::strict(it, &[it, "--in-place"], li));
}

/// `stripspace`: the four transformations, on the inputs that separate them.
///
/// The default mode does three things at once — trim trailing whitespace from
/// every line, collapse runs of blank lines to one, and drop leading and
/// trailing blank lines — and adds a final newline where the input lacked one.
/// `-s` additionally drops comment lines; `-c` does the opposite job and turns
/// plain text *into* comments. The comment character is `core.commentChar`, or
/// the multi-byte `core.commentString`, or `auto`, which picks a character no
/// line already starts with.
///
/// `plumbing_objects.rs` and `corpus.rs` own the flag list; this group supplies
/// the *inputs* those flags act on, which is where the work is: a filter that
/// handles a blank-line run and a filter that handles CRLF, a NUL byte, an empty
/// stream, or an unterminated last line are different implementations.
fn whitespace_filter(out: &mut Vec<Case>) {
    let li = Shape::Linear;
    let sp = "stripspace";

    // CRLF: the `\r` is trailing whitespace on every line, so the default mode
    // strips it. A filter that only trims spaces and tabs leaves the file in CRLF.
    out.push(Case::with_stdin(sp, &[sp], li, b"a\r\nb\r\n"));
    // Blank-line handling: runs collapse, leading and trailing blanks go, and a
    // stream of nothing but blanks becomes empty.
    out.push(Case::with_stdin(sp, &[sp], li, b"\n\n\n"));
    // No trailing newline on the last line: one is added.
    out.push(Case::with_stdin(sp, &[sp], li, b"no trailing newline"));
    // Interior whitespace is not touched; only trailing whitespace is.
    // An empty stream, with and without the comment transformation: `-c` on
    // nothing must produce nothing rather than a bare comment character.
    out.push(Case::with_stdin(sp, &[sp], li, b""));
    out.push(Case::with_stdin(sp, &[sp, "-c"], li, b""));
    // A NUL byte: the input is a byte stream, not a C string, and truncating at
    // the NUL is the failure this catches.
    out.push(Case::with_stdin(sp, &[sp], li, b"a\0b\n"));

    // The comment transformations against the comment character in force.
    out.push(Case::with_stdin(sp, &[sp, "-s"], li, b"# c\ntext\n# d\n"));
    // A line that is nothing but the comment character survives `--strip-comments`
    // as a removal, not as an empty line left behind.
    out.push(Case::with_stdin(sp, &[sp, "--strip-comments"], li, b"#\ntext\n"));
    // `core.commentChar=auto` picks a character no line already begins with, so
    // an input that already opens with `#` must come back commented some other way.
    out.push(Case::with_stdin(sp, &["-c", "core.commentChar=auto", sp, "-c"], li, b"# already a comment\n"));
}

/// `git hook run`: the one command whose whole job is to execute something else.
///
/// `hook.c:run_hooks_opt()` resolves a name against `core.hooksPath` or
/// `$GIT_DIR/hooks`, checks the file is executable, runs it with the arguments
/// after `--`, optionally feeds it a file on stdin, and propagates its exit
/// status. Every one of those steps is observable from outside because the
/// fixture's hooks each write a file naming what they were handed — so the state
/// probe, not stdout, is the assertion.
///
/// `hooks_identity.rs` owns the `HooksFail` shape's refusals. This group runs
/// the same verb against `Hooked`, where the hooks *succeed*: a hook that exits
/// 0 and a hook that exits 1 take different paths out of `run_hooks_opt`, and a
/// port that gets the failing one right can still get the succeeding one wrong.
fn hook_dispatch(out: &mut Vec<Case>) {
    let hk = Shape::Hooked;

    out.push(Case::new("hook", &["hook", "run", "pre-commit"], hk));
    out.push(Case::new("hook", &["hook", "run", "commit-msg"], hk));
    // Arguments after `--` are the hook's own argv, which `commit-msg` writes
    // into the file it appends to.
    out.push(Case::new("hook", &["hook", "run", "pre-commit", "--", "a", "b", "c"], hk));
    // `--to-stdin` opens a file and hands it to the hook. The missing-file case
    // is a `fatal:` *before* the hook runs, so the hook's own output file must
    // not exist afterwards — which only the state probe can see.
    out.push(Case::new("hook", &["hook", "run", "--to-stdin=top.txt", "pre-commit"], hk));
    out.push(Case::new("hook", &["hook", "run", "--to-stdin=no-such-file", "pre-commit"], hk));
    // A configured hook name with no file: exit 1, nothing run.
    out.push(Case::new("hook", &["hook", "run", "--ignore-missing", "post-commit"], hk));
    // A name that is not a hook git knows, with and without the escape hatch.
    out.push(Case::new("hook", &["hook", "run", "no-such-hook"], hk));
    // `core.hooksPath` redirects the lookup away from `$GIT_DIR/hooks`: pointed
    // at a directory holding no hook, the same name must now find nothing.
    out.push(Case::new("hook", &["-c", "core.hooksPath=sub", "hook", "run", "pre-commit"], hk));
    // `hook list` is the query half, and is what a tool asks before deciding to
    // run anything.
    out.push(Case::new("hook", &["hook", "list", "pre-commit"], hk));
}

/// `for-each-repo`: a configured list of paths, each entered as a repository.
///
/// `builtin/for-each-repo.c` reads a multi-valued configuration key, `chdir`s
/// into each value in turn and runs the remaining argv there as a git
/// subcommand. Three facts follow that a single-repository case cannot show: the
/// key is *multi-valued* so the order of the values is the order of the runs, a
/// value that is not a repository aborts the walk unless `--keep-going` is
/// given, and the child command's own exit status is what decides that.
///
/// `misc_commands.rs` owns the single-value and no-such-repo cases against
/// `Linear`. This group adds the shapes that actually contain a second
/// repository — the submodule at `sub`, and the linked worktree at `wt` — which
/// is the only way to see the walk visit more than one place.
fn repo_walk(out: &mut Vec<Case>) {
    let fer = "for-each-repo";

    // Two values, one good and one that is not a repository: the second aborts,
    // and `--keep-going` is what turns the abort into a continue. Both must have
    // run the first value's command either way.
    out.push(Case::new(fer, &["-c", "parity.list=.", "-c", "parity.list=src", fer, "--config=parity.list", "rev-parse", "--git-dir"], Shape::Linear));
    out.push(Case::new(fer, &["-c", "parity.list=.", "-c", "parity.list=src", fer, "--config=parity.list", "--keep-going", "rev-parse", "--git-dir"], Shape::Linear));
    // A real second repository: the submodule's own git directory is not the
    // parent's, so `rev-parse --git-dir` answering the same thing twice is the
    // failure this catches.
    out.push(Case::new(fer, &["-c", "parity.list=sub", fer, "--config=parity.list", "rev-parse", "--git-dir"], Shape::Submodule));
    out.push(Case::new(fer, &["-c", "parity.list=.", "-c", "parity.list=sub", fer, "--config=parity.list", "rev-parse", "HEAD"], Shape::Submodule));
    // A linked worktree is a repository too, and one whose `HEAD` differs from
    // the main worktree's — so a walk that never left the starting directory
    // prints the same branch twice.
    out.push(Case::new(fer, &["-c", "parity.list=.", "-c", "parity.list=wt", fer, "--config=parity.list", "rev-parse", "--abbrev-ref", "HEAD"], Shape::Worktree));
    // The child's exit status propagates: a subcommand that fails inside a real
    // repository is not the same as a path that is not one.
    out.push(Case::new(fer, &["-c", "parity.list=.", fer, "--config=parity.list", "rev-parse", "no-such-rev"], Shape::Linear));
    out.push(Case::new(fer, &["-c", "parity.list=.", fer, "--config=parity.list", "no-such-subcommand"], Shape::Linear));
    // `--` separates the option list from the child command, which matters when
    // the child's first token starts with a dash.
    // `--config` with no value is the refusal: without a key there is no list,
    // and running the child once in the current directory would be silently wrong.
    out.push(Case::strict(fer, &[fer, "--config"], Shape::Linear));
}
