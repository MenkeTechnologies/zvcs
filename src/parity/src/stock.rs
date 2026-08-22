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
//!
//! # One oracle cannot tell a port defect from a version difference
//!
//! Picking the newest git and calling its answer *the* answer has a blind spot
//! that no amount of comparing harder can close. This machine has two real gits:
//! `/usr/bin/git` is 2.50.1 (Apple Git-155) and `/opt/homebrew/bin/git` is
//! 2.55.0. When zvcs disagrees with 2.55.0, exactly one of two things is true and
//! a single-oracle harness reports both with the same words:
//!
//!   * the port is wrong — and every other git agrees it is wrong; or
//!   * git itself changed between 2.50 and 2.55, and the port reproduces the
//!     older behaviour.
//!
//! The second is not a parity failure of the kind the report's number is about,
//! and filing it as one sends somebody to "fix" code to match a behaviour that
//! upstream changed on purpose. It also runs the other way: an expectation
//! captured by hand against `/usr/bin/git` and pinned into a test file is a
//! literal from the *wrong* git, and nothing in a one-oracle harness detects
//! that. A user reported a `checkout` divergence against 2.50.1 exactly this way;
//! the two gits happened to agree on that case, and the harness could not have
//! said so.
//!
//! So a **second oracle** is resolved here too, and the runner asks it the same
//! question. [`alt_git`] is the newest *other* real git on the machine, or one
//! the caller names with `--alt-git` / `ZVCS_STOCK_GIT_ALT`. It is discovered
//! rather than opted into, because the dimension is only worth having if it is on
//! by default on the machines that have two gits — an opt-in flag is a flag
//! nobody remembers on the run that would have needed it — and because the cost
//! is bounded elsewhere: `runner::compare_in` asks the second oracle only about
//! cases that already failed against the first, which is 0 extra invocations on
//! the ~99% that pass.
//!
//! When the machine has one git, [`alt_git`] is `None` and every caller is
//! exactly what it was: no third invocation, no new report line, no new column.

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
/// It is read rather than repeated here so the two can never drift. `None` means
/// the constant could not be found, and the floor below is then not enforced —
/// a harness that cannot locate the target must not invent one.
fn target_version() -> Option<(u32, u32, u32)> {
    let src = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()?
        .join("extensions/src/porcelain/version.rs");
    let text = std::fs::read_to_string(src).ok()?;
    let line = text.lines().find(|l| l.trim_start().starts_with("const GIT_VERSION"))?;
    let literal = line.split('"').nth(1)?;
    let mut parts = literal.split('.').filter_map(|p| p.parse::<u32>().ok());
    Some((parts.next()?, parts.next().unwrap_or(0), parts.next().unwrap_or(0)))
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
    if let Some(target) = target_version().filter(|_| !explicit) {
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

// ---------------------------------------------------------------------------
// The second oracle
// ---------------------------------------------------------------------------

/// Which second git to measure against, once the caller has had a say.
///
/// Three states rather than an `Option<PathBuf>`, because "the caller said
/// nothing" and "the caller said no" are different instructions and collapsing
/// them would make one of the two unreachable: with `Auto` as the meaning of
/// absence there has to be a way to spell *off*, and with `Off` as the meaning of
/// absence the dimension would be one nobody switches on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AltChoice {
    /// No preference expressed: take the newest *other* real git on the machine,
    /// if there is one.
    Auto,
    /// `--alt-git <path>`, or `ZVCS_STOCK_GIT_ALT=<path>`.
    Named(PathBuf),
    /// `--no-alt-git`, or `ZVCS_STOCK_GIT_ALT` set to `none`, `off` or empty.
    Off,
}

/// The caller's choice, set once before any case runs.
static ALT_CHOICE: OnceLock<AltChoice> = OnceLock::new();

/// Record the caller's `--alt-git` / `--no-alt-git` decision.
///
/// Called once from `main` before the worker pool starts, so every thread reads
/// the same answer and the resolution below can be memoized. A second call is
/// ignored rather than panicking: this is a knob, not an invariant, and a
/// harness that aborts a five-minute sweep over a duplicated setter would be
/// trading a real measurement for a tidy one.
pub fn set_alt_choice(choice: AltChoice) {
    let _ = ALT_CHOICE.set(choice);
}

/// Read `ZVCS_STOCK_GIT_ALT` into a choice.
///
/// Pure so the spelling of *off* is testable without an environment: `none`,
/// `off` and the empty string all disable, anything else names a binary. The
/// disabling spellings exist because the variable is the only knob a script or a
/// CI job can reach — a caller who cannot pass `--no-alt-git` still has to be
/// able to say no, and `ZVCS_STOCK_GIT_ALT=` (set but empty) is what shell code
/// produces when it means exactly that.
fn alt_choice_from(raw: Option<&str>) -> AltChoice {
    match raw {
        None => AltChoice::Auto,
        Some(v) if v.is_empty() || v.eq_ignore_ascii_case("none") || v.eq_ignore_ascii_case("off") => {
            AltChoice::Off
        }
        Some(v) => AltChoice::Named(PathBuf::from(v)),
    }
}

/// Whether two paths name the same file on disk, following symlinks.
///
/// Compared canonically rather than textually, and that is not defensive
/// programming — it is the case this machine actually has.
/// `/usr/local/bin/git` is a symlink to `/opt/homebrew/bin/git`, so both are in
/// [`CANDIDATES`], both answer `2.55.0`, and a textual `!=` would happily hand
/// back the primary oracle under its other name as the "second" one. Every case
/// would then report that the two gits agree — which reads as independent
/// corroboration of every port defect and is worth exactly nothing.
///
/// Falls back to the textual comparison when either path will not canonicalize,
/// which is the conservative direction: an unresolvable path that happens to be
/// the primary is rejected, not accepted.
fn same_file(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(x), Ok(y)) => x == y,
        _ => a == b,
    }
}

/// The second oracle and its version, or `None` when the machine has only one
/// git — or the caller said not to use a second.
///
/// The `Auto` search reuses [`CANDIDATES`] and the same three filters the primary
/// search applies (exists, is not zvcs wearing git's name, answers `--version`),
/// and adds two of its own. Both exist to stop the harness from comparing an
/// oracle with itself, which is the exact failure this module was written for —
/// it would report "the two gits agree" on every case, which reads like
/// corroboration and is worth nothing:
///
///   * **not the same file as the primary oracle**, compared canonically. See
///     [`same_file`]: this machine has `/usr/local/bin/git` symlinked to
///     `/opt/homebrew/bin/git` and both are candidates.
///   * **not the same version as the primary oracle.** A second git of the same
///     version is not a second opinion about a *version* difference; whatever it
///     disagreed about would be an install difference, and whatever it agreed
///     about would be agreement with the primary restated. Both are noise in the
///     one dimension this is for. A caller who wants two builds of one version
///     compared can still say so with `--alt-git`, which is honoured as named.
///
/// **No version floor here, in either direction.** The primary oracle has one
/// ([`git`] refuses a git older than the port targets) because parity *is* defined
/// against the newest git. The second oracle's whole job is to be a different
/// version — usually an older one — so applying the floor to it would reject
/// precisely the binary that makes the dimension mean anything. Its version
/// travels with its path so every report line can say which git said what, which
/// is what turns "these disagree" into a fact a reader can act on.
fn alt_resolved() -> Option<&'static (PathBuf, (u32, u32, u32))> {
    static RESOLVED: OnceLock<Option<(PathBuf, (u32, u32, u32))>> = OnceLock::new();
    RESOLVED
        .get_or_init(|| {
            let choice = ALT_CHOICE.get().cloned().unwrap_or_else(|| {
                alt_choice_from(
                    std::env::var("ZVCS_STOCK_GIT_ALT").ok().as_deref(),
                )
            });
            let primary = git_path()?;
            let primary_version = git_version()?;
            match choice {
                AltChoice::Off => None,
                AltChoice::Named(path) => {
                    // Honoured as named, exactly like `ZVCS_STOCK_GIT` — with the
                    // one refusal that is not second-guessing: naming the primary
                    // oracle again, under any of its names, produces a dimension
                    // that agrees with itself on every case, so it is reported as
                    // *absent* rather than as a second opinion. The version is not
                    // second-guessed, because a caller comparing two builds of one
                    // version has asked a question this module has no business
                    // overruling.
                    if !path.exists() || same_file(&path, primary) {
                        return None;
                    }
                    let v = version_of(&path).unwrap_or((0, 0, 0));
                    Some((path, v))
                }
                AltChoice::Auto => CANDIDATES
                    .iter()
                    .map(PathBuf::from)
                    .filter(|p| p.exists() && !same_file(p, primary) && !is_zvcs(p))
                    .filter_map(|p| version_of(&p).map(|v| (v, p)))
                    .filter(|(v, _)| *v != primary_version)
                    .max_by_key(|(v, _)| *v)
                    .map(|(v, p)| (p, v)),
            }
        })
        .as_ref()
}

/// The second oracle's binary and version, or `None` when there is not one.
pub fn alt_git() -> Option<(&'static Path, (u32, u32, u32))> {
    alt_resolved().map(|(p, v)| (p.as_path(), *v))
}

/// `2.50.1` from `(2, 50, 1)` — how every report line names a git.
pub fn version_label(v: (u32, u32, u32)) -> String {
    format!("{}.{}.{}", v.0, v.1, v.2)
}

#[cfg(test)]
mod tests {
    use super::{alt_choice_from, same_file, version_label, AltChoice};
    use std::path::{Path, PathBuf};

    /// Every spelling of "no second oracle" a caller can reach through the only
    /// knob a script has.
    ///
    /// The empty string is the one that matters: `ZVCS_STOCK_GIT_ALT=` is what a
    /// shell writes for an unset-but-declared variable, and reading it as a path
    /// would make the dimension resolve to `""`, which exists on no machine and
    /// would silently degrade to "absent" for the wrong reason.
    #[test]
    fn the_second_oracle_can_be_switched_off_through_the_environment() {
        for raw in ["", "none", "NONE", "off", "Off"] {
            assert_eq!(alt_choice_from(Some(raw)), AltChoice::Off, "{raw:?}");
        }
    }

    /// An unset variable means *auto*, never *off*. This is the property that
    /// makes the dimension on by default on a machine with two gits — an opt-in
    /// second oracle is one nobody switches on for the run that needed it.
    #[test]
    fn an_unset_variable_leaves_the_search_to_the_harness() {
        assert_eq!(alt_choice_from(None), AltChoice::Auto);
    }

    /// Anything else names a binary, and is taken literally — including a path
    /// that does not exist, which `alt_resolved` then drops. Parsing is not
    /// validation: a caller who typos a path must not have it quietly reinterpreted
    /// as one of the machine's own gits.
    #[test]
    fn any_other_value_names_a_binary() {
        assert_eq!(
            alt_choice_from(Some("/usr/bin/git")),
            AltChoice::Named(PathBuf::from("/usr/bin/git"))
        );
        assert_eq!(
            alt_choice_from(Some("/nope/git")),
            AltChoice::Named(PathBuf::from("/nope/git"))
        );
    }

    /// Two candidates that name one binary must never become two oracles.
    ///
    /// The textual fallback is what is pinned here, because it is the branch a
    /// path that will not canonicalize takes and the direction it errs in is the
    /// safe one: equal spellings are treated as the same file (so a primary named
    /// twice is rejected), different spellings that cannot be resolved are treated
    /// as different (so a real second git is not silently dropped because its path
    /// is odd). The symlink branch is the reason the function exists at all —
    /// `/usr/local/bin/git` points at `/opt/homebrew/bin/git` on this machine and
    /// both are candidates — but exercising it would need a filesystem, and these
    /// tests build nothing.
    #[test]
    fn a_path_that_will_not_resolve_falls_back_to_comparing_the_spelling() {
        let a = Path::new("/definitely/not/here/git");
        let b = Path::new("/definitely/not/here/other-git");
        assert!(same_file(a, a));
        assert!(!same_file(a, b));
    }

    #[test]
    fn a_version_is_labelled_the_way_git_prints_it() {
        assert_eq!(version_label((2, 50, 1)), "2.50.1");
        assert_eq!(version_label((0, 0, 0)), "0.0.0");
    }
}
