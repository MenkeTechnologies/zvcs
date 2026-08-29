//! The environment git reads.
//!
//! `handle_options()` has a twin that no option list shows: the variables git
//! consults instead of, or alongside, its own flags. This port reads **104**
//! `GIT_*` variables; the corpus sets **17**, and 148 of those settings are one
//! variable (`GIT_CEILING_DIRECTORIES`, for discovery). The remaining 92 are
//! read by code that nothing compares against git.
//!
//! Seven of them are covered here. That is a small fraction on purpose: each
//! pair was checked before it was written, and most variables turned out to
//! change nothing that a read can see — either because the fixture has no
//! element for them (`GIT_GRAFT_FILE` needs grafts, `GIT_COMMON_DIR` needs a
//! layout no template builds), because they only affect a write
//! (`GIT_DEFAULT_HASH` is read by `init`), or because they are consulted by
//! machinery a hermetic case cannot reach (`GIT_PROTOCOL`, the `GIT_PUSH_CERT_*`
//! family, the `GIT_TRACE2_*` family, whose output carries timings anyway).
//!
//! ## The check, and why the first version of it lied
//!
//! A variable that changes no byte is a case that can never fail, so each pair
//! was measured: run stock with the variable and without it, and compare. The
//! first checker ran each side **once, in the shared template directory**, and
//! gave different answers on consecutive sweeps — because the first invocation
//! left state behind (a refreshed index, a cached lookup) that the second one
//! then read, so a difference appeared where the variable had caused none, and
//! disappeared once the state was warm. Six pairs were "discriminating" in one
//! sweep and vacuous in the next.
//!
//! The checker that produced this module copies the template per run, runs each
//! side **twice**, and requires each side to reproduce itself before the two are
//! compared — which is exactly what [`crate::runner`] does to tell a real
//! difference from a flake. Under it, every pair below is stable and every pair
//! it rejected is stably vacuous.
//!
//! One of these is worth naming for what it unlocks: **`GIT_TEST_DATE_NOW`
//! pins "now"**, which is the only way a relative date can be compared at all.
//! [`super::config_reads`] excludes `log.date=relative` and
//! `blame.highlightRecent` because their output is a function of the clock;
//! with this variable set, it is a function of a number, and the three cases
//! below are the first in the corpus to read a relative date.

use crate::fixture::Shape;
use crate::runner::Case;

pub fn cases(out: &mut Vec<Case>) {
    object_sources(out);
    ref_space(out);
    configuration_and_advice(out);
    the_clock(out);
}

/// One read under one variable.
fn with(out: &mut Vec<Case>, cmd: &'static str, env: &[(&str, &str)], args: &[&str], shape: Shape) {
    out.push(Case::new(cmd, args, shape).with_env(env));
}

/// The variables that change which objects are reachable.
fn object_sources(out: &mut Vec<Case>) {
    // An alternate directory that is not there. Git consults it and carries on;
    // the question is whether it says anything about it, and on which verbs.
    // Only these two can tell — `rev-list`, `fsck` and `cat-file
    // --batch-all-objects` answer identically either way.
    //
    // Spelled through `{repo}` because the runner asserts it: an absolute path
    // in a case environment would name the same directory for both sides, where
    // every other path in a case names each side's own fixture.
    for args in [&["cat-file", "-p", "HEAD"][..], &["log", "--oneline", "-2"]] {
        with(
            out,
            args[0],
            &[("GIT_ALTERNATE_OBJECT_DIRECTORIES", "{repo}/no-such-objects")],
            args,
            Shape::Linear,
        );
    }

    // `GIT_REPLACE_REF_BASE` moves the namespace the replacement map is read
    // from, so pointing it somewhere empty is the env spelling of
    // `--no-replace-objects` — a port that hard-codes `refs/replace/` keeps
    // replacing.
    with(
        out,
        "log",
        &[("GIT_REPLACE_REF_BASE", "refs/nosuch/")],
        &["log", "--oneline", "-3"],
        Shape::NotesReplace,
    );

    // `GIT_SHALLOW_FILE` replaces `.git/shallow`. Naming a file that is not
    // there makes the clone look complete, so the walk runs into the parent it
    // does not have — the env twin of the `--shallow-file` global, which the
    // port ignores.
    with(
        out,
        "log",
        &[("GIT_SHALLOW_FILE", "nosuch-shallow")],
        &["log", "--oneline"],
        Shape::Shallow,
    );
}

/// `GIT_NAMESPACE`, on the reads that serve refs rather than list them.
///
/// The local listings ignore it — measured, not assumed — so `ls-remote`
/// against the repository itself is where it is observable without a network.
fn ref_space(out: &mut Vec<Case>) {
    with(out, "ls-remote", &[("GIT_NAMESPACE", "ns")], &["ls-remote", "."], Shape::Branched);
    with(out, "ls-remote", &[("GIT_NAMESPACE", "ns")], &["ls-remote", "."], Shape::TagChain);
}

/// Configuration delivered as an environment variable, and the advice switch.
fn configuration_and_advice(out: &mut Vec<Case>) {
    // `GIT_CONFIG_PARAMETERS` is the serialized form `-c` is turned into before
    // it is re-exported to subprocesses, and it is readable directly. A port
    // that only parses its own `-c` sees nothing here.
    with(
        out,
        "log",
        &[("GIT_CONFIG_PARAMETERS", "'core.abbrev=16'")],
        &["log", "--oneline", "-3"],
        Shape::Branched,
    );
    with(
        out,
        "status",
        &[("GIT_CONFIG_PARAMETERS", "'status.short=true'")],
        &["status"],
        Shape::Dirty,
    );

    // `GIT_ADVICE=0` silences every hint at once, which is only observable
    // where there is a hint: a conflicted index and a sparse checkout each
    // print one.
    with(out, "status", &[("GIT_ADVICE", "0")], &["status"], Shape::Conflicted);
    with(out, "status", &[("GIT_ADVICE", "0")], &["status"], Shape::Sparse);

    // `GIT_PAGER_IN_USE` tells the command a pager is already attached, which
    // changes what it decides about colour and progress even though the pager
    // itself is pinned to `cat`.
    with(out, "log", &[("GIT_PAGER_IN_USE", "1")], &["log", "--oneline", "-2"], Shape::Linear);
    with(out, "log", &[("GIT_PAGER_IN_USE", "1")], &["log", "--oneline", "-2"], Shape::Branched);
}

/// `GIT_TEST_DATE_NOW`: the clock, as a number.
///
/// Every other case in the corpus avoids relative dates because they are a
/// function of when the run happened. This variable makes "now" an argument,
/// so `--date=relative` and `%ar` become comparable — and they are worth
/// comparing, because a relative date is arithmetic on top of the same
/// timestamp both sides already agree about, and the arithmetic is where a
/// port drifts.
fn the_clock(out: &mut Vec<Case>) {
    const NOW: (&str, &str) = ("GIT_TEST_DATE_NOW", "1800000000");
    with(out, "log", &[NOW], &["log", "--date=relative", "--format=%ad", "-2"], Shape::Linear);
    with(out, "log", &[NOW], &["log", "--format=%ar", "-2"], Shape::Linear);
    with(out, "blame", &[NOW], &["blame", "--date=relative", "README.md"], Shape::Branched);
}
