//! `diffcore_std()` prefetches the blobs of the queued pairs ahead of any format that reads content
//! (diff.c), and `promisor_remote_get_direct()` dies with `could not fetch <oid> from promisor
//! remote` after the `git fetch` child has said why on its own stderr. From a directory where the
//! partial clone's relative remote URL does not resolve (inside the git directory) the fetch
//! fails, so `git show` / `git show --stat` must end at 128 with those lines; `show` printed a
//! bare `An object with id … could not be found` and exited 1.

#[path = "support/stock_git.rs"]
mod stock_git;

use std::path::{Path, PathBuf};
use std::process::Command;
use stock_git::stock_git;

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn run(bin: &str, dir: &Path, args: &[&str]) -> (String, String, i32) {
    let out = Command::new(bin)
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "a")
        .env("GIT_AUTHOR_EMAIL", "a@example.com")
        .env("GIT_COMMITTER_NAME", "a")
        .env("GIT_COMMITTER_EMAIL", "a@example.com")
        .env("LC_ALL", "C")
        .output()
        .expect("run git");
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
        out.status.code().expect("no signal"),
    )
}

fn ok(bin: &str, dir: &Path, args: &[&str]) {
    let (_, err, code) = run(bin, dir, args);
    assert_eq!(code, 0, "git {args:?}: {err}");
}

/// A blob:none partial clone whose HEAD commit changes a blob the clone never received, with
/// its peer inside the directory as `./.remote.git` (a relative URL).
fn template(stock: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!("zvcs-promshow-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let (src, dir) = (root.join("src"), root.join("tmpl"));
    std::fs::create_dir_all(&src).unwrap();
    std::fs::create_dir_all(&dir).unwrap();
    ok(stock, &src, &["init", "-q", "-b", "main", "."]);
    for n in 0..3 {
        std::fs::write(src.join("hist.txt"), format!("hist v{n}\n")).unwrap();
        ok(stock, &src, &["add", "hist.txt"]);
        ok(stock, &src, &["commit", "-q", "-m", &format!("h{n}")]);
    }
    ok(stock, &dir, &["init", "-q", "--bare", "-b", "main", ".remote.git"]);
    ok(stock, &dir.join(".remote.git"), &["config", "uploadpack.allowFilter", "true"]);
    ok(stock, &src, &["push", "-q", dir.join(".remote.git").to_str().unwrap(), "main"]);
    ok(
        stock,
        &dir,
        &["-c", "protocol.file.allow=always", "clone", "-q", "--no-local", "--filter=blob:none", "./.remote.git", ".stage"],
    );
    std::fs::rename(dir.join(".stage/.git"), dir.join(".git")).unwrap();
    std::fs::rename(dir.join(".stage/hist.txt"), dir.join("hist.txt")).unwrap();
    std::fs::remove_dir(dir.join(".stage")).unwrap();
    ok(stock, &dir, &["config", "remote.origin.url", "./.remote.git"]);
    dir
}

fn copy(from: &Path, to: &Path) {
    let _ = std::fs::remove_dir_all(to);
    let st = Command::new("cp").arg("-R").arg(from).arg(to).status().unwrap();
    assert!(st.success());
}

#[test]
fn an_unfetchable_promised_blob_ends_show_like_the_prefetch_does() {
    let Some(stock) = stock_git() else { return };
    let tmpl = template(stock);
    let (s, z) = (tmpl.with_file_name("stock"), tmpl.with_file_name("zvcs"));
    for args in [&["show"][..], &["show", "--stat"][..], &["show", "-s"][..]] {
        copy(&tmpl, &s);
        copy(&tmpl, &z);
        let want = run(stock, &s.join(".git/hooks"), args);
        let got = run(BIN, &z.join(".git/hooks"), args);
        assert_eq!(got, want, "{args:?}");
    }
    let _ = std::fs::remove_dir_all(tmpl.parent().unwrap());
}
