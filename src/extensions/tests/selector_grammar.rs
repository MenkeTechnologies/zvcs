//! The `[selectors]` grammar every fleet verb shares, as the four documents
//! describing it promise: a bare `<pattern>` is a case-insensitive substring
//! filter on the repo's workdir path, repeatable and ANDed, and `--repo <p>` is
//! the same thing spelled long.
//!
//! Two failure modes this guards, both of which route through one helper
//! (`query::selected`) and so break for all ~29 verbs at once:
//!
//!   * a bare pattern that is silently *dropped* — the verb then runs over the
//!     whole fleet while the user believes they narrowed it to one repo, which
//!     for a mutating verb (`zgc`, `zprune`, `zabort`) is the entire fleet
//!     mutated on a command that reads as single-repo;
//!   * a bare pattern that is silently *added* where the verb owns a positional
//!     of its own (`zstale <days>`, `zbig <n>`) — the number would filter repos
//!     by path instead of setting the threshold, matching nothing.

use std::path::Path;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn git(dir: &Path, args: &[&str]) {
    assert!(Command::new(BIN).args(args).current_dir(dir).status().unwrap().success(), "git {args:?} failed");
}

/// Run a verb and return stdout+stderr together — the uniformity sweep below
/// compares verbs whose "nothing selected" note and whose real output land on
/// different streams.
fn zvcs(home: &Path, cwd: &Path, args: &[&str]) -> String {
    let out = Command::new(BIN).args(args).current_dir(cwd).env("ZVCS_HOME", home).output().unwrap();
    let mut s = String::from_utf8_lossy(&out.stdout).into_owned();
    s.push_str(&String::from_utf8_lossy(&out.stderr));
    s
}

/// Three indexed repos whose paths differ in case and in substring, so one
/// fixture can prove case-insensitivity, substring (not equality) matching, and
/// AND-composition at once.
fn fixture(tag: &str) -> (std::path::PathBuf, std::path::PathBuf) {
    let root = std::env::temp_dir().join(format!("zvcs-selgram-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let root = root.canonicalize().unwrap();
    let home = root.join("home");
    for name in ["Alpha-cask", "beta-cask", "gamma"] {
        let r = root.join(name);
        std::fs::create_dir_all(&r).unwrap();
        git(&r, &["init", "-q", "-b", "main"]);
        std::fs::write(r.join("f.txt"), b"1\n").unwrap();
        git(&r, &["add", "f.txt"]);
        git(&r, &["commit", "-q", "-m", "c0"]);
    }
    zvcs(&home, &root, &["zreindex", "--sync", root.to_str().unwrap()]);
    let all = zvcs(&home, &root, &["zheads"]);
    for name in ["Alpha-cask", "beta-cask", "gamma"] {
        assert!(all.contains(name), "precondition: {name} must be indexed:\n{all}");
    }
    (root, home)
}

#[test]
fn bare_pattern_filters_like_repo_flag() {
    let (root, home) = fixture("bare");

    // Substring, case-insensitive: "alpha" must reach the "Alpha-cask" workdir.
    let bare = zvcs(&home, &root, &["zheads", "alpha"]);
    assert!(bare.contains("Alpha-cask"), "a bare pattern must select the matching repo:\n{bare}");
    assert!(!bare.contains("beta-cask") && !bare.contains("gamma"),
        "a bare pattern must EXCLUDE non-matching repos (pattern dropped → whole fleet):\n{bare}");

    // `--repo <p>` is documented as the same thing; identical output or the two
    // spellings have drifted.
    let flag = zvcs(&home, &root, &["zheads", "--repo", "alpha"]);
    assert_eq!(bare, flag, "`zheads alpha` and `zheads --repo alpha` must select identically");

    // Substring, not equality: "cask" matches two of the three.
    let sub = zvcs(&home, &root, &["zheads", "cask"]);
    assert!(sub.contains("Alpha-cask") && sub.contains("beta-cask"), "substring must match both cask repos:\n{sub}");
    assert!(!sub.contains("gamma"), "substring must not match gamma:\n{sub}");

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn patterns_compose_with_and_and_can_match_nothing() {
    let (root, home) = fixture("and");

    // Repeated patterns AND: only the repo containing BOTH substrings.
    let both = zvcs(&home, &root, &["zheads", "cask", "beta"]);
    assert!(both.contains("beta-cask"), "both patterns hold for beta-cask:\n{both}");
    assert!(!both.contains("Alpha-cask"), "`cask beta` must not match Alpha-cask (OR bug):\n{both}");

    // Mixed spellings AND the same way.
    let mixed = zvcs(&home, &root, &["zheads", "--repo", "cask", "alpha"]);
    assert!(mixed.contains("Alpha-cask") && !mixed.contains("beta-cask"), "mixed spellings must AND:\n{mixed}");

    // Unsatisfiable pair → nothing, not everything.
    let none = zvcs(&home, &root, &["zheads", "alpha", "gamma"]);
    assert!(none.contains("no repos matched"), "an unsatisfiable pattern pair must select nothing:\n{none}");

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn unknown_flag_is_not_read_as_a_pattern() {
    let (root, home) = fixture("flag");

    // A mistyped selector must not become a path substring — that would match no
    // repo and report "no repos matched", which reads as an empty fleet rather
    // than as a bad flag. It stays ignored, so the fleet still lists.
    let typo = zvcs(&home, &root, &["zheads", "--drity"]);
    assert!(!typo.contains("no repos matched"), "an unknown flag must not act as a pattern:\n{typo}");
    assert!(typo.contains("Alpha-cask") && typo.contains("gamma"), "an unknown flag leaves the selection whole:\n{typo}");

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn verbs_owning_a_positional_keep_it() {
    let (root, home) = fixture("posn");

    // `zstale <days>` and `zbig <n>` peel their own positional before selecting.
    // If the number leaked into the pattern set it would match no path and the
    // verb would report an empty fleet instead of honoring the threshold.
    let stale = zvcs(&home, &root, &["zstale", "0"]);
    assert!(!stale.contains("no repos matched"), "`zstale 0` must read 0 as days, not as a path pattern:\n{stale}");
    let big = zvcs(&home, &root, &["zbig", "1"]);
    assert!(!big.contains("no repos matched"), "`zbig 1` must read 1 as a count, not as a path pattern:\n{big}");

    // Their selectors still work alongside the positional.
    let scoped = zvcs(&home, &root, &["zstale", "0", "--repo", "gamma"]);
    assert!(!scoped.contains("Alpha-cask"), "`zstale 0 --repo gamma` must still narrow:\n{scoped}");

    let _ = std::fs::remove_dir_all(&root);
}

/// Verbs spanning every module that routes through the shared helper — reads
/// (`query.rs`), analytics (`analytics.rs`), and fleet mutations (`pmutate.rs`)
/// — so a verb that parses its own args and bypasses the grammar is caught.
const UNIFORM: &[&str] = &[
    "zheads", "zdirty", "zbranches", "ztags", "zremotes", "zsize", "zage", "zlast", "zfiles",
    "zcommits", "zpristine", "zauthors", "zconflicts", "zdivergent", "zorphans", "zgc", "zfsck",
    "zabort", "zreview",
];

#[test]
fn every_selector_verb_honors_the_same_grammar() {
    let (root, home) = fixture("uniform");

    for verb in UNIFORM {
        // A pattern no path contains selects nothing, in every verb.
        let miss = zvcs(&home, &root, &[verb, "zzz-no-such-repo"]);
        assert!(miss.contains("no repos matched"),
            "`git {verb} zzz-no-such-repo` ignored the pattern and ran over the fleet:\n{miss}");

        // A pattern one path contains selects something, in every verb — a verb
        // that answered the line above by always reporting empty is caught here.
        let hit = zvcs(&home, &root, &[verb, "gamma"]);
        assert!(!hit.contains("no repos matched"),
            "`git {verb} gamma` matched nothing though gamma is indexed:\n{hit}");
    }

    let _ = std::fs::remove_dir_all(&root);
}
