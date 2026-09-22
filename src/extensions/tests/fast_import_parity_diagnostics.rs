//! `git fast-import`'s parser diagnostics, and the two places where the shape
//! of the grammar is not what the documentation suggests.
//!
//! Almost every `die()` in `builtin/fast-import.c` quotes the global
//! `command_buf` — the whole line as typed — rather than the fragment the
//! helper was handed, and the mark helpers split into three variants with three
//! different complaints (`no value after ':' in mark`, `garbage after mark`,
//! `missing space after mark`). A port that invents its own wording is
//! indistinguishable from a working one until a frontend matches on the text,
//! which `git-p4`, `git-remote-hg` and every home-grown importer do.
//!
//! Two behaviours here are not diagnostics at all:
//!
//!   * `parse_data()` runs its byte count through a bare `strtoumax()` and never
//!     checks for a conversion (fast-import.c:1942), so `data bogus` is a
//!     zero-length payload rather than an error; and when the stream ends early
//!     the count it reports is what is *still* missing, not what was asked for.
//!   * `parse_alias()` ends in `parse_objectish()`, whose final
//!     `read_next_command()` nothing puts back (fast-import.c:2688, 3623-3642),
//!     so the line after `to <objectish>` is consumed and discarded. A stream
//!     that separates blocks with blank lines loses the blank; one that does not
//!     loses a command.
//!
//! Every expectation was measured from stock git 2.55.0 in a fresh repository
//! before it was written down. The measurements, verbatim:
//!
//! ```text
//! $ printf 'commit refs/heads/x\n'                  -> fatal: expected committer but didn't get one
//! $ printf 'blob\ndata 5\nabc\n'                    -> fatal: EOF in data (1 bytes remaining)
//! $ printf 'blob\ndata bogus\n'                     -> (rc 0, empty blob, no output)
//! $ printf 'blob\nmark :x\ndata 0\n'                -> (rc 0, no mark declared)
//! $ printf 'get-mark x\n'                           -> fatal: not a mark: x
//! $ printf 'get-mark :\n'                           -> fatal: no value after ':' in mark: get-mark :
//! $ printf 'get-mark :1x\n'                         -> fatal: garbage after mark: get-mark :1x
//! $ ... 'M 100644 :1x f'                            -> fatal: missing space after mark: M 100644 :1x f
//! $ ... 'M 100645 :1 f'                             -> fatal: corrupt mode: M 100645 :1 f
//! $ ... 'M 100644 <unknown-40-hex> f'               -> fatal: blob not found: M 100644 <hex> f
//! $ ... 'M 40000 :1 d'      (:1 is a blob)          -> fatal: not a tree (actually a blob): M 40000 :1 d
//! $ ... 'M 100644 <41-hex> f'                       -> fatal: missing space after SHA1: M 100644 <41hex> f
//! $ ... 'M 160000 inline f'                         -> fatal: Git links cannot be specified 'inline': M 160000 inline f
//! $ ... 'M 40000 inline f'                          -> fatal: directories cannot be specified 'inline': M 40000 inline f
//! $ printf 'cat-blob <unknown-40-hex>\n'            -> <hex> missing      (on stdout, rc 0)
//! $ ... 'cat-blob :5'       (:5 is a commit)        -> fatal: object <hex> is a commit but a blob was expected.
//! $ ... 'from :1'           (:1 is a blob)          -> fatal: mark :1 not a commit
//! $ printf 'tag t\nfoo\n'                           -> fatal: expected 'from' command, got 'foo'
//! $ printf 'blob\nfoo\n'                            -> fatal: expected 'data n' command, found: foo
//! $ printf 'alias\nfoo\n'                           -> fatal: expected 'mark' command, got foo
//! $ printf 'alias\nmark :1\nfoo\n'                  -> fatal: expected 'to' command, got foo
//! $ ... alias block, blank line, reset+from         -> (rc 0, refs/tags/lw created)
//! $ ... alias block, no blank line, reset+from      -> fatal: unsupported command: from :9
//! $ git fast-import --max-pack-size=1k              -> warning: max-pack-size is now in bytes, assuming --max-pack-size=1024m
//! $ git fast-import --max-pack-size=100000          -> warning: minimum max-pack-size is 1 MiB
//! ```

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_git");

/// A fresh repository, one per stream, so no run sees another's marks or refs.
fn repo(tag: &str) -> PathBuf {
    let root = std::env::temp_dir()
        .join(format!("zvcs-fi-diag-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let root = root.canonicalize().unwrap();
    let dir = root.join("repo");
    std::fs::create_dir_all(&dir).unwrap();
    assert!(
        Command::new(BIN)
            .args(["init", "-q", "-b", "main"])
            .current_dir(&dir)
            .env("ZVCS_HOME", root.join("home"))
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .status()
            .unwrap()
            .success(),
        "git init failed"
    );
    dir
}

struct Run {
    code: i32,
    out: String,
    err: String,
}

/// Feed `stream` to `git fast-import --quiet` (quiet only suppresses the stats
/// block, never a `warning:`) plus `extra`, and collect everything it said.
fn import(tag: &str, stream: &str, extra: &[&str]) -> Run {
    let dir = repo(tag);
    let mut args = vec!["fast-import", "--quiet"];
    args.extend_from_slice(extra);
    let mut child = Command::new(BIN)
        .args(&args)
        .current_dir(&dir)
        .env("ZVCS_HOME", dir.parent().unwrap().join("home"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("LC_ALL", "C")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn fast-import");
    child.stdin.take().unwrap().write_all(stream.as_bytes()).unwrap();
    let out = child.wait_with_output().unwrap();
    let _ = std::fs::remove_dir_all(dir.parent().unwrap());
    Run {
        code: out.status.code().unwrap_or(-1),
        out: String::from_utf8_lossy(&out.stdout).into_owned(),
        err: String::from_utf8_lossy(&out.stderr).into_owned(),
    }
}

/// A blob at mark 1 and a commit at mark 5 whose tree holds it, so a stream can
/// name an object of the wrong type without inventing an id.
const PRELUDE: &str = "blob\nmark :1\ndata 1\nx\n\
                       commit refs/heads/x\nmark :5\n\
                       committer A <a@e.com> 100 +0000\ndata 0\nM 100644 :1 f\n";

const UNKNOWN: &str = "deadbeefdeadbeefdeadbeefdeadbeefdeadbeef";

fn fatal(tag: &str, stream: &str, want: &str) {
    let r = import(tag, stream, &[]);
    assert_eq!(r.err.lines().next().unwrap_or(""), format!("fatal: {want}"), "stream: {stream:?}");
    assert_eq!(r.code, 128, "stream: {stream:?}");
}

#[test]
fn diagnostics_quote_the_command_line_git_quotes() {
    fatal("committer", "commit refs/heads/x\n", "expected committer but didn't get one");
    fatal("shortdata", "blob\ndata 5\nabc\n", "EOF in data (1 bytes remaining)");
    fatal("blobdata", "blob\nfoo\n", "expected 'data n' command, found: foo");
    fatal("tagfrom", "tag t\nfoo\n", "expected 'from' command, got 'foo'");
    fatal("aliasmark", "alias\nfoo\n", "expected 'mark' command, got foo");
    fatal("aliasto", "alias\nmark :1\nfoo\n", "expected 'to' command, got foo");
    fatal("refexpr", "reset refs/heads/x\nfrom deadbeef\n",
          "invalid ref name or SHA1 expression: deadbeef");
}

#[test]
fn mark_references_split_into_three_complaints() {
    fatal("notamark", "get-mark x\n", "not a mark: x");
    fatal("novalue", "get-mark :\n", "no value after ':' in mark: get-mark :");
    fatal("garbage", "get-mark :1x\n", "garbage after mark: get-mark :1x");
    fatal(
        "nospace",
        &format!("{PRELUDE}M 100644 :1x f\n"),
        "missing space after mark: M 100644 :1x f",
    );
    // `ls` and `N` take their dataref through the *space* variant too, so a
    // mark with trailing junk there is the same complaint and not `garbage`.
    fatal(
        "lsspace",
        &format!("{PRELUDE}ls :1x \"f\"\n"),
        "missing space after mark: ls :1x \"f\"",
    );
    fatal(
        "notespace",
        &format!("{PRELUDE}N :1x :5\n"),
        "missing space after mark: N :1x :5",
    );
}

#[test]
fn filemodify_diagnostics_name_the_whole_line() {
    fatal("mode", &format!("{PRELUDE}M 100645 :1 f\n"), "corrupt mode: M 100645 :1 f");
    fatal(
        "missingblob",
        &format!("{PRELUDE}M 100644 {UNKNOWN} f\n"),
        &format!("blob not found: M 100644 {UNKNOWN} f"),
    );
    fatal(
        "missingtree",
        &format!("{PRELUDE}M 40000 {UNKNOWN} d\n"),
        &format!("tree not found: M 40000 {UNKNOWN} d"),
    );
    fatal(
        "wrongtype",
        &format!("{PRELUDE}M 40000 :1 d\n"),
        "not a tree (actually a blob): M 40000 :1 d",
    );
    fatal(
        "longsha",
        &format!("{PRELUDE}M 100644 {UNKNOWN}x f\n"),
        &format!("missing space after SHA1: M 100644 {UNKNOWN}x f"),
    );
    // Both `inline` refusals come *before* the `data` line is read, so a stream
    // that omits the payload still gets the refusal and not a parse error.
    fatal(
        "gitlinkinline",
        &format!("{PRELUDE}M 160000 inline f\n"),
        "Git links cannot be specified 'inline': M 160000 inline f",
    );
    fatal(
        "dirinline",
        &format!("{PRELUDE}M 40000 inline f\n"),
        "directories cannot be specified 'inline': M 40000 inline f",
    );
}

#[test]
fn a_mark_used_as_a_commitish_must_be_a_commit() {
    // `merge` is only legal before the file-change list, so it sits inside a
    // commit of its own rather than after `PRELUDE`'s `M` line.
    for (tag, tail) in [
        (
            "merge",
            "commit refs/heads/y\ncommitter A <a@e.com> 100 +0000\ndata 0\nmerge :1\n",
        ),
        ("from", "commit refs/heads/y\ncommitter A <a@e.com> 100 +0000\ndata 0\nfrom :1\n"),
        ("reset", "reset refs/heads/y\nfrom :1\n\n"),
        ("alias", "alias\nmark :9\nto :1\n\n"),
    ] {
        fatal(tag, &format!("blob\nmark :1\ndata 1\nx\n{tail}"), "mark :1 not a commit");
    }

    // `tag` is the one command that takes a mark of any type: it reads the
    // type out of the mark table and tags whatever it finds.
    let r = import(
        "tagblob",
        "blob\nmark :1\ndata 1\nx\ntag t\nfrom :1\ntagger A <a@e.com> 100 +0000\ndata 2\nx\n",
        &[],
    );
    assert_eq!(r.code, 0, "tagging a blob mark failed: {}", r.err);
}

#[test]
fn cat_blob_answers_a_missing_object_on_its_channel() {
    let r = import("catmissing", &format!("cat-blob {UNKNOWN}\n"), &[]);
    assert_eq!(r.out, format!("{UNKNOWN} missing\n"));
    assert_eq!(r.err, "");
    assert_eq!(r.code, 0, "a missing object is not a fatal error");

    let r = import("cattree", &format!("{PRELUDE}\ncat-blob :5\n"), &[]);
    assert!(
        r.err.starts_with("fatal: object ") && r.err.trim_end().ends_with(" is a commit but a blob was expected."),
        "wrong-type message: {:?}",
        r.err
    );
    assert_eq!(r.code, 128);
}

#[test]
fn a_data_count_that_is_not_a_number_is_a_zero_length_payload() {
    // `strtoumax("bogus")` is 0 and git never looks for a conversion error.
    let r = import("baddata", "blob\ndata bogus\n", &[]);
    assert_eq!(r.err, "", "a non-numeric data count is not an error");
    assert_eq!(r.code, 0);

    // The same leniency in `parse_mark()`: `mark :x` declares nothing at all
    // rather than failing, so the blob that follows is simply unmarked.
    let r = import("markx", "blob\nmark :x\ndata 0\n", &[]);
    assert_eq!(r.err, "");
    assert_eq!(r.code, 0);
}

#[test]
fn alias_swallows_the_line_after_its_to_command() {
    let blocks = format!(
        "{PRELUDE}\nalias\nmark :9\nto :5\n\nreset refs/tags/lw\nfrom :9\n\ndone\n"
    );
    let r = import("aliasblank", &blocks, &[]);
    assert_eq!(r.err, "", "blank-separated alias block should import cleanly");
    assert_eq!(r.code, 0);

    // The identical stream without the blank line after `to :5` loses the
    // `reset` command to `parse_objectish()`'s trailing read, which is what
    // stock git does and what a frontend has to be able to reproduce.
    let packed =
        format!("{PRELUDE}alias\nmark :9\nto :5\nreset refs/tags/lw\nfrom :9\ndone\n");
    let r = import("aliaspacked", &packed, &[]);
    assert_eq!(r.err.lines().next(), Some("fatal: unsupported command: from :9"));
    assert_eq!(r.code, 128);
}

#[test]
fn max_pack_size_keeps_its_two_legacy_unit_warnings() {
    let stream = "blob\ndata 0\n";
    for (tag, arg, want) in [
        ("k", "--max-pack-size=1k", "warning: max-pack-size is now in bytes, assuming --max-pack-size=1024m"),
        ("small", "--max-pack-size=100", "warning: max-pack-size is now in bytes, assuming --max-pack-size=100m"),
        ("mib", "--max-pack-size=100000", "warning: minimum max-pack-size is 1 MiB"),
        ("edge", "--max-pack-size=8192", "warning: minimum max-pack-size is 1 MiB"),
    ] {
        let r = import(tag, stream, &[arg]);
        assert_eq!(r.err.lines().next(), Some(want), "for {arg}");
        assert_eq!(r.code, 0);
    }

    // A value at or above 1 MiB warns about nothing.
    let r = import("big", stream, &["--max-pack-size=2000000"]);
    assert_eq!(r.err, "");

    // `parse_one_option()` serves the `option git` spelling from the same arm.
    let r = import("optform", "option git max-pack-size=1k\ndone\n", &[]);
    assert_eq!(
        r.err.lines().next(),
        Some("warning: max-pack-size is now in bytes, assuming --max-pack-size=1024m")
    );
}
