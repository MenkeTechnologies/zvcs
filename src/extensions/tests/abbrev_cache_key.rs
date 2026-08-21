//! `core.abbrev` must not be able to poison the shared abbreviation cache.
//!
//! Abbreviations are memoised machine-wide in `~/.zvcs/cache/abbrev` (see
//! `crate::rcache`), keyed by the object id and the width the abbreviation was
//! computed at. `core.abbrev` is three different things
//! (`git_default_core_config()`, environment.c):
//!
//! ```c
//! if (!strcmp(var, "core.abbrev")) {
//!         if (!value)
//!                 return config_error_nonbool(var);
//!         if (!strcasecmp(value, "auto"))
//!                 default_abbrev = -1;
//!         else if (!git_parse_maybe_bool_text(value))
//!                 default_abbrev = GIT_MAX_HEXSZ;
//!         else {
//!                 int abbrev = git_config_int(var, value, ctx->kvi);
//!                 if (abbrev < minimum_abbrev)
//!                         return error(_("abbrev length out of range: %d"), abbrev);
//!                 default_abbrev = abbrev;
//!         }
//!         return 0;
//! }
//! ```
//!
//! — so `no`, `off` and `false` mean the **whole** hash while `auto` means a
//! width derived from the object count. Reading the cache key with
//! `.integer("core.abbrev")` collapsed both onto `0`, because no integer parses
//! out of `no`. One `git -c core.abbrev=no log --oneline` then wrote every id it
//! touched at 40 characters under the key `auto` reads, and every later
//! `git log --oneline` — in any clone, in any shell, for as long as the cache
//! file lived — printed the full hash:
//!
//! ```text
//! $ git -c core.abbrev=no log --oneline -1     # one run, output discarded
//! $ git log --oneline -1 | cut -d' ' -f1 | wc -c
//! 41      # zvcs printed a 40-char oid
//! 8       # stock printed 7
//! ```
//!
//! The widths asserted below are stock git 2.55.0's, captured by running it in a
//! repository built the same way as the fixture here (one commit, no packfile, no
//! ambient config): `git log --oneline -1` prints `2199fde` — seven characters —
//! and `git -c core.abbrev=no log --oneline -1` prints all forty. Seven is not an
//! accident of that repository: `auto` is `ceil(log2(objects)/2)` floored at
//! git's `FALLBACK_DEFAULT_ABBREV`, and a loose-object repository has no packed
//! objects to count.
//!
//! Every test runs the built binary against its own `ZVCS_HOME`, so the
//! developer's real cache is never read or written and nothing here needs a
//! network, a stock git binary or ambient config.
#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

/// git's `FALLBACK_DEFAULT_ABBREV` (object-name.h), which is what `auto` lands on
/// in a repository with no packed objects — the fixture below.
const AUTO_WIDTH: usize = 7;

/// A SHA-1 object name in hex.
const FULL_WIDTH: usize = 40;

struct Fixture {
    root: PathBuf,
    work: PathBuf,
    home: PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

impl Fixture {
    /// A one-commit repository with a private, empty cache directory.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!(
            "zvcs-abbrev-cache-{tag}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        let home = root.join("home");
        std::fs::create_dir_all(&work).unwrap();
        std::fs::create_dir_all(&home).unwrap();
        let f = Fixture { root, work, home };
        std::fs::write(f.work.join("a.txt"), "base\n").unwrap();
        f.ok(&["init", "-q", "-b", "main", "."]);
        f.ok(&["add", "a.txt"]);
        f.ok(&["commit", "-q", "-m", "base"]);
        f
    }

    fn run(&self, args: &[&str]) -> (i32, String) {
        let out = Command::new(BIN)
            .args(args)
            .current_dir(&self.work)
            .env("ZVCS_HOME", &self.home)
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
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .output()
            .unwrap();
        (
            out.status.code().expect("exited via a signal"),
            String::from_utf8_lossy(&out.stdout).into_owned(),
        )
    }

    fn ok(&self, args: &[&str]) -> String {
        let (code, stdout) = self.run(args);
        assert_eq!(code, 0, "`git {args:?}` failed");
        stdout
    }

    /// The first whitespace-delimited field of the first output line — the
    /// abbreviated id every format under test leads with.
    fn first_id(&self, args: &[&str]) -> String {
        let stdout = self.ok(args);
        stdout
            .lines()
            .next()
            .unwrap_or_else(|| panic!("`git {args:?}` printed nothing"))
            .split_whitespace()
            .next()
            .unwrap_or_else(|| panic!("`git {args:?}` printed no id"))
            .to_string()
    }

    fn head(&self) -> String {
        self.ok(&["rev-parse", "HEAD"]).trim_end().to_string()
    }

    fn cache_dir(&self) -> PathBuf {
        self.home.join("cache")
    }
}

/// The abbreviated id must be `width` characters and must still be HEAD's prefix —
/// a wrong-length id that is not even the right object would pass a length-only
/// check.
fn assert_width(f: &Fixture, args: &[&str], width: usize, why: &str) {
    let id = f.first_id(args);
    assert_eq!(id.len(), width, "`git {args:?}` id width ({why}): got {id:?}");
    assert!(f.head().starts_with(&id), "`git {args:?}` id is not HEAD's prefix: {id:?}");
}

// ---------------------------------------------------------------------------
// the poisoning itself
// ---------------------------------------------------------------------------

/// The reproduction, in the order that showed it: a run under a boolean-ish
/// `core.abbrev` first, against a cold cache, then a plain run.
///
/// The second run is the assertion. Before the key carried the *resolved* width,
/// it read back the 40-character answer the first run had stored under the key
/// `auto` uses, and printed a full hash where stock prints seven characters.
#[test]
fn a_boolean_core_abbrev_does_not_poison_a_later_auto_run() {
    for word in ["no", "off", "false", "NO", "Off", "FALSE"] {
        let f = Fixture::new(&format!("poison-{word}"));
        let setting = format!("core.abbrev={word}");

        assert_width(
            &f,
            &["-c", &setting, "log", "--oneline", "-1"],
            FULL_WIDTH,
            "a false-y core.abbrev is the whole hash",
        );
        assert_width(
            &f,
            &["log", "--oneline", "-1"],
            AUTO_WIDTH,
            "the following auto run must not read the 40-char entry",
        );
    }
}

/// The same collision from the other side: a warm `auto` entry must not be handed
/// to a `core.abbrev=no` run either. Under the old key both directions were one
/// row, so whichever ran first decided what the other printed.
#[test]
fn an_auto_run_does_not_poison_a_later_boolean_core_abbrev_run() {
    let f = Fixture::new("reverse");
    assert_width(&f, &["log", "--oneline", "-1"], AUTO_WIDTH, "cold auto");
    assert_width(
        &f,
        &["-c", "core.abbrev=no", "log", "--oneline", "-1"],
        FULL_WIDTH,
        "the warm auto entry must not answer for core.abbrev=no",
    );
    assert_width(&f, &["log", "--oneline", "-1"], AUTO_WIDTH, "auto again, warm");
}

/// Every reader of the cache, not just `log --oneline`. `AbbrevCache::new` is
/// reached from `log_flavored`, `format_commit`, `rev_list_pretty_body` and
/// `warm_caches`, so a poisoned entry surfaced through all of them.
#[test]
fn no_reader_of_the_cache_serves_the_poisoned_entry() {
    let f = Fixture::new("readers");
    // Poison-attempt every writer first, so the cache is as full as it can be of
    // full-width answers before anything reads at the automatic width.
    for args in [
        &["-c", "core.abbrev=no", "log", "--oneline", "-1"][..],
        &["-c", "core.abbrev=no", "show", "-s", "--oneline"],
        &["-c", "core.abbrev=no", "log", "--format=%h", "-1"],
        &["-c", "core.abbrev=no", "rev-list", "--pretty=oneline", "-1", "HEAD"],
    ] {
        f.ok(args);
    }
    for args in [
        &["log", "--oneline", "-1"][..],
        &["show", "-s", "--oneline"],
        &["log", "--format=%h", "-1"],
        &["log", "--format=%h", "--no-walk", "HEAD"],
    ] {
        assert_width(&f, args, AUTO_WIDTH, "auto width after a false-y run");
    }
}

/// `--abbrev=<n>` is applied as a `core.abbrev` override before the cache is
/// built, so it has to key as the number it is — not share `auto`'s row and not
/// share the false-y row.
#[test]
fn an_explicit_width_keys_as_itself() {
    let f = Fixture::new("explicit");
    f.ok(&["-c", "core.abbrev=no", "log", "--oneline", "-1"]);
    for (width, arg) in [(12usize, "--abbrev=12"), (8, "--abbrev=8")] {
        assert_width(&f, &["log", arg, "--format=%h", "-1"], width, "explicit --abbrev");
    }
    assert_width(&f, &["log", "--oneline", "-1"], AUTO_WIDTH, "auto is still auto");
}

// ---------------------------------------------------------------------------
// recovery of a cache that is already poisoned
// ---------------------------------------------------------------------------

/// A cache written by the *previous* build is already wrong on disk, so refusing
/// to write new bad rows is not enough on its own — the old ones have to become
/// unreachable.
///
/// This plants exactly what the old code left behind: a journal record whose key
/// is the retired scheme's (the raw object id followed by a single `0` length
/// byte, which is what both `auto` and `core.abbrev=no` produced) and whose value
/// is the full hash. The record is written in the journal format `rcache`
/// documents — `[u32 klen][u32 vlen][key][val]`, little-endian — so this is the
/// real on-disk state and not a stand-in for it.
///
/// The run that follows must print seven characters. Two independent changes get
/// it there and either alone is enough, which is deliberate: the reader now asks
/// for the *resolved* width (`7`, not `0`), and the key carries
/// `rcache::ABBREV_KEY_VERSION`, which the planted row predates. Against the
/// build this regression is about, both were missing and the row came straight
/// back: `got "647caad421f25fe29914d891a57b8756d1bf1669"`, forty characters.
#[test]
fn an_already_poisoned_cache_on_disk_recovers() {
    let f = Fixture::new("recover");
    let head = f.head();
    let raw: Vec<u8> = (0..head.len() / 2)
        .map(|i| u8::from_str_radix(&head[i * 2..i * 2 + 2], 16).unwrap())
        .collect();
    assert_eq!(raw.len(), 20, "a SHA-1 object name is twenty bytes");

    // The retired key: object id bytes, then the length byte the old reader wrote
    // for both `auto` and every false-y spelling.
    let mut key = raw.clone();
    key.push(0);
    let value = head.as_bytes();

    let mut record = Vec::new();
    record.extend_from_slice(&(key.len() as u32).to_le_bytes());
    record.extend_from_slice(&(value.len() as u32).to_le_bytes());
    record.extend_from_slice(&key);
    record.extend_from_slice(value);

    std::fs::create_dir_all(f.cache_dir()).unwrap();
    let journal = f.cache_dir().join("abbrev.log");
    std::fs::write(&journal, &record).unwrap();
    assert!(journal.exists(), "the poisoned journal must be on disk before the run");

    assert_width(
        &f,
        &["log", "--oneline", "-1"],
        AUTO_WIDTH,
        "a v1 row must not answer a v2 lookup",
    );
    assert_width(
        &f,
        &["log", "--format=%h", "-1"],
        AUTO_WIDTH,
        "the same, through the user-format reader",
    );
}

/// The planted record is only meaningful if the reader would otherwise have used
/// it, so this pins that the journal really is consulted: the *current* key
/// scheme, planted the same way with a deliberately wrong value, does come back.
///
/// Without this, [`an_already_poisoned_cache_on_disk_recovers`] would still pass
/// against a build that had stopped reading the journal at all.
#[test]
fn the_journal_is_read_so_the_recovery_test_is_meaningful() {
    let f = Fixture::new("journal-live");
    let head = f.head();
    let raw: Vec<u8> = (0..head.len() / 2)
        .map(|i| u8::from_str_radix(&head[i * 2..i * 2 + 2], 16).unwrap())
        .collect();

    // The v2 key for the automatic width, carrying a sentinel value no
    // disambiguation could produce.
    const SENTINEL: &str = "zzzzzzz";
    let mut key = raw;
    key.push(AUTO_WIDTH as u8);
    key.push(abbrev_key_version());

    let mut record = Vec::new();
    record.extend_from_slice(&(key.len() as u32).to_le_bytes());
    record.extend_from_slice(&(SENTINEL.len() as u32).to_le_bytes());
    record.extend_from_slice(&key);
    record.extend_from_slice(SENTINEL.as_bytes());

    std::fs::create_dir_all(f.cache_dir()).unwrap();
    std::fs::write(f.cache_dir().join("abbrev.log"), &record).unwrap();

    assert_eq!(
        f.first_id(&["log", "--oneline", "-1"]),
        SENTINEL,
        "the journal must be the source of the answer, or the recovery test proves nothing"
    );
}

/// The key-schema version this test file plants records for, kept next to the
/// records themselves so a bump fails loudly here rather than quietly turning
/// [`the_journal_is_read_so_the_recovery_test_is_meaningful`] into a no-op.
fn abbrev_key_version() -> u8 {
    2
}

/// `core.abbrev = 0` — the one value that could still reach the retired key's
/// `0` — is refused before any command runs, so no reader can ask for it.
/// `git_default_core_config()` rejects anything under `minimum_abbrev`, and this
/// is stock git 2.55.0's exact first line and status.
#[test]
fn core_abbrev_zero_never_reaches_the_cache() {
    let f = Fixture::new("zero");
    for verb in ["log", "show", "rev-list"] {
        let out = Command::new(BIN)
            .args(["-c", "core.abbrev=0", verb, "--oneline", "-1"])
            .current_dir(&f.work)
            .env("ZVCS_HOME", &f.home)
            .env("HOME", &f.root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("LC_ALL", "C")
            .output()
            .unwrap();
        assert_eq!(out.status.code(), Some(128), "`git {verb}` with core.abbrev=0");
        assert_eq!(
            String::from_utf8_lossy(&out.stderr).lines().next(),
            Some("error: abbrev length out of range: 0"),
            "`git {verb}` with core.abbrev=0"
        );
    }
    assert!(
        !Path::new(&f.cache_dir().join("abbrev.log")).exists()
            || std::fs::read(f.cache_dir().join("abbrev.log")).unwrap().is_empty(),
        "a refused command must not have written an abbreviation"
    );
}
