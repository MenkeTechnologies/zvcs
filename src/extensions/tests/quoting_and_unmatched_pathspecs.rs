//! Three rules the byte-level path output and the pathspec accounting have to obey.
//!
//! * `quote.c`'s `quote_c_style()` reaches every path `ls-tree` prints, and `-z`
//!   turns it off only for the four dedicated printers — `show_tree_fmt()`, which
//!   a `--format` outside the four canonical templates selects, quotes regardless
//!   (builtin/ls-tree.c:125-133 vs :171-183).
//! * `cmd_add()`'s unmatched-pathspec loop (builtin/add.c:540-570) judges a
//!   wildcard element on whether the *matcher* found anything, so
//!   `git add 'a/nosuch/*.txt'` is a fatal even though nothing about the literal
//!   text is checked, and every diagnostic quotes `pathspec.items[i].original` —
//!   the element as typed, trailing slash and all.
//! * The gitignore block comes from `add_files()` (builtin/add.c:344-352), which
//!   runs *inside* the odb transaction: it sets `exit_status = 1` and keeps
//!   staging, so a stageable path listed beside an ignored one still lands.
//!
//! Expectations measured against stock git 2.55.0 (`/opt/homebrew/bin/git`) and
//! written out literally, because the `git` on this machine's PATH is zvcs itself.
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
        let root = std::env::temp_dir().join(format!("zvcs-quotespec-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.git(&["init", "-q", "-b", "main", "."]);
        f.git(&["config", "user.email", "t@e.co"]);
        f.git(&["config", "user.name", "t"]);
        f
    }

    fn cmd(&self, args: &[&str]) -> Command {
        let mut c = Command::new(BIN);
        c.args(args)
            .current_dir(&self.work)
            .env("HOME", &self.root)
            .env("ZVCS_HOME", self.root.join(".zvcs"))
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1");
        c
    }

    fn git(&self, args: &[&str]) {
        let out = self.cmd(args).output().unwrap();
        assert!(out.status.success(), "setup `git {args:?}` failed: {out:?}");
    }

    /// `(exit code, stdout bytes, stderr)`. stdout stays bytes because the whole
    /// point of the quoting cases is what lands there when a name is not UTF-8.
    fn run(&self, args: &[&str]) -> (i32, Vec<u8>, String) {
        let out = self.cmd(args).output().unwrap();
        (
            out.status.code().unwrap_or(-1),
            out.stdout,
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    }

    fn write(&self, path: &str, body: &[u8]) {
        if let Some(parent) = std::path::Path::new(path).parent() {
            std::fs::create_dir_all(self.work.join(parent)).unwrap();
        }
        std::fs::write(self.work.join(path), body).unwrap();
    }

    /// Feed `input` to `git <args>` and return its stdout, trimmed of the trailing
    /// newline — used for the plumbing that builds the awkward tree.
    fn plumb(&self, args: &[&str], input: &[u8]) -> String {
        use std::io::Write;
        let mut child = self
            .cmd(args)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        child.stdin.take().unwrap().write_all(input).unwrap();
        let out = child.wait_with_output().unwrap();
        assert!(out.status.success(), "`git {args:?}` failed: {out:?}");
        String::from_utf8(out.stdout).unwrap().trim_end().to_string()
    }
}

/// The entry names, one per `cq_lookup[]` class: a byte the table gives a named
/// escape, one it always octal-escapes, DEL, the two always-quoted literals, a
/// valid-UTF-8 pair above 0x7f, a lone invalid byte above 0x7f, and two names that
/// need no quoting at all (one of them with a space, which `quote_c_style` leaves
/// alone even though it looks quotable).
///
/// They are recorded with `mktree` rather than created on disk: APFS rejects some
/// of these byte sequences in a filename and silently normalizes others, and the
/// tree is the only thing under test.
const NAMES: &[&[u8]] = &[
    b"back\\slash.txt",
    b"bel\x07.txt",
    b"del\x7f.txt",
    b"dq\"uote.txt",
    b"latin1-\xe9.txt",
    b"nl\n.txt",
    b"plain.txt",
    b"space name.txt",
    b"tab\t.txt",
    b"utf8-\xc3\xa9.txt",
];

/// The same names as `quote_c_style()` renders them with `core.quotePath` on.
const QUOTED: &[&str] = &[
    "\"sub/back\\\\slash.txt\"",
    "\"sub/bel\\a.txt\"",
    "\"sub/del\\177.txt\"",
    "\"sub/dq\\\"uote.txt\"",
    "\"sub/latin1-\\351.txt\"",
    "\"sub/nl\\n.txt\"",
    "sub/plain.txt",
    "sub/space name.txt",
    "\"sub/tab\\t.txt\"",
    "\"sub/utf8-\\303\\251.txt\"",
];

/// A repository whose HEAD tree holds `sub/<every awkward name>`.
fn awkward_tree(tag: &str) -> Fixture {
    let f = Fixture::new(tag);
    let blob = f.plumb(&["hash-object", "-w", "--stdin"], b"x\n");

    let mut records = Vec::new();
    for name in NAMES {
        records.extend_from_slice(format!("100644 blob {blob}\t").as_bytes());
        records.extend_from_slice(name);
        records.push(0);
    }
    let sub = f.plumb(&["mktree", "-z", "--missing"], &records);
    let root = f.plumb(&["mktree", "-z", "--missing"], format!("040000 tree {sub}\tsub\0").as_bytes());
    let commit = f.plumb(&["commit-tree", &root, "-m", "tree"], b"");
    f.git(&["update-ref", "refs/heads/main", &commit]);
    f
}

/// The raw, unquoted form of every name, `sub/`-prefixed.
fn raw_paths() -> Vec<Vec<u8>> {
    NAMES
        .iter()
        .map(|n| {
            let mut p = b"sub/".to_vec();
            p.extend_from_slice(n);
            p
        })
        .collect()
}

fn join(parts: &[Vec<u8>], terminator: u8) -> Vec<u8> {
    let mut out = Vec::new();
    for p in parts {
        out.extend_from_slice(p);
        out.push(terminator);
    }
    out
}

#[test]
fn ls_tree_quotes_every_cq_lookup_class() {
    let f = awkward_tree("lstree");

    // `write_name_quoted()` with a newline terminator: quoted, with the escapes
    // `cq_lookup[]` names and octal for everything else it flags.
    let want = QUOTED.iter().map(|q| q.as_bytes().to_vec()).collect::<Vec<_>>();
    assert_eq!(f.run(&["ls-tree", "-r", "--name-only", "HEAD"]).1, join(&want, b'\n'));

    // The default and `--long` formats put the same rendering after their columns,
    // so checking the tail of each line is checking the same `write_name_quoted()`.
    let (_, stdout, _) = f.run(&["ls-tree", "-r", "HEAD"]);
    let tails: Vec<Vec<u8>> = stdout
        .split(|b| *b == b'\n')
        .filter(|l| !l.is_empty())
        .map(|l| l.rsplit(|b| *b == b'\t').next().unwrap().to_vec())
        .collect();
    assert_eq!(tails, want);
}

#[test]
fn ls_tree_z_writes_names_raw() {
    let f = awkward_tree("lstreez");
    let raw = join(&raw_paths(), 0);

    // `show_tree_common_default_long()` and `show_tree_name_only()` spell the `-z`
    // case out as a bare `fputs()`: no quoting, NUL terminator.
    assert_eq!(f.run(&["ls-tree", "-r", "-z", "--name-only", "HEAD"]).1, raw);

    // `--format=%(path)` is `ls_tree_cmdmode_format[MODE_NAME_ONLY].fmt` exactly, so
    // `cmd_add()`'s `m2f` loop hands it to that same printer — still raw.
    assert_eq!(f.run(&["ls-tree", "-r", "-z", "--format=%(path)", "HEAD"]).1, raw);

    // One character more and the fast path misses: `show_tree_fmt()` runs, and its
    // `%(path)` has no `-z` arm at all — `quote_c_style()` unconditionally.
    let bracketed: Vec<Vec<u8>> =
        QUOTED.iter().map(|q| format!("[{q}]").into_bytes()).collect();
    assert_eq!(
        f.run(&["ls-tree", "-r", "-z", "--format=[%(path)]", "HEAD"]).1,
        join(&bracketed, 0)
    );
}

#[test]
fn ls_tree_honours_core_quotepath() {
    let f = awkward_tree("lstreecfg");
    f.git(&["config", "core.quotePath", "false"]);

    // `cq_must_quote()` is `cq_lookup[c] + quote_path_fully > 0`: the high half's
    // table entry is 0, so it stops being quoted — and nothing else changes, which
    // is what tells the flag apart from "quoting is off".
    let mut want = QUOTED.iter().map(|q| q.as_bytes().to_vec()).collect::<Vec<_>>();
    want[4] = b"sub/latin1-\xe9.txt".to_vec();
    want[9] = b"sub/utf8-\xc3\xa9.txt".to_vec();
    assert_eq!(f.run(&["ls-tree", "-r", "--name-only", "HEAD"]).1, join(&want, b'\n'));
}

/// A worktree with one tracked file, one gitignored file and one stageable new
/// file, plus an `a/` directory that has no `nosuch` under it.
fn add_repo(tag: &str) -> Fixture {
    let f = Fixture::new(tag);
    f.write(".gitignore", b"ignored.log\n");
    f.write("tracked.txt", b"t\n");
    f.write("a/deep.txt", b"d\n");
    f.git(&["add", "-A", "."]);
    f.git(&["commit", "-q", "-m", "base"]);
    f.write("ignored.log", b"log\n");
    f.write("fresh.txt", b"new\n");
    f
}

fn staged_paths(f: &Fixture) -> Vec<String> {
    String::from_utf8(f.run(&["ls-files"]).1)
        .unwrap()
        .lines()
        .map(str::to_string)
        .collect()
}

#[test]
fn an_unmatched_wildcard_pathspec_is_fatal() {
    // Every shape whose "did it match" question a literal prefix compare cannot
    // answer, plus the two that only `.original` gets right.
    let shapes: &[(&str, &str)] = &[
        // A wildcard under a directory that does not exist: `file_exists()` on the
        // literal text fails, so the element dies even without `:(glob)` magic.
        ("a/nosuch/*.txt", "a/nosuch/*.txt"),
        // A wildcard whose directory does exist but whose glob matches nothing.
        ("a/*.md", "a/*.md"),
        ("nosuch*", "nosuch*"),
        ("*.nosuch", "*.nosuch"),
        // `PATHSPEC_GLOB` / `PATHSPEC_ICASE` short-circuit `file_exists()`
        // altogether: the matcher's answer is the only one that counts.
        (":(glob)**/nosuch", ":(glob)**/nosuch"),
        (":(icase)NOSUCH.TXT", ":(icase)NOSUCH.TXT"),
        // `:(top)` rooted, and a `:/`-rooted wildcard.
        (":(top)nosuch*", ":(top)nosuch*"),
        (":/nosuch*", ":/nosuch*"),
        // Quoted back as typed: `normalize_path_copy()` drops the trailing slash
        // from `.match`, but `.original` keeps it.
        ("nosuch/", "nosuch/"),
    ];
    for verb in ["add", "stage"] {
        for (spec, quoted) in shapes {
            let f = add_repo("wild");
            let (code, _, err) = f.run(&[verb, spec]);
            assert_eq!(code, 128, "`{verb} {spec}` should be fatal, got {err}");
            assert!(
                err.contains(&format!("fatal: pathspec '{quoted}' did not match any files")),
                "`{verb} {spec}` reported: {err}"
            );
            // A rejected pathspec stages nothing.
            assert_eq!(staged_paths(&f), [".gitignore", "a/deep.txt", "tracked.txt"]);
        }
    }
}

#[test]
fn an_unmatched_wildcard_is_reported_before_a_matching_one_stages() {
    // `nosuch*` is argv[1], so it is only reached after `fresh.txt` has been
    // accounted for — the loop still dies, and the odb transaction never opens.
    for verb in ["add", "stage"] {
        let f = add_repo("order");
        let (code, _, err) = f.run(&[verb, "fresh.txt", "nosuch*"]);
        assert_eq!(code, 128);
        assert!(err.contains("fatal: pathspec 'nosuch*' did not match any files"), "{err}");
        assert!(!staged_paths(&f).contains(&"fresh.txt".to_string()), "{:?}", staged_paths(&f));
    }
}

#[test]
fn an_exclude_pathspec_never_has_to_match() {
    // `if (pathspec.items[i].magic & PATHSPEC_EXCLUDE) continue;` — an exclude
    // element is exempt, and the positive one beside it still has to match.
    for verb in ["add", "stage"] {
        let f = add_repo("excl");
        let (code, _, err) = f.run(&[verb, ":!nosuch.txt", "fresh.txt"]);
        assert_eq!(code, 0, "{err}");
        assert!(staged_paths(&f).contains(&"fresh.txt".to_string()));
    }
}

#[test]
fn update_mode_names_every_untracked_element_before_exiting() {
    // `-u` reaches `report_path_error()` instead, which collects rather than dying:
    // both elements are named, each with `error:`, then `exit(128)`.
    for verb in ["add", "stage"] {
        let f = add_repo("update");
        let (code, _, err) = f.run(&[verb, "-u", "ignored.log", "fresh.txt"]);
        assert_eq!(code, 128, "{err}");
        assert_eq!(
            err,
            "error: pathspec 'ignored.log' did not match any file(s) known to git\n\
             error: pathspec 'fresh.txt' did not match any file(s) known to git\n"
        );
    }
}

#[test]
fn a_gitignored_pathspec_does_not_stop_the_others_from_staging() {
    // `add_files()` prints the block and sets `exit_status = 1`, then goes on to
    // stage `dir->entries` — all of it inside the odb transaction, which commits.
    for verb in ["add", "stage"] {
        let f = add_repo("ignored");
        let (code, _, err) = f.run(&[verb, "ignored.log", "fresh.txt"]);
        assert_eq!(code, 1, "{err}");
        assert!(
            err.starts_with("The following paths are ignored by one of your .gitignore files:\nignored.log\n"),
            "{err}"
        );
        assert!(
            staged_paths(&f).contains(&"fresh.txt".to_string()),
            "the stageable path should still be staged: {:?}",
            staged_paths(&f)
        );
        assert!(!staged_paths(&f).contains(&"ignored.log".to_string()));
    }
}

#[test]
fn a_fatal_pathspec_outranks_the_gitignore_block() {
    // The fatal lives in `cmd_add()`'s loop, which runs before `add_files()` can
    // print anything — in either argv order, and with nothing staged.
    for verb in ["add", "stage"] {
        for order in [["ignored.log", "nosuch.txt"], ["nosuch.txt", "ignored.log"]] {
            let f = add_repo("outrank");
            let (code, _, err) = f.run(&[verb, order[0], order[1]]);
            assert_eq!(code, 128, "{err}");
            assert_eq!(err, "fatal: pathspec 'nosuch.txt' did not match any files\n");
            assert_eq!(staged_paths(&f), [".gitignore", "a/deep.txt", "tracked.txt"]);
        }
    }
}
