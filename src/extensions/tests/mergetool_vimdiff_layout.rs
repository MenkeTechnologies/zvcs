//! `mergetool.<variant>.layout` — the window layout `mergetools/vimdiff` builds
//! vim's `-c` script from, for all nine `[g|n]vimdiff[1-3]` names.
//!
//! The layout grammar (`+` new tab, `/` horizontal split `,` vertical split,
//! parentheses to group, `@` to mark the file the result is saved to) is
//! compiled by `gen_cmd()`/`gen_cmd_aux()` in that script, and the compiled
//! string is the whole observable behaviour: it is what vim is started with.
//! So each case here points `mergetool.<variant>.path` at a stand-in that only
//! records its argument vector — no editor is ever started, and the recorded
//! `-c` argument is compared **byte for byte with the same run under stock
//! git**, which still has the real shell backend.
//!
//! Four rules of the resolution are pinned, all from `mergetools/vimdiff:375-405`:
//!
//!   * an unnumbered variant with no configuration uses git's four-window
//!     default `(LOCAL,BASE,REMOTE)/MERGED`;
//!   * `mergetool.<variant>.layout` replaces it, for any of the nine names;
//!   * `mergetool.vimdiff.layout` is the fallback for *every* variant, so it
//!     configures `gvimdiff` and `nvimdiff` too ("backward compatibility");
//!   * `vimdiff1`/`2`/`3` are fixed layouts that ignore both keys.
//!
//! And the tail of `merge_cmd`: a layout marking a file with `@` copies that
//! file over `$MERGED` when the tool exits cleanly.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");
const STOCK: &str = "/opt/homebrew/bin/git";

/// git's default when nothing is configured — the four-window layout, compiled.
/// Captured from stock git 2.55.0 so the test still asserts something exact on
/// a machine with no git installed to compare against.
const DEFAULT_SCRIPT: &str = "set hidden diffopt-=hiddenoff | echo | leftabove split | \
leftabove vertical split | 1b | wincmd l | leftabove vertical split | 2b | wincmd l | 3b | \
wincmd j | 4b | execute 'tabdo windo diffthis' | tabfirst";

fn stock_available() -> bool {
    Command::new(STOCK).arg("--version").output().is_ok_and(|o| o.status.success())
}

/// A repository stopped in a two-sided conflict, with a stand-in "editor" that
/// appends its arguments to `args.log` and exits 0.
fn fixture(tag: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!("zvcs-vimdiff-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let root = root.canonicalize().unwrap();
    std::fs::create_dir_all(root.join("home")).unwrap();

    let stub = root.join("fake-editor");
    std::fs::write(
        &stub,
        format!("#!/bin/sh\nprintf '%s\\n' \"$@\" >> {}/args.log\nexit 0\n", root.display()),
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    let repo = root.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    for args in [
        vec!["init", "-q", "-b", "main"],
        vec!["config", "user.email", "test@example.com"],
        vec!["config", "user.name", "test"],
    ] {
        assert!(git(&repo, &args).status.success(), "setup failed: {args:?}");
    }
    std::fs::write(repo.join("f"), "a\nb\nc\n").unwrap();
    assert!(git(&repo, &["add", "f"]).status.success());
    assert!(git(&repo, &["commit", "-qm", "base"]).status.success());
    assert!(git(&repo, &["checkout", "-q", "-b", "side"]).status.success());
    std::fs::write(repo.join("f"), "a\nside\nc\n").unwrap();
    assert!(git(&repo, &["commit", "-qam", "side"]).status.success());
    assert!(git(&repo, &["checkout", "-q", "main"]).status.success());
    std::fs::write(repo.join("f"), "a\nmain\nc\n").unwrap();
    assert!(git(&repo, &["commit", "-qam", "main"]).status.success());

    // Every variant points at the stand-in and trusts its exit code, so the run
    // ends without `check_unchanged` needing a real edit.
    for variant in ["vimdiff", "vimdiff1", "vimdiff2", "vimdiff3", "gvimdiff", "nvimdiff"] {
        assert!(
            git(&repo, &["config", &format!("mergetool.{variant}.path"), stub.to_str().unwrap()])
                .status
                .success()
        );
        assert!(
            git(&repo, &["config", &format!("mergetool.{variant}.trustExitCode"), "true"])
                .status
                .success()
        );
    }
    repo
}

fn command(bin: &str, repo: &Path, args: &[&str]) -> Command {
    let home = repo.parent().unwrap().join("home");
    let mut cmd = Command::new(bin);
    cmd.args(args)
        .current_dir(repo)
        .env("HOME", &home)
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("ZVCS_HOME", &home)
        .env("LC_ALL", "C")
        .stdin(std::process::Stdio::null());
    cmd
}

fn git(repo: &Path, args: &[&str]) -> Output {
    command(BIN, repo, args).output().unwrap()
}

/// Put the repository back into the conflict, run one `git mergetool` with the
/// given binary, and return the `-c` script the stand-in editor was started
/// with.
fn capture(bin: &str, repo: &Path, tool: &str, config: &[&str]) -> String {
    let log = repo.parent().unwrap().join("args.log");
    let _ = std::fs::remove_file(&log);
    // `merge --abort` is a no-op when there is nothing to abort.
    let _ = git(repo, &["merge", "--abort"]);
    let merged = git(repo, &["merge", "side"]);
    assert!(!merged.status.success(), "the fixture merge did not conflict");

    let mut args: Vec<&str> = vec!["-c", "mergetool.prompt=false"];
    for c in config {
        args.push("-c");
        args.push(c);
    }
    let tool_arg = format!("--tool={tool}");
    args.push("mergetool");
    args.push(&tool_arg);
    let out = command(bin, repo, &args).output().unwrap();
    assert!(
        out.status.success(),
        "mergetool failed ({bin}): {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let recorded = std::fs::read_to_string(&log).expect("the editor stand-in was never started");
    let mut lines = recorded.lines();
    assert_eq!(lines.next(), Some("-f"), "vim is started with -f");
    assert_eq!(lines.next(), Some("-c"), "the layout is passed as a -c script");
    lines.next().expect("no -c script was recorded").to_string()
}

/// Both binaries, same fixture, same configuration: the compiled scripts must be
/// identical. Returns the script so a caller can pin it further.
fn assert_matches_stock(repo: &Path, tool: &str, config: &[&str]) -> String {
    let ours = capture(BIN, repo, tool, config);
    if stock_available() {
        let stock = capture(STOCK, repo, tool, config);
        assert_eq!(ours, stock, "tool={tool} config={config:?} diverges from stock");
    }
    ours
}

/// With nothing configured, an unnumbered variant gets git's four-window
/// default: the three inputs side by side over the merged file.
#[test]
fn default_layout_matches_stock() {
    let repo = fixture("default");
    assert_eq!(assert_matches_stock(&repo, "vimdiff", &[]), DEFAULT_SCRIPT);
    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

/// `mergetool.<variant>.layout` replaces the default, and the whole grammar is
/// honoured: vertical splits, horizontal splits, tabs, grouping, and the `@`
/// marker (which changes the layout's window order as well as the save target).
#[test]
fn configured_layouts_compile_the_way_stock_compiles_them() {
    let repo = fixture("configured");
    for layout in [
        "(LOCAL,BASE)/MERGED",
        "@REMOTE,LOCAL",
        "LOCAL+BASE+REMOTE",
        "((LOCAL,BASE),REMOTE)/MERGED",
        "MERGED",
        "LOCAL/BASE/REMOTE/MERGED",
    ] {
        let script =
            assert_matches_stock(&repo, "vimdiff", &[&format!("mergetool.vimdiff.layout={layout}")]);
        assert_ne!(script, DEFAULT_SCRIPT, "layout {layout} was ignored");
    }
    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

/// The numbered variants *are* layouts, so they ignore both configuration keys
/// — `mergetool.vimdiff2.layout` has no effect on `vimdiff2`.
#[test]
fn numbered_variants_are_fixed_layouts() {
    let repo = fixture("numbered");
    for tool in ["vimdiff1", "vimdiff2", "vimdiff3"] {
        let plain = assert_matches_stock(&repo, tool, &[]);
        let configured = assert_matches_stock(
            &repo,
            tool,
            &[&format!("mergetool.{tool}.layout=BASE"), "mergetool.vimdiff.layout=BASE"],
        );
        assert_eq!(plain, configured, "{tool} honoured a layout it should ignore");
        assert_ne!(plain, DEFAULT_SCRIPT, "{tool} used the unnumbered default");
    }
    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

/// `mergetool.vimdiff.layout` is the compatibility fallback for every variant,
/// so it configures `gvimdiff` and `nvimdiff` as well — and the variant's own
/// key still wins over it.
#[test]
fn vimdiff_layout_is_the_fallback_for_every_variant() {
    let repo = fixture("fallback");
    for tool in ["gvimdiff", "nvimdiff"] {
        let fallback = assert_matches_stock(&repo, tool, &["mergetool.vimdiff.layout=MERGED,LOCAL"]);
        assert_ne!(fallback, DEFAULT_SCRIPT, "{tool} ignored mergetool.vimdiff.layout");

        let own = assert_matches_stock(
            &repo,
            tool,
            &[
                "mergetool.vimdiff.layout=MERGED,LOCAL",
                &format!("mergetool.{tool}.layout=REMOTE,BASE"),
            ],
        );
        assert_ne!(own, fallback, "the variant's own layout did not win");
    }
    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

/// The `@` marker is not only a window order: on a clean exit the marked file is
/// copied over `$MERGED`, so `@LOCAL` resolves the conflict to our side even
/// though the stand-in editor never wrote anything.
#[test]
fn the_at_marker_saves_that_file_as_the_result() {
    let repo = fixture("target");
    assert_matches_stock(&repo, "vimdiff", &["mergetool.vimdiff.layout=@LOCAL,REMOTE"]);
    assert_eq!(
        std::fs::read_to_string(repo.join("f")).unwrap(),
        "a\nmain\nc\n",
        "the `@LOCAL` layout did not save LOCAL as the merge result"
    );

    // Without a marker the tool's own (non-)edit stands, so the conflicted file
    // keeps its conflict markers.
    let repo2 = fixture("target2");
    assert_matches_stock(&repo2, "vimdiff", &[]);
    let text = std::fs::read_to_string(repo2.join("f")).unwrap();
    assert!(text.contains("<<<<<<<"), "an unmarked layout rewrote the merged file: {text}");

    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
    let _ = std::fs::remove_dir_all(repo2.parent().unwrap());
}
