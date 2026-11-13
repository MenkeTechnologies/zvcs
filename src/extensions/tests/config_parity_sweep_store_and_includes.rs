//! `git config` against stock git on the gaps a differential sweep of the command found.
//!
//! * **A malformed file under a write.** `repo_config_set_multivar_in_file_gently()`
//!   parses the file itself (config.c:3141-3147). A section header that does not read
//!   leaves its partial name in `cs->var`, and the error event that flushes it makes
//!   `store_aux_event()` refuse it — `invalid section name '<var>'`, then `invalid
//!   config file <path>` at exit 3 (config.c:2654-2655) — instead of the `bad config
//!   line` the reader dies with. Any other refusal dies `bad config line <n>` from the
//!   store's own parse, and `--rename-section`/`--remove-section` copy lines without
//!   parsing at all (config.c:3353-3505), so a malformed file does not stop them.
//! * **`--list --type=path`.** `format_config_path()` (builtin/config.c:343-360) calls
//!   `git_config_pathname()` whatever `gently` says, so a valueless key still prints
//!   `missing value` and is skipped, and a `~user` that does not expand still dies. No
//!   type reader runs at all under `--name-only` (builtin/config.c:418-419).
//! * **`include.path` that names no readable file.** `handle_path_include()`
//!   (config.c:142-191) refuses a valueless one with `missing value for
//!   'include.path'`, and an empty one or one naming a directory reaches `fopen()`,
//!   which refuses a directory with EISDIR; either way the including file's parse
//!   ends on that line, for every command that reads the configuration.
//!   `git config` itself reads those includes only where stock does: in a listing's
//!   pager check, for `--local`, and in a write's `core.configLockTimeout` read.
//! * **`--get-colorbool`** compares the slot with each variable by `strcmp()`, never
//!   parsing it as a key.
//! * **Conflicting actions** name each option as it was spelled (`-l`, `-e`), the
//!   first spelling sticking (parse-options.c `optname()`).
//!
//! Each case runs through stock git and this binary on identical fixtures and compares
//! stdout, stderr, the exit code and the bytes of the file the command was pointed at.
#![cfg(unix)]

#[path = "support/stock_git.rs"]
mod stock_git;
use stock_git::stock_git;

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

fn git(bin: &str, cwd: &Path, home: &Path, args: &[&str]) -> (String, String, i32) {
    let out = Command::new(bin)
        .args(args)
        .current_dir(cwd)
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", home)
        .env("GIT_CEILING_DIRECTORIES", home)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_EDITOR", "true")
        .env("GIT_AUTHOR_NAME", "A")
        .env("GIT_AUTHOR_EMAIL", "a@example.com")
        .env("GIT_COMMITTER_NAME", "A")
        .env("GIT_COMMITTER_EMAIL", "a@example.com")
        .env("GIT_AUTHOR_DATE", "1700000000 +0000")
        .env("GIT_COMMITTER_DATE", "1700000000 +0000")
        .env("LC_ALL", "C")
        .env("TERM", "dumb")
        .output()
        .unwrap();
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
        out.status.code().unwrap_or(-1),
    )
}

/// Two identical repositories, `stock/repo` and `zvcs/repo`, each with an empty commit.
fn fixture(stock: &str, tag: &str) -> Fixture {
    let root = std::env::temp_dir().join(format!("zvcs-cfg-sweep-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    for side in ["stock", "zvcs"] {
        let dir = root.join(side).join("repo");
        std::fs::create_dir_all(&dir).unwrap();
        for args in [
            &["init", "-q", "-b", "main"][..],
            &["-c", "maintenance.auto=false", "commit", "-q", "--allow-empty", "-m", "a"][..],
        ] {
            let (_, err, code) = git(stock, &dir, &root, args);
            assert_eq!(code, 0, "{args:?}: {err}");
        }
    }
    Fixture { root }
}

impl Fixture {
    fn repo(&self, side: &str) -> PathBuf {
        self.root.join(side).join("repo")
    }

    /// Write `name` (relative to each side's repository) on both sides.
    fn write(&self, name: &str, bytes: &str) {
        for side in ["stock", "zvcs"] {
            std::fs::write(self.repo(side).join(name), bytes).unwrap();
        }
    }

    /// Append to `name` on both sides.
    fn append(&self, name: &str, bytes: &str) {
        for side in ["stock", "zvcs"] {
            let path = self.repo(side).join(name);
            let mut text = std::fs::read_to_string(&path).unwrap();
            text.push_str(bytes);
            std::fs::write(path, text).unwrap();
        }
    }

    /// Run `args` on both sides; compare the output and, when `file` is given, its bytes.
    fn compare(&self, stock: &str, args: &[&str], file: Option<&str>) {
        let run = |bin: &str, side: &str| {
            let dir = self.repo(side);
            let out = git(bin, &dir, &self.root.join(side), args);
            let bytes = file.map(|f| std::fs::read(dir.join(f)).unwrap_or_default());
            (out, bytes)
        };
        let expected = run(stock, "stock");
        let actual = run(ZVCS, "zvcs");
        assert_eq!(actual, expected, "git {args:?}");
    }
}

/// Every write form over files whose header, entry or quoting does not parse.
#[test]
fn a_write_meets_a_malformed_file_in_the_stores_own_parse() {
    let Some(stock) = stock_git() else { return };
    let f = fixture(stock, "store");
    let files = [
        "[bad\n",
        "[a \"b\n",
        "[]\n",
        "[a.\n",
        "[a]\nk=1\n[b\n[c]\n",
        "[a]\nk = \"x\n",
        "[a]\n\tk = 1\n\t[ b ]\n",
    ];
    let ops: [&[&str]; 6] = [
        &["config", "-f", "b.cfg", "a.k", "v"],
        &["config", "-f", "b.cfg", "--add", "a.k", "v"],
        &["config", "-f", "b.cfg", "--unset-all", "a.k"],
        &["config", "-f", "b.cfg", "--rename-section", "a", "z"],
        &["config", "-f", "b.cfg", "--remove-section", "a"],
        &["config", "-f", "b.cfg", "--get", "a.k"],
    ];
    for text in files {
        for op in ops {
            f.write("b.cfg", text);
            f.compare(stock, op, Some("b.cfg"));
        }
    }
}

/// `--list --type=path` is not gentle about the path reader's own complaints.
#[test]
fn list_as_paths_reports_what_the_path_reader_reports() {
    let Some(stock) = stock_git() else { return };
    let f = fixture(stock, "path");
    f.append(".git/config", "[t]\n\tflag\n\trel = ./rel\n\topt = :(optional)./missing\n");
    f.compare(stock, &["config", "--type=path", "--list"], None);
    f.compare(stock, &["config", "--path", "--name-only", "--list"], None);
    f.append(".git/config", "\tuser = ~nosuchuserzz/x\n\tafter = y\n\tword = abc\n");
    f.compare(stock, &["config", "--type=path", "--list"], None);
    // `--name-only` never reaches a type reader (builtin/config.c:418-419), whichever
    // form lists the names.
    f.compare(stock, &["config", "--type=path", "--name-only", "--list"], None);
    f.compare(stock, &["config", "--type=int", "--name-only", "--get-regexp", "^t"], None);
    f.compare(stock, &["config", "--type=color", "-z", "--name-only", "--get-regexp", "^t"], None);
}

/// An `include.path` with no value, an empty one and one naming a directory end the
/// including file's parse on their own line, whichever command reads it.
#[test]
fn an_include_that_names_no_readable_file_ends_the_parse() {
    let Some(stock) = stock_git() else { return };
    for (tag, include) in [
        ("valueless", "[include]\n\tpath\n"),
        ("empty", "[include]\n\tpath =\n"),
        ("dir", "[include]\n\tpath = ../\n"),
    ] {
        let f = fixture(stock, tag);
        f.write("i.cfg", &format!("[a]\n\tb = 1\n{include}[x]\n\ty = 2\n"));
        f.compare(stock, &["config", "-f", "i.cfg", "--includes", "--list"], None);
        f.compare(stock, &["config", "-f", "i.cfg", "--list"], None);
        f.append(".git/config", include);
        f.compare(stock, &["config", "--get", "core.bare"], None);
        f.compare(stock, &["status", "--porcelain"], None);
        // `git config` meets the repository's includes only where it reads them:
        // not in setup, not under `--no-includes` or for another file, but in the
        // pager check of a listing, for `--local`, and in a write's
        // `core.configLockTimeout` read.
        f.compare(stock, &["config", "--no-includes", "--get", "core.bare"], None);
        f.compare(stock, &["config", "-f", ".git/config", "--get", "core.bare"], None);
        f.compare(stock, &["config", "--no-includes", "--list"], None);
        f.compare(stock, &["config", "--local", "--no-includes", "--get", "core.bare"], None);
        f.compare(stock, &["config", "--no-includes", "bad", "v"], Some(".git/config"));
        f.compare(stock, &["config", "--no-includes", "a.b", "c"], Some(".git/config"));
    }
}

/// `--get-colorbool` compares each variable with the slot as typed, so a slot with no
/// section or with capitals matches nothing and falls back to the default.
#[test]
fn a_colorbool_slot_is_matched_verbatim() {
    let Some(stock) = stock_git() else { return };
    let f = fixture(stock, "colorbool");
    f.append(".git/config", "[color]\n\tdiff = always\n\tui = never\n[a \"B\"]\n\tc = always\n");
    for slot in ["color.diff", "Color.Diff", "x", ".x", "a.B.c", "a.b.c", "COLOR.UI"] {
        f.compare(stock, &["config", "--get-colorbool", slot, "true"], None);
        f.compare(stock, &["config", "--get-colorbool", slot], None);
    }
}

/// `-l` and `-e` are named as typed in a conflict, and the first spelling sticks.
#[test]
fn a_conflict_names_each_action_as_typed() {
    let Some(stock) = stock_git() else { return };
    let f = fixture(stock, "conflict");
    for args in [
        &["config", "-l", "--get", "a.b"][..],
        &["config", "--get", "-l", "a.b"][..],
        &["config", "-e", "-l"][..],
        &["config", "-l", "--list", "-e"][..],
        &["config", "--list", "-l", "-e"][..],
        &["config", "-le"][..],
    ] {
        f.compare(stock, args, None);
    }
}
