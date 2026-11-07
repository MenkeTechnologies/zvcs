//! Bytes after a pack's trailer.
//!
//! `parse_pack_objects()` ends by checking that a regular-file input has been read
//! to its end, and dies `pack has junk at the end` otherwise (builtin/index-pack.c:
//! 1301-1306), before any index is written: the pack it streamed is left in
//! `tmp_pack_XXXXXX` (0444 under the umask, `odb_mkstemp()`) or the named
//! `<pack-file>`, short of the trailer it had not flushed. A pipe is never checked;
//! `final()` copies what followed the pack into its buffer to stdout after the
//! `pack\t<hash>` line (:1656-1657), and the pack kept is the pack alone. A named
//! `<pack-file>` is a regular file, so `index-pack <pack>` refuses junk too.
//!
//! Stock git is the oracle (`support/stock_git.rs`).

#[path = "support/stock_git.rs"]
mod stock_git;
use stock_git::stock_git;

use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

const ZVCS: &str = env!("CARGO_BIN_EXE_git");

struct Fixture {
    root: PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn command(bin: &str, cwd: &Path, root: &Path, args: &[&str]) -> Command {
    let mut cmd = Command::new(bin);
    cmd.args(args)
        .current_dir(cwd)
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", root)
        .env("GIT_CEILING_DIRECTORIES", root)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "A")
        .env("GIT_AUTHOR_EMAIL", "a@example.com")
        .env("GIT_COMMITTER_NAME", "A")
        .env("GIT_COMMITTER_EMAIL", "a@example.com")
        .env("GIT_AUTHOR_DATE", "1700000000 +0000")
        .env("GIT_COMMITTER_DATE", "1700000000 +0000")
        .env("LC_ALL", "C");
    cmd
}

/// `p.pack` holds a small history; `t.git` is an empty bare repository.
fn fixture(stock: &str, tag: &str) -> Fixture {
    let root = std::env::temp_dir().join(format!("zvcs-idxjunk-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let root = root.canonicalize().unwrap();
    let ok = |cwd: &Path, args: &[&str]| {
        let out = command(stock, cwd, &root, args).output().unwrap();
        assert!(out.status.success(), "{args:?}: {}", String::from_utf8_lossy(&out.stderr));
        out.stdout
    };
    ok(&root, &["init", "-q", "-b", "main", "src"]);
    let src = root.join("src");
    for i in 1..=3 {
        let body: String = (0..300).map(|n| format!("line {n} of file revision {i}\n")).collect();
        std::fs::write(src.join("f"), body).unwrap();
        ok(&src, &["add", "."]);
        ok(&src, &["commit", "-qm", &format!("c{i}")]);
    }
    let objects = ok(&src, &["rev-list", "--objects", "main"]);
    let mut pack = command(stock, &src, &root, &["pack-objects", "--stdout"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    pack.stdin.take().unwrap().write_all(&objects).unwrap();
    let pack = pack.wait_with_output().unwrap();
    std::fs::write(root.join("p.pack"), &pack.stdout).unwrap();
    ok(&root, &["init", "-q", "--bare", "t.git"]);
    Fixture { root }
}

/// The empty pack: a header declaring no objects and its SHA-1 trailer.
fn empty_pack() -> Vec<u8> {
    let mut pack = b"PACK\0\0\0\x02\0\0\0\0".to_vec();
    let trailer = "029d08823bd8a8eab510ad6ac75c823cfd3ed31e";
    pack.extend((0..20).map(|i| u8::from_str_radix(&trailer[2 * i..2 * i + 2], 16).unwrap()));
    pack
}

/// How the pack reaches `index-pack`.
#[derive(Clone, Copy)]
enum Input {
    /// `--stdin` reading a regular file.
    File,
    /// `--stdin` reading a pipe.
    Pipe,
    /// The pack named on the command line.
    Named,
}

/// `objects/pack` (or the work directory for a named pack): every file with its
/// mode and size, the hash in a name made generic.
fn listing(dir: &Path) -> Vec<String> {
    let mut out: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| {
            let e = e.unwrap();
            let meta = e.metadata().unwrap();
            let name = e.file_name().to_string_lossy().into_owned();
            let name = match name.strip_prefix("tmp_pack_") {
                Some(_) => "tmp_pack_X".to_string(),
                None => name,
            };
            format!("{name} {:o} {}", meta.permissions().mode() & 0o777, meta.len())
        })
        .collect();
    out.sort();
    out
}

fn check(tag: &str, pack: impl Fn(&Path) -> Vec<u8>, junk: &[u8], input: Input, extra: &[&str]) {
    let Some(stock) = stock_git() else { return };
    let mut results = Vec::new();
    for bin in [stock, ZVCS] {
        let fx = fixture(stock, tag);
        let mut bytes = pack(&fx.root);
        bytes.extend_from_slice(junk);
        let t = fx.root.join("t.git");
        let (out, dir) = match input {
            Input::Named => {
                let named = fx.root.join("named");
                std::fs::create_dir_all(&named).unwrap();
                std::fs::write(named.join("x.pack"), &bytes).unwrap();
                let mut args = extra.to_vec();
                args.push("x.pack");
                (command(bin, &named, &fx.root, &[&["index-pack"], &args[..]].concat()).output().unwrap(), named)
            }
            Input::File => {
                std::fs::write(fx.root.join("in.pack"), &bytes).unwrap();
                let file = std::fs::File::open(fx.root.join("in.pack")).unwrap();
                let out = command(bin, &t, &fx.root, &[&["index-pack", "--stdin"], extra].concat())
                    .stdin(file)
                    .output()
                    .unwrap();
                (out, t.join("objects/pack"))
            }
            Input::Pipe => {
                let mut child = command(bin, &t, &fx.root, &[&["index-pack", "--stdin"], extra].concat())
                    .stdin(Stdio::piped())
                    .stdout(Stdio::piped())
                    .stderr(Stdio::piped())
                    .spawn()
                    .unwrap();
                child.stdin.take().unwrap().write_all(&bytes).unwrap();
                (child.wait_with_output().unwrap(), t.join("objects/pack"))
            }
        };
        results.push((
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
            out.status.code(),
            listing(&dir),
        ));
    }
    assert_eq!(results[0], results[1], "{tag}: stock (left) vs zvcs (right)");
}

fn small(root: &Path) -> Vec<u8> {
    std::fs::read(root.join("p.pack")).unwrap()
}

#[test]
fn index_pack_parity_junk_after_a_regular_file_is_refused() {
    check("file", small, b"junk", Input::File, &[]);
    check("file-thin", small, b"junk", Input::File, &["--fix-thin"]);
    check("file-limit", small, b"junk", Input::File, &["--max-input-size=1000000"]);
    check("file-empty", |_| empty_pack(), b"zz", Input::File, &[]);
    check("named", small, b"junk", Input::Named, &[]);
    check("named-empty", |_| empty_pack(), b"zz", Input::Named, &[]);
}

#[test]
fn index_pack_parity_junk_after_a_pipe_goes_to_stdout() {
    check("pipe", small, b"junk", Input::Pipe, &[]);
    check("pipe-keep", small, b"junk", Input::Pipe, &["--keep"]);
    check("pipe-thin", small, b"junk", Input::Pipe, &["--fix-thin"]);
    check("pipe-empty", |_| empty_pack(), b"zz", Input::Pipe, &[]);
}

#[test]
fn index_pack_parity_pack_without_junk_is_unchanged() {
    check("clean-file", small, b"", Input::File, &[]);
    check("clean-pipe", small, b"", Input::Pipe, &[]);
}
