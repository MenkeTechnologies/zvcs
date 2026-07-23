use anyhow::{bail, Result};
use prodash::Root as _;
use std::io::IsTerminal;
use std::num::NonZeroU32;
use std::path::Path;
use std::process::ExitCode;
use std::sync::atomic::AtomicBool;
use std::time::SystemTime;

use gix::remote::fetch::{Shallow, Tags};

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
///   * `-o`/`--origin <name>`        → track upstream under `<name>` instead of `origin`
///   * `-b`/`--branch <branch>`      → check out `<branch>` instead of the remote's `HEAD`
///   * `-n`/`--no-checkout`          → set up refs/`HEAD` but do not populate a worktree
///   * `--depth <n>`                 → shallow clone truncated to `<n>` commits
///   * `--shallow-since <time>`      → shallow boundary at a cutoff date
///   * `--shallow-exclude <ref>`     → exclude history reachable from a ref (repeatable)
///   * `--no-tags`                   → do not fetch tags
///   * `--reject-shallow`            → refuse to clone from a shallow remote
///   * `--recursive`/`--recurse-submodules[=<pathspec>]` → update submodules after clone
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
    let mut no_checkout = false;
    let mut origin: Option<String> = None;
    let mut branch: Option<String> = None;
    // `--no-tags` → `Tags::None`. Left `None` for git's default (fetch all tags).
    let mut tags: Option<Tags> = None;
    // `--reject-shallow` (`true`) / `--no-reject-shallow` (`false`); `None` leaves
    // it unspecified so the `clone.rejectShallow` config value, if any, decides.
    let mut reject_shallow: Option<bool> = None;
    // Shallow-boundary selectors that combine (git's `--shallow-exclude` is a
    // repeatable list → `deepen_not`, `--shallow-since` → `deepen_since`, and the
    // two may be given together). Resolved into a single `Shallow` after parsing.
    let mut depth: Option<NonZeroU32> = None;
    let mut shallow_exclude: Vec<gix::refs::PartialName> = Vec::new();
    let mut shallow_since: Option<gix::date::Time> = None;
    let mut positionals: Vec<&str> = Vec::new();

    let mut i = 0;
    let mut end_of_options = false;
    while i < args.len() {
        let a = args[i].as_str();
        i += 1;

        if end_of_options {
            positionals.push(a);
            continue;
        }

        // Split `--opt=value` for the value-taking long options.
        let (key, inline_val) = match (a.starts_with("--"), a.split_once('=')) {
            (true, Some((k, v))) => (k, Some(v.to_string())),
            _ => (a, None),
        };

        // Fetch the value for a value-taking option (inline `=v` or next arg).
        // Kept as a plain expression (not a closure) so the `i` cursor stays
        // freely borrowable in the other match arms.
        macro_rules! take_value {
            ($name:literal) => {
                match inline_val.clone() {
                    Some(v) => v,
                    None => {
                        let v = args
                            .get(i)
                            .cloned()
                            .ok_or_else(|| anyhow::anyhow!(concat!($name, " requires a value")))?;
                        i += 1;
                        v
                    }
                }
            };
        }

        match key {
            "--" => end_of_options = true,
            "--bare" => bare = true,
            "--no-bare" => bare = false,
            "-q" | "--quiet" => quiet = true,
            "--no-quiet" => quiet = false,
            // `-v`/`--verbose` only adds output detail in git; the clone result
            // is identical, so it is accepted without changing behavior.
            "-v" | "--verbose" | "--no-verbose" => {}
            "--progress" => force_progress = Some(true),
            "--no-progress" => force_progress = Some(false),
            // Local-clone optimizations: git uses these to control hardlinking /
            // copying of objects when the source is a local path. gitoxide always
            // copies objects over its transport, so the resulting repository is
            // identical regardless of these flags — accept them as no-ops.
            "-l" | "--local" | "--no-local" | "--no-hardlinks" | "--hardlinks" => {}
            // `-n`/`--no-checkout`: fetch refs and set `HEAD`, but do not populate
            // a worktree (leaves an empty index, exactly like git).
            "-n" | "--no-checkout" => no_checkout = true,
            "--checkout" => no_checkout = false,
            "-o" | "--origin" => origin = Some(take_value!("--origin")),
            "--no-origin" => origin = None,
            "-b" | "--branch" => branch = Some(take_value!("--branch")),
            "--no-branch" => branch = None,
            "--no-tags" => tags = Some(Tags::None),
            // `--tags` resets to git's default (all tags fetched).
            "--tags" => tags = None,
            "--reject-shallow" => reject_shallow = Some(true),
            "--no-reject-shallow" => reject_shallow = Some(false),
            "--depth" => {
                let v = take_value!("--depth");
                let n: u32 = v
                    .parse()
                    .map_err(|_| anyhow::anyhow!("depth {v:?} is not a positive number"))?;
                depth = Some(
                    NonZeroU32::new(n)
                        .ok_or_else(|| anyhow::anyhow!("depth {v:?} is not a positive number"))?,
                );
            }
            // Shallow boundary at a cutoff date (git's `deepen_since`). Parsed with
            // gitoxide's git-compatible date parser, relative to the current time.
            "--shallow-since" => {
                let v = take_value!("--shallow-since");
                let t = gix::date::parse(&v, Some(SystemTime::now()))
                    .map_err(|_| anyhow::anyhow!("--shallow-since expects a valid date, got {v:?}"))?;
                shallow_since = Some(t);
            }
            // Exclude history reachable from a ref (git's repeatable `deepen_not`).
            "--shallow-exclude" => {
                let v = take_value!("--shallow-exclude");
                let name = gix::refs::PartialName::try_from(v.as_str())
                    .map_err(|_| anyhow::anyhow!("--shallow-exclude expects a valid ref, got {v:?}"))?;
                shallow_exclude.push(name);
            }
            // `--recursive` / `--recurse-submodules[=<pathspec>]`: after the clone,
            // initialize and update submodules recursively. A pathspec-limited
            // recurse is honored as a full recursive update.
            "--recursive" | "--recurse-submodules" => recurse_submodules = true,
            "--no-recursive" | "--no-recurse-submodules" => recurse_submodules = false,
            other if other.starts_with('-') && other.len() > 1 => {
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

    // Resolve the shallow-boundary selectors into a single `Shallow` value. The
    // exclude form supersedes a lone `--shallow-since`, which supersedes
    // `--depth`, matching git's treatment of them as one deepen group.
    let shallow = if !shallow_exclude.is_empty() {
        Shallow::Exclude {
            remote_refs: shallow_exclude,
            since_cutoff: shallow_since,
        }
    } else if let Some(cutoff) = shallow_since {
        Shallow::Since { cutoff }
    } else if let Some(n) = depth {
        Shallow::DepthAtRemote(n)
    } else {
        Shallow::NoChange
    };

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

    // Build the clone platform and apply the parsed options. `--origin` renames
    // the remote (and its tracking refspec), `--branch` selects the ref to fetch
    // and check out, `--depth`/`--shallow-*` set the shallow boundary, `--no-tags`
    // suppresses tag fetching, and `--reject-shallow` is injected as an in-memory
    // `clone.rejectShallow` override so the fetch aborts on a shallow remote.
    let mut prepare = if bare {
        gix::prepare_clone_bare(url, dst)?
    } else {
        gix::prepare_clone(url, dst)?
    };
    if let Some(name) = &origin {
        prepare = prepare
            .with_remote_name(name.as_str())
            .map_err(|_| anyhow::anyhow!("{name:?} is not a valid remote name"))?;
    }
    if let Some(name) = &branch {
        prepare = prepare
            .with_ref_name(Some(name.as_str()))
            .map_err(|_| anyhow::anyhow!("--branch expects a valid branch name, got {name:?}"))?;
    }
    prepare = prepare.with_shallow(shallow);
    if let Some(tags) = tags {
        prepare = prepare.configure_remote(move |r| Ok(r.with_fetch_tags(tags)));
    }
    if let Some(reject) = reject_shallow {
        prepare = prepare
            .with_in_memory_config_overrides(Some(format!("clone.rejectShallow={reject}")));
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
        if bare || no_checkout {
            // A bare clone never checks out a worktree; `--no-checkout` likewise
            // fetches the pack and writes the refs/`HEAD` but leaves the worktree
            // (and index) empty. Fetching is the whole job in both cases.
            prepare.fetch_only(op.add_child("fetch"), &should_interrupt)?;
        } else {
            // `git clone 'url'...`
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

    // `--recursive` / `--recurse-submodules`: after a successful non-bare,
    // checked-out clone, initialize and update submodules recursively by
    // re-executing this binary's own ported `submodule update --init --recursive`
    // in the new worktree.
    if recurse_submodules && !bare && !no_checkout {
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
