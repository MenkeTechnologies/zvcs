//! `pretty.<name>` — the user-defined `--pretty`/`--format` names, and the name
//! resolution every command sharing git's pretty machinery performs.
//!
//! Guards the port of `pretty.c`'s format table (`git_pretty_formats_config`,
//! `setup_commit_formats`, `find_commit_format_recursive`, `get_commit_format`).
//! Three things here are easy to get wrong and are each pinned to bytes captured
//! from stock git 2.55.0 on this same fixture:
//!
//!   * lookup is a **case-insensitive shortest-prefix** match against the whole
//!     table, not an equality test — `--pretty=one` is `oneline`, `--pretty=r` is
//!     `raw` rather than the longer `reference`, `--pretty=f` is `full` rather than
//!     `fuller`;
//!   * a `pretty.<name>` value with no `%` in it is an **alias** to another format
//!     name, followed recursively, and a loop is a `die()` naming the format;
//!   * a `format:`/`tformat:` prefix **inside the config value** decides whether
//!     the format separates records or terminates them.
//!
//! Every assertion runs against the zvcs binary alone, so the suite needs no
//! stock git on PATH.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

/// A fixed author *and* committer date, so the commit ids the built-in formats
/// print are the same on every run and on every machine.
const DATE: &str = "1136214245 +0000";

/// The fixture's identity and clock, pinned against a CI runner's own
/// `GIT_AUTHOR_*`/`GIT_COMMITTER_*` and against any user or system config.
fn cmd(dir: &Path, home: &Path, args: &[&str]) -> Command {
    let mut c = Command::new(BIN);
    c.args(args)
        .current_dir(dir)
        .env("HOME", home)
        .env("ZVCS_HOME", home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "Alice")
        .env("GIT_AUTHOR_EMAIL", "alice@example.com")
        .env("GIT_COMMITTER_NAME", "Alice")
        .env("GIT_COMMITTER_EMAIL", "alice@example.com")
        .env("GIT_AUTHOR_DATE", DATE)
        .env("GIT_COMMITTER_DATE", DATE);
    c
}

/// A repo with two commits, `c0` (creates `f`) and `c1` (modifies it).
fn fixture(tag: &str) -> (PathBuf, PathBuf) {
    let root = std::env::temp_dir().join(format!("zvcs-prettycfg-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let home = root.join("home");
    std::fs::create_dir_all(&home).unwrap();
    let repo = root.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    let root = root.canonicalize().unwrap();
    let (home, repo) = (root.join("home"), root.join("repo"));
    let run = |args: &[&str]| {
        let o = cmd(&repo, &home, args).output().unwrap();
        assert!(
            o.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&o.stderr)
        );
    };
    run(&["init", "-q", "-b", "main"]);
    run(&["config", "user.email", "alice@example.com"]);
    run(&["config", "user.name", "Alice"]);
    std::fs::write(repo.join("f"), "hello\n").unwrap();
    run(&["add", "f"]);
    run(&["-c", "commit.gpgsign=false", "commit", "-q", "-m", "c0"]);
    std::fs::write(repo.join("f"), "hello\nworld\n").unwrap();
    run(&["add", "f"]);
    run(&["-c", "commit.gpgsign=false", "commit", "-q", "-m", "c1"]);
    (repo, home)
}

fn run(repo: &Path, home: &Path, args: &[&str]) -> Output {
    cmd(repo, home, args).output().unwrap()
}

fn out(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

fn err(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

fn cleanup(repo: &Path) {
    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

/// The 40-hex object name a built-in format prints, split off its trailing text.
fn split_oid(line: &str) -> (&str, &str) {
    let (oid, rest) = line.split_at(40);
    assert!(
        oid.len() == 40 && oid.bytes().all(|b| b.is_ascii_hexdigit()),
        "expected a 40-hex object name, got {oid:?} in {line:?}"
    );
    (oid, rest)
}

#[test]
fn user_format_is_resolved_for_both_pretty_and_format() {
    let (repo, home) = fixture("user");
    // stock: `git -c pretty.custom='%s %an' log -1 --pretty=custom` -> "c1 Alice\n"
    for opt in ["--pretty=custom", "--format=custom"] {
        let o = run(&repo, &home, &["-c", "pretty.custom=%s %an", "log", "-1", opt]);
        assert!(o.status.success(), "{opt}: {}", err(&o));
        assert_eq!(out(&o), "c1 Alice\n", "{opt}");
    }
    cleanup(&repo);
}

#[test]
fn user_format_name_matches_case_blind_and_by_prefix() {
    let (repo, home) = fixture("names");
    let get = |name: &str| run(&repo, &home, &["-c", "pretty.custom=%s", "log", "-1", &format!("--pretty={name}")]);

    // A prefix of the configured name resolves, and case is ignored on both sides.
    for name in ["custom", "CUSTOM", "cus", "c"] {
        let o = get(name);
        assert!(o.status.success(), "--pretty={name}: {}", err(&o));
        assert_eq!(out(&o), "c1\n", "--pretty={name}");
    }
    // The table entry must *start with* what was typed, so a longer value is not a
    // match: stock says `fatal: invalid --pretty format: customx`.
    let o = get("customx");
    assert_eq!(o.status.code(), Some(128));
    assert_eq!(err(&o), "fatal: invalid --pretty format: customx\n");

    // git lower-cases the config key, so `[pretty] Custom` defines `custom`.
    let o = run(&repo, &home, &["-c", "pretty.Custom=%s", "log", "-1", "--pretty=custom"]);
    assert!(o.status.success(), "{}", err(&o));
    assert_eq!(out(&o), "c1\n");
    cleanup(&repo);
}

#[test]
fn builtin_names_resolve_by_shortest_prefix() {
    let (repo, home) = fixture("builtin");
    let get = |name: &str| {
        let o = run(&repo, &home, &["log", "-1", &format!("--pretty={name}")]);
        assert!(o.status.success(), "--pretty={name}: {}", err(&o));
        out(&o)
    };

    // `oneline` is the only name starting with "one"; case is ignored.
    let oneline = get("oneline");
    let (_, subject) = split_oid(oneline.trim_end());
    assert_eq!(subject, " c1", "oneline body: {oneline:?}");
    assert_eq!(get("one"), oneline);
    assert_eq!(get("ONELINE"), oneline);

    // `r` starts both `raw` and `reference`; the shorter name wins.
    assert_eq!(get("r"), get("raw"));
    assert!(get("r").starts_with("commit "), "raw: {:?}", get("r"));
    assert!(get("r").contains("\ntree "), "raw prints the object header");

    // `f` starts both `full` and `fuller`; `full` is shorter, and the two differ —
    // `fuller` is the one with the AuthorDate/CommitDate pair.
    assert_eq!(get("f"), get("full"));
    assert!(get("full").contains("\nCommit: "), "full: {:?}", get("full"));
    assert!(!get("full").contains("AuthorDate:"), "full is not fuller");
    assert!(get("fuller").contains("AuthorDate:"), "fuller: {:?}", get("fuller"));

    // `m` starts `medium` and `mboxrd`, which are the same length; the table order
    // decides, and `medium` comes first.
    assert_eq!(get("m"), get("medium"));
    assert_eq!(get("med"), get("medium"));
    assert!(get("medium").contains("\nDate:   "), "medium: {:?}", get("medium"));
    assert_eq!(get("Full"), get("full"), "built-in names are matched case-blind");
    cleanup(&repo);
}

#[test]
fn config_value_prefix_selects_separator_or_terminator() {
    let (repo, home) = fixture("term");
    let get = |key: &str, name: &str| {
        let o = run(&repo, &home, &["-c", key, "log", &format!("--pretty={name}")]);
        assert!(o.status.success(), "{key}: {}", err(&o));
        out(&o)
    };
    // stock, on this fixture's two commits:
    //   pretty.sep   = format:%s   -> "c1\nc0"    (separator: no trailing newline)
    //   pretty.term  = tformat:%s  -> "c1\nc0\n"  (terminator)
    //   pretty.plain = %s          -> "c1\nc0\n"  (a bare value is a terminator too)
    assert_eq!(get("pretty.sep=format:%s", "sep"), "c1\nc0");
    assert_eq!(get("pretty.term=tformat:%s", "term"), "c1\nc0\n");
    assert_eq!(get("pretty.plain=%s", "plain"), "c1\nc0\n");
    cleanup(&repo);
}

#[test]
fn a_percent_less_value_is_an_alias_and_chains() {
    let (repo, home) = fixture("alias");
    // An alias may name a built-in…
    let mine = run(&repo, &home, &["-c", "pretty.mine=oneline", "log", "-1", "--pretty=mine"]);
    assert!(mine.status.success(), "{}", err(&mine));
    let plain = run(&repo, &home, &["log", "-1", "--pretty=oneline"]);
    assert_eq!(out(&mine), out(&plain), "pretty.mine=oneline must be `oneline`");

    // …a prefix of one…
    let z = run(&repo, &home, &["-c", "pretty.z=onel", "log", "-1", "--pretty=z"]);
    assert_eq!(out(&z), out(&plain), "an alias target is resolved by prefix too");

    // …or another user format, followed as far as it goes.
    let chain = run(
        &repo,
        &home,
        &[
            "-c", "pretty.a=b",
            "-c", "pretty.b=c",
            "-c", "pretty.c=%s",
            "log", "-1", "--pretty=a",
        ],
    );
    assert!(chain.status.success(), "{}", err(&chain));
    assert_eq!(out(&chain), "c1\n");
    cleanup(&repo);
}

#[test]
fn alias_loops_die_with_gits_self_reference_message() {
    let (repo, home) = fixture("loop");
    // Each of these is a loop for a different reason, and stock reports all four
    // with the same sentence, naming the format the *user typed*:
    //   pretty.a=b + pretty.b=a  — a two-step ring
    //   pretty.a=a               — a direct self-reference
    //   pretty.a=A               — a self-reference found by the case-blind match
    //   pretty.e=                — an empty value, and every name starts with ""
    let cases: [(&[&str], &str); 4] = [
        (&["-c", "pretty.a=b", "-c", "pretty.b=a"], "a"),
        (&["-c", "pretty.a=a"], "a"),
        (&["-c", "pretty.a=A"], "a"),
        (&["-c", "pretty.e="], "e"),
    ];
    for (cfg, name) in cases {
        let mut args: Vec<&str> = cfg.to_vec();
        let opt = format!("--pretty={name}");
        args.extend_from_slice(&["log", "-1", &opt]);
        let o = run(&repo, &home, &args);
        assert_eq!(o.status.code(), Some(128), "{cfg:?}: {}", out(&o));
        assert_eq!(
            err(&o),
            format!(
                "fatal: invalid --pretty format: '{name}' references an alias which points to itself\n"
            ),
            "{cfg:?}"
        );
    }
    cleanup(&repo);
}

#[test]
fn builtin_names_cannot_be_shadowed() {
    let (repo, home) = fixture("shadow");
    // `git_pretty_formats_config()` drops a `pretty.<name>` whose name collides
    // with a built-in, so the built-in printer still runs.
    for name in ["oneline", "medium", "raw"] {
        let o = run(
            &repo,
            &home,
            &["-c", &format!("pretty.{name}=SHADOW %s"), "log", "-1", &format!("--pretty={name}")],
        );
        assert!(o.status.success(), "{name}: {}", err(&o));
        let shadowed = out(&o);
        assert!(!shadowed.contains("SHADOW"), "pretty.{name} must be ignored: {shadowed:?}");
        let plain = run(&repo, &home, &["log", "-1", &format!("--pretty={name}")]);
        assert_eq!(shadowed, out(&plain), "pretty.{name} changed the built-in");
    }
    // The name is lower-cased before the collision test, so an upper-case spelling
    // is dropped too.
    let o = run(&repo, &home, &["-c", "pretty.ONELINE=SHADOW %s", "log", "-1", "--pretty=ONELINE"]);
    assert!(!out(&o).contains("SHADOW"), "pretty.ONELINE must be ignored: {:?}", out(&o));
    cleanup(&repo);
}

#[test]
fn a_shorter_user_name_wins_the_prefix_race() {
    let (repo, home) = fixture("race");
    // `pretty.o` is one character and `oneline` is seven, so `--pretty=o` prefers
    // the user format — while the built-in's own full name still resolves to it.
    let o = run(&repo, &home, &["-c", "pretty.o=USER %s", "log", "-1", "--pretty=o"]);
    assert!(o.status.success(), "{}", err(&o));
    assert_eq!(out(&o), "USER c1\n");

    let one = run(&repo, &home, &["-c", "pretty.o=USER %s", "log", "-1", "--pretty=oneline"]);
    let one = out(&one);
    let (_, subject) = split_oid(one.trim_end());
    assert_eq!(subject, " c1", "--pretty=oneline must stay the built-in");
    cleanup(&repo);
}

#[test]
fn an_unknown_name_is_still_fatal_and_config_round_trips() {
    let (repo, home) = fixture("misc");
    let o = run(&repo, &home, &["-c", "pretty.custom=%s", "log", "-1", "--pretty=nosuch"]);
    assert_eq!(o.status.code(), Some(128));
    assert_eq!(err(&o), "fatal: invalid --pretty format: nosuch\n");

    // The key is an ordinary string as far as `git config` is concerned: the value
    // comes back exactly as it went in, `%` placeholders and all.
    let o = run(&repo, &home, &["-c", "pretty.x=%s %an", "config", "--get", "pretty.x"]);
    assert!(o.status.success(), "{}", err(&o));
    assert_eq!(out(&o), "%s %an\n");

    // The last definition supplies the value.
    let o = run(&repo, &home, &["-c", "pretty.x=%H", "-c", "pretty.x=%s", "log", "-1", "--pretty=x"]);
    assert!(o.status.success(), "{}", err(&o));
    assert_eq!(out(&o), "c1\n");
    cleanup(&repo);
}

#[test]
fn every_verb_sharing_the_pretty_machinery_reads_the_config() {
    let (repo, home) = fixture("verbs");
    let cfg = "pretty.custom=%s";
    // Each expectation is stock git 2.55.0's output on this fixture. `rev-list`
    // prints its own object-name line above the format; `diff-tree` and
    // `whatchanged` print the format and then the raw record, with the blank line
    // `log_tree_diff_flush()` puts between them; `shortlog` groups by author.
    let go = |args: &[&str]| {
        let mut a = vec!["-c", cfg];
        a.extend_from_slice(args);
        let o = run(&repo, &home, &a);
        assert!(o.status.success(), "{args:?}: {}", err(&o));
        out(&o)
    };
    assert_eq!(go(&["log", "-1", "--pretty=custom"]), "c1\n");
    assert_eq!(go(&["show", "-s", "--pretty=custom"]), "c1\n");
    assert_eq!(go(&["shortlog", "HEAD", "--pretty=custom"]), "Alice (2):\n      c0\n      c1\n\n");
    assert_eq!(go(&["reflog", "--pretty=custom"]), "c1\nc0\n");
    assert_eq!(go(&["log", "-g", "--pretty=custom"]), "c1\nc0\n");

    let revlist = go(&["rev-list", "-1", "HEAD", "--pretty=custom"]);
    let (head, body) = revlist.split_once('\n').expect("two lines");
    assert_eq!(&head[..7], "commit ");
    split_oid(&head[7..]);
    assert_eq!(body, "c1\n");

    for verb in [
        vec!["diff-tree", "HEAD", "--pretty=custom"],
        vec!["whatchanged", "--i-still-use-this", "-1", "--pretty=custom"],
    ] {
        let text = go(&verb);
        assert!(text.starts_with("c1\n\n:"), "{verb:?} body: {text:?}");
        assert!(text.trim_end().ends_with("M\tf"), "{verb:?} raw record: {text:?}");
    }
    cleanup(&repo);
}
