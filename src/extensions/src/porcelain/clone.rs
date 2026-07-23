use anyhow::{bail, Result};
use prodash::Root as _;
use std::io::IsTerminal;
use std::path::Path;
use std::process::ExitCode;
use std::sync::atomic::AtomicBool;

/// `git clone <url> [<directory>]` — clone a repository into a new directory.
///
/// Backed by gitoxide's clone builder (`gix::prepare_clone` /
/// `gix::prepare_clone_bare`): a fresh repository is initialized at the
/// destination, a pack is fetched over the compiled-in blocking transport,
/// `HEAD`/remote-tracking refs are written, and — for a non-bare clone — the
/// main worktree is checked out.
///
/// Supported forms:
///   * `git clone <url>`             → clone into a directory named after the URL
///   * `git clone <url> <directory>` → clone into an explicit directory
///   * `git clone --bare <url> [<directory>]`
///
/// Progress (remote sideband + local receive/resolve/checkout) is rendered to
/// stderr like git: shown when stderr is a terminal, forced on with `--progress`,
/// and suppressed by `-q`/`--quiet` or `--no-progress`.
/// Any other option is rejected explicitly rather than silently mis-handled.
pub fn clone(args: &[String]) -> Result<ExitCode> {
    let mut bare = false;
    let mut quiet = false;
    // `Some(true)` = `--progress` forced, `Some(false)` = `--no-progress`,
    // `None` = default (progress iff stderr is a terminal), matching git.
    let mut force_progress: Option<bool> = None;
    let mut recurse_submodules = false;
    let mut positionals: Vec<&str> = Vec::new();
    let mut end_of_options = false;

    for arg in args {
        if end_of_options {
            positionals.push(arg);
            continue;
        }
        match arg.as_str() {
            "--" => end_of_options = true,
            "--bare" => bare = true,
            "-q" | "--quiet" => quiet = true,
            // `-v`/`--verbose` only adds output detail in git; the clone result
            // is identical, so it is accepted without changing behavior.
            "-v" | "--verbose" => {}
            "--progress" => force_progress = Some(true),
            "--no-progress" => force_progress = Some(false),
            // `--recursive` / `--recurse-submodules[=<pathspec>]`: after the
            // clone, initialize and update submodules recursively.
            "--recursive" | "--recurse-submodules" => recurse_submodules = true,
            other if other.starts_with("--recurse-submodules=") => {
                // A pathspec-limited recurse; this port does the full recursive
                // update rather than honoring the pathspec.
                recurse_submodules = true;
                let _ = other;
            }
            other if other.starts_with('-') => {
                bail!("unsupported option {other:?}");
            }
            other => positionals.push(other),
        }
    }

    let url_str = match positionals.first() {
        Some(u) => *u,
        None => bail!("you must specify a repository to clone"),
    };
    if positionals.len() > 2 {
        bail!("too many arguments");
    }

    // Parse the URL up front so a malformed one is reported before touching disk.
    let url = gix::url::parse(url_str.into())?;

    // Destination directory: explicit second positional, else derived from the
    // URL the way git's `guess_dir_name` does (humanish last component).
    let dir = match positionals.get(1) {
        Some(d) => (*d).to_string(),
        None => match derive_dir_name(url_str, bare) {
            Some(name) => name,
            None => bail!("could not derive a directory name from {url_str:?}"),
        },
    };
    let dst = Path::new(&dir);

    // git refuses to clone onto a non-empty existing path; gix's create options
    // enforce the same (destination_must_be_empty), so surface that early with a
    // git-matching message rather than a lower-level init error.
    if dst.exists()
        && dst
            .read_dir()
            .map(|mut e| e.next().is_some())
            .unwrap_or(true)
    {
        bail!("destination path {dir:?} already exists and is not an empty directory");
    }
    std::fs::create_dir_all(dst)?;

    // Serialize the ref/index writes through the repo coordinator, matching the
    // other write commands. On a freshly created clone no daemon is listening, so
    // this degrades to a no-op guard; it is held for the whole fetch+checkout.
    let git_dir = if bare {
        dst.to_path_buf()
    } else {
        dst.join(".git")
    };
    let _lock = crate::lock::RepoLock::acquire(&git_dir);

    let should_interrupt = AtomicBool::new(false);

    // git prints the "Cloning into ..." banner before any progress; keep it above
    // the live renderer so the two don't overdraw each other.
    if bare {
        eprintln!("Cloning into bare repository '{dir}'...");
    } else {
        eprintln!("Cloning into '{dir}'...");
    }

    // Drive gitoxide's fetch/checkout through a prodash tree and render it to
    // stderr with the line renderer. The tree relays the remote's sideband
    // progress (Enumerating/Counting/Compressing objects, Total …) as well as the
    // local pack receive/resolve and worktree checkout counters — the same
    // information git surfaces. When progress is suppressed the tree is still used
    // but no renderer is attached, so nothing is drawn.
    let show_progress = force_progress.unwrap_or_else(|| std::io::stderr().is_terminal()) && !quiet;
    let root = prodash::tree::Root::new();
    // Create the operation node BEFORE launching the renderer. prodash's line
    // renderer can otherwise race an empty tree at startup and exit before gix
    // adds any progress, rendering nothing at all (documented on
    // `keep_running_if_progress_is_empty`). gix's fetch adds a "remote" child here
    // (the server's Enumerating/Counting/Compressing sideband) plus local
    // "receiving pack"/"resolving" and checkout counters.
    let mut op = root.add_child(if bare { "clone (bare)" } else { "clone" });
    let no_color = std::env::var_os("NO_COLOR").is_some();
    let render = show_progress.then(|| {
        let mut opts = prodash::render::line::Options {
            throughput: true,
            ..Default::default()
        }
        .auto_configure(prodash::render::line::StreamKind::Stderr);
        // `--progress` forces the live display even when stderr is not a terminal,
        // matching git; auto_configure would otherwise disable it in that case.
        if force_progress == Some(true) {
            opts.output_is_terminal = true;
        }
        // auto_configure clobbers both of the following; reassert them after it:
        //   * git never hides the cursor, but auto_hide_cursor (signal-hook) forces
        //     hide_cursor on — that can strand the cursor hidden if a render is
        //     interrupted. Keep it visible.
        //   * colorize whenever we draw live to a terminal (git colors on a tty),
        //     honoring only the NO_COLOR standard — not the CLICOLOR/CLICOLOR_FORCE
        //     env quirks crosstermion::color::allowed keys on.
        opts.hide_cursor = false;
        opts.colored = opts.output_is_terminal && !no_color;
        prodash::render::line::render(std::io::stderr(), root.downgrade(), opts)
    });

    // Run the clone, capturing the result so the renderer is always torn down
    // (cursor restored, thread joined) before any error is propagated.
    let result = (|| -> Result<()> {
        if bare {
            // `git clone --bare 'url'...`: a bare clone never checks out a
            // worktree; fetching the pack and writing the refs is the whole job.
            let mut prepare = gix::prepare_clone_bare(url, dst)?;
            prepare.fetch_only(op.add_child("fetch"), &should_interrupt)?;
        } else {
            // `git clone 'url'...`
            let mut prepare = gix::prepare_clone(url, dst)?;
            let (mut checkout, _) =
                prepare.fetch_then_checkout(op.add_child("fetch"), &should_interrupt)?;
            // Check out the branch `HEAD` points to. This is a no-op for an empty
            // remote, leaving an empty repository exactly like git does.
            checkout.main_worktree(op.add_child("checkout"), &should_interrupt)?;
        }
        Ok(())
    })();

    if let Some(handle) = render {
        handle.shutdown_and_wait();
    }
    result?;

    // git closes the clone banner with `done.` (suppressed by `-q`).
    if !quiet {
        eprintln!("done.");
    }

    // `--recursive` / `--recurse-submodules`: after a successful non-bare clone,
    // initialize and update submodules recursively by re-executing this binary's
    // own ported `submodule update --init --recursive` in the new worktree.
    if recurse_submodules && !bare {
        let exe = std::env::current_exe()?;
        let status = std::process::Command::new(&exe)
            .arg("-C")
            .arg(&dir)
            .args(["submodule", "update", "--init", "--recursive"])
            .status()?;
        if !status.success() {
            return Ok(ExitCode::from(status.code().unwrap_or(1) as u8));
        }
    }

    Ok(ExitCode::SUCCESS)
}

/// Derive the default clone directory from a repository URL, mirroring git's
/// `guess_dir_name`: take the last `/`- or `:`-separated component, drop a
/// single trailing `.git`, and (for a bare clone) re-append `.git`.
///
/// Returns `None` when nothing usable remains (e.g. the URL is just `/` or `.`).
fn derive_dir_name(url: &str, bare: bool) -> Option<String> {
    let trimmed = url.trim_end_matches('/');
    let last = trimmed.rsplit(['/', ':']).next().unwrap_or("");
    let name = last.strip_suffix(".git").unwrap_or(last);
    if name.is_empty() || name == "." || name == ".." {
        return None;
    }
    Some(if bare {
        format!("{name}.git")
    } else {
        name.to_string()
    })
}
