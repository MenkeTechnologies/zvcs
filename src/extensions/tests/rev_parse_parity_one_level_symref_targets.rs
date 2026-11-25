//! Symbolic refs whose target is a one-level name, against stock git:
//!
//! * `refs_resolve_ref_unsafe()` holds a symref's target to
//!   `check_refname_format(refname, REFNAME_ALLOW_ONELEVEL)` (refs.c:2190-2195)
//!   and reads each link by its exact name (refs.c:2151-2152), so `ref: x` and
//!   `ref: A1` are `$GIT_DIR/x` and `$GIT_DIR/A1`. gitoxide's `FullName` refused
//!   both names and its lookup expanded them through `refs/`, so zvcs stopped at
//!   the symref: `rev-parse`, `log`, `show-ref` and `branch` lost it.
//! * The first `ref_rev_parse_rules` entry is the operand itself (refs.c:620-621),
//!   so `rev-parse x` reads `$GIT_DIR/x`.
//! * `ref-filter` lists a symref with the referent the iterator resolved it to
//!   (ref-filter.c:3080, refs/files-backend.c:324-327), the *last* name of the
//!   chain: a symref to a symref is `chain -> side`, never the name in between.
//! * A symref file with nothing usable after `ref:` is still `REF_ISSYMREF`
//!   (refs/files-backend.c:674-681), so `expand_ref()` warns `ignoring dangling
//!   symref` (refs.c:829-830), not `ignoring broken ref`.
//! * `validate_headref()` (setup.c:350-400) accepts only `ref: refs/…` or an
//!   object id as `HEAD`, so a `HEAD` that is empty, `ref:` alone or `ref: x`
//!   makes the directory no repository at all, while an object id followed by
//!   anything but whitespace passes there and is a broken `HEAD` after
//!   (refs/files-backend.c:687-692).

#[path = "support/stock_git.rs"]
mod stock_git;
#[path = "support/twin_repo.rs"]
mod twin_repo;

use twin_repo::Side;

/// Write `setup` into both sides, then run each command on both and compare.
fn same(s: &Side, z: &Side, setup: &[(&str, &str)], commands: &[&[&str]]) {
    for side in [s, z] {
        for (path, body) in setup {
            side.write(path, body);
        }
    }
    for args in commands {
        assert_eq!(z.git(args), s.git(args), "{setup:?} {args:?}");
    }
}

#[test]
fn one_level_symref_targets_are_followed() {
    let Some(stock) = stock_git::stock_git() else { return };
    let (s, z) = twin_repo::pair("one-level-symref-targets", stock);
    let main = format!("{}\n", s.git(&["rev-parse", "main"]).stdout.trim());
    for side in [&s, &z] {
        side.git(&["symbolic-ref", "refs/heads/sym", "refs/heads/side"]);
        side.git(&["symbolic-ref", "refs/heads/chain", "refs/heads/sym"]);
    }
    same(
        &s,
        &z,
        &[
            (".git/x", &main),
            (".git/A1", &main),
            (".git/y", "ref: refs/heads/side\n"),
            (".git/refs/heads/lx", "ref: x\n"),
            (".git/refs/heads/la", "ref: A1\n"),
            (".git/refs/heads/ly", "ref: y\n"),
        ],
        &[
            &["rev-parse", "lx", "la", "ly", "x", "A1"],
            &["rev-parse", "--symbolic-full-name", "lx", "ly"],
            &["rev-parse", "--abbrev-ref", "la", "ly"],
            &["rev-parse", "--verify", "-q", "refs/heads/lx"],
            &["symbolic-ref", "refs/heads/ly"],
            &["symbolic-ref", "--short", "refs/heads/ly"],
            &["log", "-1", "--oneline", "--decorate", "lx"],
            &["log", "-1", "--oneline", "ly"],
            &["branch", "-v"],
            &["branch", "--column=always"],
            &["for-each-ref", "--format=%(refname) %(symref) %(symref:short) %(objectname:short)"],
            &["show-ref"],
            &["show-ref", "--verify", "refs/heads/la"],
        ],
    );
}

#[test]
fn unusable_symref_and_head_contents() {
    let Some(stock) = stock_git::stock_git() else { return };
    let (s, z) = twin_repo::pair("unusable-symref-head", stock);
    same(
        &s,
        &z,
        &[
            (".git/refs/heads/empty", ""),
            (".git/refs/heads/bare", "ref:"),
            (".git/refs/heads/blank", "ref: \n"),
        ],
        &[
            &["rev-parse", "--verify", "-q", "empty"],
            &["rev-parse", "--verify", "-q", "bare"],
            &["rev-parse", "--verify", "blank"],
            &["rev-parse", "bare"],
            &["branch"],
            &["show-ref"],
        ],
    );
    // An object id followed by anything but whitespace is a broken ref
    // (refs/files-backend.c:687-692), though `validate_headref()` accepts it.
    let garbage = format!("{}garbage\n", s.git(&["rev-parse", "main"]).stdout.trim());
    same(
        &s,
        &z,
        &[(".git/HEAD", &garbage)],
        &[&["rev-parse", "HEAD"], &["symbolic-ref", "HEAD"], &["log", "-1"]],
    );
    for head in ["", "ref:\n", "ref: x\n"] {
        same(&s, &z, &[(".git/HEAD", head)], &[&["rev-parse", "--verify", "-q", "HEAD"]]);
    }
}
