//! The C-style quoting `git archive --add-virtual-file=<name>:<content>` uses
//! for the name, and specifically its octal escape.
//!
//! `archive.c:592-606` hands a name that opens with `"` to `unquote_c_style()`,
//! whose octal arm is deliberately narrow:
//!
//! ```c
//! /* octal values with first digit over 4 overflow */
//! case '0': case '1': case '2': case '3':
//!         ac = ((ch - '0') << 6);
//!         if ((ch = *quoted++) < '0' || '7' < ch)
//!                 goto error;
//!         ac |= ((ch - '0') << 3);
//!         if ((ch = *quoted++) < '0' || '7' < ch)
//!                 goto error;
//!         ac |= (ch - '0');
//! ```
//!
//! (quote.c:422-432.) Exactly three digits, and only `0`-`3` may lead. A decoder
//! that takes one or two digits, or wraps a leading `4`-`7` into a byte, accepts
//! names git refuses and — worse — silently writes a *different* name into the
//! archive than the one the caller spelled.
//!
//! Every expectation was measured from stock git 2.55.0 before it was written
//! down. The measurements, verbatim (`name` is the last `tar tf` line):
//!
//! ```text
//! $ git archive --format=tar '--add-virtual-file="\401":x' HEAD
//! fatal: unclosed quote: '"\401":x'            rc 128
//! $ git archive --format=tar '--add-virtual-file="\12":x'  HEAD
//! fatal: unclosed quote: '"\12":x'             rc 128
//! $ git archive --format=tar '--add-virtual-file="\1":x'   HEAD
//! fatal: unclosed quote: '"\1":x'              rc 128
//! $ git archive --format=tar '--add-virtual-file="\8":x'   HEAD
//! fatal: unclosed quote: '"\8":x'              rc 128
//! $ git archive --format=tar '--add-virtual-file="\101":x' HEAD   -> name A
//! $ git archive --format=tar '--add-virtual-file="\377":x' HEAD   -> name \377
//! $ git archive --format=tar '--add-virtual-file="a\tb":x' HEAD   -> name a<TAB>b
//! $ git archive --format=tar '--add-virtual-file=plain:x'  HEAD   -> name plain
//! ```

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn run(dir: &Path, home: &Path, args: &[&str]) -> Output {
    Command::new(BIN)
        .args(args)
        .current_dir(dir)
        .env("HOME", home)
        .env("ZVCS_HOME", home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("LC_ALL", "C")
        .env("TZ", "UTC0")
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@e.x")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@e.x")
        .env("GIT_AUTHOR_DATE", "1700000000 +0000")
        .env("GIT_COMMITTER_DATE", "1700000000 +0000")
        .output()
        .expect("run binary")
}

fn fixture() -> (PathBuf, PathBuf, PathBuf) {
    let root = std::env::temp_dir().join(format!("zvcs-avf-quote-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let repo = root.join("repo");
    let home = root.join("home");
    std::fs::create_dir_all(&repo).unwrap();
    std::fs::create_dir_all(&home).unwrap();
    let root = root.canonicalize().unwrap();
    let repo = repo.canonicalize().unwrap();
    let home = home.canonicalize().unwrap();
    for args in [
        &["init", "-q", "-b", "main"][..],
        &["add", "-A"][..],
        &["commit", "-q", "-m", "c1", "--allow-empty"][..],
    ] {
        let o = run(&repo, &home, args);
        assert!(o.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&o.stderr));
    }
    (root, repo, home)
}

/// The names in a tar stream, read straight out of the 512-byte headers so the
/// test does not depend on a `tar(1)` being present or on its quoting.
fn tar_names(tar: &[u8]) -> Vec<Vec<u8>> {
    let mut names = Vec::new();
    let mut at = 0;
    while at + 512 <= tar.len() {
        let block = &tar[at..at + 512];
        if block.iter().all(|&b| b == 0) {
            break;
        }
        let name: Vec<u8> = block[..100].iter().copied().take_while(|&b| b != 0).collect();
        let size = std::str::from_utf8(&block[124..135])
            .ok()
            .and_then(|s| usize::from_str_radix(s.trim_end_matches([' ', '\0']).trim(), 8).ok())
            .unwrap_or(0);
        // A `g`-type pax global header carries the commit id, not a path.
        if block[156] != b'g' {
            names.push(name);
        }
        at += 512 + size.div_ceil(512) * 512;
    }
    names
}

#[test]
fn add_virtual_file_takes_only_gits_three_digit_octal() {
    let (root, repo, home) = fixture();

    // Rejected: a leading digit above 3 overflows the byte, and a run shorter
    // than three digits ends the escape early — both are `goto error`, which
    // `archive.c` reports as an unclosed quote.
    for spec in [r#""\401":x"#, r#""\12":x"#, r#""\1":x"#, r#""\8":x"#, r#""\47":x"#] {
        let arg = format!("--add-virtual-file={spec}");
        let o = run(&repo, &home, &["archive", "--format=tar", &arg, "HEAD"]);
        assert_eq!(
            String::from_utf8_lossy(&o.stderr).lines().next().unwrap_or(""),
            format!("fatal: unclosed quote: '{spec}'"),
            "spec {spec}"
        );
        assert_eq!(o.status.code(), Some(128), "spec {spec}");
        assert!(o.stdout.is_empty(), "spec {spec} still wrote an archive");
    }

    // Accepted, and the decoded byte is the name: `\101` is `A`, `\377` is the
    // top of the range a three-digit escape can reach.
    for (spec, want) in [
        (r#""\101":x"#, vec![b'A']),
        (r#""\377":x"#, vec![0xffu8]),
        (r#""\000":x"#, Vec::new()),
        (r#""a\tb":x"#, b"a\tb".to_vec()),
        ("plain:x", b"plain".to_vec()),
    ] {
        let arg = format!("--add-virtual-file={spec}");
        let o = run(&repo, &home, &["archive", "--format=tar", &arg, "HEAD"]);
        assert!(o.status.success(), "spec {spec}: {}", String::from_utf8_lossy(&o.stderr));
        let names = tar_names(&o.stdout);
        // `"\000":x` decodes to an empty name, which `archive.c`'s
        // `buf.len ? … : xstrndup(arg, p - arg)` falls back to the literal
        // text for — so only the non-empty decodes are asserted by value.
        if want.is_empty() {
            continue;
        }
        assert!(
            names.iter().any(|n| *n == want),
            "spec {spec}: {:?} does not contain {want:?}",
            names.iter().map(|n| String::from_utf8_lossy(n).into_owned()).collect::<Vec<_>>()
        );
    }

    let _ = std::fs::remove_dir_all(&root);
}
