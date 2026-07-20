//! The differential runner: one case, run twice, compared four ways.
//!
//! A case is judged on stdout bytes, exit code, and the *resulting repository
//! state* — that last one is what makes this more than an output diff. A
//! command can print the right thing and still corrupt the index; probing the
//! post-state with stock git in both repos catches that.
//!
//! stderr is deliberately not byte-compared. Error prose is not a compatibility
//! surface and zvcs is specified to be terser than git. It is still recorded so
//! a human can read it, and whether the command *errored at all* is compared
//! via the exit code.

use crate::env;
use crate::fixture::{Shape, Templates};
use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use std::process::Command;

/// One invocation to compare.
#[derive(Clone, Debug)]
pub struct Case {
    /// Subcommand, e.g. `rev-parse`. Used for per-command scoring.
    pub cmd: &'static str,
    /// Full argv after the binary name, including the subcommand.
    pub args: Vec<String>,
    /// Repository shape the case runs against.
    pub shape: Shape,
}

impl Case {
    pub fn new(cmd: &'static str, args: &[&str], shape: Shape) -> Self {
        Self { cmd, args: args.iter().map(|s| s.to_string()).collect(), shape }
    }

    /// Stable identity for reporting and for reproducing a single failure.
    pub fn id(&self) -> String {
        format!("{}::{}::{}", self.shape.name(), self.cmd, self.args.join(" "))
    }
}

/// Why a case did not match. Ordered roughly by how damning it is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    /// stdout, exit code, and post-state all agree.
    Match,
    /// zvcs refused the subcommand or a flag it has not ported yet.
    ///
    /// Counted as a **failure** for parity scoring. An unported command is
    /// exactly the gap being measured; scoring it as a skip would inflate the
    /// number, which is the one thing this harness must never do.
    Unsupported,
    /// Same exit code and state, different bytes on stdout.
    StdoutDiff,
    /// Different exit codes.
    ExitDiff,
    /// Same output, but the repository was left in a different state.
    StateDiff,
    /// zvcs crashed (signal, or a panic surfacing as a Rust backtrace).
    Crash,
}

impl Verdict {
    pub fn is_match(self) -> bool {
        self == Verdict::Match
    }

    pub fn label(self) -> &'static str {
        match self {
            Verdict::Match => "MATCH",
            Verdict::Unsupported => "UNSUPPORTED",
            Verdict::StdoutDiff => "STDOUT-DIFF",
            Verdict::ExitDiff => "EXIT-DIFF",
            Verdict::StateDiff => "STATE-DIFF",
            Verdict::Crash => "CRASH",
        }
    }
}

/// Raw result of running one side.
struct Side {
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    code: Option<i32>,
}

/// Full record of a compared case, retained so failures can be printed with
/// enough detail to act on without re-running.
pub struct Outcome {
    pub case: Case,
    pub verdict: Verdict,
    pub stock_stdout: String,
    pub zvcs_stdout: String,
    pub stock_stderr: String,
    pub zvcs_stderr: String,
    pub stock_code: Option<i32>,
    pub zvcs_code: Option<i32>,
    pub stock_state: String,
    pub zvcs_state: String,
}

fn run_side(bin: &Path, repo: &Path, home: &Path, args: &[String]) -> Result<Side> {
    let mut cmd = Command::new(bin);
    env::harden(&mut cmd, home);
    cmd.current_dir(repo).args(args);
    let out = cmd
        .output()
        .with_context(|| format!("spawn {} {:?}", bin.display(), args))?;
    Ok(Side { stdout: out.stdout, stderr: out.stderr, code: out.status.code() })
}

/// Probe repository state with **stock** git, so the probe itself is never the
/// thing under test. Any single probe failing is folded into the digest as an
/// `<err>` marker rather than aborting: a command under test is allowed to
/// leave a repo in a state some probes reject, and that difference is signal.
fn probe_state(repo: &Path, home: &Path) -> String {
    const PROBES: &[&[&str]] = &[
        &["status", "--porcelain=v1", "--untracked-files=all"],
        &["for-each-ref", "--format=%(refname) %(objecttype) %(objectname)"],
        &["rev-parse", "--abbrev-ref", "HEAD"],
        &["rev-parse", "HEAD"],
        &["ls-files", "--stage"],
        &["stash", "list"],
        &["cat-file", "--batch-check", "--batch-all-objects"],
    ];

    let mut digest = String::new();
    for probe in PROBES {
        let mut cmd = Command::new("git");
        env::harden(&mut cmd, home);
        cmd.current_dir(repo).args(*probe);
        let rendered = match cmd.output() {
            Ok(out) if out.status.success() => String::from_utf8_lossy(&out.stdout).into_owned(),
            Ok(_) => "<err>\n".to_string(),
            Err(_) => "<spawn-failed>\n".to_string(),
        };
        digest.push_str(&format!("# {}\n{}", probe.join(" "), rendered));
    }
    digest
}

/// Strip the two things that legitimately differ between two copies of the same
/// repo: their filesystem paths, and the binary's own name in usage text.
///
/// This is the only masking applied, and it is intentionally narrow. Every
/// widening of this function weakens the parity number, so it stays auditable
/// in one place.
fn normalize(raw: &[u8], repo: &Path, home: &Path) -> String {
    let mut s = String::from_utf8_lossy(raw).into_owned();
    for (path, token) in [(repo, "<REPO>"), (home, "<HOME>")] {
        let p = path.to_string_lossy().into_owned();
        s = s.replace(&p, token);
        // Both the symlinked and resolved forms show up on macOS (/tmp vs /private/tmp).
        if let Ok(canon) = path.canonicalize() {
            s = s.replace(&canon.to_string_lossy().into_owned(), token);
        }
    }
    s
}

/// True when zvcs is reporting a gap rather than disagreeing about behavior.
/// Matched against the exact wording `dispatch.rs` and the porcelain modules
/// emit; kept narrow so a genuine error is never miscounted as a known gap.
fn is_unsupported(stderr: &str) -> bool {
    stderr.contains("not yet ported")
        || stderr.contains("unsupported flag")
        || stderr.contains("is ported so far")
}

fn looks_like_panic(stderr: &str) -> bool {
    stderr.contains("panicked at") || stderr.contains("RUST_BACKTRACE")
}

/// Run one case against both implementations and judge it.
pub fn run_case(
    case: &Case,
    zvcs_bin: &Path,
    templates: &Templates,
    workdir: &Path,
) -> Result<Outcome> {
    let stock_repo = workdir.join("stock");
    let zvcs_repo = workdir.join("zvcs");
    let _ = std::fs::remove_dir_all(&stock_repo);
    let _ = std::fs::remove_dir_all(&zvcs_repo);
    templates.instantiate(case.shape, &stock_repo)?;
    templates.instantiate(case.shape, &zvcs_repo)?;

    let home = &templates.home;
    let stock = run_side(Path::new("git"), &stock_repo, home, &case.args)?;
    let zvcs = run_side(zvcs_bin, &zvcs_repo, home, &case.args)?;

    let stock_state = probe_state(&stock_repo, home);
    let zvcs_state = probe_state(&zvcs_repo, home);

    let stock_stdout = normalize(&stock.stdout, &stock_repo, home);
    let zvcs_stdout = normalize(&zvcs.stdout, &zvcs_repo, home);
    let stock_stderr = normalize(&stock.stderr, &stock_repo, home);
    let zvcs_stderr = normalize(&zvcs.stderr, &zvcs_repo, home);
    let stock_state_n = normalize(stock_state.as_bytes(), &stock_repo, home);
    let zvcs_state_n = normalize(zvcs_state.as_bytes(), &zvcs_repo, home);

    // Ordering matters: a crash outranks a gap, and a gap outranks the ordinary
    // diffs it would otherwise masquerade as.
    let verdict = if looks_like_panic(&zvcs_stderr) || zvcs.code.is_none() {
        Verdict::Crash
    } else if is_unsupported(&zvcs_stderr) {
        Verdict::Unsupported
    } else if stock.code != zvcs.code {
        Verdict::ExitDiff
    } else if stock_stdout != zvcs_stdout {
        Verdict::StdoutDiff
    } else if stock_state_n != zvcs_state_n {
        Verdict::StateDiff
    } else {
        Verdict::Match
    };

    Ok(Outcome {
        case: case.clone(),
        verdict,
        stock_stdout,
        zvcs_stdout,
        stock_stderr,
        zvcs_stderr,
        stock_code: stock.code,
        zvcs_code: zvcs.code,
        stock_state: stock_state_n,
        zvcs_state: zvcs_state_n,
    })
}

/// Locate the zvcs `git` binary. Explicit override wins; otherwise the usual
/// cargo output paths, debug first to match the project's local-dev rule.
pub fn locate_zvcs_bin(explicit: Option<&str>) -> Result<PathBuf> {
    if let Some(p) = explicit {
        let p = PathBuf::from(p);
        anyhow::ensure!(p.exists(), "zvcs binary not found at {}", p.display());
        return Ok(p);
    }
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .context("locating zvcs repo root")?
        .to_path_buf();
    for candidate in ["target/debug/git", "target/release/git"] {
        let p = root.join(candidate);
        if p.exists() {
            return Ok(p);
        }
    }
    anyhow::bail!("no zvcs `git` binary found; run `cargo build` first")
}
