//! `git hash-object -t <non-blob>` runs `fsck_buffer()`, not a parse.
//!
//! ```c
//! if (flags & INDEX_FORMAT_CHECK) {
//!         struct fsck_options opts;
//!
//!         fsck_options_init(&opts, the_repository, FSCK_OPTIONS_DEFAULT);
//!         opts.strict = 1;
//!         opts.error_func = hash_format_check_report;
//!         if (fsck_buffer(null_oid(istate->repo->hash_algo), type, buf, size, &opts))
//!                 die(_("refusing to create malformed object"));
//!         fsck_finish(&opts);
//! }
//! ```
//!
//! (`index_mem()`, object-file.c:1007-1016.) `INDEX_FORMAT_CHECK` is on unless
//! `--literally` clears it, and `hash_format_check_report()` (object-file.c:974-982)
//! prints `error: object fails fsck: %s`, where `%s` is `fsck_vreport()`'s
//! `<camelCasedId>: <text>`.
//!
//! Two things follow that a plain parse cannot reproduce. First, the message
//! names the fsck id, so a caller can tell `missingTree` from `missingAuthor`;
//! the port printed one `error: object fails check: object parsing failed` for
//! every shape of damage. Second, `hash_format_check_report()` returns 1 for any
//! severity that is not `FSCK_IGNORE`, so even a check `git fsck` would only warn
//! about is fatal here.
//!
//! `git hash-object -t tag` and `git mktag` are the only two entry points that
//! fsck a raw *tag* buffer, and `-t commit` the only one that fscks a raw commit:
//! every walking caller parses first and answers `object could not be parsed` /
//! `invalid commit` instead. So the ids for a missing `tree`, `object`, `type` or
//! `tag` header are reachable here and nowhere else.
//!
//! Every expectation here was captured from stock git 2.55.0.
#![cfg(unix)]

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_git");

/// A placeholder that is a well-formed object id but names nothing; the checks
/// under test are all about the *shape* of the header, never about what resolves.
const NULL_OID: &str = "0000000000000000000000000000000000000000";

struct Fixture {
    root: PathBuf,
    work: PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

impl Fixture {
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-hofsck-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let fx = Fixture { root, work };
        fx.ok(&["init", "-q", "-b", "main", "."]);
        fx
    }

    fn feed(&self, args: &[&str], stdin: &[u8]) -> Output {
        let mut child = Command::new(BIN)
            .args(args)
            .current_dir(&self.work)
            .env("HOME", &self.root)
            .env("ZVCS_HOME", &self.root)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", self.root.join("gitconfig"))
            .env("GIT_CONFIG_SYSTEM", self.root.join("gitconfig-system"))
            .env("LC_ALL", "C")
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("run binary");
        child.stdin.take().unwrap().write_all(stdin).unwrap();
        child.wait_with_output().unwrap()
    }

    fn ok(&self, args: &[&str]) -> Output {
        let out = self.feed(args, b"");
        assert!(out.status.success(), "setup git {args:?}: {out:?}");
        out
    }

    /// Hash `body` as `kind`, feeding it on stdin so nothing touches the worktree.
    fn hash(&self, kind: &str, body: &str) -> Output {
        self.feed(&["hash-object", "-t", kind, "--stdin"], body.as_bytes())
    }
}

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

/// The two lines a rejected buffer prints, in order.
fn refusal(id: &str, text: &str) -> String {
    format!("error: object fails fsck: {id}: {text}\nfatal: refusing to create malformed object\n")
}

#[test]
fn a_malformed_commit_names_the_fsck_id_that_rejected_it() {
    let fx = Fixture::new("commit");

    for (body, id, text) in [
        // `fsck_commit()` never runs: `verify_headers()` stops first.
        ("no-headers-at-all", "unterminatedHeader", "unterminated header"),
        // fsck.c:969-970, reachable only because nothing parsed the buffer first.
        ("notree\n\nmsg\n", "missingTree", "invalid format - expected 'tree' line"),
        // fsck.c:971-975.
        ("tree zz\n\nmsg\n", "badTreeSha1", "invalid 'tree' line format - bad sha1"),
        // fsck.c:986-990.
        ("tree 0\n\nmsg\n", "missingAuthor", "invalid format - expected 'author' line"),
    ] {
        let body = body.replace("tree 0\n", &format!("tree {NULL_OID}\n"));
        let out = fx.hash("commit", &body);
        assert_eq!(out.status.code(), Some(128), "{id}: {out:?}");
        assert_eq!(stderr(&out), refusal(id, text), "{id}");
        assert_eq!(stdout(&out), "", "{id} still printed an id");
    }
}

#[test]
fn a_malformed_tag_names_the_fsck_id_that_rejected_it() {
    let fx = Fixture::new("tag");

    for (body, id, text) in [
        // fsck.c:1041-1044.
        ("notobject\n\nmsg\n", "missingObject", "invalid format - expected 'object' line"),
        // fsck.c:1045-1049.
        ("object zz\ntype commit\ntag t\n", "badObjectSha1", "invalid 'object' line format - bad sha1"),
        // fsck.c:1052-1055.
        ("object 0\n", "missingTypeEntry", "invalid format - expected 'type' line"),
        // fsck.c:1061-1065; `type_from_string_gently()` knows four names.
        ("object 0\ntype bogus\ntag t\n", "badType", "invalid 'type' value"),
        // fsck.c:1068-1071.
        ("object 0\ntype commit\n", "missingTagEntry", "invalid format - expected 'tag' line"),
        // fsck.c:1078-1085: `check_refname_format("refs/tags/.bad")` refuses a
        // component that starts with a dot.
        (
            "object 0\ntype commit\ntag .bad\ntagger A <a@e.co> 1 +0000\n",
            "badTagName",
            "invalid 'tag' name: .bad",
        ),
    ] {
        let body = body.replace("object 0\n", &format!("object {NULL_OID}\n"));
        let out = fx.hash("tag", &body);
        assert_eq!(out.status.code(), Some(128), "{id}: {out:?}");
        assert_eq!(stderr(&out), refusal(id, text), "{id}");
        assert_eq!(stdout(&out), "", "{id} still printed an id");
    }
}

#[test]
fn a_tree_reports_the_decoders_own_line_before_the_fsck_id() {
    let fx = Fixture::new("tree");

    // `init_tree_desc_gently()` calls `error()` itself, without a msg-id, and
    // `fsck_tree()` then reports `badTree` — two lines, decoder first.
    let out = fx.hash("tree", "garbage");
    assert_eq!(out.status.code(), Some(128), "{out:?}");
    assert_eq!(
        stderr(&out),
        format!(
            "error: too-short tree object\n{}",
            refusal("badTree", "cannot be parsed as a tree")
        )
    );
    assert_eq!(stdout(&out), "", "an id was printed for a broken tree");
}

#[test]
fn literally_skips_the_check_entirely() {
    let fx = Fixture::new("literally");

    // `OPT_NEGBIT(0, "literally", &flags, ..., INDEX_FORMAT_CHECK)`: the flag
    // *clears* the bit, so the same buffers above hash silently. The id itself is
    // never asserted on — only that one was produced and nothing was said.
    for (kind, body) in [("commit", "notree\n"), ("tag", "notobject\n"), ("tree", "garbage")] {
        let out = fx.feed(
            &["hash-object", "-t", kind, "--literally", "--stdin"],
            body.as_bytes(),
        );
        assert_eq!(out.status.code(), Some(0), "{kind}: {out:?}");
        assert_eq!(stderr(&out), "", "{kind} complained under --literally");
        assert_eq!(stdout(&out).trim_end().len(), 40, "{kind}: {out:?}");
    }
}

#[test]
fn a_well_formed_object_of_each_type_still_hashes() {
    let fx = Fixture::new("good");

    // The guard must only fire on real damage: round-trip the three non-blob
    // objects a normal repository produces and confirm each hashes back to the
    // id it already has.
    std::fs::write(fx.work.join("f.txt"), "hello\n").unwrap();
    fx.ok(&["add", "f.txt"]);
    fx.ok(&["commit", "-q", "-m", "one"]);
    fx.ok(&["tag", "-a", "-m", "annotated", "v1"]);

    for (kind, rev) in [("commit", "HEAD"), ("tree", "HEAD^{tree}"), ("tag", "v1")] {
        let want = String::from_utf8_lossy(&fx.ok(&["rev-parse", rev]).stdout)
            .trim_end()
            .to_string();
        let body = fx.ok(&["cat-file", kind, rev]).stdout;
        let out = fx.feed(&["hash-object", "-t", kind, "--stdin"], &body);
        assert_eq!(out.status.code(), Some(0), "{kind}: {out:?}");
        assert_eq!(stderr(&out), "", "{kind} was rejected: {out:?}");
        assert_eq!(stdout(&out).trim_end(), want, "{kind} hashed to the wrong id");
    }
}
