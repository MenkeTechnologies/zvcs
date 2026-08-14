//! `--pretty=format:` column padding (`%<`, `%>`, `%><`, `%>>`) and wrapping
//! (`%w`) for `log` and `show`.
//!
//! Every expectation below was captured from stock git 2.55.0 on this fixture,
//! whose subjects are chosen to break a byte-counting implementation: `café naïve
//! accents` is 18 columns in 21 bytes, and `日本語のサブジェクト wide` is 25
//! columns in 35 bytes because each CJK glyph occupies two. A port that pads by
//! `str::len()` passes on ASCII alone and misaligns every real-world table.

use std::path::{Path, PathBuf};
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");
const DATE: &str = "1112911993 -0700";

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new(BIN)
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("HOME", dir.parent().expect("repo has a parent").join("home"))
        .env("ZVCS_HOME", dir.parent().expect("repo has a parent").join("home"))
        .env("GIT_AUTHOR_DATE", DATE)
        .env("GIT_COMMITTER_DATE", DATE)
        .env("GIT_AUTHOR_NAME", "A U Thor")
        .env("GIT_AUTHOR_EMAIL", "author@example.com")
        .env("GIT_COMMITTER_NAME", "C O Mitter")
        .env("GIT_COMMITTER_EMAIL", "committer@example.com")
        // `term_columns()` reads COLUMNS; pinning it keeps `%<|(-<N>)` stable.
        .env("COLUMNS", "80")
        .output()
        .unwrap();
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn fixture(name: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!("zvcs-padfmt-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let repo = root.join("repo");
    std::fs::create_dir_all(root.join("home")).unwrap();
    std::fs::create_dir_all(&repo).unwrap();
    let repo = repo.canonicalize().unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    git(&repo, &["config", "user.email", "author@example.com"]);
    git(&repo, &["config", "user.name", "A U Thor"]);
    for (n, subject) in
        ["first commit subject", "日本語のサブジェクト wide", "café naïve accents", "short"]
            .iter()
            .enumerate()
    {
        std::fs::write(repo.join(format!("f{n}")), "x\n").unwrap();
        git(&repo, &["add", &format!("f{n}")]);
        git(&repo, &["commit", "-q", "-m", subject]);
    }
    repo
}

#[test]
fn padding_is_measured_in_display_columns() {
    let repo = fixture("columns");
    // `%<` pads on the right, `%>` on the left, `%><` both.
    assert_eq!(
        git(&repo, &["log", "--format=%<(20)%s|"]),
        "short               |\n\
         café naïve accents  |\n\
         日本語のサブジェクト wide|\n\
         first commit subject|\n"
    );
    assert_eq!(
        git(&repo, &["log", "-1", "--format=%>(20)%s|"]),
        "               short|\n"
    );
    assert_eq!(
        git(&repo, &["log", "-1", "--format=%><(20)%s|"]),
        "       short        |\n"
    );
    // Content wider than the field simply overflows when no truncation is asked
    // for — the CJK subject is 25 columns.
    assert_eq!(
        git(&repo, &["log", "--skip=2", "-1", "--format=%<(20)%s|"]),
        "日本語のサブジェクト wide|\n"
    );
}

#[test]
fn truncation_modifiers_cut_where_git_cuts() {
    let repo = fixture("trunc");
    assert_eq!(git(&repo, &["log", "-1", "--format=%<(10,trunc)%s|"]), "short     |\n");
    let accents = ["--skip=1", "-1"];
    let case = |modifier: &str| {
        let fmt = format!("--format=%<(10,{modifier})%s|");
        git(&repo, &[&["log"][..], &accents[..], &[fmt.as_str()][..]].concat())
    };
    assert_eq!(case("trunc"), "café naï..|\n");
    assert_eq!(case("ltrunc"), ".. accents|\n");
    assert_eq!(case("mtrunc"), "café..ents|\n");
    // Wide glyphs are two columns each, so the cut lands on a different glyph
    // than a byte- or char-counting implementation would pick.
    let cjk = ["--skip=2", "-1"];
    let wide = |modifier: &str| {
        let fmt = format!("--format=%<(10,{modifier})%s|");
        git(&repo, &[&["log"][..], &cjk[..], &[fmt.as_str()][..]].concat())
    };
    assert_eq!(wide("trunc"), "日本語の..|\n");
    assert_eq!(wide("mtrunc"), "日本..wide|\n");
    // A field too narrow for anything but the marker collapses to it.
    assert_eq!(git(&repo, &["log", "-1", "--format=%<(2,trunc)%s|"]), "..|\n");
    assert_eq!(git(&repo, &["log", "-1", "--format=%<(4,mtrunc)%s|"]), "s..t|\n");
}

#[test]
fn column_targets_and_stealing_account_for_what_is_already_on_the_line() {
    let repo = fixture("target");
    // `%<|(<N>)` pads *to* column N, so the hash in front counts: whatever the
    // abbreviation length is, the `|` lands in column 21.
    let hash = git(&repo, &["log", "-1", "--format=%h"]).trim_end().to_owned();
    let line = git(&repo, &["log", "-1", "--format=%h %<|(20)%s|"]);
    let line = line.trim_end_matches('\n');
    assert!(line.starts_with(&format!("{hash} short")), "{line:?}");
    assert_eq!(line.chars().count(), 21, "the field did not end at column 20: {line:?}");
    assert!(line.ends_with("|"), "{line:?}");
    // Already past the target: nothing is added.
    assert_eq!(git(&repo, &["log", "-1", "--format=%h%<|(1)|"]), format!("{hash}|\n"));
    // A negative column target is measured from `term_columns()` (COLUMNS=80).
    let line = git(&repo, &["log", "-1", "--format=%<|(-5)%s|"]);
    assert_eq!(line.trim_end_matches('\n').chars().count(), 76, "line: {line:?}");
    assert!(line.starts_with("short "), "{line:?}");
    // `%>>` eats the spaces already in the buffer when the text does not fit,
    // and right-aligns within the field when it does. Only a *placeholder* spends
    // the pending field, which is why the literal `x` below is padded and the
    // author name above is not.
    assert_eq!(git(&repo, &["log", "-1", "--format=%s   %>>(4)%an|"]), "shortA U Thor|\n");
    // The three literal spaces are eaten and the 20-wide field right-aligns the
    // name: `short` + 15 spaces + `A U Thor` is 28 columns.
    assert_eq!(
        git(&repo, &["log", "-1", "--format=%s   %>>(20)%an|"]),
        format!("short{}A U Thor|\n", " ".repeat(15))
    );
    assert_eq!(git(&repo, &["log", "-1", "--format=x%>>(10)%s|"]), "x     short|\n");
}

#[test]
fn malformed_atoms_print_literally_and_still_arm_the_field() {
    let repo = fixture("malformed");
    // Width 0, a bare atom, a negative plain width and a non-decimal width are
    // not placeholders at all: git prints them as typed.
    for fmt in ["%<(0)%s|", "%<()%s|", "%<(abc)%s|", "%<(-5)%s|", "%<(20%s|"] {
        let want = format!("{}short|\n", &fmt[..fmt.len() - 3]);
        assert_eq!(git(&repo, &["log", "-1", &format!("--format={fmt}")]), want, "{fmt}");
    }
    // A bad truncation modifier prints literally but the width it already stored
    // still pads the next placeholder — git assigns before it validates.
    assert_eq!(
        git(&repo, &["log", "-1", "--format=%<(20,bogus)%s|"]),
        "%<(20,bogus)short               |\n"
    );
    // A pending field is spent by the next placeholder even when that
    // placeholder is one git does not know.
    assert_eq!(git(&repo, &["log", "-1", "--format=%<(20)%xZZ|"]), "                    %xZZ|\n");
    // `%%` is expanded by the driver, ahead of the padding machinery, so it
    // neither consumes the field nor is padded by it — the subject after it is.
    assert_eq!(git(&repo, &["log", "-1", "--format=%<(9)%%%s|"]), "%short    |\n");
}

#[test]
fn wrapping_rewraps_everything_emitted_after_it() {
    let repo = fixture("wrap");
    assert_eq!(
        git(&repo, &["log", "-1", "--format=%w(20,4)aaa bbb ccc ddd eee fff ggg"]),
        "    aaa bbb ccc ddd\neee fff ggg\n"
    );
    // indent2 applies from the second line on.
    assert_eq!(
        git(&repo, &["log", "-1", "--format=%w(20,2,4)%s %s %s %s %s"]),
        "  short short short\n    short short\n"
    );
    // `%w()` and `%w(0)` are width zero: no wrapping.
    assert_eq!(git(&repo, &["log", "-1", "--format=%w()%s"]), "short\n");
    assert_eq!(git(&repo, &["log", "-1", "--format=%w(0)%s"]), "short\n");
    // A malformed atom is literal.
    assert_eq!(git(&repo, &["log", "-1", "--format=%w(abc)%s"]), "%w(abc)short\n");
}

#[test]
fn show_pads_the_same_way_log_does() {
    let repo = fixture("show");
    assert_eq!(git(&repo, &["show", "-s", "--format=%<(20)%s|", "HEAD"]), "short               |\n");
    assert_eq!(
        git(&repo, &["show", "-s", "--format=%<(10,mtrunc)%s|", "HEAD~1"]),
        "café..ents|\n"
    );
    assert_eq!(git(&repo, &["show", "-s", "--format=%w(10)%s", "HEAD~3"]), "first\ncommit\nsubject\n");
}

#[test]
fn a_colour_chain_swallows_the_percent_of_the_placeholder_it_pulls_in() {
    let repo = fixture("chain");
    // `format_and_pad_commit()` keeps pulling placeholders into the field while
    // each one is a `%C…`, and counts the `%` it steps over. When what follows
    // turns out not to be a placeholder at all, that `%` has already been
    // consumed — so git prints the rest as literal text *without* re-emitting it.
    // Found by differential fuzzing against stock git 2.55.0.
    assert_eq!(
        git(&repo, &["log", "-1", "--format=,x%>>(20%C(red)x%C(auto)%w("]),
        format!(",xx{}w(\n", " ".repeat(20))
    );
    // With no chain in front, the same unparsable atom keeps its `%`.
    assert_eq!(git(&repo, &["log", "-1", "--format=%<(20)%w("]), format!("{}%w(\n", " ".repeat(20)));
}
