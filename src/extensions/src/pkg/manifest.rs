//! A plugin's optional `znative.toml`, declaring how the plugin is loaded.
//!
//! Ported from the zshrs package manager's `pkg/manifest.rs`, with the shell
//! kind (`source` + `fpath`) replaced by the VCS one: a script plugin is a repo
//! of `git-<verb>` executables, which is the shape every third-party git
//! subcommand already ships in (`git-lfs`, `git-flow`, `git-absorb`, …).
//!
//! Schema:
//! ```toml
//! [plugin]
//! name = "hello"
//! version = "0.1.0"
//! description = "a native zvcs subcommand"
//!
//! # Native (Rust cdylib) plugin — dlopened through the znative ABI:
//! [native]
//! lib = "hello"            # produces lib<lib>.{dylib,so}
//!
//! # …OR script plugin — `git-<verb>` executables run from the store:
//! [script]
//! bin = ["bin"]            # dirs holding the executables (default ".")
//! verbs = ["hello"]        # the subcommands they provide (default: scanned)
//! ```

use serde::{Deserialize, Serialize};
use std::path::Path;

use super::{PkgError, PkgResult};

/// Manifest filename, at the root of a plugin's tree.
pub const MANIFEST_FILE: &str = "znative.toml";

/// Parsed `znative.toml`.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PluginManifest {
    /// `[plugin]` metadata.
    #[serde(default)]
    pub plugin: PluginMeta,
    /// `[native]` — present for Rust cdylib plugins.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub native: Option<NativeSpec>,
    /// `[script]` — present for `git-<verb>` executable plugins.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub script: Option<ScriptSpec>,
}

/// `[plugin]` table.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PluginMeta {
    /// `name` — defaults to the source basename when absent.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub name: String,
    /// `version` — defaults to `"0.0.0"` when absent.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub version: String,
    /// One-line description (shown by `znative list`/`info`).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
}

/// `[native]` — a Rust cdylib plugin using the `znative` SDK.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct NativeSpec {
    /// Library file stem — produces `lib<lib>.{dylib,so}`. When empty the
    /// installer infers it from the built artifact.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub lib: String,
    /// When true, run `cargo build --release` in the staged tree before looking
    /// for the cdylib. Defaults to true when a `Cargo.toml` exists.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub build: Option<bool>,
}

/// `[script]` — a plugin shipping `git-<verb>` executables.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ScriptSpec {
    /// Directories holding the executables, relative to the plugin root.
    /// Defaults to `["."]`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub bin: Vec<String>,
    /// The subcommands provided. Defaults to every `git-<verb>` found in `bin`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub verbs: Vec<String>,
}

/// Resolved plugin kind — either an explicit `znative.toml` or an inferred
/// layout.
#[derive(Debug, Clone)]
pub enum PluginKind {
    /// Rust cdylib loaded through the znative ABI.
    Native(NativeSpec),
    /// `git-<verb>` executables run out of the store.
    Script(ScriptSpec),
}

impl PluginManifest {
    /// Parse a `znative.toml` string.
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> PkgResult<PluginManifest> {
        toml::from_str::<PluginManifest>(s)
            .map_err(|e| PkgError::Manifest(format!("{MANIFEST_FILE}: {}", e.message())))
    }

    /// Load a plugin's `znative.toml` if present at `dir/znative.toml`.
    pub fn load(dir: &Path) -> PkgResult<Option<PluginManifest>> {
        let path = dir.join(MANIFEST_FILE);
        if !path.is_file() {
            return Ok(None);
        }
        let s = std::fs::read_to_string(&path)
            .map_err(|e| PkgError::Io(format!("read {}: {e}", path.display())))?;
        Ok(Some(PluginManifest::from_str(&s)?))
    }
}

impl PluginKind {
    /// Determine the plugin kind for a staged tree. Prefers an explicit
    /// `znative.toml` (`[native]` beats `[script]` when both are present), then
    /// falls back to layout detection:
    ///
    /// 1. A prebuilt `lib*.{dylib,so}` at the root, or a `Cargo.toml` whose
    ///    `[lib] crate-type` mentions `cdylib` → [`PluginKind::Native`].
    /// 2. Any executable `git-<verb>` at the root or in `bin/` →
    ///    [`PluginKind::Script`].
    ///
    /// Returns [`PkgError::Unknown`] when nothing matches.
    pub fn detect(dir: &Path, manifest: Option<&PluginManifest>) -> PkgResult<PluginKind> {
        if let Some(m) = manifest {
            if let Some(n) = &m.native {
                return Ok(PluginKind::Native(n.clone()));
            }
            if let Some(s) = &m.script {
                let mut s = s.clone();
                if s.bin.is_empty() {
                    s.bin.push(".".into());
                }
                if s.verbs.is_empty() {
                    s.verbs = scan_verbs(dir, &s.bin);
                }
                return Ok(PluginKind::Script(s));
            }
        }
        // Native first: a repo can carry helper scripts alongside a build tree,
        // but if it declares a cdylib it is a native plugin.
        if has_cdylib(dir) || cargo_is_cdylib(dir) {
            return Ok(PluginKind::Native(NativeSpec::default()));
        }
        let bin: Vec<String> =
            [".", "bin"].iter().filter(|d| dir.join(d).is_dir()).map(|d| (*d).to_string()).collect();
        let verbs = scan_verbs(dir, &bin);
        if verbs.is_empty() {
            return Err(PkgError::Unknown(
                "could not determine plugin kind: no znative.toml, no executable \
                 git-<verb>, and no Rust cdylib/Cargo.toml"
                    .into(),
            ));
        }
        // Keep only the directories that actually contributed a verb.
        let bin = bin.into_iter().filter(|d| !scan_verbs(dir, &[d.clone()]).is_empty()).collect();
        Ok(PluginKind::Script(ScriptSpec { bin, verbs }))
    }
}

/// Every `git-<verb>` executable in `dirs` (relative to `root`), as verb names,
/// sorted and deduplicated.
pub fn scan_verbs(root: &Path, dirs: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for d in dirs {
        let Ok(rd) = std::fs::read_dir(root.join(d)) else { continue };
        for entry in rd.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            let Some(verb) = name.strip_prefix("git-") else { continue };
            if verb.is_empty() || !is_executable(&entry.path()) {
                continue;
            }
            out.push(verb.to_string());
        }
    }
    out.sort();
    out.dedup();
    out
}

/// True if `path` is a regular file with any execute bit set — the same test
/// `execvp` would pass or fail on.
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path)
        .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

/// True if a `lib*.{dylib,so}` exists at the tree root (a prebuilt cdylib).
fn has_cdylib(dir: &Path) -> bool {
    let Ok(rd) = std::fs::read_dir(dir) else { return false };
    for entry in rd.flatten() {
        let n = entry.file_name();
        let n = n.to_string_lossy();
        if n.starts_with("lib") && (n.ends_with(".dylib") || n.ends_with(".so")) {
            return true;
        }
    }
    false
}

/// True if `Cargo.toml` declares a `cdylib` crate-type (so `cargo build`
/// produces a dlopen-able library).
fn cargo_is_cdylib(dir: &Path) -> bool {
    std::fs::read_to_string(dir.join("Cargo.toml")).map(|s| s.contains("cdylib")).unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    /// A private directory for one test.
    ///
    /// The counter is what makes it private. Seeding the name from
    /// `SystemTime::now().subsec_nanos()` looks unique and is not: macOS
    /// reports the wall clock at microsecond granularity, so the low three
    /// digits are always zero and two of these tests running in parallel
    /// inside the same process — same pid, same microsecond — got the *same*
    /// directory and overwrote each other's store. That is what failed
    /// `index_round_trip_and_side_tables` on the macOS CI runner while every
    /// local run passed.
    fn tmp() -> std::path::PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static N: AtomicU64 = AtomicU64::new(0);
        let p = std::env::temp_dir().join(format!(
            "zvcs-manifest-test-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    fn exe(path: &Path) {
        std::fs::write(path, b"#!/bin/sh\nexit 0\n").unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    #[test]
    fn parses_native_manifest() {
        let m = PluginManifest::from_str(
            "[plugin]\nname='x'\nversion='0.1.0'\n[native]\nlib='foo'\n",
        )
        .unwrap();
        assert_eq!(m.plugin.name, "x");
        assert_eq!(m.native.unwrap().lib, "foo");
    }

    #[test]
    fn parses_script_manifest_and_scans_when_verbs_omitted() {
        let dir = tmp();
        std::fs::create_dir_all(dir.join("bin")).unwrap();
        exe(&dir.join("bin/git-hi"));
        let m = PluginManifest::from_str("[plugin]\nname='y'\n[script]\nbin=['bin']\n").unwrap();
        let PluginKind::Script(s) = PluginKind::detect(&dir, Some(&m)).unwrap() else {
            panic!("expected a script plugin");
        };
        assert_eq!(s.bin, vec!["bin".to_string()]);
        assert_eq!(s.verbs, vec!["hi".to_string()]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn detects_script_layout_with_no_manifest() {
        let dir = tmp();
        exe(&dir.join("git-hi"));
        // A non-executable `git-*` is documentation, not a verb.
        std::fs::write(dir.join("git-notes.md"), b"# notes").unwrap();
        let PluginKind::Script(s) = PluginKind::detect(&dir, None).unwrap() else {
            panic!("expected a script plugin");
        };
        assert_eq!(s.verbs, vec!["hi".to_string()]);
        assert_eq!(s.bin, vec![".".to_string()]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn cargo_cdylib_beats_a_stray_script() {
        let dir = tmp();
        exe(&dir.join("git-hi"));
        std::fs::write(dir.join("Cargo.toml"), b"[lib]\ncrate-type = [\"cdylib\"]\n").unwrap();
        assert!(matches!(PluginKind::detect(&dir, None), Ok(PluginKind::Native(_))));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_empty_tree_has_no_determinable_kind() {
        let dir = tmp();
        assert!(matches!(PluginKind::detect(&dir, None), Err(PkgError::Unknown(_))));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
