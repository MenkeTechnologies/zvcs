//! The option layer that runs *before* the verb.
//!
//! `handle_options()` (git.c) parses a fixed set of options ahead of the
//! subcommand — `-C`, `--git-dir`, `--bare`, `--namespace`, the pathspec-magic
//! switches, `--config-env`, `--shallow-file` and the rest — and every one of
//! them changes what the verb that follows sees. [`crate::runner::Case::globals`]
//! exists to reach them, and it is barely used: of the ~800 cases that carry a
//! global, **743 carry only `-c`**. The rest of the layer has between two and
//! eleven cases each, and ten of the options have none at all.
//!
//! That is a whole dispatch stage measured almost entirely through one option.
//!
//! Every case here was checked the way [`super::config_reads`] checks a
//! setting, and for the same reason — a global that changes nothing about the
//! output is a case that can never fail. The check is: run stock git in
//! `templates/<shape>/` with the global and without it, and compare the bytes.
//! Only pairs that changed stock's output are here, which is why the set is
//! narrower than the option list: most globals need a specific shape before
//! they say anything.
//!
//! **Measured as not comparable, and deliberately absent.** Four options print
//! an *installation* path — `--exec-path`, `--man-path`, `--html-path`,
//! `--info-path` — and the two sides answer differently by construction: stock
//! says `/opt/homebrew/opt/git/libexec/git-core` where the port says
//! `<HOME>/.zvcs/bin`. Neither answer is wrong and no port could make them
//! agree, so a case asserting they match would measure the installation rather
//! than the implementation. They are the one part of this layer that this
//! harness structurally cannot score.
//!
//! **Measured as vacuous against every shape tried**, and therefore absent
//! rather than decorative: `--paginate` (the harness pins `GIT_PAGER=cat`, so
//! forcing the pager changes no byte), `--no-literal-pathspecs` on its own
//! (it cancels a mode nothing turned on), `--namespace` on *local* ref reads
//! (`for-each-ref` and `rev-parse` ignore it — it is served-refs machinery, so
//! it appears below on `ls-remote` where it does bite), `--no-optional-locks`,
//! `--attr-source`, and `--bare` on the verbs that answer the same either way
//! (`log`, `rev-parse --git-dir`, `config --list --local`).

use crate::fixture::Shape;
use crate::runner::Case;

pub fn cases(out: &mut Vec<Case>) {
    bare(out);
    working_directory(out);
    pathspec_magic(out);
    object_graph(out);
    namespaces(out);
    config_env(out);
    advice_and_version(out);
}

/// One read under one global.
fn with(out: &mut Vec<Case>, cmd: &'static str, globals: &[&[&str]], args: &[&str], shape: Shape) {
    out.push(Case::new(cmd, args, shape).with_globals(globals));
}

/// `--bare`: the repository is treated as bare whatever is on disk, so the
/// work tree stops existing for the command that follows.
///
/// Only the verbs that can tell are here — `log` and `config --list --local`
/// answer identically either way, because neither consults the work tree.
fn bare(out: &mut Vec<Case>) {
    for args in [
        &["rev-parse", "--is-bare-repository"][..],
        &["rev-parse", "--show-toplevel"],
        &["status"],
        &["ls-files"],
    ] {
        with(out, args[0], &[&["--bare"]], args, Shape::Linear);
    }
}

/// `-C <dir>`: chdir before anything else, which moves the prefix every
/// path-printing read is relative to.
fn working_directory(out: &mut Vec<Case>) {
    for args in [
        &["rev-parse", "--show-toplevel"][..],
        &["rev-parse", "--show-cdup"],
        &["rev-parse", "--show-prefix"],
        &["ls-files"],
        &["status", "--short"],
        &["log", "--oneline", "-1"],
    ] {
        with(out, args[0], &[&["-C", "sub"]], args, Shape::Hooked);
    }
}

/// The pathspec-magic switches, on the only shape whose tracked names contain
/// the characters they argue about.
///
/// `--icase-pathspecs` is spelled against an upper-case name because that is
/// the one form where case-folding has something to fold; against `*.txt` it
/// answers identically to the default and could not fail.
fn pathspec_magic(out: &mut Vec<Case>) {
    for global in ["--literal-pathspecs", "--glob-pathspecs", "--noglob-pathspecs"] {
        with(out, "ls-files", &[&[global]], &["ls-files", "--", "*.txt"], Shape::AwkwardPaths);
        with(out, "grep", &[&[global]], &["grep", "-l", ".", "--", "*.txt"], Shape::AwkwardPaths);
    }
    with(
        out,
        "ls-files",
        &[&["--icase-pathspecs"]],
        &["ls-files", "WITH SPACE.TXT"],
        Shape::AwkwardPaths,
    );
    with(
        out,
        "log",
        &[&["--literal-pathspecs"]],
        &["log", "--oneline", "--", "*.txt"],
        Shape::AwkwardPaths,
    );
}

/// The two globals that change which objects the command can see.
fn object_graph(out: &mut Vec<Case>) {
    // `--no-replace-objects` bypasses the replacement map, so the history the
    // walk reports is the recorded one rather than the replaced one.
    with(
        out,
        "log",
        &[&["--no-replace-objects"]],
        &["log", "--oneline", "-3"],
        Shape::NotesReplace,
    );
    // `--no-lazy-fetch` forbids the promisor round-trip, so an object the
    // filter left behind is an error instead of a fetch.
    with(
        out,
        "cat-file",
        &[&["--no-lazy-fetch"]],
        &["cat-file", "-p", "HEAD~3:hist.txt"],
        Shape::Promisor,
    );
}

/// `--namespace`, on the reads that serve refs rather than list them locally.
///
/// The local listings (`for-each-ref`, `rev-parse`) ignore it entirely —
/// measured, not assumed — so `ls-remote` against the repository itself is
/// where the option is observable without a network.
fn namespaces(out: &mut Vec<Case>) {
    for args in [
        &["ls-remote", "."][..],
        &["ls-remote", "--heads", "."],
        &["ls-remote", "--tags", "."],
    ] {
        with(out, "ls-remote", &[&["--namespace=ns"]], args, Shape::Branched);
    }
    with(out, "ls-remote", &[&["--namespace=ns"]], &["ls-remote", "."], Shape::TagChain);
}

/// `--config-env=<key>=<envvar>`: the value arrives in the environment rather
/// than on the command line, which is the whole point of the option — a
/// secret-bearing value that must not appear in `ps` output.
///
/// A port that treats it as `-c <key>=<envvar>` sets the key to the *name* of
/// the variable, which both of these catch: `core.abbrev=ZZZ` is not a number
/// and `status.short=ZZZ` is not a boolean.
fn config_env(out: &mut Vec<Case>) {
    out.push(
        Case::new("log", &["log", "--oneline", "-3"], Shape::Branched)
            .with_globals(&[&["--config-env=core.abbrev=ZZZ"]])
            .with_env(&[("ZZZ", "16")]),
    );
    out.push(
        Case::new("status", &["status"], Shape::Branched)
            .with_globals(&[&["--config-env=status.short=ZZZ"]])
            .with_env(&[("ZZZ", "true")]),
    );
}

/// `--shallow-file`, `--no-advice` and the `--version` rewrite.
fn advice_and_version(out: &mut Vec<Case>) {
    // `--shallow-file` replaces `.git/shallow`, so naming a file that is not
    // there makes a shallow clone look complete — and the walk runs off the
    // end of the objects it actually has.
    out.push(
        Case::new("log", &["log", "--oneline"], Shape::Shallow)
            .with_globals(&[&["--shallow-file", ""]]),
    );
    out.push(
        Case::new("log", &["log", "--oneline"], Shape::Shallow)
            .with_globals(&[&["--shallow-file", "nosuch-shallow"]]),
    );

    // `--no-advice` silences the hints, which is only observable where there is
    // a hint: a conflicted index and a sparse checkout each print one.
    with(out, "status", &[&["--no-advice"]], &["status"], Shape::Conflicted);
    with(out, "status", &[&["--no-advice"]], &["status"], Shape::Sparse);

    // `cmd_main()` rewrites `--version` to the `version` verb before dispatch,
    // so the token that follows is never reached.
    with(out, "status", &[&["--version"]], &["status"], Shape::Linear);
}
