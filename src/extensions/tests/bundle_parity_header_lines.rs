//! How `git bundle` reads the header lines between the signature and the pack.
//!
//! `read_bundle_header_fd()` (bundle.c:77-151) reads lines with
//! `strbuf_getwholeline_fd()`, which answers `EOF` for an unterminated last
//! line (strbuf.c:762-776), and stops quietly at EOF as at the empty line.
//! Each line is `strbuf_rtrim()`med, then must be a v3 `@capability`, a
//! `-<oid>[ <subject>]` prerequisite or an `<oid><space><refname>` tip; anything
//! else is `error: unrecognized header: <line> (<len>)`, and an unknown
//! `@object-format=` is `unrecognized bundle hash algorithm: <name>`
//! (`parse_capability()`, :47-62) — each exit 1. zvcs died
//! `malformed bundle header …` (exit 128) for all of these, demanded the empty
//! line, and read an unterminated last line as a ref.
//!
//! Expectations measured from stock git 2.55.0 under the same environment.

use std::path::PathBuf;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

struct Fixture {
    root: PathBuf,
    work: PathBuf,
    head: String,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

impl Fixture {
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir()
            .join(format!("zvcs-bundle-header-lines-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let mut f = Fixture { root, work, head: String::new() };
        f.run(&["init", "-q", "-b", "main", "."]);
        std::fs::write(f.work.join("a"), "a\n").unwrap();
        f.run(&["add", "a"]);
        f.run(&["commit", "-q", "-m", "one"]);
        f.head = f.run(&["rev-parse", "HEAD"]).0.trim_end().to_string();
        f
    }

    fn run(&self, args: &[&str]) -> (String, String, i32) {
        let out = Command::new(BIN)
            .args(args)
            .current_dir(&self.work)
            .env("HOME", &self.root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_NAME", "A U Thor")
            .env("GIT_AUTHOR_EMAIL", "author@example.com")
            .env("GIT_COMMITTER_NAME", "C O Mitter")
            .env("GIT_COMMITTER_EMAIL", "committer@example.com")
            .env("GIT_AUTHOR_DATE", "1700000000 +0000")
            .env("GIT_COMMITTER_DATE", "1700000000 +0000")
            .env("LC_ALL", "C")
            .env("TZ", "UTC")
            .output()
            .unwrap();
        (
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
            out.status.code().expect("no signal"),
        )
    }

    /// Write `../<name>` and run `bundle <verb>` on it.
    fn bundle(&self, verb: &str, name: &str, body: &str) -> (String, String, i32) {
        std::fs::write(self.root.join(name), body).unwrap();
        self.run(&["bundle", verb, &format!("../{name}")])
    }
}

#[test]
fn malformed_lines_are_unrecognized_headers() {
    let f = Fixture::new("unrecognized");
    let h = &f.head;
    for (body, line) in [
        ("# v2 git bundle\n1234 refs/heads/x\n\n".to_string(), "1234 refs/heads/x (17)".to_string()),
        (format!("# v2 git bundle\n{h}refs/heads/x\n\n"), format!("{h}refs/heads/x (52)")),
        (format!("# v2 git bundle\n{h}\n\n"), format!("{h} (40)")),
        (format!("# v2 git bundle\n-{h}x\n\n"), format!("-{h}x (41)")),
        // A capability line in a v2 bundle is just another bad line.
        (format!("# v2 git bundle\n@object-format=sha1\n{h} refs/heads/x\n\n"), "@object-format=sha1 (19)".to_string()),
        // A sha1 id is too short for a sha256 bundle.
        (format!("# v3 git bundle\n@object-format=sha256\n{h} refs/heads/x\n\n"), format!("{h} refs/heads/x (53)")),
    ] {
        let want = format!("error: unrecognized header: {line}\n");
        assert_eq!(f.bundle("list-heads", "b", &body), (String::new(), want, 1), "{body:?}");
    }
}

#[test]
fn an_unknown_hash_algorithm_is_named() {
    let f = Fixture::new("algo");
    assert_eq!(
        f.bundle("verify", "b", "# v3 git bundle\n@object-format=md5\n\n"),
        (String::new(), "error: unrecognized bundle hash algorithm: md5\n".to_string(), 1)
    );
}

#[test]
fn eof_ends_the_header_and_drops_an_unterminated_line() {
    let f = Fixture::new("eof");
    let h = &f.head;
    for body in ["# v2 git bundle\n".to_string(), "# v3 git bundle\n".to_string()] {
        assert_eq!(
            f.bundle("verify", "b", &body),
            (
                "The bundle contains these 0 refs:\n\
                 The bundle records a complete history.\n\
                 The bundle uses this hash algorithm: sha1\n"
                    .to_string(),
                "../b is okay\n".to_string(),
                0
            ),
            "{body:?}"
        );
    }
    assert_eq!(
        f.bundle("list-heads", "b", &format!("# v2 git bundle\n{h} refs/heads/x")),
        (String::new(), String::new(), 0)
    );
}

#[test]
fn a_prerequisite_subject_and_trailing_blanks_are_accepted() {
    let f = Fixture::new("accepted");
    let h = &f.head;
    let body = format!("# v2 git bundle\n-{h} subject line\n{h}\trefs/heads/tab   \n\n");
    assert_eq!(
        f.bundle("list-heads", "b", &body),
        (format!("{h} refs/heads/tab\n"), String::new(), 0)
    );
}
