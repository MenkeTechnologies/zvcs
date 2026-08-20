//! `git log --encoding=<charset>` and `git log --author-date-order`.
//!
//! The encoding cases turn on a commit stored under an `encoding ISO-8859-1`
//! header: the fixture writes that object with `hash-object`/`commit-tree` so the
//! bytes are fixed, and every expectation below is what stock git 2.55.0 prints
//! for it. Three behaviours the shape of `repo_logmsg_reencode()` decides, and
//! that this file pins:
//!
//!   * `--encoding=none` is the *empty* output encoding, which is an early return:
//!     the commit is printed exactly as stored, `encoding` header included.
//!   * the header is rewritten to name the encoding the message is now in, and
//!     dropped when that is UTF-8 — so `--pretty=raw` shows no `encoding` line
//!     under the default output encoding and does show one under `--encoding=none`.
//!   * a conversion `iconv(3)` cannot do — an unknown charset, or a character the
//!     target cannot represent — is not an error: the stored bytes are printed.
//!
//! A *user* format takes the other road (`repo_format_commit_message()`): the
//! commit is re-coded to UTF-8 whatever `--encoding` said, and only the finished
//! record is converted afterwards. `--encoding=none` therefore prints UTF-8 there.

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_git");

/// `café subject\n\nbody é ü end\n` in ISO-8859-1.
const LATIN1_MESSAGE: &[u8] = b"caf\xe9 subject\n\nbody \xe9 \xfc end\n";
/// The same text in UTF-8.
const UTF8_MESSAGE: &[u8] = "café subject\n\nbody é ü end\n".as_bytes();

fn run(dir: &Path, args: &[&str]) -> Output {
    run_with_stdin(dir, args, &[])
}

fn run_with_stdin(dir: &Path, args: &[&str], stdin: &[u8]) -> Output {
    let mut child = Command::new(BIN)
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("LC_ALL", "C")
        .env("GIT_AUTHOR_NAME", "A")
        .env("GIT_AUTHOR_EMAIL", "a@example.com")
        .env("GIT_COMMITTER_NAME", "A")
        .env("GIT_COMMITTER_EMAIL", "a@example.com")
        .env("GIT_AUTHOR_DATE", "2020-01-01T00:00:00Z")
        .env("GIT_COMMITTER_DATE", "2020-01-01T00:00:00Z")
        .env_remove("GIT_DIR")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn zvcs git");
    {
        use std::io::Write;
        child.stdin.as_mut().expect("stdin").write_all(stdin).expect("write stdin");
    }
    child.wait_with_output().expect("run zvcs git")
}

fn bytes(o: &Output) -> Vec<u8> {
    o.stdout.clone()
}

fn text(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

fn err(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

fn code(o: &Output) -> i32 {
    o.status.code().unwrap_or(-1)
}

fn tmp(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("zvcs-logenc-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("mkdir");
    dir.canonicalize().expect("canonicalize")
}

/// A repository whose single commit carries `encoding ISO-8859-1` and a Latin-1
/// message. `commit-tree` is fed the message on stdin so the stored bytes are
/// exactly [`LATIN1_MESSAGE`] rather than whatever an editor or locale would do.
fn latin1_repo(tag: &str) -> PathBuf {
    let dir = tmp(tag);
    assert!(run(&dir, &["init", "-q", "-b", "main"]).status.success(), "init");
    std::fs::write(dir.join("f"), "x\n").expect("write");
    assert!(run(&dir, &["add", "f"]).status.success(), "add");
    let tree = text(&run(&dir, &["write-tree"])).trim_end().to_string();
    let o = run_with_stdin(
        &dir,
        &["-c", "i18n.commitEncoding=ISO-8859-1", "commit-tree", &tree],
        LATIN1_MESSAGE,
    );
    assert_eq!(code(&o), 0, "commit-tree: {}", err(&o));
    let commit = text(&o).trim_end().to_string();
    let o = run(&dir, &["update-ref", "refs/heads/main", &commit]);
    assert_eq!(code(&o), 0, "update-ref: {}", err(&o));

    // The fixture is only meaningful if the object really holds the header and the
    // Latin-1 bytes, so check that before any assertion depends on it.
    let raw = bytes(&run(&dir, &["cat-file", "commit", "HEAD"]));
    assert!(
        raw.windows(21).any(|w| w == b"encoding ISO-8859-1\n\n"),
        "fixture commit is missing its encoding header"
    );
    assert!(raw.contains(&0xe9), "fixture commit is not Latin-1");
    dir
}

#[test]
fn the_default_output_encoding_recodes_to_utf8_and_drops_the_header() {
    let dir = latin1_repo("default");
    // No `--encoding`: the output encoding is UTF-8, so the message is converted
    // and `replace_encoding_header()` removes the now-wrong header.
    let o = run(&dir, &["log", "--pretty=raw"]);
    assert_eq!(code(&o), 0, "{}", err(&o));
    let got = bytes(&o);
    assert!(!got.contains(&0xe9), "should be UTF-8, not Latin-1: {got:?}");
    assert!(
        !text(&o).contains("encoding ISO-8859-1"),
        "the header is dropped once the message is UTF-8: {}",
        text(&o)
    );
    assert!(text(&o).contains("café subject"), "{}", text(&o));
}

#[test]
fn encoding_none_prints_the_stored_bytes_and_keeps_the_header() {
    let dir = latin1_repo("none");
    let o = run(&dir, &["log", "--encoding=none", "--pretty=raw"]);
    assert_eq!(code(&o), 0, "{}", err(&o));
    let got = bytes(&o);
    assert!(got.contains(&0xe9), "the stored Latin-1 bytes survive: {got:?}");
    assert!(text(&o).contains("encoding ISO-8859-1"), "{}", text(&o));

    // The separated spelling reaches the same slot.
    assert_eq!(bytes(&run(&dir, &["log", "--encoding", "none", "--pretty=raw"])), got);
}

#[test]
fn an_explicit_charset_recodes_and_rewrites_the_header() {
    let dir = latin1_repo("explicit");
    // Already ISO-8859-1: `same_encoding()` short-circuits the conversion, but the
    // header is still rewritten (to the same name), so the bytes are the stored ones.
    let o = run(&dir, &["log", "--encoding=ISO-8859-1", "--pretty=raw"]);
    assert!(bytes(&o).contains(&0xe9), "{:?}", bytes(&o));
    assert!(text(&o).contains("encoding ISO-8859-1"), "{}", text(&o));

    // `latin1` and `iso-8859-1` name the same charset as far as this conversion is
    // concerned, and the header is rewritten to the *requested* spelling.
    let o = run(&dir, &["log", "--encoding=latin1", "--pretty=raw"]);
    assert!(text(&o).contains("encoding latin1"), "{}", text(&o));
    assert!(bytes(&o).contains(&0xe9));

    // UTF-8 in any spelling drops the header.
    for spelling in ["UTF-8", "utf8", "utf-8", "UTF8"] {
        let o = run(&dir, &["log", &format!("--encoding={spelling}"), "--pretty=raw"]);
        assert!(!text(&o).contains("encoding "), "{spelling}: {}", text(&o));
        assert!(!bytes(&o).contains(&0xe9), "{spelling}");
    }
}

#[test]
fn a_conversion_that_cannot_be_done_leaves_the_message_alone() {
    let dir = latin1_repo("fallback");
    // An unknown charset name: `iconv_open()` fails, `reencode_string()` returns
    // NULL, and git prints the stored buffer — header and all — rather than dying.
    let o = run(&dir, &["log", "--encoding=bogus-charset", "--pretty=raw"]);
    assert_eq!(code(&o), 0, "{}", err(&o));
    assert!(bytes(&o).contains(&0xe9), "{:?}", bytes(&o));
    assert!(text(&o).contains("encoding ISO-8859-1"), "{}", text(&o));

    // A charset that cannot represent the text is the same story (EILSEQ).
    let o = run(&dir, &["log", "--encoding=US-ASCII", "--pretty=raw"]);
    assert_eq!(code(&o), 0, "{}", err(&o));
    assert!(bytes(&o).contains(&0xe9), "{:?}", bytes(&o));
}

#[test]
fn i18n_config_supplies_the_output_encoding_and_the_option_overrides_it() {
    let dir = latin1_repo("i18n");
    // `get_log_output_encoding()`: `i18n.logOutputEncoding` first, then
    // `i18n.commitEncoding`, then UTF-8.
    for key in ["i18n.logOutputEncoding", "i18n.commitEncoding"] {
        let o = run(&dir, &["-c", &format!("{key}=ISO-8859-1"), "log", "--pretty=raw"]);
        assert!(bytes(&o).contains(&0xe9), "{key}: {:?}", bytes(&o));
        assert!(text(&o).contains("encoding ISO-8859-1"), "{key}");
    }
    // `logOutputEncoding` wins over `commitEncoding`.
    let o = run(
        &dir,
        &["-c", "i18n.logOutputEncoding=UTF-8", "-c", "i18n.commitEncoding=ISO-8859-1", "log", "--pretty=raw"],
    );
    assert!(!bytes(&o).contains(&0xe9), "{:?}", bytes(&o));

    // `--encoding=none` beats a configured output encoding: it *sets* the same slot.
    let o = run(&dir, &["-c", "i18n.logOutputEncoding=UTF-8", "log", "--encoding=none", "--pretty=raw"]);
    assert!(bytes(&o).contains(&0xe9), "{:?}", bytes(&o));
}

#[test]
fn a_user_format_always_renders_from_utf8_and_converts_the_record() {
    let dir = latin1_repo("userfmt");
    // `--encoding=none` is UTF-8 here, unlike every built-in format: the commit is
    // re-coded to UTF-8 before the format is expanded, and the empty output encoding
    // asks for no conversion of the result.
    let o = run(&dir, &["log", "--encoding=none", "--pretty=format:%s"]);
    assert_eq!(bytes(&o), "café subject".as_bytes(), "{:?}", bytes(&o));

    // An explicit charset converts the rendered record.
    let o = run(&dir, &["log", "--encoding=ISO-8859-1", "--pretty=format:%s"]);
    assert_eq!(bytes(&o), b"caf\xe9 subject");

    // A conversion that cannot be done leaves the UTF-8 record in place.
    let o = run(&dir, &["log", "--encoding=US-ASCII", "--pretty=format:%s"]);
    assert_eq!(bytes(&o), "café subject".as_bytes());

    // `reference` is `CMIT_FMT_USERFORMAT` with a built-in format, so it takes the
    // same road — `--encoding=none` prints UTF-8 there too.
    let o = run(&dir, &["log", "--encoding=none", "--pretty=reference"]);
    assert!(!bytes(&o).contains(&0xe9), "{:?}", bytes(&o));
}

#[test]
fn a_utf8_commit_with_no_header_is_recoded_all_the_same() {
    let dir = tmp("noheader");
    assert!(run(&dir, &["init", "-q", "-b", "main"]).status.success());
    std::fs::write(dir.join("f"), "x\n").expect("write");
    assert!(run(&dir, &["add", "f"]).status.success());
    let tree = text(&run(&dir, &["write-tree"])).trim_end().to_string();
    let commit = text(&run_with_stdin(&dir, &["commit-tree", &tree], UTF8_MESSAGE)).trim_end().to_string();
    assert!(run(&dir, &["update-ref", "refs/heads/main", &commit]).status.success());

    // No `encoding` header at all, so `use_encoding` is UTF-8 — and asking for
    // Latin-1 still converts, because git assumes the stored bytes are UTF-8.
    let o = run(&dir, &["log", "--encoding=ISO-8859-1", "--pretty=raw"]);
    assert_eq!(code(&o), 0, "{}", err(&o));
    assert!(bytes(&o).contains(&0xe9), "{:?}", bytes(&o));
    // `replace_encoding_header()` finds no header to rewrite, so none is added.
    assert!(!text(&o).contains("encoding "), "{}", text(&o));

    // `--encoding=none` and UTF-8 both leave it alone.
    for arg in ["--encoding=none", "--encoding=UTF-8"] {
        let o = run(&dir, &["log", arg, "--pretty=raw"]);
        assert!(!bytes(&o).contains(&0xe9), "{arg}");
    }
}

// ---------------------------------------------------------------------------
// --author-date-order
// ---------------------------------------------------------------------------

/// A history whose author dates deliberately disagree with its committer dates,
/// with a side branch merged back in — so a commit-date order, an author-date
/// order and a graph order are all different.
fn skewed_repo(tag: &str) -> PathBuf {
    let dir = tmp(tag);
    assert!(run(&dir, &["init", "-q", "-b", "main"]).status.success(), "init");
    let commit = |name: &str, author: &str, committer: &str| {
        std::fs::write(dir.join(name), format!("{name}\n")).expect("write");
        assert!(run(&dir, &["add", name]).status.success(), "add {name}");
        let o = Command::new(BIN)
            .args(["commit", "-q", "-m", name])
            .current_dir(&dir)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_AUTHOR_NAME", "A")
            .env("GIT_AUTHOR_EMAIL", "a@example.com")
            .env("GIT_COMMITTER_NAME", "A")
            .env("GIT_COMMITTER_EMAIL", "a@example.com")
            .env("GIT_AUTHOR_DATE", author)
            .env("GIT_COMMITTER_DATE", committer)
            .output()
            .expect("commit");
        assert!(o.status.success(), "commit {name}: {}", String::from_utf8_lossy(&o.stderr));
    };
    commit("A", "2021-01-01T00:00:00Z", "2020-01-01T00:00:00Z");
    commit("B", "2019-01-01T00:00:00Z", "2020-01-02T00:00:00Z");
    commit("C", "2020-06-01T00:00:00Z", "2020-01-03T00:00:00Z");
    assert!(run(&dir, &["checkout", "-q", "-b", "side", "HEAD~2"]).status.success());
    commit("D", "2020-03-01T00:00:00Z", "2020-01-04T00:00:00Z");
    commit("E", "2022-01-01T00:00:00Z", "2020-01-05T00:00:00Z");
    assert!(run(&dir, &["checkout", "-q", "main"]).status.success());
    let o = Command::new(BIN)
        .args(["merge", "-q", "--no-ff", "side", "-m", "M"])
        .current_dir(&dir)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_AUTHOR_NAME", "A")
        .env("GIT_AUTHOR_EMAIL", "a@example.com")
        .env("GIT_COMMITTER_NAME", "A")
        .env("GIT_COMMITTER_EMAIL", "a@example.com")
        .env("GIT_AUTHOR_DATE", "2020-09-01T00:00:00Z")
        .env("GIT_COMMITTER_DATE", "2020-01-06T00:00:00Z")
        .output()
        .expect("merge");
    assert!(o.status.success(), "merge: {}", String::from_utf8_lossy(&o.stderr));
    dir
}

/// The subjects `git log --oneline` printed, in order.
fn subjects(o: &Output) -> Vec<String> {
    text(o)
        .lines()
        .map(|l| l.split_once(' ').map_or(String::new(), |(_, s)| s.to_string()))
        .collect()
}

#[test]
fn author_date_order_breaks_topological_ties_by_the_author_clock() {
    let dir = skewed_repo("ado");
    // `D` and `C` are the pair the tie-break moves: `D` has the later *commit* date
    // and `C` the later *author* date, so the two orders swap them. Both are pinned
    // to what stock git 2.55.0 printed for this fixture.
    let by_commit = subjects(&run(&dir, &["log", "--date-order", "--oneline"]));
    let by_author = subjects(&run(&dir, &["log", "--author-date-order", "--oneline"]));
    assert_eq!(by_commit, ["M", "E", "D", "C", "B", "A"]);
    assert_eq!(by_author, ["M", "E", "C", "D", "B", "A"]);
    assert_ne!(by_commit, by_author, "the fixture must actually exercise the tie-break");

    // Every parent still follows all of its children: the sort is topological, and
    // only the tie-break changed.
    let position = |name: &str| by_author.iter().position(|s| s == name).expect(name);
    assert!(position("M") < position("C"), "{by_author:?}");
    assert!(position("M") < position("E"), "{by_author:?}");
    assert!(position("C") < position("B"), "{by_author:?}");
    assert!(position("E") < position("D"), "{by_author:?}");
    assert!(position("D") < position("A"), "{by_author:?}");
    assert!(position("B") < position("A"), "{by_author:?}");
}

#[test]
fn the_last_order_flag_on_the_line_wins() {
    let dir = skewed_repo("ado-last");
    let a = subjects(&run(&dir, &["log", "--date-order", "--author-date-order", "--oneline"]));
    assert_eq!(a, subjects(&run(&dir, &["log", "--author-date-order", "--oneline"])));
    let b = subjects(&run(&dir, &["log", "--author-date-order", "--date-order", "--oneline"]));
    assert_eq!(b, subjects(&run(&dir, &["log", "--date-order", "--oneline"])));
}

#[test]
fn author_date_order_is_inert_under_no_walk() {
    let dir = skewed_repo("ado-nowalk");
    // `prepare_revision_walk()` returns before `sort_in_topological_order()` when
    // `no_walk` survived, so the pending order stands.
    let plain = subjects(&run(&dir, &["log", "--no-walk", "--oneline", "HEAD", "side"]));
    let sorted = subjects(&run(&dir, &["log", "--author-date-order", "--no-walk", "--oneline", "HEAD", "side"]));
    assert_eq!(plain, sorted);
}
