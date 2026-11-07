//! `fetch-pack`'s choice between `unpack-objects` and `index-pack` for the pack it
//! receives, and what each leaves behind.
//!
//! `get_pack()` reads the pack header only when `--keep` was not given and the unpack
//! limit is non-zero; `do_keep` starts at `args->keep_pack` and only that read changes
//! it, so `fetch.unpackLimit=0` unpacks every pack it does not keep
//! (fetch-pack.c:991-998, :1007). A kept pack gets `index-pack --keep=<msg>` only when
//! `args->lock_pack || unpack_limit` (:1019-1020), so `--keep` under a limit of 0 writes
//! no `.keep` and prints `pack <hash>`, and a second `-k` sets `lock_pack`
//! (builtin/fetch-pack.c:95-98). `--lock-pack` and `fetch.fsckObjects` capture
//! `index-pack`'s output (fetch-pack.c:1010-1011). zvcs refused a pack at or above the
//! limit without `--keep`, and kept one with a `.keep` under a limit of 0. `fetch-pack`
//! has no smart HTTP of its own: `parse_connect_url()` dies on the scheme
//! (connect.c:1114-1116).
//!
//! Stock git is the oracle (`support/stock_git.rs`); each binary fetches from its own
//! `upload-pack`.

#[path = "support/stock_git.rs"]
mod stock_git;

use std::path::{Path, PathBuf};
use std::process::Command;

const ZVCS: &str = env!("CARGO_BIN_EXE_git");

struct Fixture {
    root: PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn git(bin: &str, dir: &Path, root: &Path, args: &[&str]) -> (String, String, i32) {
    let out = Command::new(bin)
        .args(args)
        .current_dir(dir)
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
        .env("LC_ALL", "C")
        .output()
        .unwrap();
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
        out.status.code().expect("no signal"),
    )
}

/// Every 40-hex run as `<H>`: the pack each binary's own `upload-pack` builds may
/// differ in bytes, and so in name.
fn mask(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = String::new();
    let mut i = 0;
    while i < b.len() {
        if b.len() - i >= 40 && b[i..i + 40].iter().all(u8::is_ascii_hexdigit) {
            out.push_str("<H>");
            i += 40;
        } else {
            out.push(b[i] as char);
            i += 1;
        }
    }
    out
}

impl Fixture {
    /// `small` holds 9 objects, `big` 120.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-fetch-pack-unpack-limit-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let f = Fixture { root };
        let stock = stock_git::stock_git().expect("checked by caller");
        for (name, n) in [("small", 3), ("big", 40)] {
            let dir = f.root.join(name);
            std::fs::create_dir_all(&dir).unwrap();
            git(stock, &dir, &f.root, &["init", "-q", "-b", "main"]);
            for i in 1..=n {
                std::fs::write(dir.join(format!("f{i}")), format!("{i}\n")).unwrap();
                git(stock, &dir, &f.root, &["add", "."]);
                git(stock, &dir, &f.root, &["commit", "-qm", &format!("c{i}")]);
            }
            if name == "small" {
                git(stock, &dir, &f.root, &["tag", "-a", "-m", "t", "v1"]);
            }
        }
        f
    }

    /// Run `fetch-pack` with `bin` in a fresh repository and describe the outcome:
    /// stdout, stderr, exit code, loose object count, and each file under
    /// `objects/pack` with its mode and, for a `.keep`, its message.
    fn run(&self, bin: &str, tag: &str, cfg: &[&str], args: &[&str], url: &str) -> String {
        let stock = stock_git::stock_git().expect("checked by caller");
        let dir = self.root.join(tag);
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        git(stock, &dir, &self.root, &["init", "-q"]);
        let mut argv: Vec<&str> = Vec::new();
        for c in cfg {
            argv.extend(["-c", c]);
        }
        argv.extend(["fetch-pack", "--no-progress"]);
        argv.extend(args);
        argv.extend([url, "refs/heads/main"]);
        let (out, err, code) = git(bin, &dir, &self.root, &argv);

        let objects = dir.join(".git/objects");
        let loose = std::fs::read_dir(&objects)
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().len() == 2)
            .map(|e| std::fs::read_dir(e.path()).unwrap().count())
            .sum::<usize>();
        let mut pack: Vec<String> = std::fs::read_dir(objects.join("pack"))
            .unwrap()
            .flatten()
            .map(|e| {
                use std::os::unix::fs::PermissionsExt;
                let name = e.file_name().to_string_lossy().into_owned();
                let ext = name.rsplit('.').next().unwrap().to_owned();
                let mode = e.metadata().unwrap().permissions().mode() & 0o777;
                let keep = if ext == "keep" {
                    let text = std::fs::read_to_string(e.path()).unwrap();
                    // `fetch-pack <pid> on <hostname>`
                    text.split(' ').take(1).collect::<String>()
                } else {
                    String::new()
                };
                format!("{ext} {mode:o} {keep}")
            })
            .collect();
        pack.sort();
        mask(&format!(
            "exit={code}\n--stdout\n{out}--stderr\n{err}--loose {loose}\n--pack\n{}\n",
            pack.join("\n")
        ))
    }
}

#[test]
fn fetch_pack_parity_unpack_limit() {
    if stock_git::stock_git().is_none() {
        return;
    }
    let stock = stock_git::stock_git().unwrap();
    let f = Fixture::new("matrix");
    let configs: &[&[&str]] = &[
        &[],
        &["fetch.unpackLimit=0"],
        &["fetch.unpackLimit=1"],
        &["fetch.unpackLimit=100"],
        &["fetch.unpackLimit=-1"],
        &["transfer.unpackLimit=0"],
        &["transfer.unpackLimit=0", "fetch.unpackLimit=5"],
        &["fetch.fsckObjects=true"],
        &["fetch.unpackLimit=0", "fetch.fsckObjects=true"],
    ];
    let flags: &[&[&str]] = &[&[], &["-k"], &["-k", "-k"], &["--lock-pack"], &["-k", "--lock-pack"]];
    let mut failures = Vec::new();
    for src in ["small", "big"] {
        let url = format!("file://{}", f.root.join(src).display());
        for cfg in configs {
            for args in flags {
                let want = f.run(stock, "stock", cfg, args, &url);
                let got = f.run(ZVCS, "zvcs", cfg, args, &url);
                if want != got {
                    failures.push(format!("{src} {cfg:?} {args:?}\n--- stock\n{want}--- zvcs\n{got}"));
                }
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// `fetch-pack` connects through `git_connect()`, which knows no smart HTTP.
#[test]
fn fetch_pack_parity_http_url_is_refused() {
    if stock_git::stock_git().is_none() {
        return;
    }
    let stock = stock_git::stock_git().unwrap();
    let f = Fixture::new("http");
    for url in ["http://127.0.0.1:1/small", "https://example.invalid/r.git", "HTTP://x/y", "bogus+x://h/p"] {
        let want = f.run(stock, "stock", &[], &[], url);
        let got = f.run(ZVCS, "zvcs", &[], &[], url);
        assert_eq!(want, got, "{url}");
    }
}
