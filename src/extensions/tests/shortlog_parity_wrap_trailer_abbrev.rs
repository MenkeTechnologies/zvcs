//! Three things `git shortlog` does that a naive reading of its output hides:
//! how a `--group=trailer:<tok>` key is built, how the wrap counts columns, and
//! what `--abbrev` reaches.
//!
//! **Trailer keys go through `parse_ident()`.** `insert_records_from_trailers()`
//! does not group on the raw trailer value:
//!
//! ```c
//! strbuf_reset(&ident);
//! if (!parse_ident(log, &ident, value))
//!         value = ident.buf;
//! if (!strset_add(dups, value))
//!         continue;
//! insert_one_record(log, value, oneline);
//! ```
//!
//! (builtin/shortlog.c:199-205.) `parse_ident()` splits `Name <mail>`, runs it
//! through the mailmap, and appends ` <mail>` only when `log->email` is set
//! (:106-116) — so `--group=trailer:Signed-off-by` without `-e` groups under the
//! bare name. Two consequences fall out of that one line: a mailmapped trailer
//! author collapses onto their canonical name, and the `dups` strset now sees
//! the *same* string the author group will produce, so `--group=trailer:…
//! --group=author` files a self-signed commit once rather than twice.
//!
//! **The wrap counts display columns, not code points.** `shortlog_output()`
//! hands each subject to `strbuf_add_wrapped_text()`, whose accumulator advances
//! by `utf8_width(&text, NULL)` (utf8.c:345) — `git_wcwidth()` of the decoded
//! character. A CJK ideograph is two columns, so a line of them reaches the
//! wrap width in half as many characters as a line of ASCII. When the bytes turn
//! out not to be UTF-8 at all, `utf8_width()` nulls the cursor and the whole
//! wrap restarts with `assume_utf8 = 0`, counting one column per byte
//! (utf8.c:344-351).
//!
//! **`--abbrev` is a revision option shortlog forwards.** `cmd_shortlog()` routes
//! every `PARSE_OPT_UNKNOWN` to `parse_revision_opt()` (builtin/shortlog.c:430-445)
//! and then copies `log.abbrev = rev.abbrev` (:461) into the context `%h`, `%t`
//! and `%p` render through. `handle_revision_opt()` clamps the value to
//! `[MINIMUM_ABBREV, hexsz]` and treats `--no-abbrev` as zero, i.e. the whole
//! name (revision.c:2639-2648).
//!
//! Every expectation below was measured from stock git 2.55.0 in an identical
//! throwaway repository under the same pinned environment.
#![cfg(unix)]

use std::path::PathBuf;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

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
    /// A repository whose every commit is authored by `A U Thor` so that the
    /// author group is a single bucket, and whose second commit carries three
    /// trailers: one that is the author's own ident, one that is a different
    /// ident, and one that is not an ident at all.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-sl-parity-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.git(&["init", "-q", "-b", "main", "."]);
        f.commit("f1", b"plain subject\n");
        f.commit(
            "f2",
            b"second subject\n\
              \n\
              Signed-off-by: A U Thor <author@example.com>\n\
              Reviewed-by: Rev Iewer <rev@example.com>\n\
              Tested-by: bare-token-no-ident\n",
        );
        f.commit(
            "f3",
            "unicode 日本語 テキスト padding words here to force the wrap boundary\n".as_bytes(),
        );
        // Deliberately not UTF-8: 0xff 0xfe can start no sequence, which is what
        // drives git's `assume_utf8` retry. It goes in through `commit-tree`,
        // which writes the message bytes verbatim.
        f.commit_raw(
            b"broken \xff\xfe bytes then many more words follow to push this past the wrap width\n",
        );
        f
    }

    fn cmd(&self, args: &[&str]) -> Command {
        let mut c = Command::new(BIN);
        c.args(args)
            .current_dir(&self.work)
            .env("HOME", &self.root)
            .env("ZVCS_HOME", &self.root)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_NAME", "A U Thor")
            .env("GIT_AUTHOR_EMAIL", "author@example.com")
            .env("GIT_COMMITTER_NAME", "C O Mitter")
            .env("GIT_COMMITTER_EMAIL", "committer@example.com")
            .env("GIT_AUTHOR_DATE", "@1577836800 +0000")
            .env("GIT_COMMITTER_DATE", "@1577836800 +0000");
        c
    }

    fn git(&self, args: &[&str]) -> String {
        let out = self.cmd(args).output().unwrap();
        assert!(out.status.success(), "setup: git {args:?}\n{out:?}");
        String::from_utf8_lossy(&out.stdout).trim().to_owned()
    }

    /// Stage `file` and commit `message`, which is read from a file so that a
    /// multi-line message with trailers needs no argv quoting.
    fn commit(&self, file: &str, message: &[u8]) {
        std::fs::write(self.work.join(file), b"x\n").unwrap();
        let msg = self.root.join("msg");
        std::fs::write(&msg, message).unwrap();
        self.git(&["add", file]);
        self.git(&["commit", "-q", "-F", msg.to_str().unwrap()]);
    }

    /// Commit a message that is not valid UTF-8, reusing HEAD's tree.
    ///
    /// Neither `commit -F` nor `commit-tree` can carry those bytes: both run the
    /// message through git's UTF-8 repair before writing. `hash-object -t commit`
    /// stores the object text exactly as given, which is the only way to get a
    /// subject the wrap has to fall back to byte counting for.
    fn commit_raw(&self, message: &[u8]) {
        let tree = self.git(&["rev-parse", "HEAD^{tree}"]);
        let parent = self.git(&["rev-parse", "HEAD"]);
        let mut object = format!(
            "tree {tree}\n\
             parent {parent}\n\
             author A U Thor <author@example.com> 1577836800 +0000\n\
             committer C O Mitter <committer@example.com> 1577836800 +0000\n\
             \n"
        )
        .into_bytes();
        object.extend_from_slice(message);
        let path = self.root.join("raw-commit");
        std::fs::write(&path, &object).unwrap();

        let stdin = std::fs::File::open(&path).unwrap();
        let out = self
            .cmd(&["hash-object", "-t", "commit", "-w", "--stdin"])
            .stdin(std::process::Stdio::from(stdin))
            .output()
            .unwrap();
        assert!(out.status.success(), "setup: hash-object\n{out:?}");
        let id = String::from_utf8_lossy(&out.stdout).trim().to_owned();
        self.git(&["update-ref", "refs/heads/main", &id]);
    }

    /// `shortlog`'s raw stdout — the wrap operates on bytes, and one fixture
    /// subject is deliberately not UTF-8.
    fn shortlog_bytes(&self, args: &[&str]) -> Vec<u8> {
        let mut full = vec!["shortlog"];
        full.extend_from_slice(args);
        let out = self.cmd(&full).output().unwrap();
        assert!(
            out.status.success(),
            "shortlog {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        out.stdout
    }

    fn shortlog(&self, args: &[&str]) -> String {
        String::from_utf8_lossy(&self.shortlog_bytes(args)).into_owned()
    }
}

#[test]
fn trailer_group_key_is_the_parsed_ident_not_the_raw_value() {
    let f = Fixture::new("trailer-ident");

    // `Signed-off-by: A U Thor <author@example.com>` splits, so the key is the
    // name alone — the address is `log->email` territory.
    assert_eq!(
        f.shortlog(&["-s", "--group=trailer:Signed-off-by", "HEAD"]),
        "     1\tA U Thor\n"
    );
    // `-e` is what puts it back (builtin/shortlog.c:113-114).
    assert_eq!(
        f.shortlog(&["-s", "-e", "--group=trailer:Signed-off-by", "HEAD"]),
        "     1\tA U Thor <author@example.com>\n"
    );
    // A value with no `<...>` fails `split_ident_line()`, and `parse_ident()`'s
    // -1 leaves `value` pointing at the raw trailer text.
    assert_eq!(
        f.shortlog(&["-s", "--group=trailer:Tested-by", "HEAD"]),
        "     1\tbare-token-no-ident\n"
    );
}

#[test]
fn trailer_group_key_is_mailmapped() {
    let f = Fixture::new("trailer-mailmap");
    std::fs::write(
        f.work.join(".mailmap"),
        "Proper Name <proper@example.com> <rev@example.com>\n",
    )
    .unwrap();

    // `parse_ident()` calls `map_user()` on the split trailer ident before it
    // ever reaches the group list, so the rewrite lands on both halves.
    assert_eq!(
        f.shortlog(&["-s", "--group=trailer:Reviewed-by", "HEAD"]),
        "     1\tProper Name\n"
    );
    assert_eq!(
        f.shortlog(&["-s", "-e", "--group=trailer:Reviewed-by", "HEAD"]),
        "     1\tProper Name <proper@example.com>\n"
    );
}

#[test]
fn trailer_group_dedups_against_the_author_group() {
    let f = Fixture::new("trailer-dedup");

    // The second commit signs itself off. Its trailer key and its author key are
    // both the bare `A U Thor`, so the per-commit `dups` strset lets exactly one
    // record through and the commit is counted once, not twice — which is only
    // true because the trailer key dropped the address.
    assert_eq!(
        f.shortlog(&[
            "-s",
            "--group=trailer:Signed-off-by",
            "--group=author",
            "HEAD",
        ]),
        "     4\tA U Thor\n"
    );
    // `-e` widens both keys the same way, so they still collapse.
    assert_eq!(
        f.shortlog(&[
            "-s",
            "-e",
            "--group=trailer:Signed-off-by",
            "--group=author",
            "HEAD",
        ]),
        "     4\tA U Thor <author@example.com>\n"
    );
    // A trailer naming somebody else does contribute a second bucket.
    assert_eq!(
        f.shortlog(&["-s", "--group=trailer:Reviewed-by", "--group=author", "HEAD"]),
        "     4\tA U Thor\n     1\tRev Iewer\n"
    );
}

#[test]
fn wrapping_counts_display_columns_and_falls_back_to_bytes() {
    let f = Fixture::new("wrap-width");

    // Width 40, indents 4 and 8. `unicode ` is 8 columns and `日本語 テキスト`
    // is seven double-width glyphs around one space — 15 columns — so the first
    // line is full after `padding`, far earlier than a per-code-point count
    // would break it.
    //
    // The fourth subject is not UTF-8, so its wrap is the `assume_utf8 = 0`
    // retry: one column per byte, which puts `words` on the first line where
    // the two stray bytes would otherwise have cost nothing.
    let want: Vec<u8> = [
        b"A U Thor (4):\n".as_slice(),
        b"    plain subject\n",
        b"    second subject\n",
        "    unicode 日本語 テキスト padding\n".as_bytes(),
        b"        words here to force the wrap\n",
        b"        boundary\n",
        b"    broken \xff\xfe bytes then many more words\n",
        b"        follow to push this past the\n",
        b"        wrap width\n",
        b"\n",
    ]
    .concat();
    let got = f.shortlog_bytes(&["-w40,4,8", "HEAD"]);
    assert_eq!(
        got,
        want,
        "wrapped output:\n{}",
        String::from_utf8_lossy(&got)
    );
}

#[test]
fn abbrev_sets_the_width_percent_h_renders() {
    let f = Fixture::new("abbrev");

    let full: Vec<String> = f
        .shortlog(&["--no-abbrev", "--format=%h", "HEAD"])
        .lines()
        .filter_map(|l| l.strip_prefix("      "))
        .map(str::to_owned)
        .collect();
    assert_eq!(full.len(), 4, "one id per commit: {full:?}");
    assert!(
        full.iter().all(|id| id.len() == 40 && id.chars().all(|c| c.is_ascii_hexdigit())),
        "--no-abbrev zeroes revs->abbrev, which means the whole name: {full:?}"
    );

    for (arg, want) in [("--abbrev=4", 4usize), ("--abbrev=12", 12), ("--abbrev=1", 4)] {
        let short: Vec<String> = f
            .shortlog(&[arg, "--format=%h", "HEAD"])
            .lines()
            .filter_map(|l| l.strip_prefix("      "))
            .map(str::to_owned)
            .collect();
        // `--abbrev=<n>` is a floor clamped at MINIMUM_ABBREV, and each prefix
        // still belongs to the id it came from.
        assert_eq!(short.len(), full.len(), "{arg}: {short:?}");
        for (s, f) in short.iter().zip(&full) {
            assert_eq!(s.len(), want, "{arg} width: {s}");
            assert!(f.starts_with(s), "{arg}: {s} is not a prefix of {f}");
        }
    }

    // Bare `--abbrev` restores git's DEFAULT_ABBREV sentinel, so it renders
    // exactly what no `--abbrev` at all renders.
    assert_eq!(
        f.shortlog(&["--abbrev", "--format=%h", "HEAD"]),
        f.shortlog(&["--format=%h", "HEAD"])
    );

    // `revs->abbrev` is an `unsigned int` filled by `strtoul`: 2^32 truncates to
    // zero and clamps up to MINIMUM_ABBREV, while -1 saturates and clamps down
    // to the full width. Both are silent, unlike `OPT__ABBREV`.
    let truncated = f.shortlog(&["--abbrev=4294967296", "--format=%h", "HEAD"]);
    assert_eq!(truncated, f.shortlog(&["--abbrev=4", "--format=%h", "HEAD"]));
    let saturated = f.shortlog(&["--abbrev=-1", "--format=%h", "HEAD"]);
    assert_eq!(saturated, f.shortlog(&["--no-abbrev", "--format=%h", "HEAD"]));
}
