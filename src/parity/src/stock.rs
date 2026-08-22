//! Finding the *stock* git to measure against.
//!
//! Every number this crate produces is a comparison against real git: the
//! fixtures are built with it, each case is run against it, and the report's
//! denominators (`--list-cmds=main`, `git <cmd> -h`, `git help --config`) are
//! read out of it. All of that was resolved as the bare name `git`, through
//! `PATH`.
//!
//! On the machine this is developed on, `PATH` finds `~/.zvcs/bin/git` — zvcs
//! itself, which shadows git deliberately and reports `git version 2.55.0` to be
//! indistinguishable. So the oracle *was the thing under test*: every case
//! compared zvcs with zvcs and matched by construction, and the report measured
//! zvcs's surface against its own. A harness that cannot fail is worse than no
//! harness, because its output still reads like evidence.
//!
//! So the binary is resolved explicitly here, and *verified*: `ZVCS_STOCK_GIT`
//! when set, else the newest of the usual install locations, with each candidate
//! probed before it is trusted. Nothing falls back to a silent `PATH` lookup —
//! when no stock git can be found, the caller is told rather than handed zvcs.

use anyhow::Context as _;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

/// Where a real git usually lives. Order does not matter: the newest of them
/// wins (see [`git_path`]).
const CANDIDATES: [&str; 3] = ["/usr/bin/git", "/opt/homebrew/bin/git", "/usr/local/bin/git"];

/// `git version X.Y.Z` as a comparable tuple, or `None` when it will not answer.
///
/// A machine usually has more than one git — an OS-vendored one under `/usr/bin`
/// and a current one from a package manager — and the port targets the *newest*
/// one it can find. Measuring against the older one silently reports parity with
/// a git nobody is running: `/usr/bin/git` here is 2.50.1 while the port tracks
/// 2.55.0, and the two disagree about real behaviour.
fn version_of(bin: &Path) -> Option<(u32, u32, u32)> {
    let out = Command::new(bin).arg("--version").env_clear().output().ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    let rest = text.trim().strip_prefix("git version ")?;
    let mut parts = rest.split(['.', ' ', '-']).filter_map(|p| p.parse::<u32>().ok());
    Some((parts.next()?, parts.next().unwrap_or(0), parts.next().unwrap_or(0)))
}

/// Whether `bin` is zvcs wearing git's name.
///
/// The probe is a superset verb, run with an emptied environment: zvcs serves
/// `zverbs` itself, while git looks for a `git-zverbs` on `PATH` and says
/// `'zverbs' is not a git command`. Clearing the environment is what makes the
/// probe sound — zvcs's own installation puts a `git-zverbs` shim on `PATH`, so
/// with it a *stock* git answers the verb too.
///
/// The question can only be answered by *running* the candidate, so the run is
/// made harmless first. An emptied environment leaves a zvcs with no `HOME` to
/// put its state under, and a build old enough to fall back to a relative path
/// writes a `.zvcs/` into whatever directory the probe was standing in — which is
/// the crate root under `cargo test`. Naming a throwaway `ZVCS_HOME` and running
/// from the temp directory bounds that: a stock git ignores both, and any zvcs,
/// however old, keeps its state where it is told.
fn is_zvcs(bin: &Path) -> bool {
    let scratch = std::env::temp_dir().join(format!("zvcs-probe-{}", std::process::id()));
    let answered = Command::new(bin)
        .arg("zverbs")
        .env_clear()
        .env("ZVCS_HOME", &scratch)
        .current_dir(std::env::temp_dir())
        .output()
        .map(|o| o.status.success() && !o.stdout.is_empty())
        .unwrap_or(false);
    let _ = std::fs::remove_dir_all(&scratch);
    answered
}

/// The version this port reproduces, read from its single source of truth:
/// `GIT_VERSION` in `porcelain/version.rs`, which is what `git version` prints.
///
/// It is read rather than repeated here so the two can never drift.
///
/// **This must not fail silently.** An earlier version matched the declaration
/// with `starts_with("const GIT_VERSION")`, which stopped matching the moment the
/// constant gained a `pub(crate)` visibility prefix — `trim_start` removes
/// whitespace, not modifiers. The lookup then returned `None`, the caller's
/// `.filter()` dropped the floor, and the harness measured against whatever git
/// it found while still printing numbers that read exactly like enforced ones.
/// That is the same silent-downgrade failure the floor exists to prevent, so a
/// constant that cannot be located is now a hard error rather than an absent
/// floor: a harness that cannot find its target must say so, not proceed.
fn target_version() -> anyhow::Result<(u32, u32, u32)> {
    let src = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .context("parity crate has no parent directory")?
        .join("extensions/src/porcelain/version.rs");
    let text = std::fs::read_to_string(&src)
        .with_context(|| format!("reading {} for GIT_VERSION", src.display()))?;
    // Match the declaration anywhere on the line, so any visibility modifier
    // (`pub`, `pub(crate)`, none) parses identically.
    let line = text
        .lines()
        .find(|l| l.contains("const GIT_VERSION"))
        .with_context(|| format!("no `const GIT_VERSION` declaration in {}", src.display()))?;
    let literal = line
        .split('"')
        .nth(1)
        .with_context(|| format!("GIT_VERSION in {} has no string literal", src.display()))?;
    let mut parts = literal.split('.').filter_map(|p| p.parse::<u32>().ok());
    let major = parts
        .next()
        .with_context(|| format!("GIT_VERSION {literal:?} has no numeric major component"))?;
    Ok((major, parts.next().unwrap_or(0), parts.next().unwrap_or(0)))
}

/// The newest usable stock git and its version, or `None` when there is none.
///
/// "Usable" means: it exists, it is not zvcs wearing git's name, and it answers
/// `--version`. A binary that will not run — a clobbered install, a signature the
/// kernel rejects — fails the last test and drops out, which is why the version
/// travels with the path: without it the caller cannot tell a resolved oracle
/// from a silently downgraded one.
fn resolved() -> Option<&'static (PathBuf, (u32, u32, u32))> {
    static RESOLVED: OnceLock<Option<(PathBuf, (u32, u32, u32))>> = OnceLock::new();
    RESOLVED
        .get_or_init(|| {
            if let Some(explicit) = std::env::var_os("ZVCS_STOCK_GIT") {
                let path = PathBuf::from(explicit);
                // An explicit choice is honoured even if the probe dislikes it:
                // the caller named a binary, and second-guessing that would make
                // the escape hatch useless. It still has to exist.
                if !path.exists() {
                    return None;
                }
                let v = version_of(&path).unwrap_or((0, 0, 0));
                return Some((path, v));
            }
            CANDIDATES
                .iter()
                .map(PathBuf::from)
                .filter(|p| p.exists() && !is_zvcs(p))
                .filter_map(|p| version_of(&p).map(|v| (v, p)))
                .max_by_key(|(v, _)| *v)
                .map(|(v, p)| (p, v))
        })
        .as_ref()
}

/// The stock git binary, or `None` when the machine has none that is not zvcs.
pub fn git_path() -> Option<&'static Path> {
    resolved().map(|(p, _)| p.as_path())
}

/// The version of the binary [`git_path`] resolved to.
pub fn git_version() -> Option<(u32, u32, u32)> {
    resolved().map(|(_, v)| *v)
}

/// The stock git binary, or an error naming what to do about its absence.
///
/// A git older than the one the port targets is refused rather than measured
/// against. The two disagree about real behaviour, so parity with the older one
/// is parity with a git nobody is running — and the failure mode this guards is
/// silent: when the newest candidate stops answering (overwritten, unsigned,
/// removed), the next-newest is picked up and the run keeps producing numbers
/// that read exactly like the ones before it.
pub fn git() -> anyhow::Result<&'static Path> {
    let Some((path, version)) = resolved() else {
        anyhow::bail!(
            "no stock git found to measure against (tried {}); \
             set ZVCS_STOCK_GIT to one. `git` on PATH is not used: it is zvcs on \
             any machine where the shadow is installed, and comparing zvcs with \
             itself measures nothing",
            CANDIDATES.join(", ")
        );
    };
    // The floor is for the *search*. A caller who names a binary has taken
    // responsibility for it, and the refusal below tells them to do exactly that —
    // applying the floor to their choice as well would make that advice a dead end.
    let explicit = std::env::var_os("ZVCS_STOCK_GIT").is_some();
    if !explicit {
        let target = target_version()?;
        if *version < target {
            let (a, b, c) = *version;
            let (x, y, z) = target;
            anyhow::bail!(
                "the newest stock git found is {} at {} — older than the {}.{}.{} this port \
                 targets, which measures parity against a git nobody is running. Install \
                 {}.{}.{} or newer, or name one with ZVCS_STOCK_GIT",
                format!("{a}.{b}.{c}"),
                path.display(),
                x,
                y,
                z,
                x,
                y,
                z
            );
        }
    }
    Ok(path.as_path())
}

/// A [`Command`] for the stock git, or an error when there is none.
pub fn command() -> anyhow::Result<Command> {
    Ok(Command::new(git()?))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The version floor is only enforceable if the target can be read, and the
    /// read is a text scan of another crate's source — so it breaks silently
    /// whenever that declaration is reformatted. It already did once: the scan
    /// required the line to *start* with `const GIT_VERSION`, and a `pub(crate)`
    /// prefix defeated it, leaving the harness measuring against any git it
    /// found while still printing numbers that read like enforced ones.
    ///
    /// This test is the guard. It fails if the constant is renamed, moved, or
    /// written in a shape the scan cannot parse.
    #[test]
    fn target_version_is_readable() {
        let v = target_version().expect(
            "GIT_VERSION must stay readable from porcelain/version.rs — if this fails, the \
             parity version floor is silently disabled and every reported number is suspect",
        );
        assert!(v.0 >= 2, "git major version should be >= 2, got {v:?}");
    }

    /// Any visibility modifier must parse identically: the declaration's
    /// privacy is not the harness's business, and a change to it must never
    /// disable the floor.
    #[test]
    fn version_line_parses_under_any_visibility() {
        for decl in [
            r#"const GIT_VERSION: &str = "2.55.0";"#,
            r#"pub const GIT_VERSION: &str = "2.55.0";"#,
            r#"pub(crate) const GIT_VERSION: &str = "2.55.0";"#,
            r#"    pub(super) const GIT_VERSION: &str = "2.55.0";"#,
        ] {
            assert!(
                decl.contains("const GIT_VERSION"),
                "scan predicate must match {decl:?}"
            );
            let literal = decl.split('"').nth(1).expect("literal");
            assert_eq!(literal, "2.55.0", "parsed wrong literal from {decl:?}");
        }
    }
}
