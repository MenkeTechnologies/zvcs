//! `git --list-cmds=<group>[,<group>...]`, the query global every shell
//! completion drives off (`git-completion.bash:1261`, `:2136`, `:3812`,
//! `:3937`).
//!
//! Three kinds of assertion, because the groups fall into three kinds:
//!
//!   * **Documentation groups** (`list-<category>`) classify git's documented
//!     command set, so they must match stock **byte for byte** — a completion
//!     offering `scalar` on stock and not here would be a divergence in what
//!     the user can tab to. Every category git 2.55.0 knows is compared.
//!   * **Binary groups** (`builtins`, `main`, `others`) are facts about the
//!     running binary, and this binary serves a superset of git's verbs, so
//!     they are asserted structurally: every stock builtin present, the `z*`
//!     verbs present, the exec-path and `$PATH` scans really performed. A
//!     hermetic `$HOME`/`$PATH` is used so the scans see only what the fixture
//!     puts there.
//!   * **Failure shapes** are stock's, down to which part of the spec the
//!     message quotes: a bad top-level token is reported with the whole
//!     remainder of the spec, a bad `list-<category>` with the bare category.
//!
//! `--list-cmds=parseopt` is the one deliberate divergence and is asserted as
//! such, together with the invariant that keeps it honest: whatever it lists
//! must really answer `--git-completion-helper`.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");
const STOCK: &str = "/opt/homebrew/bin/git";

/// A repository with an isolated `$HOME` — `alias` and `config` read the
/// configuration, and the `main`/`others` scans read `$PATH`.
fn fixture(tag: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!("zvcs-listcmds-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let root = root.canonicalize().unwrap();
    std::fs::create_dir_all(root.join("home")).unwrap();
    let repo = root.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    assert!(run(&repo, &["init", "-q", "-b", "main"]).status.success());
    repo
}

fn home_of(repo: &Path) -> PathBuf {
    repo.parent().unwrap().join("home")
}

fn command(bin: &str, dir: &Path, args: &[&str]) -> Command {
    let mut cmd = Command::new(bin);
    cmd.args(args)
        .current_dir(dir)
        .env("HOME", home_of(dir))
        .env("XDG_CONFIG_HOME", home_of(dir).join(".config"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("ZVCS_HOME", home_of(dir))
        .env("LC_ALL", "C")
        .stdin(std::process::Stdio::null());
    cmd
}

fn run(dir: &Path, args: &[&str]) -> Output {
    command(BIN, dir, args).output().unwrap()
}

fn run_stock(dir: &Path, args: &[&str]) -> Output {
    command(STOCK, dir, args).output().unwrap()
}

fn lines(out: &Output) -> Vec<String> {
    String::from_utf8_lossy(&out.stdout).lines().map(str::to_owned).collect()
}

fn set_of(out: &Output) -> BTreeSet<String> {
    lines(out).into_iter().collect()
}

/// Whether the installed stock git can answer for us. The documentation groups
/// are pinned against it rather than against a copy of `command-list.txt`, so a
/// machine without it skips those comparisons instead of asserting stale bytes.
fn stock_available() -> bool {
    Command::new(STOCK).arg("--version").output().is_ok_and(|o| o.status.success())
}

/// Every `command-list.txt` category git 2.55.0 accepts after `list-`: the ten
/// type attributes, the two documentation ones, `guide`, `complete`, the five
/// common groups and `synchelpers`.
const CATEGORIES: &[&str] = &[
    "mainporcelain",
    "ancillarymanipulators",
    "ancillaryinterrogators",
    "foreignscminterface",
    "plumbingmanipulators",
    "plumbinginterrogators",
    "synchingrepositories",
    "synchelpers",
    "purehelpers",
    "userinterfaces",
    "developerinterfaces",
    "guide",
    "complete",
    "init",
    "worktree",
    "info",
    "history",
    "remote",
];

/// Each `list-<category>` is git's documented classification, so each answer
/// must be stock's — same names, same order, same bytes.
#[test]
fn every_documentation_category_matches_stock_byte_for_byte() {
    if !stock_available() {
        return;
    }
    let repo = fixture("categories");
    for cat in CATEGORIES {
        let spec = format!("--list-cmds=list-{cat}");
        let ours = run(&repo, &[&spec]);
        let stock = run_stock(&repo, &[&spec]);
        assert!(ours.status.success(), "list-{cat} failed: {}", String::from_utf8_lossy(&ours.stderr));
        assert_eq!(
            String::from_utf8_lossy(&ours.stdout),
            String::from_utf8_lossy(&stock.stdout),
            "list-{cat} diverges from stock"
        );
        assert!(!ours.stdout.is_empty(), "list-{cat} answered nothing");
    }
    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

/// Several groups accumulate into one list in spec order, which is also how the
/// completion script asks (`main,others,alias,nohelpers`). Concatenation, not
/// merging: the combined answer is the two answers back to back.
#[test]
fn groups_accumulate_in_spec_order() {
    let repo = fixture("accumulate");
    let porcelain = lines(&run(&repo, &["--list-cmds=list-mainporcelain"]));
    let guides = lines(&run(&repo, &["--list-cmds=list-guide"]));
    let both = lines(&run(&repo, &["--list-cmds=list-mainporcelain,list-guide"]));

    let mut expected = porcelain.clone();
    expected.extend(guides.clone());
    assert_eq!(both, expected);

    // Reversed spec, reversed output — nothing sorts the accumulated list.
    let reversed = lines(&run(&repo, &["--list-cmds=list-guide,list-mainporcelain"]));
    let mut expected_rev = guides;
    expected_rev.extend(porcelain);
    assert_eq!(reversed, expected_rev);
    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

/// `nohelpers` is a *filter over what came before*, not a group and not a
/// negation, so its position in the spec decides whether it does anything at
/// all. The names it drops are the ones spelled with `--`.
#[test]
fn nohelpers_filters_only_what_precedes_it() {
    let repo = fixture("nohelpers");
    let all = set_of(&run(&repo, &["--list-cmds=builtins"]));
    assert!(all.contains("submodule--helper"), "fixture assumption: a `--` helper is dispatched");

    let filtered = set_of(&run(&repo, &["--list-cmds=builtins,nohelpers"]));
    assert!(!filtered.iter().any(|n| n.contains("--")), "`--` helpers survived the filter");
    assert!(filtered.contains("commit"), "the filter removed ordinary commands");
    assert_eq!(
        all.iter().filter(|n| !n.contains("--")).count(),
        filtered.len(),
        "the filter removed more than the `--` helpers"
    );

    // The other order leaves the list untouched: there is nothing accumulated
    // yet when the filter runs.
    assert_eq!(set_of(&run(&repo, &["--list-cmds=nohelpers,builtins"])), all);

    // On its own it prints nothing at all, exit 0.
    let alone = run(&repo, &["--list-cmds=nohelpers"]);
    assert!(alone.status.success());
    assert!(alone.stdout.is_empty());
    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

/// `builtins` answers from the dispatch tables, so it must carry every verb
/// stock serves *and* the superset verbs this binary adds — the completion
/// script offers exactly what this prints.
#[test]
fn builtins_covers_stock_and_the_superset_verbs() {
    let repo = fixture("builtins");
    let ours = set_of(&run(&repo, &["--list-cmds=builtins"]));

    if stock_available() {
        let stock = set_of(&run_stock(&repo, &["--list-cmds=builtins"]));
        let missing: Vec<&String> = stock.difference(&ours).collect();
        assert!(missing.is_empty(), "stock builtins missing from this port's listing: {missing:?}");
    }

    for verb in ["zstatus", "zrepos", "zdaemon"] {
        assert!(ours.contains(verb), "superset verb {verb} is dispatched but not listed");
    }

    // Sorted and de-duplicated, like git's `commands[]` walk over an
    // alphabetical table.
    let listed = lines(&run(&repo, &["--list-cmds=builtins"]));
    let mut sorted = listed.clone();
    sorted.sort();
    sorted.dedup();
    assert_eq!(listed, sorted, "the builtins listing is not sorted/de-duplicated");
    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

/// `main` is the builtins *plus* a live scan of the exec-path, and `others` a
/// live scan of `$PATH` with everything `main` already covers excluded — the
/// split `load_command_list()` makes. Both scans are asserted with planted
/// executables so the answer cannot come from a table.
#[test]
fn main_and_others_scan_the_real_directories() {
    let repo = fixture("scan");
    let root = repo.parent().unwrap();
    let exec_dir = root.join("execdir");
    let path_dir = root.join("pathdir");
    std::fs::create_dir_all(&exec_dir).unwrap();
    std::fs::create_dir_all(&path_dir).unwrap();
    plant(&exec_dir.join("git-zvcsplanted-exec"));
    plant(&path_dir.join("git-zvcsplanted-path"));

    let out = command(BIN, &repo, &["--list-cmds=main"])
        .env("GIT_EXEC_PATH", &exec_dir)
        .env("PATH", &path_dir)
        .output()
        .unwrap();
    let main = set_of(&out);
    assert!(main.contains("zvcsplanted-exec"), "the exec-path scan did not run");
    assert!(!main.contains("zvcsplanted-path"), "a `$PATH` command leaked into `main`");
    assert!(main.contains("commit"), "`main` lost the builtins");
    // The superset verbs are builtins of this binary, so they are in `main`
    // whether or not `git zshadow` has installed their dashed links — a
    // completion built from `main,others` has to offer them.
    assert!(main.contains("zstatus"), "`main` omitted a superset verb");

    let out = command(BIN, &repo, &["--list-cmds=others"])
        .env("GIT_EXEC_PATH", &exec_dir)
        .env("PATH", &path_dir)
        .output()
        .unwrap();
    let others = set_of(&out);
    assert!(others.contains("zvcsplanted-path"), "the `$PATH` scan did not run");
    assert!(!others.contains("zvcsplanted-exec"), "an exec-path command leaked into `others`");
    assert!(!others.contains("commit"), "a builtin was listed as external");

    // A `git-z*` link on `$PATH` — what `git zdashed` installs — is this
    // binary's own verb, not an external command, so it must not be listed in
    // both groups.
    plant(&path_dir.join("git-zstatus"));
    let out = command(BIN, &repo, &["--list-cmds=others"])
        .env("GIT_EXEC_PATH", &exec_dir)
        .env("PATH", &path_dir)
        .output()
        .unwrap();
    assert!(!set_of(&out).contains("zstatus"), "a dashed superset link was listed as external");
    let _ = std::fs::remove_dir_all(root);
}

/// A no-op executable for the directory scans to find.
fn plant(path: &Path) {
    std::fs::write(path, "#!/bin/sh\nexit 0\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
}

/// `alias` prints the configured alias *names* with the expansion dropped, in
/// **configuration order** rather than sorted — `list_aliases()` is a config
/// callback that appends what it is handed, so a name defined twice is listed
/// twice and `-c` overrides land last. The same answer stock gives.
#[test]
fn alias_group_matches_stock() {
    let repo = fixture("alias");
    assert!(run(&repo, &["config", "alias.zz", "status"]).status.success());
    assert!(run(&repo, &["config", "alias.aa", "!echo hi"]).status.success());

    let ours = run(&repo, &["--list-cmds=alias"]);
    assert_eq!(lines(&ours), vec!["zz".to_string(), "aa".to_string()]);
    if stock_available() {
        assert_eq!(ours.stdout, run_stock(&repo, &["--list-cmds=alias"]).stdout);
    }

    // A redefinition is appended, not merged: `zz` twice, in file-then-`-c`
    // order, and an upper-case key is lower-cased on the way out.
    let args = ["-c", "alias.zz=diff", "-c", "alias.QQ=log", "--list-cmds=alias"];
    let dup = run(&repo, &args);
    assert_eq!(
        lines(&dup),
        vec!["zz".to_string(), "aa".to_string(), "zz".to_string(), "qq".to_string()]
    );
    if stock_available() {
        assert_eq!(dup.stdout, run_stock(&repo, &args).stdout);
    }
    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

/// `config` applies `completion.commands` as an edit script over whatever the
/// earlier tokens accumulated: a bare word adds, a `-`-prefixed word removes,
/// and the list is sorted and de-duplicated first. Pinned against stock over a
/// documentation group, where both binaries start from the same list.
#[test]
fn completion_commands_edits_the_accumulated_list() {
    let repo = fixture("config");
    let spec = "--list-cmds=list-mainporcelain,config";

    // Unset: the group is left exactly as it was.
    let untouched = run(&repo, &[spec]);
    assert_eq!(lines(&untouched), lines(&run(&repo, &["--list-cmds=list-mainporcelain"])));

    let args = ["-c", "completion.commands=zvcsadded -status", spec];
    let ours = run(&repo, &args);
    let listed = lines(&ours);
    assert!(listed.contains(&"zvcsadded".to_string()), "an added command is missing");
    assert!(!listed.contains(&"status".to_string()), "a `-`-prefixed command was not removed");
    let mut sorted = listed.clone();
    sorted.sort();
    assert_eq!(listed, sorted, "the edited list is not sorted");

    if stock_available() {
        assert_eq!(ours.stdout, run_stock(&repo, &args).stdout, "diverges from stock");
    }

    // On its own the group still applies the edit script — to an empty list, so
    // the additions are all that is left and the removals match nothing.
    let alone_args = ["-c", "completion.commands=zvcsadded -status", "--list-cmds=config"];
    let alone = run(&repo, &alone_args);
    assert!(alone.status.success());
    assert_eq!(lines(&alone), vec!["zvcsadded".to_string()]);
    if stock_available() {
        assert_eq!(alone.stdout, run_stock(&repo, &alone_args).stdout);
    }

    // Unset, it is a no-op even on its own.
    let unset = run(&repo, &["--list-cmds=config"]);
    assert!(unset.status.success());
    assert!(unset.stdout.is_empty());
    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

/// `deprecated` is git's `DEPRECATED`-flagged builtins, and this port serves
/// both of them, so the answer is stock's.
#[test]
fn deprecated_group_matches_stock() {
    let repo = fixture("deprecated");
    let ours = run(&repo, &["--list-cmds=deprecated"]);
    assert_eq!(lines(&ours), vec!["pack-redundant".to_string(), "whatchanged".to_string()]);
    if stock_available() {
        assert_eq!(ours.stdout, run_stock(&repo, &["--list-cmds=deprecated"]).stdout);
    }
    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

/// There is no negation syntax. `no-<group>` is not "everything but"; it is an
/// unknown token, and stock says so.
#[test]
fn unknown_tokens_die_the_way_stock_does() {
    let repo = fixture("errors");
    for spec in ["--list-cmds=no-main", "--list-cmds=nomain", "--list-cmds=bogus"] {
        let ours = run(&repo, &[spec]);
        let token = spec.trim_start_matches("--list-cmds=");
        assert_eq!(ours.status.code(), Some(128), "{spec} did not exit 128");
        assert_eq!(
            String::from_utf8_lossy(&ours.stderr),
            format!("fatal: unsupported command listing type '{token}'\n")
        );
        assert!(ours.stdout.is_empty(), "{spec} printed a listing anyway");
        if stock_available() {
            let stock = run_stock(&repo, &[spec]);
            assert_eq!(ours.stderr, stock.stderr, "{spec} diverges from stock");
            assert_eq!(ours.status.code(), stock.status.code());
        }
    }

    // The top-level `die()` quotes the *rest of the spec*, the category one
    // quotes the bare category — two different pointers in the C, and the
    // difference is visible.
    let ours = run(&repo, &["--list-cmds=bogus,main"]);
    assert_eq!(
        String::from_utf8_lossy(&ours.stderr),
        "fatal: unsupported command listing type 'bogus,main'\n"
    );
    let ours = run(&repo, &["--list-cmds=list-bogus,main"]);
    assert_eq!(
        String::from_utf8_lossy(&ours.stderr),
        "fatal: unsupported command listing type 'bogus'\n"
    );
    // A failing token stops the walk before anything is printed, even when an
    // earlier token succeeded.
    let ours = run(&repo, &["--list-cmds=list-guide,bogus"]);
    assert_eq!(ours.status.code(), Some(128));
    assert!(ours.stdout.is_empty());

    // An empty spec is not an error: nothing to walk, nothing to print.
    let empty = run(&repo, &["--list-cmds="]);
    assert!(empty.status.success());
    assert!(empty.stdout.is_empty() && empty.stderr.is_empty());
    if stock_available() {
        let stock = run_stock(&repo, &["--list-cmds="]);
        assert_eq!(empty.stdout, stock.stdout);
        assert_eq!(empty.status.code(), stock.status.code());
    }
    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

/// `parseopt` is the one group that cannot match stock: it names the commands
/// that answer `--git-completion-helper`, and this port implements that flag
/// nowhere, so the honest answer is the empty list. The invariant asserted here
/// is what keeps it honest — every name it *does* print must really answer.
#[test]
fn parseopt_lists_only_commands_that_answer_the_completion_helper() {
    let repo = fixture("parseopt");
    let out = run(&repo, &["--list-cmds=parseopt"]);
    assert!(out.status.success());

    let names: Vec<String> =
        String::from_utf8_lossy(&out.stdout).split_whitespace().map(str::to_owned).collect();
    for name in &names {
        let helper = run(&repo, &[name, "--git-completion-helper"]);
        assert!(
            helper.status.success(),
            "{name} is listed under parseopt but `--git-completion-helper` failed"
        );
        let text = String::from_utf8_lossy(&helper.stderr).into_owned();
        assert!(!text.contains("unsupported flag"), "{name} rejects --git-completion-helper: {text}");
    }

    // Today that leaves nothing to print — and stock's format is still matched
    // for the empty case: no trailing newline, exit 0.
    assert!(names.is_empty(), "the invariant above now covers a non-empty list: {names:?}");
    assert!(out.stdout.is_empty());
    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

/// `--list-cmds` names itself `_query_` to Trace2 before it answers
/// (`trace2_cmd_name("_query_")`, git.c:326), so a trace of a completion run
/// shows the query rather than an unnamed command.
#[test]
fn the_query_names_itself_to_trace2() {
    let repo = fixture("trace2");
    let log = repo.parent().unwrap().join("trace2.json");
    let out = command(BIN, &repo, &["--list-cmds=deprecated"])
        .env("GIT_TRACE2_EVENT", &log)
        .output()
        .unwrap();
    assert!(out.status.success());

    let text = std::fs::read_to_string(&log).unwrap();
    let cmd_name = text
        .lines()
        .find(|l| l.contains("\"event\":\"cmd_name\""))
        .expect("no cmd_name record was written");
    assert!(cmd_name.contains("\"name\":\"_query_\""), "cmd_name record: {cmd_name}");
    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}
