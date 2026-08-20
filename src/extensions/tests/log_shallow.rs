//! `git log` must honour `.git/shallow`: a commit listed there is grafted to have
//! no parents, so the walk stops at it instead of reading its (out-of-clone) parent
//! — which a `--depth` clone leaves absent, previously erroring with "object … could
//! not be found". Self-contained: fabricate the shallow file rather than clone.

use std::path::Path;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn run(cwd: &Path, home: &Path, args: &[&str]) -> String {
    let out = Command::new(BIN)
        .args(args)
        .current_dir(cwd)
        .env("HOME", home)
        .env("ZVCS_HOME", home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .expect("run binary");
    assert!(
        out.status.success() || !args.contains(&"log"),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn log_stops_at_the_shallow_boundary() {
    let root = std::env::temp_dir().join(format!("zvcs-logshallow-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let home = root.join("home");
    let repo = root.join("repo");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&repo).unwrap();

    run(&repo, &home, &["init", "-q", "-b", "main"]);
    run(&repo, &home, &["config", "user.email", "t@e.co"]);
    run(&repo, &home, &["config", "user.name", "t"]);
    for m in ["c0", "c1", "c2"] {
        std::fs::write(repo.join("f"), format!("{m}\n")).unwrap();
        run(&repo, &home, &["add", "f"]);
        run(&repo, &home, &["commit", "-q", "-m", m]);
    }

    // Full history is three commits.
    assert_eq!(run(&repo, &home, &["log", "--oneline"]).lines().count(), 3);

    // Graft HEAD as shallow: the walk must now show only HEAD (its parent is
    // "outside the clone"), not error and not descend.
    let head = run(&repo, &home, &["rev-parse", "HEAD"]).trim().to_string();
    std::fs::write(repo.join(".git/shallow"), format!("{head}\n")).unwrap();

    let out = run(&repo, &home, &["log", "--oneline"]);
    assert_eq!(out.lines().count(), 1, "shallow HEAD must stop the walk; got:\n{out}");
    assert!(out.contains("c2"), "the one line should be HEAD (c2); got:\n{out}");

    let _ = std::fs::remove_dir_all(&root);
}

/// The other half of `.git/shallow`: `load_ref_decorations()` walks the graft
/// table after the refs and after `HEAD` (log-tree.c:242) and hangs a
/// `DECORATION_GRAFTED` entry spelled `grafted` on every commit it names
/// (`add_graft_decoration`, log-tree.c:211-219). Because `add_name_decoration`
/// prepends, that entry renders *first* — ahead of `HEAD -> main`.
///
/// The colored expectations are stock git 2.55.0's bytes for the same fixture:
/// `color.decorate.grafted` defaults to `GIT_COLOR_BOLD_BLUE` (`\e[1;34m`) and is
/// configurable like every other slot, and `format_decorations()` closes every
/// entry with `color_reset` even when the slot itself resolved to nothing.
#[test]
fn shallow_boundary_is_decorated_as_grafted() {
    let root = std::env::temp_dir().join(format!("zvcs-graftdeco-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let home = root.join("home");
    let repo = root.join("repo");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&repo).unwrap();

    run(&repo, &home, &["init", "-q", "-b", "main"]);
    run(&repo, &home, &["config", "user.email", "t@e.co"]);
    run(&repo, &home, &["config", "user.name", "t"]);
    for m in ["c0", "c1"] {
        std::fs::write(repo.join("f"), format!("{m}\n")).unwrap();
        run(&repo, &home, &["add", "f"]);
        run(&repo, &home, &["commit", "-q", "-m", m]);
    }

    // No graft table yet: only the ref decorations.
    let plain = run(&repo, &home, &["log", "-1", "--format=%D"]);
    assert_eq!(plain.trim_end(), "HEAD -> main", "no grafts, no `grafted` entry");

    let head = run(&repo, &home, &["rev-parse", "HEAD"]).trim().to_string();
    std::fs::write(repo.join(".git/shallow"), format!("{head}\n")).unwrap();

    let grafted = run(&repo, &home, &["log", "-1", "--format=%D"]);
    assert_eq!(
        grafted.trim_end(),
        "grafted, HEAD -> main",
        "the graft entry is added last and therefore rendered first"
    );

    // The default slot, byte for byte against stock: bold blue, closed by `\e[m`.
    let colored = run(
        &repo,
        &home,
        &["log", "-1", "--format=%C(auto)%D", "--color=always"],
    );
    assert_eq!(
        colored,
        "\u{1b}[1;34mgrafted\u{1b}[m\u{1b}[33m, \u{1b}[m\u{1b}[1;36mHEAD\u{1b}[m\
         \u{1b}[33m -> \u{1b}[m\u{1b}[1;32mmain\u{1b}[m\n",
        "default color.decorate.grafted is GIT_COLOR_BOLD_BLUE"
    );

    // Configured: the slot is genuinely read, and an empty spec still emits the
    // trailing reset git writes unconditionally.
    run(&repo, &home, &["config", "color.decorate.grafted", "red"]);
    let red = run(
        &repo,
        &home,
        &["log", "-1", "--format=%C(auto)%D", "--color=always"],
    );
    assert!(
        red.starts_with("\u{1b}[31mgrafted\u{1b}[m"),
        "color.decorate.grafted=red must repaint the entry; got {red:?}"
    );

    run(&repo, &home, &["config", "color.decorate.grafted", "normal"]);
    let normal = run(
        &repo,
        &home,
        &["log", "-1", "--format=%C(auto)%D", "--color=always"],
    );
    assert!(
        normal.starts_with("grafted\u{1b}[m"),
        "an empty spec still closes with color_reset; got {normal:?}"
    );

    let _ = std::fs::remove_dir_all(&root);
}
