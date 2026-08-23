//! `git zshadow [<dir>]` — install the whole `~/.zvcs` shadow and print the
//! shell lines that activate it.
//!
//! One command for what was three: a `git` symlink to this binary in `<dir>`
//! (default `$ZVCS_HOME/bin`), the `git-<verb>` dashed links beside it, every
//! superset man page under `~/.zvcs/man`, the HTML documentation set under
//! `~/.zvcs/share/doc/git-doc` (where `git --html-path` points and `git help -w`
//! looks), and the forked zsh `_git` under `~/.zvcs/completions`. Everything
//! installed is compiled into the binary, so the install needs nothing from the
//! source tree.
//!
//! stdout carries only shell code, so `eval "$(git zshadow)"` sets the current
//! shell up and the same three lines paste into an rc file; the install summary
//! goes to stderr. A `PATH`/`MANPATH` line the environment already satisfies is
//! emitted commented-out (so re-evaluating never duplicates an entry, and the
//! line is still there to uncomment when pasting into `.zshrc`); `--all` prints
//! every line uncommented. `fpath` is a zsh variable, not an exported one, so
//! this process cannot see it — that line is always emitted live.

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use crate::superset::{dashed, doctor, htmldoc, manpage, zdaemon};

/// The zsh completion shipped in the tree (`completions/_git`): the stock `_git`
/// forked with the `z*` verbs. Embedded so the installed shadow is self-contained.
const COMPLETION: &str = include_str!("../../../../completions/_git");

/// `git zshadow [<dir>] [-n|--print] [--all]`
pub fn zshadow(args: &[String]) -> Result<ExitCode> {
    let print_only = args.iter().any(|a| a == "-n" || a == "--print");
    let all = args.iter().any(|a| a == "--all");
    let home = zdaemon::zvcs_home();
    let bin: PathBuf = args
        .iter()
        .find(|a| !a.starts_with('-'))
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join("bin"));
    let man = manpage::man_dir();
    let comp = home.join("completions");

    if !print_only {
        install(&bin, &comp)?;
    }

    // stdout is shell code only — everything else went to stderr above.
    emit("PATH", &format!(r#"export PATH="{}:$PATH""#, shell_path(&bin)), &bin, all);
    emit("MANPATH", &format!(r#"export MANPATH="{}:$MANPATH""#, shell_path(&man)), &man, all);
    println!(r#"fpath=("{}" $fpath)  # zsh: before compinit"#, shell_path(&comp));

    Ok(ExitCode::SUCCESS)
}

/// Install the shim, the dashed links, the man pages, and the completion,
/// reporting one summary line on stderr.
fn install(bin: &Path, comp: &Path) -> Result<()> {
    std::fs::create_dir_all(bin).with_context(|| format!("cannot create {}", bin.display()))?;

    // The `git` shim first: with it in place the dashed links are made relative
    // to it, so a rebuild at a new path only needs the shim repointed.
    let me = crate::hosted::git_exe().context("cannot resolve the zvcs binary path")?;
    let mut shim = dashed::LinkStats::default();
    dashed::link_to(&bin.join("git"), &me, &mut shim)?;
    let links = dashed::install_links(bin, &dashed::link_target(bin)?)?;
    let pages = manpage::install_all().unwrap_or(0);
    let html = htmldoc::install_all().unwrap_or(0);
    let completion = install_completion(comp)?;

    eprintln!(
        "zshadow: {} ({}, {} dashed link(s) new / {} current / {} skipped); {pages} man page(s) in {}; {html} HTML page(s) in {}; {completion} in {}",
        bin.join("git").display(),
        if shim.created > 0 { "shim linked" } else if shim.skipped > 0 { "shim left alone: real file" } else { "shim current" },
        links.created,
        links.current,
        links.skipped,
        manpage::man_dir().join("man1").display(),
        htmldoc::html_dir().display(),
        comp.display(),
    );
    Ok(())
}

/// Write the embedded `_git` into `dir`, skipping the write when the installed
/// copy is already identical (the file is ~430 KiB; a no-op run should not
/// rewrite it).
fn install_completion(dir: &Path) -> Result<&'static str> {
    std::fs::create_dir_all(dir).with_context(|| format!("cannot create {}", dir.display()))?;
    let path = dir.join("_git");
    if std::fs::read_to_string(&path).is_ok_and(|on_disk| on_disk == COMPLETION) {
        return Ok("_git current");
    }
    std::fs::write(&path, COMPLETION).with_context(|| format!("cannot write {}", path.display()))?;
    Ok("_git written")
}

/// Print one shell line, commented out when `var` already lists `dir` (and
/// `--all` was not given) so an `eval` cannot duplicate the entry.
fn emit(var: &str, code: &str, dir: &Path, all: bool) {
    if !all && doctor::path_list_contains(var, dir) {
        println!("# {code}  # already on {var}");
    } else {
        println!("{code}");
    }
}

/// A path written for a shell rc file: `$HOME`-relative when it is under `$HOME`
/// (so the line is portable across machines), absolute otherwise. Always used
/// inside double quotes, where `$HOME` expands.
fn shell_path(p: &Path) -> String {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    match home.as_ref().and_then(|h| p.strip_prefix(h).ok()) {
        Some(rest) => format!("$HOME/{}", rest.display()),
        None => p.display().to_string(),
    }
}
