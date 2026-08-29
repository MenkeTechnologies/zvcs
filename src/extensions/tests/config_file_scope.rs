//! `git config -f <path>` / `--file <path>` — the explicit-file scope.
//!
//! The motivating caller is submodule tooling, which reads `.gitmodules` with
//! `config -f .gitmodules --get-regexp '^submodule\..*\.path$'`; that file is
//! tracked content, not repository config, so it is only ever reachable through
//! this scope.
//!
//! Every expectation below was taken from stock git (2.55.0) run on the same
//! inputs. The sharp edges it encodes:
//!   * reading a missing file is exit 1 for the get forms but
//!     `fatal: unable to read config file …` at exit 128 for `--list`
//!   * a write creates the file but never its parent directory — a missing one
//!     is `could not lock config file …` at exit 255
//!   * `include.path` is not followed (git needs `--includes` for that)
//!   * a `--file` read sees that file alone, never the repository's own config

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn zvcs(dir: &Path, args: &[&str]) -> Output {
    Command::new(BIN).args(args).current_dir(dir).output().expect("run zvcs git")
}

fn stdout_of(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn stderr_of(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

/// The bare `strerror` text of an OS error, as git prints it — Rust's Display
/// appends ` (os error N)`, git's does not.
fn errno_text(err: &std::io::Error) -> String {
    let text = err.to_string();
    match text.find(" (os error ") {
        Some(cut) => text[..cut].to_owned(),
        None => text,
    }
}

/// A scratch directory holding `cfg` — a config file with a multivar, a
/// subsection, and a second section — and nothing else. Named per test and per
/// pid so concurrent test binaries never share one.
fn scratch(tag: &str) -> PathBuf {
    let p = std::env::temp_dir().join(format!("zvcs-cfgfile-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&p);
    std::fs::create_dir_all(&p).expect("mkdir fixture");
    std::fs::write(
        p.join("cfg"),
        "[submodule \"a/b\"]\n\tpath = a/b\n\turl = https://x/y\n[user]\n\tname = a\n\tname = b\n",
    )
    .expect("write fixture config");
    p
}

#[test]
fn reads_the_named_file_in_every_option_spelling() {
    let dir = scratch("spellings");

    // `-f p`, `--file p`, `--file=p` and the sticky `-fp` are one option.
    for args in [
        vec!["config", "-f", "cfg", "user.name"],
        vec!["config", "--file", "cfg", "user.name"],
        vec!["config", "--file=cfg", "user.name"],
        vec!["config", "-fcfg", "user.name"],
    ] {
        let out = zvcs(&dir, &args);
        assert!(out.status.success(), "{args:?}: exit {:?}", out.status.code());
        assert_eq!(stdout_of(&out), "b\n", "{args:?}: last value of the multivar");
    }
}

#[test]
fn get_regexp_reads_dot_gitmodules() {
    // The submodule-tooling call that motivated the scope.
    let dir = scratch("gitmodules");
    std::fs::write(
        dir.join(".gitmodules"),
        "[submodule \"a/b\"]\n\tpath = a/b\n\turl = https://x/y\n\
         [submodule \"c\"]\n\tpath = c\n\turl = https://x/z\n",
    )
    .expect("write .gitmodules");

    let out = zvcs(&dir, &["config", "-f", ".gitmodules", "--get-regexp", r"^submodule\..*\.path$"]);

    assert!(out.status.success(), "exit {:?}", out.status.code());
    assert_eq!(
        stdout_of(&out),
        "submodule.a/b.path a/b\nsubmodule.c.path c\n",
        "subsection case and `key value` spacing preserved"
    );
}

#[test]
fn list_and_get_all_cover_the_whole_file() {
    let dir = scratch("listall");

    let all = zvcs(&dir, &["config", "--file", "cfg", "--get-all", "user.name"]);
    assert_eq!(stdout_of(&all), "a\nb\n", "both multivar values, in file order");

    let list = zvcs(&dir, &["config", "-f", "cfg", "--list"]);
    assert_eq!(
        stdout_of(&list),
        "submodule.a/b.path=a/b\nsubmodule.a/b.url=https://x/y\nuser.name=a\nuser.name=b\n"
    );
}

#[test]
fn the_file_scope_hides_the_repositorys_own_config() {
    // The point of the scope: a `--file` read must not merge in local/global
    // config, or `.gitmodules` reads would pick up unrelated keys.
    let dir = scratch("isolation");
    assert!(zvcs(&dir, &["init", "-q", "-b", "main"]).status.success(), "init failed");
    assert!(zvcs(&dir, &["config", "--local", "core.zvcsprobe", "leaked"]).status.success());

    let out = zvcs(&dir, &["config", "-f", "cfg", "core.zvcsprobe"]);
    assert_eq!(out.status.code(), Some(1), "the local key is invisible under --file");
    assert!(stdout_of(&out).is_empty());
}

#[test]
fn include_directives_are_not_followed() {
    // git only honors `include.path` under `--file` when `--includes` is given,
    // so the included file's keys must stay out of the listing.
    let dir = scratch("includes");
    std::fs::write(dir.join("outer"), "[include]\n\tpath = cfg\n").expect("write outer");

    let out = zvcs(&dir, &["config", "-f", "outer", "--list"]);

    assert!(out.status.success());
    assert_eq!(stdout_of(&out), "include.path=cfg\n", "only the file itself");
}

/// The mirror image of the case above: for every scope that is not a named
/// file, `location_options_init()` turns include following ON by default
/// (builtin/config.c:1001-1004), and only `--no-includes` turns it back off. The
/// repository cascade resolves its includes while it is being built, so the
/// flag has to reach the read before the snapshot exists — a regression here
/// shows up as the included key still being answered for.
#[test]
fn the_repository_cascade_follows_includes_unless_told_not_to() {
    let dir = scratch("cascade-includes");
    assert!(zvcs(&dir, &["init", "-q", "-b", "main"]).status.success(), "init failed");
    std::fs::write(dir.join("extra.cfg"), "[inc]\n\tk = from-included\n").expect("write extra");
    let local = dir.join(".git").join("config");
    let mut text = std::fs::read_to_string(&local).expect("read local config");
    // Relative to the including file, which is `.git/config`.
    text.push_str("[include]\n\tpath = ../extra.cfg\n");
    std::fs::write(&local, text).expect("write local config");

    let followed = zvcs(&dir, &["config", "--get", "inc.k"]);
    assert!(followed.status.success(), "the default cascade follows includes");
    assert_eq!(stdout_of(&followed), "from-included\n");

    let not_followed = zvcs(&dir, &["config", "--no-includes", "--get", "inc.k"]);
    assert_eq!(not_followed.status.code(), Some(1), "--no-includes makes the key absent");
    assert_eq!(stdout_of(&not_followed), "");

    // The include directive itself is still config, so it stays in the listing.
    let listed = zvcs(&dir, &["config", "--no-includes", "--local", "--list"]);
    assert!(
        stdout_of(&listed).contains("include.path=../extra.cfg"),
        "the directive is still an entry: {}",
        stdout_of(&listed)
    );
    assert!(!stdout_of(&listed).contains("inc.k"), "but what it points at is not read");
}

/// `--show-origin` prints its paths from the top of the work tree, because
/// `setup_git_directory()` has chdir'd there before the first one is built. Run
/// from a subdirectory the local config is still `.git/config`, while a
/// `--file` path has instead been through `prefix_filename()` and carries the
/// subdirectory on its front — as typed, unnormalized.
#[test]
fn show_origin_paths_are_spelled_from_the_top_of_the_work_tree() {
    let dir = scratch("origin-prefix");
    assert!(zvcs(&dir, &["init", "-q", "-b", "main"]).status.success(), "init failed");
    assert!(zvcs(&dir, &["config", "demo.one", "1"]).status.success(), "set failed");
    let sub = dir.join("sub");
    std::fs::create_dir_all(&sub).expect("mkdir sub");
    std::fs::write(sub.join("f.cfg"), "[a]\n\tb = c\n").expect("write f.cfg");

    let top = zvcs(&dir, &["config", "--show-origin", "--get", "demo.one"]);
    assert_eq!(stdout_of(&top), "file:.git/config\t1\n");

    let from_sub = zvcs(&sub, &["config", "--show-origin", "--get", "demo.one"]);
    assert_eq!(stdout_of(&from_sub), "file:.git/config\t1\n", "not ../.git/config");

    let named = zvcs(&sub, &["config", "--show-origin", "-f", "f.cfg", "--get", "a.b"]);
    assert_eq!(stdout_of(&named), "file:sub/f.cfg\tc\n", "--file carries the prefix");

    // `prefix_filename()` concatenates; it does not normalize what it is given.
    let dotted = zvcs(&sub, &["config", "--show-origin", "-f", "./f.cfg", "--get", "a.b"]);
    assert_eq!(stdout_of(&dotted), "file:sub/./f.cfg\tc\n");

    // An absolute path is left alone, prefix or no prefix.
    let abs = sub.join("f.cfg");
    let abs = abs.to_str().expect("utf-8 fixture path");
    let absolute = zvcs(&sub, &["config", "--show-origin", "-f", abs, "--get", "a.b"]);
    assert_eq!(stdout_of(&absolute), format!("file:{abs}\tc\n"));
}

/// `--blob <rev>` reads configuration out of an object. Its four refusals are
/// each a different message: an unresolvable spec, a spec that resolves to
/// something other than a blob, a blob whose contents will not parse, and any
/// attempt to write one — and only `--list` turns a failed read into a fatal,
/// because `cmd_config_list()` dies where `get_value()` just finds nothing.
#[test]
fn blob_config_is_read_only_and_names_itself_in_every_refusal() {
    let dir = scratch("blob");
    assert!(zvcs(&dir, &["init", "-q", "-b", "main"]).status.success(), "init failed");
    std::fs::write(dir.join("cfg.ini"), "[a]\n\tb = c\n").expect("write cfg.ini");
    std::fs::write(dir.join("bad.ini"), "not config\n").expect("write bad.ini");
    std::fs::create_dir(dir.join("d")).expect("mkdir d");
    std::fs::write(dir.join("d").join("f"), "x\n").expect("write d/f");
    assert!(zvcs(&dir, &["add", "cfg.ini", "bad.ini", "d/f"]).status.success(), "add failed");
    let commit = Command::new(BIN)
        .args(["-c", "user.email=a@example.com", "-c", "user.name=n", "commit", "-qm", "x"])
        .current_dir(&dir)
        .output()
        .expect("run zvcs git");
    assert!(commit.status.success(), "commit failed: {}", stderr_of(&commit));

    let read = zvcs(&dir, &["config", "--blob", "HEAD:cfg.ini", "--get", "a.b"]);
    assert!(read.status.success(), "reading a blob: {}", stderr_of(&read));
    assert_eq!(stdout_of(&read), "c\n");

    // Every entry carries `CONFIG_ORIGIN_BLOB` and the spec, and the scope is
    // `command` — a blob is `CONFIG_SCOPE_COMMAND` like `--file` is.
    let origin =
        zvcs(&dir, &["config", "--blob", "HEAD:cfg.ini", "--show-origin", "--show-scope", "--list"]);
    assert_eq!(stdout_of(&origin), "command\tblob:HEAD:cfg.ini\ta.b=c\n");

    // Under --null those two column separators are NULs, not tabs.
    let nul =
        zvcs(&dir, &["config", "--blob", "HEAD:cfg.ini", "--show-origin", "-z", "--list"]);
    assert_eq!(stdout_of(&nul), "blob:HEAD:cfg.ini\0a.b\nc\0");

    let unparsable = zvcs(&dir, &["config", "--blob", "HEAD:bad.ini", "--list"]);
    assert_eq!(unparsable.status.code(), Some(128));
    assert_eq!(
        stderr_of(&unparsable),
        "error: bad config line 1 in blob HEAD:bad.ini\nfatal: error processing config file(s)\n"
    );

    // The same blob under a get reports the error and then simply finds nothing.
    let unparsable_get = zvcs(&dir, &["config", "--blob", "HEAD:bad.ini", "--get", "a.b"]);
    assert_eq!(unparsable_get.status.code(), Some(1));
    assert_eq!(stderr_of(&unparsable_get), "error: bad config line 1 in blob HEAD:bad.ini\n");

    let missing = zvcs(&dir, &["config", "--blob", "HEAD:nope", "--list"]);
    assert_eq!(missing.status.code(), Some(128));
    assert!(
        stderr_of(&missing).starts_with("error: unable to resolve config blob 'HEAD:nope'\n"),
        "{}",
        stderr_of(&missing)
    );

    let tree = zvcs(&dir, &["config", "--blob", "HEAD:d", "--list"]);
    assert!(
        stderr_of(&tree).starts_with("error: reference 'HEAD:d' does not point to a blob\n"),
        "{}",
        stderr_of(&tree)
    );

    let write = zvcs(&dir, &["config", "--blob", "HEAD:cfg.ini", "a.b", "v"]);
    assert_eq!(write.status.code(), Some(128));
    assert_eq!(stderr_of(&write), "fatal: writing config blobs is not supported\n");

    let edit = zvcs(&dir, &["config", "--blob", "HEAD:cfg.ini", "--edit"]);
    assert_eq!(edit.status.code(), Some(128));
    assert_eq!(stderr_of(&edit), "fatal: editing blobs is not supported\n");

    // A blob and a file are two config files, and git takes only one.
    let both = zvcs(&dir, &["config", "--blob", "HEAD:cfg.ini", "-f", "cfg.ini", "--list"]);
    assert_eq!(both.status.code(), Some(129));
    assert_eq!(stderr_of(&both), "error: only one config file at a time\n");
}

/// git's parser dies where it stands, so a `--file` that will not parse is fatal
/// before the action runs — including a write, which is why git refuses to
/// append to a file it cannot read back. `--edit` never parses it.
#[test]
fn an_unparsable_named_file_is_fatal_before_anything_reads_or_writes_it() {
    let dir = scratch("unparsable");
    let bad = dir.join("bad.cfg");
    std::fs::write(&bad, "not config\n").expect("write bad.cfg");
    let expected = "fatal: bad config line 1 in file bad.cfg\n";

    for args in [
        vec!["config", "-f", "bad.cfg", "--list"],
        vec!["config", "-f", "bad.cfg", "--get", "a.b"],
        vec!["config", "-f", "bad.cfg", "--unset", "a.b"],
        vec!["config", "-f", "bad.cfg", "a.b", "v"],
    ] {
        let out = zvcs(&dir, &args);
        assert_eq!(out.status.code(), Some(128), "{args:?}");
        assert_eq!(stderr_of(&out), expected, "{args:?}");
    }
    assert_eq!(
        std::fs::read_to_string(&bad).expect("read back"),
        "not config\n",
        "the refused write must not have touched the file"
    );
}

#[test]
fn missing_file_is_exit_1_to_read_but_fatal_to_list() {
    let dir = scratch("missing");

    let get = zvcs(&dir, &["config", "-f", "nope", "user.name"]);
    assert_eq!(get.status.code(), Some(1), "a get treats it as an absent key");
    assert!(stderr_of(&get).is_empty(), "and says nothing");

    let regexp = zvcs(&dir, &["config", "-f", "nope", "--get-regexp", ".*"]);
    assert_eq!(regexp.status.code(), Some(1));

    let list = zvcs(&dir, &["config", "-f", "nope", "--list"]);
    assert_eq!(list.status.code(), Some(128), "--list makes the read error fatal");
    assert_eq!(
        stderr_of(&list),
        "fatal: unable to read config file 'nope': No such file or directory\n"
    );

    assert!(!dir.join("nope").exists(), "a read never creates the file");
}

#[test]
fn writes_create_the_file_but_not_its_directory() {
    let dir = scratch("write");

    let set = zvcs(&dir, &["config", "-f", "new", "foo.bar", "baz"]);
    assert!(set.status.success(), "exit {:?}", set.status.code());
    assert_eq!(std::fs::read_to_string(dir.join("new")).unwrap(), "[foo]\n\tbar = baz\n");

    let add = zvcs(&dir, &["config", "-f", "new", "--add", "foo.bar", "qux"]);
    assert!(add.status.success());
    assert_eq!(
        std::fs::read_to_string(dir.join("new")).unwrap(),
        "[foo]\n\tbar = baz\n\tbar = qux\n",
        "--add appends a multivar entry"
    );

    // git takes a lock beside the target and never creates the directory for
    // it, so a missing one fails there rather than being silently made.
    let nested = zvcs(&dir, &["config", "-f", "d/e/new", "foo.bar", "baz"]);
    assert_eq!(nested.status.code(), Some(255));
    assert_eq!(
        stderr_of(&nested),
        "error: could not lock config file d/e/new: No such file or directory\n"
    );
    assert!(!dir.join("d").exists(), "the directory is not created");
}

#[test]
fn unset_of_a_missing_file_is_exit_5_and_writes_nothing() {
    let dir = scratch("unset");

    let out = zvcs(&dir, &["config", "-f", "nope", "--unset", "foo.bar"]);

    assert_eq!(out.status.code(), Some(5), "git's 'key not found' for unset");
    assert!(!dir.join("nope").exists(), "an unset never creates the file");
}

#[test]
fn one_config_file_at_a_time() {
    let dir = scratch("conflict");

    for args in [
        vec!["config", "-f", "cfg", "--global", "user.name"],
        vec!["config", "--global", "-f", "cfg", "user.name"],
        vec!["config", "--file", "cfg", "--system", "--list"],
    ] {
        let out = zvcs(&dir, &args);
        assert_eq!(out.status.code(), Some(129), "{args:?}");
        assert_eq!(stderr_of(&out), "error: only one config file at a time\n", "{args:?}");
    }

    // A repeated --file is not a conflict in git: the last path simply wins.
    let repeated = zvcs(&dir, &["config", "-f", "nope", "-f", "cfg", "user.name"]);
    assert!(repeated.status.success(), "exit {:?}", repeated.status.code());
    assert_eq!(stdout_of(&repeated), "b\n");
}

#[test]
fn a_valueless_file_option_is_a_usage_error() {
    let dir = scratch("novalue");

    let short = zvcs(&dir, &["config", "-f"]);
    assert_eq!(short.status.code(), Some(129));
    assert_eq!(stderr_of(&short), "error: switch `f' requires a value\n");

    let long = zvcs(&dir, &["config", "--file"]);
    assert_eq!(long.status.code(), Some(129));
    assert_eq!(stderr_of(&long), "error: option `file' requires a value\n");
}

#[test]
fn a_directory_target_is_reported_like_git() {
    let dir = scratch("directory");
    std::fs::create_dir(dir.join("adir")).expect("mkdir adir");
    // git prints the bare `strerror` text ("Is a directory" for EISDIR on both
    // Linux and macOS); take it from the platform rather than hard-coding it,
    // so the assertions stay about git's message *shape*.
    let errno = errno_text(&std::fs::read(dir.join("adir")).expect_err("reading a dir must fail"));
    let warning = format!("warning: unable to access 'adir': {errno}\n");

    // A get warns and reports the key as absent.
    let get = zvcs(&dir, &["config", "-f", "adir", "user.name"]);
    assert_eq!(get.status.code(), Some(1));
    assert_eq!(stderr_of(&get), warning);

    // `--list` warns and then makes it fatal.
    let list = zvcs(&dir, &["config", "-f", "adir", "--list"]);
    assert_eq!(list.status.code(), Some(128));
    assert_eq!(
        stderr_of(&list),
        format!("{warning}fatal: unable to read config file 'adir': Is a directory\n")
    );

    // A write refuses the target — once, not once per read of it.
    let set = zvcs(&dir, &["config", "-f", "adir", "foo.bar", "v"]);
    assert_eq!(set.status.code(), Some(3));
    assert_eq!(stderr_of(&set), format!("{warning}error: invalid config file adir\n"));
}
