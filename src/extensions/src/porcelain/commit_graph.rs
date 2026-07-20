//! `git commit-graph` — manage the serialized commit-graph file.
//!
//! Implemented:
//!   * `git commit-graph verify [--object-dir <dir>] [--[no-]progress]`
//!     Opens `<object-dir>/info/commit-graph` (or the split chain under
//!     `<object-dir>/info/commit-graphs/`) through `gix_commitgraph` and runs
//!     `Graph::verify_integrity`. On success stdout is empty and the exit code
//!     is 0, matching stock git byte-for-byte. When no commit-graph file exists
//!     at all, git treats that as success too, and so does this.
//!   * `-h`, the no-subcommand and unknown-subcommand usage paths, reproducing
//!     git's usage block verbatim and its exit code 129.
//!
//! Not implemented — these bail rather than producing a plausible-looking result:
//!   * `git commit-graph write` (any form). The vendored `gix-commitgraph`
//!     crate is read-only: it exposes `File`/`Graph` parsing, access and
//!     verification, and has no serializer (`src/{init,access,verify}.rs`,
//!     `src/file/`). There is no chunk writer, no fanout/OIDL/CDAT/EDGE
//!     emitter and no corrected-commit-date (GDA2) computation anywhere in the
//!     vendored crates, so a byte-identical `.git/objects/info/commit-graph`
//!     cannot be produced without writing that serializer from scratch.
//!   * `verify --shallow`. gix always loads the entire split chain via
//!     `Graph::from_commit_graphs_dir`; there is no "tip file only" entry point,
//!     so the shallow check cannot be honoured faithfully.
//!
//! Verification *failure* text is gix's diagnostic, not git's wording; only the
//! success path is byte-identical. Exit codes follow git: 1 when the file fails
//! to parse, 2 when it parses but fails integrity checks.

use anyhow::{bail, Result};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

/// The parse-options usage block, byte-for-byte as git 2.55 emits it.
const USAGE: &str = "\
usage: git commit-graph verify [--object-dir <dir>] [--shallow] [--[no-]progress]
   or: git commit-graph write [--object-dir <dir>] [--append]
                              [--split[=<strategy>]] [--reachable | --stdin-packs | --stdin-commits]
                              [--changed-paths] [--[no-]max-new-filters <n>] [--[no-]progress]
                              <split-options>

    --[no-]object-dir <dir>
                          the object directory to store the graph

";

pub fn commit_graph(args: &[String]) -> Result<ExitCode> {
    // `-h` is handled before anything else, exactly as parse-options does:
    // usage to stdout, exit 129.
    if args[1..].iter().any(|a| a == "-h") {
        print!("{USAGE}");
        return Ok(ExitCode::from(129));
    }

    let Some(sub) = args.get(1) else {
        eprint!("error: need a subcommand\n{USAGE}");
        return Ok(ExitCode::from(129));
    };

    match sub.as_str() {
        "verify" => verify(&args[2..]),
        "write" => bail!(
            "`commit-graph write` is unsupported: vendored gix-commitgraph is read-only \
             (no chunk serializer, no GDA2 generation writer), so no byte-identical \
             commit-graph file can be produced (ported: verify)"
        ),
        other => {
            eprint!("error: unknown subcommand: `{other}'\n{USAGE}");
            Ok(ExitCode::from(129))
        }
    }
}

/// `git commit-graph verify` — read the commit-graph and check it against itself.
fn verify(args: &[String]) -> Result<ExitCode> {
    let mut object_dir: Option<String> = None;

    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();
        match a {
            "--progress" | "--no-progress" => {} // progress goes to a tty only; nothing to emit
            "--object-dir" => {
                i += 1;
                let Some(dir) = args.get(i) else {
                    bail!("option `--object-dir` requires a value");
                };
                object_dir = Some(dir.clone());
            }
            s if s.starts_with("--object-dir=") => {
                object_dir = Some(s["--object-dir=".len()..].to_string());
            }
            "--shallow" => bail!(
                "`verify --shallow` is unsupported: gix always loads the whole split chain \
                 (ported: --object-dir, --[no-]progress)"
            ),
            s => bail!("unsupported flag {s:?} (ported: --object-dir, --[no-]progress)"),
        }
        i += 1;
    }

    let repo = gix::discover(".")?;
    let repo_objects = repo.objects.store_ref().path().to_owned();

    let objects = match object_dir {
        None => repo_objects,
        Some(dir) => match resolve_object_dir(&dir, &repo_objects) {
            Some(p) => p,
            None => {
                // git: `fatal: could not find object directory matching <dir>`, exit 128.
                eprintln!("fatal: could not find object directory matching {dir}");
                return Ok(ExitCode::from(128));
            }
        },
    };

    let info = objects.join("info");
    // No commit-graph at all is not an error for git: it verifies nothing and succeeds.
    if !info.join("commit-graph").is_file()
        && !info.join("commit-graphs").join("commit-graph-chain").is_file()
    {
        return Ok(ExitCode::SUCCESS);
    }

    let graph = match gix::commitgraph::at(&info) {
        Ok(g) => g,
        Err(e) => {
            eprintln!("error: {e}");
            return Ok(ExitCode::from(1));
        }
    };

    match graph.verify_integrity(|_| -> Result<(), std::convert::Infallible> { Ok(()) }) {
        Ok(_) => Ok(ExitCode::SUCCESS),
        Err(e) => {
            eprintln!("{e}");
            Ok(ExitCode::from(2))
        }
    }
}

/// Map a user-supplied `--object-dir` onto a known object directory.
///
/// git rejects a directory that is neither the repository's own object database
/// nor one of its alternates; mirror that by comparing canonicalised paths
/// against the main objects dir and every entry of `info/alternates`.
fn resolve_object_dir(dir: &str, repo_objects: &Path) -> Option<PathBuf> {
    let want = std::fs::canonicalize(dir).ok()?;
    let mut candidates = vec![repo_objects.to_path_buf()];

    if let Ok(alternates) = std::fs::read_to_string(repo_objects.join("info").join("alternates")) {
        for line in alternates.lines().filter(|l| !l.trim().is_empty()) {
            let p = PathBuf::from(line);
            candidates.push(if p.is_absolute() {
                p
            } else {
                repo_objects.join(p)
            });
        }
    }

    candidates
        .into_iter()
        .find(|c| std::fs::canonicalize(c).is_ok_and(|c| c == want))
        .map(|_| want)
}
