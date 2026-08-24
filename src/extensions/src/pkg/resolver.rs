//! Resolve a user-supplied source spec into a staged plugin directory. Ported
//! from the zshrs package manager's `pkg/resolver.rs`; the source forms and the
//! `@REF` pinning are unchanged.
//!
//! Source forms accepted by `git znative add <SOURCE>`:
//! - `owner/repo` or `github:owner/repo` → clone `https://github.com/owner/repo`
//! - `git+URL`, or any URL ending in `.git` → clone `URL`
//! - `path:DIR`, an absolute path, or `./rel`, `../rel` → a local directory
//!
//! `@REF` may be appended to a git/github source to pin a branch, tag or commit
//! (`owner/repo@v1.2.0`). Clones land under `$ZVCS_HOME/pkg/git/` and the
//! caller copies the loadable subset into the content-addressed store.
//!
//! The clone runs through **this binary** ([`crate::hosted::git_exe`]), not a
//! stock git off PATH: zvcs serves `clone` natively, so installing a plugin
//! needs no second VCS on the machine.

use std::path::{Path, PathBuf};
use std::process::Command;

use super::store::Store;
use super::{PkgError, PkgResult};

/// A staged source ready to install into the store.
pub struct Staged {
    /// Working directory containing the plugin tree.
    pub dir: PathBuf,
    /// Inferred plugin name (repo/dir basename).
    pub name: String,
    /// Provenance label recorded in the index: `github:owner/repo`, `git+URL`,
    /// or `path+file://DIR`.
    pub source: String,
}

/// Resolve `spec` into a [`Staged`] tree. Clones land under `store.git_dir()`;
/// local paths are used in place.
pub fn resolve(spec: &str, store: &Store) -> PkgResult<Staged> {
    let (base, git_ref) = split_ref(spec);

    // Local path forms.
    if let Some(p) = local_path(base) {
        let dir =
            p.canonicalize().map_err(|e| PkgError::Resolve(format!("path {}: {e}", p.display())))?;
        if !dir.is_dir() {
            return Err(PkgError::Resolve(format!("path {} is not a directory", dir.display())));
        }
        let name = basename(&dir);
        let source = format!("path+file://{}", dir.display());
        return Ok(Staged { dir, name, source });
    }

    // Git / GitHub forms.
    let (url, label, name) = git_url(base)?;
    store.ensure_layout().map_err(|e| PkgError::Resolve(e.to_string()))?;
    let dir = store.git_dir().join(&name);
    if dir.exists() {
        std::fs::remove_dir_all(&dir)
            .map_err(|e| PkgError::Io(format!("clear {}: {e}", dir.display())))?;
    }
    git_clone(&url, &dir, git_ref)?;
    Ok(Staged {
        dir,
        name,
        // Record the pinned ref in the source so `update` re-fetches the SAME
        // version and `load owner/repo@REF` matches only that pin.
        source: label_with_ref(label, git_ref),
    })
}

/// Append `@REF` to a provenance label when a version/ref was pinned, so the
/// recorded source round-trips back through [`resolve`]/[`split_ref`].
fn label_with_ref(label: String, git_ref: Option<&str>) -> String {
    match git_ref {
        Some(r) => format!("{label}@{r}"),
        None => label,
    }
}

/// The provenance label a `spec` WOULD receive, computed WITHOUT cloning or
/// network access. Used by `znative load <spec>` to check whether a source is
/// already installed (the index keys on this label, since a repo's basename
/// often differs from its `znative.toml` plugin name). Returns `None` for a
/// bare plugin name, which is not a source form.
pub fn source_label(spec: &str) -> Option<String> {
    let (base, git_ref) = split_ref(spec);
    if let Some(p) = local_path(base) {
        // Match the `path+file://<canonical>` the installer records.
        let dir = p.canonicalize().ok()?;
        return Some(format!("path+file://{}", dir.display()));
    }
    git_url(base).ok().map(|(_url, label, _name)| label_with_ref(label, git_ref))
}

/// Split a trailing `@REF` (branch/tag/commit) off a spec. Only splits on an
/// `@` that comes after the last `/`, so `git@host:owner/repo.git` SSH URLs
/// keep theirs.
fn split_ref(spec: &str) -> (&str, Option<&str>) {
    if let Some(at) = spec.rfind('@') {
        let after_slash = spec.rfind('/').map(|s| at > s).unwrap_or(true);
        if after_slash && at + 1 < spec.len() {
            return (&spec[..at], Some(&spec[at + 1..]));
        }
    }
    (spec, None)
}

/// Recognize local-path forms; returns the path when `spec` is one.
fn local_path(spec: &str) -> Option<PathBuf> {
    if let Some(rest) = spec.strip_prefix("path:") {
        return Some(PathBuf::from(rest));
    }
    if spec.starts_with('/')
        || spec.starts_with("./")
        || spec.starts_with("../")
        || spec.starts_with('~')
    {
        let expanded = match spec.strip_prefix("~/") {
            Some(rest) => match std::env::var_os("HOME") {
                Some(home) => PathBuf::from(home).join(rest),
                None => PathBuf::from(spec),
            },
            None => PathBuf::from(spec),
        };
        return Some(expanded);
    }
    None
}

/// Map a non-local spec to `(clone_url, provenance_label, name)`.
fn git_url(spec: &str) -> PkgResult<(String, String, String)> {
    if let Some(rest) = spec.strip_prefix("git+") {
        let name = repo_basename(rest);
        return Ok((rest.to_string(), format!("git+{rest}"), name));
    }
    if let Some(rest) = spec.strip_prefix("github:") {
        let owner_repo = rest.trim_end_matches(".git");
        let url = format!("https://github.com/{owner_repo}");
        let name = repo_basename(&url);
        return Ok((url, format!("github:{owner_repo}"), name));
    }
    if spec.ends_with(".git") || spec.contains("://") {
        let name = repo_basename(spec);
        return Ok((spec.to_string(), format!("git+{spec}"), name));
    }
    // `owner/repo` shorthand → GitHub.
    if spec.split('/').count() == 2 && !spec.contains(' ') {
        let owner_repo = spec.trim_end_matches(".git");
        let url = format!("https://github.com/{owner_repo}");
        let name = repo_basename(&url);
        return Ok((url, format!("github:{owner_repo}"), name));
    }
    Err(PkgError::Resolve(format!(
        "unrecognized source '{spec}': expected owner/repo, github:owner/repo, \
         git+URL, or a local path"
    )))
}

/// This binary, which serves `clone` natively.
fn git() -> Command {
    Command::new(crate::hosted::git_exe().unwrap_or_else(|_| PathBuf::from("git")))
}

/// `git clone --depth 1 [--branch REF] URL DIR` — shallow for speed.
fn git_clone(url: &str, dir: &Path, git_ref: Option<&str>) -> PkgResult<()> {
    let mut cmd = git();
    cmd.arg("clone").arg("--depth").arg("1");
    if let Some(r) = git_ref {
        cmd.arg("--branch").arg(r);
    }
    cmd.arg(url).arg(dir);
    let out = cmd.output().map_err(|e| PkgError::Resolve(format!("git clone: {e}")))?;
    if !out.status.success() {
        // Retry without `--branch`: a REF that is a commit id cannot be reached
        // by a shallow branch clone. Fall back to a full clone + checkout.
        if let Some(r) = git_ref {
            return git_clone_checkout(url, dir, r);
        }
        return Err(PkgError::Resolve(format!(
            "git clone {url} failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    Ok(())
}

/// Full clone + `git checkout REF` — the fallback when a shallow `--branch`
/// clone cannot reach an arbitrary commit.
fn git_clone_checkout(url: &str, dir: &Path, git_ref: &str) -> PkgResult<()> {
    if dir.exists() {
        let _ = std::fs::remove_dir_all(dir);
    }
    let out =
        git().arg("clone").arg(url).arg(dir).output().map_err(|e| PkgError::Resolve(format!("git clone: {e}")))?;
    if !out.status.success() {
        return Err(PkgError::Resolve(format!(
            "git clone {url} failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    let out = git()
        .current_dir(dir)
        .arg("checkout")
        .arg(git_ref)
        .output()
        .map_err(|e| PkgError::Resolve(format!("git checkout: {e}")))?;
    if !out.status.success() {
        return Err(PkgError::Resolve(format!(
            "git checkout {git_ref} failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    Ok(())
}

/// Basename of a directory path, sans trailing separators.
fn basename(p: &Path) -> String {
    p.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "plugin".into())
}

/// Repo name from a clone URL: strip `.git`, take the last path segment.
fn repo_basename(url: &str) -> String {
    url.trim_end_matches('/')
        .trim_end_matches(".git")
        .rsplit(['/', ':'])
        .next()
        .unwrap_or("plugin")
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_ref_only_after_last_slash() {
        assert_eq!(split_ref("o/r@v1"), ("o/r", Some("v1")));
        assert_eq!(split_ref("o/r"), ("o/r", None));
        // An SSH URL's `@` must not split.
        assert_eq!(split_ref("git@github.com:o/r.git"), ("git@github.com:o/r.git", None));
        // A trailing `@` is not a ref.
        assert_eq!(split_ref("o/r@"), ("o/r@", None));
    }

    #[test]
    fn git_url_forms() {
        let (u, l, n) = git_url("owner/repo").unwrap();
        assert_eq!((u.as_str(), l.as_str(), n.as_str()), (
            "https://github.com/owner/repo",
            "github:owner/repo",
            "repo"
        ));
        let (u, _, n) = git_url("github:a/b").unwrap();
        assert_eq!((u.as_str(), n.as_str()), ("https://github.com/a/b", "b"));
        let (u, l, _) = git_url("git+https://x.com/y.git").unwrap();
        assert_eq!((u.as_str(), l.as_str()), ("https://x.com/y.git", "git+https://x.com/y.git"));
        assert!(git_url("not a source").is_err());
    }

    #[test]
    fn local_path_forms() {
        assert!(local_path("path:/tmp/x").is_some());
        assert!(local_path("/abs").is_some());
        assert!(local_path("./rel").is_some());
        assert!(local_path("owner/repo").is_none());
    }

    #[test]
    fn a_pinned_ref_round_trips_through_the_recorded_label() {
        // `update` re-resolves from the label, so the pin has to survive it.
        assert_eq!(source_label("owner/repo@v1.2.0").unwrap(), "github:owner/repo@v1.2.0");
        let label = source_label("owner/repo@v1.2.0").unwrap();
        let (base, r) = split_ref(&label);
        assert_eq!((base, r), ("github:owner/repo", Some("v1.2.0")));
    }
}
