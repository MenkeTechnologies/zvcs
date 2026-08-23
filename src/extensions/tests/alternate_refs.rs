//! `core.alternateRefsCommand` and `core.alternateRefsPrefixes` — the two keys
//! that decide what a repository learns about its lenders' history, reached
//! through `git rev-list --alternate-refs`.
//!
//! `add_alternate_refs_to_pending()` (`revision.c:1878-1886`) is the whole of the
//! `--alternate-refs` pseudo-option, and it delegates to
//! `odb_for_each_alternate_ref()` (`odb.c:463-470`), which for each alternate
//! runs a child process and reads object ids off its stdout. The command is
//! built by `fill_alternate_refs_command()` (`odb.c:371-397`):
//!
//! ```c
//! if (!repo_config_get_value(repo, "core.alternateRefsCommand", &value)) {
//!         cmd->use_shell = 1;
//!
//!         strvec_push(&cmd->args, value);
//!         strvec_push(&cmd->args, repo_path);
//! } else {
//!         cmd->git_cmd = 1;
//!
//!         strvec_pushf(&cmd->args, "--git-dir=%s", repo_path);
//!         strvec_push(&cmd->args, "for-each-ref");
//!         strvec_push(&cmd->args, "--format=%(objectname)");
//!
//!         if (!repo_config_get_value(repo, "core.alternateRefsPrefixes", &value)) {
//!                 strvec_push(&cmd->args, "--");
//!                 strvec_split(&cmd->args, value);
//!         }
//! }
//!
//! strvec_pushv(&cmd->env, (const char **)local_repo_env);
//! ```
//!
//! Every expectation below was captured from stock git 2.55.0 against the same
//! fixture this file builds: three repositories, the borrower listing two of them
//! plus a bare object directory that is not a repository at all.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("zvcs-alternate-refs-it-{name}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create scratch dir");
    dir
}

/// The PATH with this repository's shim removed, so a nested call cannot reach
/// the installed `zvcs` instead of the binary under test.
fn real_git_path() -> String {
    std::env::var("PATH")
        .unwrap_or_default()
        .split(':')
        .filter(|p| !p.contains(".zvcs"))
        .collect::<Vec<_>>()
        .join(":")
}

fn git_in(dir: &Path, args: &[&str]) -> Output {
    let mut cmd = Command::new(BIN);
    cmd.args(args)
        .env("PATH", real_git_path())
        .env("HOME", dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@e.x")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@e.x")
        // Equal commit dates keep the walk's order the order the tips were
        // queued in, which is the property the source-order test asserts.
        .env("GIT_AUTHOR_DATE", "1700000000 +0000")
        .env("GIT_COMMITTER_DATE", "1700000000 +0000")
        .current_dir(dir);
    cmd.output().expect("run the binary under test")
}

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

fn oid(dir: &Path, rev: &str) -> String {
    stdout(&git_in(dir, &["rev-parse", rev])).trim().to_string()
}

/// The lenders and the borrower.
struct Fixture {
    root: PathBuf,
    borrower: PathBuf,
    /// `donor`'s second commit, which `refs/heads/main` and `refs/heads/side`
    /// both point at.
    d2: String,
    /// `donor`'s first commit, reached only through the annotated tag `at1`.
    d1: String,
    /// `donor2`'s only commit.
    e1: String,
}

/// Three repositories and a bare object directory:
///
/// * `donor` — `main` and `side` at `d2`, annotated tag `at1` at `d1`.
/// * `loose/objects` — an object directory with no repository around it, which
///   `refs_from_alternate_cb()` skips because it has no `refs` (`odb.c:450-453`).
/// * `donor2` — `main` and `other` at `e1`.
///
/// `borrower` lists all three, in that order.
fn fixture(name: &str) -> Fixture {
    let root = scratch(name);

    let donor = root.join("donor");
    std::fs::create_dir_all(&donor).unwrap();
    git_in(&donor, &["init", "-q", "-b", "main", "."]);
    git_in(&donor, &["commit", "-q", "--allow-empty", "-m", "d1"]);
    let d1 = oid(&donor, "HEAD");
    git_in(&donor, &["tag", "-a", "-m", "annotated", "at1"]);
    git_in(&donor, &["commit", "-q", "--allow-empty", "-m", "d2"]);
    let d2 = oid(&donor, "HEAD");
    git_in(&donor, &["branch", "side"]);

    let donor2 = root.join("donor2");
    std::fs::create_dir_all(&donor2).unwrap();
    git_in(&donor2, &["init", "-q", "-b", "main", "."]);
    git_in(&donor2, &["commit", "-q", "--allow-empty", "-m", "e1"]);
    let e1 = oid(&donor2, "HEAD");
    git_in(&donor2, &["branch", "other"]);

    std::fs::create_dir_all(root.join("loose").join("objects")).unwrap();

    let borrower = root.join("borrower");
    std::fs::create_dir_all(&borrower).unwrap();
    git_in(&borrower, &["init", "-q", "-b", "main", "."]);
    std::fs::write(
        borrower.join(".git/objects/info/alternates"),
        format!(
            "{}\n{}\n{}\n",
            donor.join(".git/objects").display(),
            root.join("loose/objects").display(),
            donor2.join(".git/objects").display(),
        ),
    )
    .unwrap();
    git_in(&borrower, &["commit", "-q", "--allow-empty", "-m", "b1"]);

    Fixture { root, borrower, d2, d1, e1 }
}

/// `git rev-list --alternate-refs` with `settings` prepended as `-c` pairs.
fn rev_list(fx: &Fixture, settings: &[(&str, &str)]) -> Output {
    let mut args: Vec<String> = Vec::new();
    for (key, value) in settings {
        args.push("-c".into());
        args.push(format!("{key}={value}"));
    }
    args.push("rev-list".into());
    args.push("--alternate-refs".into());
    let borrowed: Vec<&str> = args.iter().map(String::as_str).collect();
    git_in(&fx.borrower, &borrowed)
}

/// Write an executable `/bin/sh` script and return its path.
fn script(fx: &Fixture, name: &str, body: &str) -> String {
    let path = fx.root.join(name);
    std::fs::write(&path, body).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    path.display().to_string()
}

/// With neither key set, each alternate is listed by `for-each-ref
/// --format=%(objectname)` — every ref, refname-ordered — and the tips come out
/// in `odb->sources` order, which `odb_add_alternate_recursively()`
/// (`odb.c:169-205`) builds by appending each entry before descending into it.
///
/// The bare `loose/objects` entry contributes nothing: it has no `refs`
/// directory, so `refs_from_alternate_cb()` never runs a command for it.
#[test]
fn every_alternates_refs_are_walked_in_source_order() {
    let fx = fixture("plain");
    let out = rev_list(&fx, &[]);
    assert_eq!(
        stdout(&out),
        format!("{}\n{}\n{}\n", fx.d2, fx.d1, fx.e1),
        "donor's tips before donor2's, and the annotated tag peeled to its commit"
    );
    assert_eq!(stderr(&out), "");
    assert_eq!(out.status.code(), Some(0));

    // The borrower's own commit is not an alternate tip, so it is absent — the
    // pending set really is the alternates' and nothing else.
    let head = oid(&fx.borrower, "HEAD");
    assert!(!stdout(&out).contains(&head), "the borrower's own HEAD is not a tip");
}

/// `core.alternateRefsPrefixes` becomes the `for-each-ref` operands after `--`,
/// split on whitespace by `strvec_split()`. Only the tag is under `refs/tags`,
/// and it peels to `d1` — so the branch tip `d2` disappears entirely, which no
/// amount of walking from `d1` could produce.
#[test]
fn a_prefix_narrows_the_refs_each_alternate_lists() {
    let fx = fixture("prefixes");
    assert_eq!(
        stdout(&rev_list(&fx, &[("core.alternateRefsPrefixes", "refs/tags")])),
        format!("{}\n", fx.d1)
    );

    // Two prefixes, one value: the whitespace split is what makes this work.
    assert_eq!(
        stdout(&rev_list(
            &fx,
            &[("core.alternateRefsPrefixes", "refs/tags refs/heads")]
        )),
        format!("{}\n{}\n{}\n", fx.d2, fx.d1, fx.e1)
    );

    // A prefix nothing matches leaves the pending set empty, and `rev-list` with
    // no revisions is a usage error — the same exit stock gives.
    let none = rev_list(&fx, &[("core.alternateRefsPrefixes", "refs/nope")]);
    assert_eq!(none.status.code(), Some(129));
    assert_eq!(stdout(&none), "");
}

/// `core.alternateRefsCommand` replaces `for-each-ref` outright, and its first
/// argument is the alternate's git directory — so a command that lists exactly
/// one branch of whatever repository it is pointed at yields exactly that tip
/// from each alternate.
#[test]
fn a_configured_command_replaces_for_each_ref() {
    let fx = fixture("command");
    // `$1` is the alternate's git directory; `refs/heads/side` exists only in
    // `donor`, so `donor2` contributes nothing and the output is `d2` alone.
    let cmd = script(
        &fx,
        "list-side.sh",
        &format!(
            "#!/bin/sh\nexec {} --git-dir=\"$1\" for-each-ref \
             --format='%(objectname)' -- refs/heads/side\n",
            BIN
        ),
    );
    assert_eq!(
        stdout(&rev_list(&fx, &[("core.alternateRefsCommand", &cmd)])),
        format!("{}\n{}\n", fx.d2, fx.d1),
        "the side tip, then its parent — the walk from one seed"
    );
}

/// The command branch of `fill_alternate_refs_command()` never reads
/// `core.alternateRefsPrefixes`: "If `core.alternateRefsCommand` is set, setting
/// `core.alternateRefsPrefixes` has no effect"
/// (`Documentation/config/core.adoc:312-313`).
#[test]
fn the_command_leaves_the_prefixes_unread() {
    let fx = fixture("command-wins");
    let cmd = script(
        &fx,
        "list-heads.sh",
        &format!(
            "#!/bin/sh\nexec {} --git-dir=\"$1\" for-each-ref \
             --format='%(objectname)' -- refs/heads\n",
            BIN
        ),
    );
    let with_prefixes = rev_list(
        &fx,
        &[
            ("core.alternateRefsCommand", &cmd),
            ("core.alternateRefsPrefixes", "refs/tags"),
        ],
    );
    assert_eq!(
        stdout(&with_prefixes),
        stdout(&rev_list(&fx, &[("core.alternateRefsCommand", &cmd)])),
        "the prefixes are not consulted at all"
    );
    // And it is not the `refs/tags` answer either — `d1` alone would be that.
    // Both branch tips are seeds and `d1` is reached by walking, so it trails
    // them rather than sitting between them as it does when the tag seeds it.
    assert_eq!(stdout(&with_prefixes), format!("{}\n{}\n{}\n", fx.d2, fx.e1, fx.d1));
}

/// `read_alternate_refs()` warns and `break`s on the first line that is not a
/// bare object id (`odb.c:418-422`), keeping whatever it read before it. The
/// command runs once per usable alternate, so a two-alternate borrower warns
/// twice.
#[test]
fn a_line_that_is_not_an_object_id_stops_that_alternates_stream() {
    let fx = fixture("badline");
    let cmd = script(
        &fx,
        "mixed.sh",
        &format!("#!/bin/sh\nprintf '{}\\nnothex\\n{}\\n'\n", fx.d2, fx.e1),
    );
    let out = rev_list(&fx, &[("core.alternateRefsCommand", &cmd)]);
    assert_eq!(
        stderr(&out),
        "warning: invalid line while parsing alternate refs: nothex\n\
         warning: invalid line while parsing alternate refs: nothex\n"
    );
    // `d2` was read before the bad line and is still a seed; `e1`, printed after
    // it, never reached the pending set.
    assert_eq!(stdout(&out), format!("{}\n{}\n", fx.d2, fx.d1));
    assert_eq!(out.status.code(), Some(0));
}

/// `strvec_pushv(&cmd->env, local_repo_env)` unsets the borrower's
/// repository-local environment for the child, so a command that runs a git of
/// its own operates on the alternate it was handed rather than on the repository
/// that invoked it.
#[test]
fn the_child_cannot_see_the_borrowers_repository_environment() {
    let fx = fixture("env");
    let marker = fx.root.join("marker");
    let cmd = script(
        &fx,
        "env.sh",
        &format!(
            "#!/bin/sh\nprintf '%s\\n' \"${{GIT_DIR-unset}}\" > {}\nprintf '{}\\n'\n",
            marker.display(),
            fx.d2
        ),
    );

    let out = Command::new(BIN)
        .args(["-c", &format!("core.alternateRefsCommand={cmd}")])
        .args(["rev-list", "--alternate-refs"])
        .env("PATH", real_git_path())
        .env("HOME", &fx.borrower)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        // The variable that must not survive into the child.
        .env("GIT_DIR", ".git")
        .current_dir(&fx.borrower)
        .output()
        .expect("run the binary under test");

    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert_eq!(
        std::fs::read_to_string(&marker).unwrap(),
        "unset\n",
        "GIT_DIR must not reach the alternate's command"
    );
}

/// An `objects` directory with no repository around it is skipped, not asked:
/// `refs_from_alternate_cb()` requires `<base>/refs` to be a directory
/// (`odb.c:450-453`). With that as the only alternate there are no tips at all,
/// and `rev-list` fails as it does with no revisions.
#[test]
fn an_object_directory_without_refs_is_not_asked_for_any() {
    let fx = fixture("norefs");
    std::fs::write(
        fx.borrower.join(".git/objects/info/alternates"),
        format!("{}\n", fx.root.join("loose/objects").display()),
    )
    .unwrap();

    let out = rev_list(&fx, &[]);
    assert_eq!(stdout(&out), "");
    assert_eq!(out.status.code(), Some(129));
}
