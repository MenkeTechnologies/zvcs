//! `advise_if_enabled()` prints its `Disable this message with …` trailer only
//! while the slot is *unconfigured* — `vadvise()` is handed
//! `!advice_setting[type].level` as `display_instructions` (advice.c:157), and an
//! explicit `advice.<slot> = true` raises that level to `ADVICE_LEVEL_ENABLED`.
//! So `advice.<slot>=true` keeps the hint and drops the trailer, while unset
//! keeps both. Every site that spells the trailer out by hand instead of going
//! through the shared gate gets this wrong in the same direction: it prints the
//! trailer unconditionally.
//!
//! Every command — fixture setup included — runs in a hermetic environment: its
//! own `HOME` and `XDG_CONFIG_HOME`, `GIT_CONFIG_GLOBAL=/dev/null` and
//! `GIT_CONFIG_NOSYSTEM`, so no user excludes file or advice setting reaches it.
//! Each fixture is built twice, by zvcs and by stock git when one is installed:
//! every command under test must exit as stock does, and every text this file
//! looks for must be present or absent in zvcs's stderr exactly as in stock's.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

#[path = "support/stock_git.rs"]
mod stock_git;
use stock_git::stock_git;

const BIN: &str = env!("CARGO_BIN_EXE_git");

/// One copy of a fixture: the binary that builds and drives it, its repository
/// and its private `HOME`.
struct Side {
    bin: &'static str,
    root: PathBuf,
    repo: PathBuf,
    home: PathBuf,
}

impl Side {
    fn new(bin: &'static str, tag: &str) -> Self {
        let side = if bin == BIN { "zvcs" } else { "stock" };
        let root = std::env::temp_dir()
            .join(format!("zvcs-advtrail-{tag}-{side}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("home")).unwrap();
        std::fs::create_dir_all(root.join("repo")).unwrap();
        let root = root.canonicalize().unwrap();
        let (repo, home) = (root.join("repo"), root.join("home"));
        Side { bin, root, repo, home }
    }

    fn command(&self, dir: &Path, args: &[&str]) -> Command {
        let mut cmd = Command::new(self.bin);
        cmd.args(args)
            .current_dir(dir)
            .env("HOME", &self.home)
            .env("ZVCS_HOME", &self.home)
            .env("XDG_CONFIG_HOME", self.home.join(".config"))
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("LC_ALL", "C")
            .env_remove("GIT_ADVICE")
            .stdin(std::process::Stdio::null());
        cmd
    }

    fn run_in(&self, dir: &Path, args: &[&str]) -> Output {
        self.command(dir, args).output().unwrap()
    }

    fn git(&self, args: &[&str]) {
        let out = self.run_in(&self.repo, args);
        assert!(
            out.status.success(),
            "{} {args:?} failed: {}",
            self.bin,
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

impl Drop for Side {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// The zvcs copy of a fixture and, when stock git is installed, the stock one.
struct Fixture {
    zvcs: Side,
    stock: Option<Side>,
}

impl Fixture {
    fn sides(&self) -> impl Iterator<Item = &Side> {
        std::iter::once(&self.zvcs).chain(self.stock.iter())
    }

    /// Fixture setup, on both copies.
    fn git(&self, args: &[&str]) {
        for side in self.sides() {
            side.git(args);
        }
    }

    fn write(&self, rel: &str, body: &str) {
        for side in self.sides() {
            let path = side.repo.join(rel);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, body).unwrap();
        }
    }

    /// The command under test, in directory `rel` of each copy. The exit code
    /// must be stock's.
    fn run_in(&self, rel: &str, args: &[&str]) -> Ran {
        let got = self.zvcs.run_in(&self.zvcs.repo.join(rel), args);
        let stock = self.stock.as_ref().map(|stock| {
            let want = stock.run_in(&stock.repo.join(rel), args);
            assert_eq!(got.status.code(), want.status.code(), "{args:?}: exit code must match stock");
            err_of(&want)
        });
        Ran { text: err_of(&got), stock, code: got.status.code() }
    }

    fn run(&self, args: &[&str]) -> Ran {
        self.run_in("", args)
    }
}

/// zvcs's stderr for one command, beside stock git's when one is installed.
struct Ran {
    text: String,
    stock: Option<String>,
    code: Option<i32>,
}

impl Ran {
    /// Whether the advice text holds `needle` — the answer stock git gives too.
    fn has(&self, needle: &str) -> bool {
        let got = self.text.contains(needle);
        if let Some(stock) = &self.stock {
            assert_eq!(
                got,
                stock.contains(needle),
                "presence of {needle:?} must match stock\nzvcs:\n{}\nstock:\n{stock}",
                self.text
            );
        }
        got
    }

    /// Whether every line satisfies `pred`, in zvcs's output and in stock's alike.
    fn all_lines(&self, pred: impl Fn(&str) -> bool) -> bool {
        let got = self.text.lines().all(&pred);
        if let Some(stock) = &self.stock {
            assert_eq!(got, stock.lines().all(&pred), "zvcs:\n{}\nstock:\n{stock}", self.text);
        }
        got
    }
}

impl std::fmt::Display for Ran {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.text)
    }
}

fn err_of(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

/// An empty pair of copies, nothing initialised.
fn empty(tag: &str) -> Fixture {
    Fixture {
        zvcs: Side::new(BIN, tag),
        stock: stock_git().map(|bin| Side::new(bin, tag)),
    }
}

/// A repository with one commit and `main` as the branch, so nothing here
/// depends on the compiled-in initial-branch default.
fn fixture(tag: &str) -> Fixture {
    let f = empty(tag);
    f.git(&["init", "-q", "-b", "main"]);
    f.git(&["config", "user.email", "t@e.x"]);
    f.git(&["config", "user.name", "t"]);
    f.write("a.txt", "a\n");
    f.git(&["add", "a.txt"]);
    f.git(&["commit", "-qm", "one"]);
    f
}

/// The three states of one slot, over one command: the hint body must survive an
/// explicit `true`, the trailer must not, and `false` must take both away.
fn assert_trailer_tracks_configuration(f: &Fixture, slot: &str, body: &str, args: &[&str]) {
    let trailer = format!("Disable this message with \"git config set {slot} false\"");

    let err = f.run(args);
    assert!(err.has(body), "{slot}: hint must show while unconfigured:\n{err}");
    assert!(err.has(&trailer), "{slot}: unconfigured slot must carry the trailer:\n{err}");

    let mut on = vec!["-c".to_string(), format!("{slot}=true")];
    on.extend(args.iter().map(|a| a.to_string()));
    let on: Vec<&str> = on.iter().map(String::as_str).collect();
    let err = f.run(&on);
    assert!(err.has(body), "{slot}: explicit true must keep the hint:\n{err}");
    assert!(!err.has(&trailer), "{slot}: a configured slot must drop the trailer:\n{err}");

    let mut off = vec!["-c".to_string(), format!("{slot}=false")];
    off.extend(args.iter().map(|a| a.to_string()));
    let off: Vec<&str> = off.iter().map(String::as_str).collect();
    let err = f.run(&off);
    assert!(!err.has(body), "{slot}: false must suppress the hint:\n{err}");
    assert!(!err.has(&trailer), "{slot}: false must suppress the trailer too:\n{err}");
}

/// `cmd_add()`'s `Nothing specified, nothing added.` (builtin/add.c:466-471) is a
/// plain stderr line; only the `git add .` suggestion is the hint. `git stage`
/// is the same code path and must answer identically.
#[test]
fn add_empty_pathspec_trailer_tracks_configuration() {
    let f = fixture("emptypathspec");
    for verb in ["add", "stage"] {
        assert_trailer_tracks_configuration(
            &f,
            "advice.addEmptyPathspec",
            "Maybe you wanted to say 'git add .'?",
            &[verb],
        );
        let err = f.run(&["-c", "advice.addEmptyPathspec=true", verb]);
        assert!(
            err.has("Nothing specified, nothing added."),
            "{verb}: the non-hint line is not gated:\n{err}"
        );
    }
}

/// `add_files()` (builtin/add.c:347-353): the preamble and the path list are
/// plain stderr writes, the `Use -f` line is the only `advise_if_enabled()`.
#[test]
fn add_ignored_file_trailer_tracks_configuration() {
    let f = fixture("ignoredfile");
    f.write(".gitignore", "ign\n");
    f.git(&["add", ".gitignore"]);
    f.git(&["commit", "-qm", "ig"]);
    f.write("ign", "z\n");

    assert_trailer_tracks_configuration(
        &f,
        "advice.addIgnoredFile",
        "Use -f if you really want to add them.",
        &["add", "ign"],
    );
    let err = f.run(&["-c", "advice.addIgnoredFile=false", "add", "ign"]);
    assert!(
        err.has("The following paths are ignored by one of your .gitignore files:")
            && err.has("ign"),
        "the report itself is not gated on the slot:\n{err}"
    );
}

/// A sparse-checkout fixture: `in/` is in the cone, `out/` is not, so
/// `sparse-checkout set in` takes `out/` out of the work tree.
fn sparse_fixture(tag: &str) -> Fixture {
    let f = fixture(tag);
    f.write("in/f", "i\n");
    f.write("out/f", "o\n");
    f.git(&["add", "."]);
    f.git(&["commit", "-qm", "two"]);
    f.git(&["sparse-checkout", "init", "--cone", "--sparse-index"]);
    f.git(&["sparse-checkout", "set", "in"]);
    f
}

/// `advise_on_updating_sparse_paths()` (advice.c:262-272): the three-line
/// preamble and the path list print unconditionally, the closing block is the
/// hint.
#[test]
fn update_sparse_path_trailer_tracks_configuration() {
    let f = sparse_fixture("updatesparse");
    // A file written back at an out-of-cone path.
    f.write("out/f", "z\n");

    assert_trailer_tracks_configuration(
        &f,
        "advice.updateSparsePath",
        "If you intend to update such entries, try one of the following:",
        &["add", "out/f"],
    );
    let err = f.run(&["-c", "advice.updateSparsePath=false", "add", "out/f"]);
    assert!(
        err.has("outside of your sparse-checkout definition, so will not be"),
        "the report itself is not gated on the slot:\n{err}"
    );
    assert_eq!(err.code, Some(1), "advice must not move the exit code:\nerr:\n{err}");
}

/// `die_user_resolve()` (builtin/am.c:1161-1184) composes the whole block into
/// one `strbuf` and passes it to a *single* `advise_if_enabled()`, so one trailer
/// decision covers all of the lines — and the `--allow-empty` line inside, the
/// one part gated on `advice.amWorkDir`, does not get a trailer of its own.
///
/// A failed `git am` leaves `.git/rebase-apply` behind, so each state is run on
/// a freshly aborted tree.
#[test]
fn am_merge_conflict_trailer_tracks_configuration() {
    let f = fixture("amresolve");
    f.write("a.txt", "a\nb\n");
    f.git(&["commit", "-qam", "two"]);
    f.git(&["format-patch", "-q", "-1", "-o", "."]);
    let patch = std::fs::read_dir(&f.zvcs.repo)
        .unwrap()
        .filter_map(Result::ok)
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .find(|n| n.ends_with(".patch"))
        .expect("format-patch wrote a patch");

    const BODY: &str = "When you have resolved this problem, run \"git am --continue\".";
    const TRAILER: &str = "Disable this message with \"git config set advice.mergeConflict false\"";

    let attempt = |extra: &[&str]| -> Ran {
        f.write("a.txt", "a\ndirty\n");
        let mut args: Vec<&str> = extra.to_vec();
        args.extend_from_slice(&["am", &patch]);
        let out = f.run(&args);
        for side in f.sides() {
            let _ = side.run_in(&side.repo, &["am", "--abort"]);
        }
        out
    };

    let err = attempt(&[]);
    assert!(err.has(BODY), "unconfigured slot must print the block:\n{err}");
    assert!(err.has(TRAILER), "unconfigured slot must carry the trailer:\n{err}");
    assert_eq!(err.code, Some(128), "exit code is the `die(NULL)`:\n{err}");

    let err = attempt(&["-c", "advice.mergeConflict=true"]);
    assert!(err.has(BODY), "explicit true must keep the block:\n{err}");
    assert!(!err.has(TRAILER), "a configured slot must drop the trailer:\n{err}");

    let err = attempt(&["-c", "advice.mergeConflict=false"]);
    assert!(!err.has(BODY), "false must suppress the block:\n{err}");
    assert!(!err.has(TRAILER), "false must suppress the trailer too:\n{err}");
    assert!(
        !err.has("--abort") && !err.has("--skip"),
        "no part of the die_user_resolve block may survive:\n{err}"
    );
    // The `Use 'git am --show-current-patch=diff'` line is a different slot
    // (`advice.amWorkDir`, builtin/am.c:1913-1914) and is unaffected.
    assert!(
        err.has("Use 'git am --show-current-patch=diff' to see the failed patch"),
        "advice.mergeConflict must not reach the amWorkDir hint:\n{err}"
    );
    assert_eq!(err.code, Some(128), "advice must not move the exit code:\n{err}");
}

/// `repo_default_branch_name()` (refs.c:703-712) hints only when it reaches its
/// compiled-in fallback: an explicit `-b`, a configured `init.defaultBranch` and
/// `git init -q` each return before the `advise_if_enabled()`.
#[test]
fn default_branch_name_hint_fires_only_on_the_compiled_in_fallback() {
    let f = empty("defbranch");

    const FIRST: &str = "Using 'master' as the name for the initial branch. This default branch name";
    const LAST: &str = "\tgit branch -m <name>";
    const TRAILER: &str =
        "Disable this message with \"git config set advice.defaultBranchName false\"";

    let init = |args: &[&str], tag: &str| -> Ran {
        for side in f.sides() {
            std::fs::create_dir_all(side.repo.join(tag)).unwrap();
        }
        let mut a: Vec<&str> = args.to_vec();
        a.push(".");
        f.run_in(tag, &a)
    };

    let err = init(&["init"], "plain");
    for line in [FIRST, "\tgit config --global init.defaultBranch <name>", LAST, TRAILER] {
        assert!(err.has(line), "fallback init must hint {line:?}:\n{err}");
    }
    assert!(
        err.all_lines(|l| l.starts_with("hint:")),
        "every advice line carries the hint: prefix:\n{err}"
    );
    // `vadvise()` walks one buffer, so the body's trailing newline is what puts a
    // bare `hint:` between the block and the trailer.
    assert!(err.has("\nhint:\nhint: Disable this message"), "blank hint line:\n{err}");

    let err = init(&["-c", "advice.defaultBranchName=true", "init"], "on");
    assert!(err.has(FIRST) && err.has(LAST), "explicit true keeps the hint:\n{err}");
    assert!(!err.has(TRAILER), "a configured slot drops the trailer:\n{err}");

    let err = init(&["-c", "advice.defaultBranchName=false", "init"], "off");
    assert!(!err.has("hint:"), "false suppresses the whole block:\n{err}");

    for (args, tag, why) in [
        (vec!["init", "-q"], "quiet", "-q passes quiet=1 to repo_default_branch_name"),
        (vec!["init", "-b", "zz"], "explicitb", "-b returns before the fallback"),
        (
            vec!["-c", "init.defaultBranch=qq", "init"],
            "configured",
            "a configured init.defaultBranch returns before the fallback",
        ),
    ] {
        let err = init(&args, tag);
        assert!(!err.has("hint:"), "{why}:\n{err}");
    }

    for side in f.sides() {
        let dir = side.repo.join("envoff");
        std::fs::create_dir_all(&dir).unwrap();
        let out = side.command(&dir, &["init", "."]).env("GIT_ADVICE", "0").output().unwrap();
        assert!(!err_of(&out).contains("hint:"), "{}: GIT_ADVICE=0 squelches it:\n{}", side.bin, err_of(&out));
    }
}
