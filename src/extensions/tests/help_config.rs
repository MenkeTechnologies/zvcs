//! Config-driven `git help` behavior. Five keys reach this command:
//!   * `help.browser` — which browser the web format opens, and its precedence
//!     over `web.browser`.
//!   * `help.autocorrect` — parsed in the unknown-verb path (see the
//!     `autocorrect` integration test); not re-tested here.
//!   * `help.format` — picks the viewer. All three of git's formats are
//!     implemented, so `html`/`web` must actually route to the HTML viewer
//!     rather than being rejected, and an unrecognized value must die with
//!     git's own message.
//!   * `help.htmlpath` — overrides the directory the HTML viewer resolves the
//!     page in, and is never written to (it is the user's own tree).
//!   * `man.viewer` — the viewer chain, driven here through a stand-in program
//!     so nothing real is launched (the whole chain lives in `man_viewer.rs`);
//!
//! Every case that reaches the HTML viewer pins `web.browser` to a custom tool
//! whose `browser.<tool>.cmd` echoes its arguments, so no real browser is ever
//! detected or launched and the resolved page path lands on stdout where the
//! test can compare it byte-for-byte.
//!
//! The one place `help.format` is git-parity rather than a divergence is the
//! alias path: an alias resolves and prints its expansion BEFORE the viewer is
//! consulted, exactly as stock git does — that case is asserted byte-for-byte
//! against the installed git.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn git(dir: &Path, args: &[&str]) {
    assert!(
        Command::new(BIN).args(args).current_dir(dir).status().unwrap().success(),
        "git {args:?} failed"
    );
}

/// A hermetic repo with an isolated HOME so config reads only what we set.
fn fixture(tag: &str) -> (PathBuf, PathBuf) {
    let root = std::env::temp_dir().join(format!("zvcs-help-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let root = root.canonicalize().unwrap();
    let home = root.join("home");
    std::fs::create_dir_all(&home).unwrap();
    let repo = root.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    (repo, home)
}

/// Run the zvcs binary with stdin closed so a viewer can never block the test.
fn run(dir: &Path, home: &Path, args: &[&str]) -> Output {
    Command::new(BIN)
        .args(args)
        .current_dir(dir)
        .env("HOME", home)
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("ZVCS_HOME", home)
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap()
}

/// An alias resolves and prints its expansion before `help.format` is consulted,
/// so `git help <alias>` succeeds even under an unsupported viewer — matching
/// stock git byte-for-byte on stdout and exit code.
#[test]
fn alias_expansion_precedes_format_gate() {
    let (repo, home) = fixture("alias");
    git(&repo, &["config", "alias.co", "checkout"]);
    git(&repo, &["config", "help.format", "html"]);

    let out = run(&repo, &home, &["help", "co"]);
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(String::from_utf8_lossy(&out.stdout), "'co' is aliased to 'checkout'\n");

    // Byte-for-byte against the installed git: it resolves the alias ahead of the
    // viewer selection too.
    let real = Command::new(BIN)
        .args(["-c", "help.format=html", "help", "co"])
        .current_dir(&repo)
        .env("HOME", &home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .unwrap();
    assert_eq!(out.stdout, real.stdout, "alias line must match stock git");

    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

/// Pin `web.browser` to a custom tool that only echoes the URLs it is handed, so
/// the viewer runs to completion without a browser existing. Returns the config
/// arguments; the echoed path shows up on the command's stdout.
fn echoing_browser(repo: &Path) {
    git(repo, &["config", "web.browser", "zvcsecho"]);
    git(repo, &["config", "browser.zvcsecho.cmd", "printf '%s\\n'"]);
}

/// `help.format=html` selects the web viewer, exactly as `-w` does: the page is
/// resolved under `git --html-path` and its path handed to `web--browse`.
/// The echoed path is the whole contract — it proves the lookup resolved to the
/// directory `--html-path` reports, under the name `cmd_to_page()` produces.
///
/// Which set that directory is depends on the host: git's own installed HTML
/// manual when there is one, and the set this binary generates otherwise. The
/// assertions below hold for either, so the test does not depend on whether the
/// machine running it has git's documentation installed — the page must be an
/// HTML document (git's asciidoc manual opens `<!DOCTYPE html>`, the generated
/// set `<!doctype html>`) carrying git's own description of `status`.
#[test]
fn help_format_html_selects_the_web_viewer() {
    let (repo, home) = fixture("format");
    git(&repo, &["config", "help.format", "html"]);
    echoing_browser(&repo);

    let out = run(&repo, &home, &["help", "status"]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "stderr:\n{stderr}");

    let dir = String::from_utf8(run(&repo, &home, &["--html-path"]).stdout).unwrap();
    let page = PathBuf::from(dir.trim()).join("git-status.html");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        format!("{}\n", page.display()),
        "the viewer was handed the wrong path"
    );

    // …and that path is a real page carrying git's own description of `status`.
    let body = std::fs::read_to_string(&page).unwrap();
    assert!(
        body.to_ascii_lowercase().starts_with("<!doctype html>"),
        "not an HTML document: {:?}",
        body.lines().next()
    );
    assert!(body.contains("Show the working tree status"), "no summary in:\n{body}");

    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

/// An unrecognized `help.format` dies with git's own message and exit code,
/// rather than being silently treated as `man`.
#[test]
fn unrecognized_help_format_dies() {
    let (repo, home) = fixture("badformat");
    git(&repo, &["config", "help.format", "hologram"]);

    let out = run(&repo, &home, &["help", "status"]);
    assert_eq!(out.status.code(), Some(128));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(stderr, "fatal: unrecognized help format 'hologram'\n", "stderr:\n{stderr}");
    assert!(String::from_utf8_lossy(&out.stdout).is_empty());

    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

/// `help.htmlpath` moves the lookup to the configured directory — and that
/// directory is the user's own tree, so nothing is generated into it: an empty
/// one produces git's `documentation file not found` rather than a page.
#[test]
fn help_htmlpath_redirects_the_lookup_and_is_never_written_to() {
    let (repo, home) = fixture("htmlpath");
    let elsewhere = home.join("manual");
    std::fs::create_dir_all(&elsewhere).unwrap();
    git(&repo, &["config", "help.htmlpath", elsewhere.to_str().unwrap()]);
    echoing_browser(&repo);

    let out = run(&repo, &home, &["help", "-w", "status"]);
    assert_eq!(out.status.code(), Some(128));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(
        stderr,
        format!("fatal: '{}/git-status.html': documentation file not found.\n", elsewhere.display()),
        "stderr:\n{stderr}"
    );
    assert_eq!(std::fs::read_dir(&elsewhere).unwrap().count(), 0, "the configured tree was written to");

    // With the page present, the same lookup succeeds and hands over that path —
    // the configured directory, not the built-in one.
    std::fs::write(elsewhere.join("git-status.html"), "<!doctype html>\n").unwrap();
    let out = run(&repo, &home, &["help", "-w", "status"]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "stderr:\n{stderr}");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        format!("{}/git-status.html\n", elsewhere.display())
    );

    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

/// `man.viewer=konqueror`, one of the three viewers git drives itself. It used
/// to be a faithful-unsupported gate here; the viewer chain is implemented now
/// (see the `man_viewer` integration test for the whole chain), so what is
/// asserted is git's actual konqueror behaviour, which is two-sided:
///
///   * outside a graphical session (`$DISPLAY` empty) `exec_man_konqueror()`
///     runs nothing at all and the chain falls through to `man`;
///   * inside one it starts `kfmclient newTab man:<page>(1)`, with
///     `man.konqueror.path` naming the program — pointed at `/bin/echo` here, so
///     the argument vector lands on stdout instead of a browser.
#[test]
fn konqueror_man_viewer_is_driven_the_way_stock_drives_it() {
    let (repo, home) = fixture("viewer");
    git(&repo, &["config", "man.viewer", "konqueror"]);
    git(&repo, &["config", "man.konqueror.path", "/bin/echo"]);

    let out = Command::new(BIN)
        .args(["help", "-m", "status"])
        .current_dir(&repo)
        .env("HOME", &home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("ZVCS_HOME", &home)
        .env("DISPLAY", ":0")
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "newTab man:git-status(1)\n",
        "konqueror was not started the way git starts it"
    );

    // With no graphical session the viewer declines silently — nothing is run,
    // so the stand-in prints nothing.
    let out = Command::new(BIN)
        .args(["help", "-m", "status"])
        .current_dir(&repo)
        .env("HOME", &home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("ZVCS_HOME", &home)
        .env_remove("DISPLAY")
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();
    assert!(
        !String::from_utf8_lossy(&out.stdout).contains("newTab"),
        "konqueror ran without a DISPLAY"
    );

    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

/// `help.browser` names the browser for the web format. git does not read the
/// key in `builtin/help.c` at all: `open_html()` execs
/// `git web--browse -c help.browser <path>`, and `git-web--browse` then consults
/// *that* key first and `web.browser` only as a fallback — which is the
/// precedence asserted here, with both keys pointing at echoing stand-ins so the
/// one that wins is visible on stdout.
#[test]
fn help_browser_wins_over_web_browser() {
    let (repo, home) = fixture("browser");
    git(&repo, &["config", "help.format", "html"]);
    git(&repo, &["config", "help.browser", "zvcshelp"]);
    git(&repo, &["config", "browser.zvcshelp.cmd", "printf 'help:%s\\n'"]);
    git(&repo, &["config", "web.browser", "zvcsweb"]);
    git(&repo, &["config", "browser.zvcsweb.cmd", "printf 'web:%s\\n'"]);

    let out = run(&repo, &home, &["help", "status"]);
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.starts_with("help:"), "help.browser did not win over web.browser: {text}");
    assert!(text.trim_end().ends_with("git-status.html"), "the page was not handed over: {text}");

    // Unset it and the fallback takes over — proving the first result came from
    // `help.browser` rather than from the browser list.
    git(&repo, &["config", "--unset", "help.browser"]);
    let out = run(&repo, &home, &["help", "status"]);
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    assert!(
        String::from_utf8_lossy(&out.stdout).starts_with("web:"),
        "web.browser did not take over"
    );

    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

/// `git help --config` prints git's configuration-variable-name list: one name
/// per line, ASCII-sorted and unique, then a blank line and the
/// `'git help config' for more information` trailer, exit 0. The exact set is
/// pinned to git 2.55.0 in [`CONFIG_VARS`], so this asserts the structure and a
/// few stable anchors rather than a byte diff against whatever git the CI host
/// carries (the variable set drifts across git versions; this shape does not).
#[test]
fn config_lists_variable_names() {
    let (repo, home) = fixture("config");

    let out = run(&repo, &home, &["help", "--config"]);
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    let stdout = String::from_utf8(out.stdout).unwrap();

    // Structure: <names>\n\n<trailer>\n.
    let trailer = "\n\n'git help config' for more information\n";
    assert!(stdout.ends_with(trailer), "missing trailer:\n{stdout}");
    let names: Vec<&str> = stdout[..stdout.len() - trailer.len()].lines().collect();

    // Sorted, unique, non-empty — matching git's `string_list_sort` output.
    assert!(!names.is_empty());
    let mut sorted = names.clone();
    sorted.sort_unstable();
    assert_eq!(names, sorted, "names must be ASCII-sorted");
    let unique: std::collections::BTreeSet<&str> = names.iter().copied().collect();
    assert_eq!(unique.len(), names.len(), "names must be unique");

    // Anchors that have existed for many git releases, including a wildcard and a
    // placeholder form git emits verbatim.
    for anchor in ["user.name", "core.editor", "alias.*", "branch.<name>.remote"] {
        assert!(names.contains(&anchor), "expected {anchor:?} in list");
    }

    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

/// The short spelling `-c` is the same cmdmode as `--config` and produces the
/// identical listing.
#[test]
fn config_short_flag_matches_long() {
    let (repo, home) = fixture("config-short");

    let long = run(&repo, &home, &["help", "--config"]);
    let short = run(&repo, &home, &["help", "-c"]);
    assert!(short.status.success());
    assert_eq!(short.stdout, long.stdout, "-c must match --config byte-for-byte");

    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}
