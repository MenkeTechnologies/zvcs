use anyhow::{bail, Result};
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
/// `-q`/`--quiet` are accepted and ignored (progress is discarded either way).
/// Any other option is rejected explicitly rather than silently mis-handled.
pub fn clone(args: &[String]) -> Result<ExitCode> {
    let mut bare = false;
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
            // Progress is discarded in both directions, so quiet is a no-op.
            "-q" | "--quiet" => {}
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

    if bare {
        // `git clone --bare 'url'...`
        eprintln!("Cloning into bare repository {dir:?}...");
        let mut prepare = gix::prepare_clone_bare(url, dst)?;
        // A bare clone never checks out a worktree; fetching the pack and writing
        // the refs is the whole operation.
        prepare.fetch_only(gix::progress::Discard, &should_interrupt)?;
    } else {
        // `git clone 'url'...`
        eprintln!("Cloning into {dir:?}...");
        let mut prepare = gix::prepare_clone(url, dst)?;
        let (mut checkout, _) =
            prepare.fetch_then_checkout(gix::progress::Discard, &should_interrupt)?;
        // Check out the branch `HEAD` points to. This is a no-op for an empty
        // remote, leaving an empty repository exactly like git does.
        checkout.main_worktree(gix::progress::Discard, &should_interrupt)?;
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
