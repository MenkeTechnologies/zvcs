//! `git archive` and the two `.gitattributes` knobs that decide what a blob's
//! bytes look like inside the archive: `export-subst` and `diff=<name>`.
//!
//!   * `export-subst` makes `object_file_to_archive()` run `format_subst()` over
//!     the *converted* blob, replacing each `$Format:<pretty>$` with that pretty
//!     format rendered against the archived commit (archive.c:52-83,107-109).
//!     The substitution only happens when the tree-ish peeled to a commit —
//!     `const struct commit *commit = args->convert ? args->commit : NULL`
//!     (archive.c:93) — so archiving a raw tree leaves the marker alone.
//!   * An unterminated `$Format:` breaks the loop with `src` still pointing
//!     before the marker, so the rest of the file is copied out verbatim.
//!   * `%h` is abbreviated, not spelled out: `write_archive()` builds its
//!     pretty context with `ctx.abbrev = DEFAULT_ABBREV` (archive.c:769) rather
//!     than the zeroed `abbrev` that makes `odb_find_abbrev_len()` answer with
//!     the full hex length (odb.c:957-961).
//!   * `entry_is_binary()` (archive-zip.c) asks `userdiff_find_by_path()`, and a
//!     `diff=<name>` whose `diff.<name>.binary` is configured decides without the
//!     content being looked at (userdiff.c:442-448,478-479) — so a text file
//!     under `diff=custom` + `diff.custom.binary=true` is stored with zip's
//!     "text" bit clear and survives `unzip -a` unchanged.
//!
//! Every expectation was read off stock git 2.55.0 in the same throwaway
//! repository, under the same pinned environment.
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
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-ar-subst-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.git(&["init", "-q", "-b", "main", "."]);
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
            .env("GIT_AUTHOR_NAME", "A U Thor")
            .env("GIT_AUTHOR_EMAIL", "author@example.com")
            .env("GIT_COMMITTER_NAME", "C O Mitter")
            .env("GIT_COMMITTER_EMAIL", "committer@example.com")
            .env("GIT_AUTHOR_DATE", "1700000000 +0000")
            .env("GIT_COMMITTER_DATE", "1700000000 +0000")
            .env("LC_ALL", "C")
            .env("TZ", "UTC")
            .env("TERM", "dumb")
            .env("NO_COLOR", "1")
            .env("GIT_PAGER", "cat");
        c
    }

    fn git(&self, args: &[&str]) {
        let out = self.cmd(args).output().unwrap();
        assert!(out.status.success(), "`git {args:?}` failed: {out:?}");
    }

    fn stdout_bytes(&self, args: &[&str]) -> Vec<u8> {
        let out = self.cmd(args).output().unwrap();
        assert!(
            out.status.success(),
            "`git {args:?}` failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        out.stdout
    }

    fn stdout(&self, args: &[&str]) -> String {
        String::from_utf8(self.stdout_bytes(args)).unwrap()
    }

    fn write(&self, path: &str, content: &str) {
        std::fs::write(self.work.join(path), content).unwrap();
    }
}

/// Pull one file's bytes out of a `ustar` stream: 512-byte header blocks whose
/// name field opens the record, followed by the payload rounded up to 512.
fn tar_entry(tar: &[u8], want: &str) -> Option<Vec<u8>> {
    let mut at = 0usize;
    while at + 512 <= tar.len() {
        let header = &tar[at..at + 512];
        if header.iter().all(|&b| b == 0) {
            return None;
        }
        let name_end = header[..100].iter().position(|&b| b == 0).unwrap_or(100);
        let name = String::from_utf8_lossy(&header[..name_end]).into_owned();
        let size_field = String::from_utf8_lossy(&header[124..135]).trim_end().to_string();
        let size = usize::from_str_radix(size_field.trim_end_matches('\0').trim(), 8).unwrap_or(0);
        at += 512;
        if name == want {
            return Some(tar[at..at + size].to_vec());
        }
        at += size.div_ceil(512) * 512;
    }
    None
}

/// `$Format:%H%n$` becomes the archived commit's full hash plus a newline, and
/// the bytes on either side of the marker are kept. Archiving the *tree* rather
/// than the commit leaves the marker untouched, because `args->commit` is NULL.
#[test]
fn archive_export_subst_expands_only_against_a_commit() {
    let f = Fixture::new("commit");
    f.write("subst.txt", "A$Format:%H%n$O");
    f.write("plain.txt", "A$Format:%H%n$O");
    std::fs::create_dir_all(f.work.join(".git/info")).unwrap();
    f.write(".git/info/attributes", "subst.txt export-subst\n");
    f.git(&["add", "subst.txt", "plain.txt"]);
    f.git(&["commit", "-q", "-m", "one"]);

    let head = f.stdout(&["rev-parse", "HEAD"]).trim().to_string();
    let tar = f.stdout_bytes(&["archive", "--format=tar", "HEAD"]);
    assert_eq!(
        tar_entry(&tar, "subst.txt").expect("subst.txt in archive"),
        format!("A{head}\nO").into_bytes()
    );
    // No `export-subst` attribute: the marker is content, not a directive.
    assert_eq!(
        tar_entry(&tar, "plain.txt").expect("plain.txt in archive"),
        b"A$Format:%H%n$O".to_vec()
    );

    // `git archive HEAD^{tree}` leaves `args->commit` NULL, so nothing expands.
    let tar = f.stdout_bytes(&["archive", "--format=tar", "HEAD^{tree}"]);
    assert_eq!(
        tar_entry(&tar, "subst.txt").expect("subst.txt in tree archive"),
        b"A$Format:%H%n$O".to_vec()
    );
}

/// `%h` uses `DEFAULT_ABBREV`, an unterminated `$Format:` is left verbatim, and
/// a second marker on the same line is expanded too.
#[test]
fn archive_export_subst_abbreviates_and_tolerates_an_unclosed_marker() {
    let f = Fixture::new("abbrev");
    f.write("a.txt", "[$Format:%h$][$Format:%s$]\n$Format:%H no close\n");
    std::fs::create_dir_all(f.work.join(".git/info")).unwrap();
    f.write(".git/info/attributes", "a.txt export-subst\n");
    f.git(&["add", "a.txt"]);
    f.git(&["commit", "-q", "-m", "subject line"]);

    let head = f.stdout(&["rev-parse", "HEAD"]).trim().to_string();
    let short = f.stdout(&["rev-parse", "--short", "HEAD"]).trim().to_string();
    assert!(short.len() < head.len(), "--short must abbreviate: {short}");

    let tar = f.stdout_bytes(&["archive", "--format=tar", "HEAD"]);
    let got = tar_entry(&tar, "a.txt").expect("a.txt in archive");
    assert_eq!(
        String::from_utf8(got).unwrap(),
        format!("[{short}][subject line]\n$Format:%H no close\n")
    );
}

/// `diff.<name>.binary = true` makes zip store the entry as binary although its
/// bytes are plain text; `= auto` leaves the driver at -1 and the content
/// decides. The "text" bit is the low bit of the internal-attributes field in
/// the local file header's central-directory twin, so read it back through
/// `git archive`'s own output rather than through an external unzip.
#[test]
fn archive_zip_honours_configured_userdiff_binary() {
    let f = Fixture::new("zipbin");
    f.write("custom.txt", "text\r\n");
    f.write("auto.txt", "text\r\n");
    std::fs::create_dir_all(f.work.join(".git/info")).unwrap();
    f.write(
        ".git/info/attributes",
        "custom.txt diff=custom\nauto.txt diff=autodrv\n",
    );
    f.git(&["add", "custom.txt", "auto.txt"]);
    f.git(&["commit", "-q", "-m", "one"]);
    f.git(&["config", "diff.custom.binary", "true"]);
    f.git(&["config", "diff.autodrv.binary", "auto"]);

    let zip = f.stdout_bytes(&["archive", "--format=zip", "HEAD"]);
    assert_eq!(internal_attrs(&zip, "custom.txt"), 0, "diff.custom.binary=true");
    assert_eq!(internal_attrs(&zip, "auto.txt"), 1, "diff.autodrv.binary=auto");
}

/// The internal-attributes field of `name`'s central-directory record: bit 0 is
/// zip's "apparently a text file" flag, which `write_zip_entry()` sets from
/// `!entry_is_binary()`.
fn internal_attrs(zip: &[u8], name: &str) -> u16 {
    const CENTRAL: &[u8] = b"PK\x01\x02";
    let mut at = 0usize;
    while at + 46 <= zip.len() {
        let Some(rel) = zip[at..].windows(4).position(|w| w == CENTRAL) else {
            break;
        };
        let rec = at + rel;
        let name_len = u16::from_le_bytes([zip[rec + 28], zip[rec + 29]]) as usize;
        let extra_len = u16::from_le_bytes([zip[rec + 30], zip[rec + 31]]) as usize;
        let comment_len = u16::from_le_bytes([zip[rec + 32], zip[rec + 33]]) as usize;
        let entry = String::from_utf8_lossy(&zip[rec + 46..rec + 46 + name_len]).into_owned();
        if entry == name {
            return u16::from_le_bytes([zip[rec + 36], zip[rec + 37]]);
        }
        at = rec + 46 + name_len + extra_len + comment_len;
    }
    panic!("{name} not found in zip central directory");
}
