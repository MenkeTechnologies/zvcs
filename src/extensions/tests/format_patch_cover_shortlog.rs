//! The shortlog block `--cover-letter` embeds.
//!
//! `generate_shortlog_cover_letter()` (builtin/log.c:1349-1366) drives
//! `builtin/shortlog.c` with `wrap_lines`/`wrap`/`in1`/`in2` set to `1`/`72`/`2`/`4`
//! and `SHORTLOG_GROUP_AUTHOR`, so the block is not a flat two-space-indented list:
//!
//! * Group order is whatever `string_list_insert()` (builtin/shortlog.c:63) keeps,
//!   which is sorted by the ident string. `shortlog_init()` zeroes `sort_by_number`,
//!   so `shortlog_output()` (builtin/shortlog.c:497-499) never re-sorts by count.
//! * The ident is `%aN` (builtin/shortlog.c:374), and `%aN` always runs the mailmap
//!   (pretty.c:806-807), independently of `--use-mailmap`/`log.mailmap`.
//! * A commit whose `%s` is empty is listed as `<none>` (builtin/shortlog.c:260).
//! * Each subject goes through `strbuf_add_wrapped_text()` (builtin/shortlog.c:488),
//!   indenting the first line by two columns and every continuation by four.
//!
//! Expectations measured against stock git 2.55.0.
#![cfg(unix)]

use std::path::PathBuf;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

/// A pinned-identity repository whose commits are spread over two authors.
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
        let root = std::env::temp_dir().join(format!("zvcs-fpshortlog-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.git(&["init", "-q", "-b", "main", "."]);
        f.git(&["config", "user.name", "C O Mitter"]);
        f.git(&["config", "user.email", "committer@example.com"]);
        f
    }

    fn cmd(&self, args: &[&str]) -> Command {
        let mut c = Command::new(BIN);
        c.args(args)
            .current_dir(&self.work)
            .env("HOME", &self.root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_DATE", "2005-04-07T15:13:13-0700")
            .env("GIT_COMMITTER_DATE", "2005-04-07T15:13:13-0700")
            .env("GIT_COMMITTER_NAME", "C O Mitter")
            .env("GIT_COMMITTER_EMAIL", "committer@example.com");
        c
    }

    fn git(&self, args: &[&str]) {
        let out = self.cmd(args).output().unwrap();
        assert!(out.status.success(), "setup `git {args:?}` failed: {out:?}");
    }

    /// One commit attributed to `author`, whose subject is `subject` verbatim —
    /// including the empty one, which needs `--allow-empty-message`.
    fn commit(&self, author: &str, email: &str, subject: &str) {
        let n = self.work.read_dir().map(|d| d.count()).unwrap_or(0);
        std::fs::write(self.work.join(format!("f{n}.txt")), "x\n").unwrap();
        self.git(&["add", "-A"]);
        let out = self
            .cmd(&["commit", "-q", "--allow-empty-message", "-m", subject])
            .env("GIT_AUTHOR_NAME", author)
            .env("GIT_AUTHOR_EMAIL", email)
            .output()
            .unwrap();
        assert!(out.status.success(), "commit failed: {out:?}");
    }

    /// The cover letter's shortlog: everything from the first `Name (n):` header
    /// up to the diffstat that follows it.
    fn shortlog(&self, n: &str) -> String {
        let out = self
            .cmd(&["format-patch", "--stdout", n, "--cover-letter"])
            .output()
            .unwrap();
        assert!(out.status.success(), "{out:?}");
        let body = String::from_utf8(out.stdout).unwrap();
        let start = body
            .find("\n\n*** BLURB HERE ***\n\n")
            .map(|at| at + "\n\n*** BLURB HERE ***\n\n".len())
            .expect("the cover letter carries the placeholder blurb");
        let rest = &body[start..];
        // The diffstat that follows opens with ` <path> | <n> <marks>`; a wrapped
        // shortlog continuation is indented too, so the ` | ` column is what
        // separates them.
        let mut shortlog = String::new();
        for line in rest.split_inclusive('\n') {
            if line.starts_with(' ') && line.contains(" | ") {
                return shortlog;
            }
            shortlog.push_str(line);
        }
        panic!("the diffstat follows the shortlog: {body}")
    }
}

/// Groups come out ordered by author name, never by how many commits an author
/// has, because the cover letter leaves `sort_by_number` zeroed.
#[test]
fn shortlog_groups_are_ordered_by_author_name_not_by_commit_count() {
    let f = Fixture::new("order");
    f.commit("Zed Zulu", "zed@example.com", "root");
    f.commit("Zed Zulu", "zed@example.com", "zed one");
    f.commit("Zed Zulu", "zed@example.com", "zed two");
    f.commit("Zed Zulu", "zed@example.com", "zed three");
    f.commit("Alice Apple", "alice@example.com", "alice only");

    // Alice leads with a single commit; sorting by count would put Zed first.
    assert_eq!(
        f.shortlog("-4"),
        "Alice Apple (1):\n  alice only\n\nZed Zulu (3):\n  zed one\n  zed two\n  zed three\n\n"
    );
}

/// A subject longer than the 72-column mail width is wrapped, the continuation
/// indented by four rather than two. An empty subject is listed as `<none>`.
#[test]
fn shortlog_wraps_long_subjects_and_names_an_empty_one() {
    let f = Fixture::new("wrap");
    f.commit("Zed Zulu", "zed@example.com", "root");
    f.commit(
        "Zed Zulu",
        "zed@example.com",
        "wrap this subject across the mail width because it is much longer than \
         seventy two columns in total",
    );
    f.commit("Zed Zulu", "zed@example.com", "");
    f.commit("Zed Zulu", "zed@example.com", "plain one");

    assert_eq!(
        f.shortlog("-3"),
        "Zed Zulu (3):\n  \
         wrap this subject across the mail width because it is much longer than\n    \
         seventy two columns in total\n  \
         <none>\n  \
         plain one\n\n"
    );
}

/// The wrapper measures display columns, not bytes: an accented Latin letter
/// costs one column for its two bytes, a CJK ideograph two columns for its
/// three (`utf8_width()`, utf8.c:344-345).
#[test]
fn shortlog_wrapping_counts_display_columns_not_bytes() {
    let f = Fixture::new("utf8");
    f.commit("Zed Zulu", "zed@example.com", "root");
    // 63 columns of `é` before ` tail words here` — under 72 as columns, well
    // over it as bytes.
    f.commit(
        "Zed Zulu",
        "zed@example.com",
        "ééééééééé ééééééééé ééééééééé ééééééééé ééééééééé ééééééééé ééé tail words here",
    );
    // Each ideograph is three bytes wide and two columns wide.
    f.commit(
        "Zed Zulu",
        "zed@example.com",
        "漢字漢字漢字漢字漢字 漢字漢字漢字漢字漢字 漢字漢字漢字漢字漢字 漢字漢字 tail words",
    );

    assert_eq!(
        f.shortlog("-2"),
        "Zed Zulu (2):\n  \
         ééééééééé ééééééééé ééééééééé ééééééééé ééééééééé ééééééééé ééé tail\n    \
         words here\n  \
         漢字漢字漢字漢字漢字 漢字漢字漢字漢字漢字 漢字漢字漢字漢字漢字\n    \
         漢字漢字 tail words\n\n"
    );
}

/// The `From:` and `Subject:` headers wrap through the same measurement. They
/// only carry raw UTF-8 when the RFC2047 encoding is turned off, which is where
/// counting bytes instead of columns shows up.
#[test]
fn unencoded_headers_wrap_on_display_columns() {
    let f = Fixture::new("hdrs");
    let out = f
        .cmd(&["init", "-q", "-b", "main", "."])
        .output()
        .unwrap();
    assert!(out.status.success(), "{out:?}");
    std::fs::write(f.work.join("f.txt"), "x\n").unwrap();
    f.git(&["add", "-A"]);
    let out = f
        .cmd(&[
            "commit",
            "-q",
            "-m",
            "Sübjéct thät ïs quïte lông ãnd hãs mãny àccénted lettérs sö it wrãps \
             àcröss thé heãder wïdth",
        ])
        .env(
            "GIT_AUTHOR_NAME",
            "Ünïcödé Nàmé Thät Ïs Vèry Lông Ïndéed Fôr Wrãpping Pürpöses Hère",
        )
        .env("GIT_AUTHOR_EMAIL", "u@e.x")
        .output()
        .unwrap();
    assert!(out.status.success(), "{out:?}");

    let out = f
        .cmd(&["format-patch", "--stdout", "-1", "--no-encode-email-headers"])
        .output()
        .unwrap();
    assert!(out.status.success(), "{out:?}");
    let page = String::from_utf8(out.stdout).unwrap();

    assert!(
        page.contains(
            "From: Ünïcödé Nàmé Thät Ïs Vèry Lông Ïndéed Fôr Wrãpping Pürpöses Hère\n <u@e.x>\n"
        ),
        "{page}"
    );
    assert!(
        page.contains(
            "Subject: [PATCH] Sübjéct thät ïs quïte lông ãnd hãs mãny àccénted lettérs sö\n \
             it wrãps àcröss thé heãder wïdth\n"
        ),
        "{page}"
    );
}

/// The group header is `%aN`, so a `.mailmap` rewrites it — with no
/// `--use-mailmap` and no `log.mailmap` in play.
#[test]
fn shortlog_group_names_go_through_the_mailmap() {
    let f = Fixture::new("mailmap");
    f.commit("Zed Zulu", "zed@example.com", "root");
    f.commit("Zed Zulu", "zed@example.com", "zed one");
    f.commit("Alice Apple", "alice@example.com", "alice only");
    std::fs::write(f.work.join(".mailmap"), "Mapped Zed <zed@example.com>\n").unwrap();
    f.commit("Zed Zulu", "zed@example.com", "add mailmap");

    // `Mapped Zed` also re-sorts the groups: the raw `Zed Zulu` would still sort
    // after `Alice Apple`, so the name alone would not prove the mapping ran.
    assert_eq!(
        f.shortlog("-3"),
        "Alice Apple (1):\n  alice only\n\nMapped Zed (2):\n  zed one\n  add mailmap\n\n"
    );
}
