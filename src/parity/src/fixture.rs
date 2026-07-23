//! Deterministic fixture repositories, built once with *stock* git and then
//! copied per case.
//!
//! Stock git is the builder on purpose: the fixture is the shared premise of a
//! differential run, so it must not depend on the implementation under test.
//! Each shape isolates a class of repository state that porcelain has to read
//! correctly — history, refs, index/worktree divergence, conflicts, and the
//! encoding edge cases that break naive path handling.

use crate::env;
use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};
use std::process::Command;

/// A repository shape. Every corpus case names the shape it needs.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Shape {
    /// Single commit, clean worktree. The floor case.
    Linear,
    /// Several commits, two branches, a lightweight and an annotated tag.
    Branched,
    /// A real merge commit with two parents.
    Merged,
    /// Staged, unstaged, and untracked changes coexisting.
    Dirty,
    /// Mid-conflict merge: index has stage 1/2/3 entries.
    Conflicted,
    /// Detached HEAD, as `git submodule update` leaves things.
    Detached,
    /// Unicode, spaces, and quote-worthy bytes in tracked paths.
    AwkwardPaths,
    /// A parent repo with one real submodule — zvcs's stated target topology.
    Submodule,
}

impl Shape {
    pub const ALL: &'static [Shape] = &[
        Shape::Linear,
        Shape::Branched,
        Shape::Merged,
        Shape::Dirty,
        Shape::Conflicted,
        Shape::Detached,
        Shape::AwkwardPaths,
        Shape::Submodule,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Shape::Linear => "linear",
            Shape::Branched => "branched",
            Shape::Merged => "merged",
            Shape::Dirty => "dirty",
            Shape::Conflicted => "conflicted",
            Shape::Detached => "detached",
            Shape::AwkwardPaths => "awkward-paths",
            Shape::Submodule => "submodule",
        }
    }
}

/// Run stock git in `dir`, failing loudly on non-zero exit.
///
/// Fixture construction has no tolerance for partial success: a half-built
/// premise would silently weaken every case that uses it.
fn git(dir: &Path, home: &Path, args: &[&str]) -> Result<String> {
    let mut cmd = Command::new("git");
    env::harden(&mut cmd, home);
    cmd.current_dir(dir).args(args);
    let out = cmd
        .output()
        .with_context(|| format!("spawn stock git {args:?}"))?;
    if !out.status.success() {
        bail!(
            "fixture: stock git {args:?} in {} failed ({})\n{}",
            dir.display(),
            out.status,
            String::from_utf8_lossy(&out.stderr)
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

fn write(dir: &Path, rel: &str, body: &str) -> Result<()> {
    let path = dir.join(rel);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&path, body).with_context(|| format!("write {}", path.display()))
}

/// Build `shape` at `dir`. `home` is the hermetic HOME for the build commands.
pub fn build(shape: Shape, dir: &Path, home: &Path) -> Result<()> {
    std::fs::create_dir_all(dir)?;
    // `-b main` pins the initial branch so the fixture does not inherit the
    // host's `init.defaultBranch` — another way the machine leaks in.
    git(dir, home, &["init", "-q", "-b", "main"])?;

    write(dir, "README.md", "# fixture\n")?;
    write(dir, "src/lib.rs", "pub fn one() -> u32 { 1 }\n")?;
    git(dir, home, &["add", "."])?;
    git(dir, home, &["commit", "-q", "-m", "initial"])?;

    match shape {
        Shape::Linear => {}

        Shape::Branched => {
            write(dir, "src/lib.rs", "pub fn one() -> u32 { 1 }\npub fn two() -> u32 { 2 }\n")?;
            git(dir, home, &["commit", "-qam", "add two"])?;
            git(dir, home, &["tag", "v0.1.0"])?;
            git(dir, home, &["tag", "-a", "v0.2.0", "-m", "annotated"])?;
            git(dir, home, &["branch", "feature"])?;
            git(dir, home, &["checkout", "-q", "feature"])?;
            write(dir, "feature.txt", "feature work\n")?;
            git(dir, home, &["add", "feature.txt"])?;
            git(dir, home, &["commit", "-qm", "feature commit"])?;
            git(dir, home, &["checkout", "-q", "main"])?;
        }

        Shape::Merged => {
            git(dir, home, &["checkout", "-q", "-b", "side"])?;
            write(dir, "side.txt", "side\n")?;
            git(dir, home, &["add", "side.txt"])?;
            git(dir, home, &["commit", "-qm", "side commit"])?;
            git(dir, home, &["checkout", "-q", "main"])?;
            write(dir, "main.txt", "main\n")?;
            git(dir, home, &["add", "main.txt"])?;
            git(dir, home, &["commit", "-qm", "main commit"])?;
            git(dir, home, &["merge", "--no-ff", "-m", "merge side", "side"])?;
        }

        Shape::Dirty => {
            // One of each: staged, unstaged, untracked, and a deletion.
            write(dir, "staged.txt", "staged\n")?;
            git(dir, home, &["add", "staged.txt"])?;
            write(dir, "README.md", "# fixture\nmodified, not staged\n")?;
            write(dir, "untracked.txt", "untracked\n")?;
            std::fs::remove_file(dir.join("src/lib.rs"))?;
        }

        Shape::Conflicted => {
            git(dir, home, &["checkout", "-q", "-b", "theirs"])?;
            write(dir, "conflict.txt", "theirs\n")?;
            git(dir, home, &["add", "conflict.txt"])?;
            git(dir, home, &["commit", "-qm", "theirs"])?;
            git(dir, home, &["checkout", "-q", "main"])?;
            write(dir, "conflict.txt", "ours\n")?;
            git(dir, home, &["add", "conflict.txt"])?;
            git(dir, home, &["commit", "-qm", "ours"])?;
            // Expected to exit non-zero — that *is* the state being built.
            let mut cmd = Command::new("git");
            env::harden(&mut cmd, home);
            cmd.current_dir(dir).args(["merge", "theirs"]);
            let out = cmd.output()?;
            if out.status.success() {
                bail!("fixture: conflicted merge unexpectedly succeeded");
            }
        }

        Shape::Detached => {
            write(dir, "second.txt", "second\n")?;
            git(dir, home, &["add", "second.txt"])?;
            git(dir, home, &["commit", "-qm", "second"])?;
            git(dir, home, &["checkout", "-q", "--detach", "HEAD~1"])?;
        }

        Shape::AwkwardPaths => {
            write(dir, "with space.txt", "space\n")?;
            write(dir, "üñïçødé.txt", "unicode\n")?;
            write(dir, "quote\"name.txt", "quote\n")?;
            write(dir, "nested/deep/path.txt", "deep\n")?;
            git(dir, home, &["add", "."])?;
            git(dir, home, &["commit", "-qm", "awkward paths"])?;
        }

        Shape::Submodule => {
            // A real submodule needs a real upstream; build one beside the parent.
            let upstream = dir.join("..").join(format!("{}-upstream", dir.file_name().unwrap().to_string_lossy()));
            std::fs::create_dir_all(&upstream)?;
            let upstream = upstream.canonicalize()?;
            git(&upstream, home, &["init", "-q", "-b", "main"])?;
            write(&upstream, "mod.txt", "submodule content\n")?;
            git(&upstream, home, &["add", "."])?;
            git(&upstream, home, &["commit", "-qm", "submodule initial"])?;

            git(
                dir,
                home,
                &[
                    "-c",
                    "protocol.file.allow=always",
                    "submodule",
                    "add",
                    "-q",
                    upstream.to_str().context("upstream path not utf-8")?,
                    "sub",
                ],
            )?;
            git(dir, home, &["commit", "-qm", "add submodule"])?;
        }
    }
    Ok(())
}

/// Recursive copy used to clone a prebuilt template per case. Copying beats
/// rebuilding: fixture construction is the slowest part of a run, and every
/// case needs a pristine repo.
pub fn copy_tree(src: &Path, dst: &Path) -> Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let from = entry.path();
        let to = dst.join(entry.file_name());
        let meta = std::fs::symlink_metadata(&from)?;
        if meta.is_dir() {
            copy_tree(&from, &to)?;
        } else if meta.is_symlink() {
            let target = std::fs::read_link(&from)?;
            #[cfg(unix)]
            std::os::unix::fs::symlink(&target, &to)?;
            #[cfg(not(unix))]
            std::fs::copy(&from, &to)?;
        } else {
            std::fs::copy(&from, &to)?;
        }
    }
    Ok(())
}

/// Prebuilt template directories, one per shape.
pub struct Templates {
    root: PathBuf,
    pub home: PathBuf,
}

impl Templates {
    /// Build every shape once under `root`.
    pub fn build_all(root: &Path) -> Result<Self> {
        let home = root.join("home");
        std::fs::create_dir_all(&home)?;
        let templates = root.join("templates");
        std::fs::create_dir_all(&templates)?;
        for &shape in Shape::ALL {
            let dir = templates.join(shape.name());
            if dir.exists() {
                continue;
            }
            build(shape, &dir, &home)
                .with_context(|| format!("building fixture shape {}", shape.name()))?;
        }
        Ok(Self { root: templates, home })
    }

    /// Materialize a pristine copy of `shape` at `dst`.
    pub fn instantiate(&self, shape: Shape, dst: &Path) -> Result<()> {
        copy_tree(&self.root.join(shape.name()), dst)
    }
}
