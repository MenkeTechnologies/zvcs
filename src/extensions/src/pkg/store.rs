//! Global store + installed index. Ported from the zshrs package manager's
//! `pkg/store.rs`, retargeted to `$ZVCS_HOME/pkg/` and to plugins that
//! contribute git subcommands.
//!
//! Layout (`$ZVCS_HOME` defaults to `~/.zvcs`, via
//! [`crate::superset::zdaemon::zvcs_home`]):
//! ```text
//! $ZVCS_HOME/pkg/
//!   store/  name@version/     # one extracted copy per (name, version)
//!   cache/                    # download scratch
//!   git/                      # clones land here, then copy to store/
//!   bin/                      # launcher links
//!   installed.toml            # the global install index (source of truth)
//!   verbs.tsv                 # derived: verb -> plugin, for dispatch
//!   overrides.tsv             # derived: overridden verb -> plugin
//! ```
//! Human-readable `name@version` paths give reproducibility from the index's
//! content hashes without opaque store paths.
//!
//! The two `.tsv` side tables are what make a per-process `git` affordable. They
//! are pure projections of `installed.toml`, rewritten by every mutation
//! ([`InstalledIndex::save_to`]), and their *absence* is the fast answer: a
//! machine with no plugin installed fails one `stat` per lookup and never opens
//! a file. `git znative load` rebuilds them if they are ever deleted.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

use super::{PkgError, PkgResult};

/// Installed-index filename under the store root.
pub const INSTALLED_FILE: &str = "installed.toml";
/// Derived table of plugin-provided verbs: `verb\tplugin` per line.
pub const VERBS_FILE: &str = "verbs.tsv";
/// Derived table of plugin-provided verb overrides: `verb\tplugin` per line.
pub const OVERRIDES_FILE: &str = "overrides.tsv";

/// Resolves and lazily creates the `$ZVCS_HOME/pkg/...` layout.
pub struct Store {
    root: PathBuf,
}

impl Store {
    /// Construct a [`Store`] rooted at `$ZVCS_HOME/pkg/` (default
    /// `~/.zvcs/pkg/`).
    pub fn user_default() -> Store {
        Store { root: crate::superset::zdaemon::zvcs_home().join("pkg") }
    }

    /// Root at an explicit path (tests).
    pub fn at(root: impl Into<PathBuf>) -> Store {
        Store { root: root.into() }
    }

    /// `store/` — extracted packages.
    pub fn store_dir(&self) -> PathBuf {
        self.root.join("store")
    }
    /// `cache/` — download scratch.
    pub fn cache_dir(&self) -> PathBuf {
        self.root.join("cache")
    }
    /// `git/` — clones.
    pub fn git_dir(&self) -> PathBuf {
        self.root.join("git")
    }
    /// `bin/` — launcher links.
    pub fn bin_dir(&self) -> PathBuf {
        self.root.join("bin")
    }
    /// `$ZVCS_HOME/pkg/` root.
    pub fn root(&self) -> &Path {
        &self.root
    }
    /// The derived verb table.
    pub fn verbs_file(&self) -> PathBuf {
        self.root.join(VERBS_FILE)
    }
    /// The derived override table.
    pub fn overrides_file(&self) -> PathBuf {
        self.root.join(OVERRIDES_FILE)
    }

    /// Where a package extraction lives: `store/{name}@{version}/`.
    pub fn package_dir(&self, name: &str, version: &str) -> PathBuf {
        self.store_dir().join(format!("{name}@{version}"))
    }

    /// Create the full directory layout. Idempotent.
    pub fn ensure_layout(&self) -> PkgResult<()> {
        for d in [self.store_dir(), self.cache_dir(), self.git_dir(), self.bin_dir()] {
            std::fs::create_dir_all(&d)
                .map_err(|e| PkgError::Io(format!("create {}: {e}", d.display())))?;
        }
        Ok(())
    }

    /// True if a `name@version` extraction already exists.
    pub fn has_package(&self, name: &str, version: &str) -> bool {
        self.package_dir(name, version).is_dir()
    }

    /// Copy a staged plugin tree wholesale into `store/{name}@{version}/`,
    /// excluding VCS/build scratch (`.git/`, `target/`) so the store holds only
    /// the loadable plugin. The destination is cleared first for fresh
    /// re-installs. Returns the store path.
    pub fn install_dir(&self, name: &str, version: &str, src: &Path) -> PkgResult<PathBuf> {
        let dst = self.package_dir(name, version);
        if dst.exists() {
            std::fs::remove_dir_all(&dst)
                .map_err(|e| PkgError::Io(format!("clear {}: {e}", dst.display())))?;
        }
        std::fs::create_dir_all(&dst)?;
        copy_dir_filtered(src, &dst)?;
        Ok(dst)
    }
}

/// The global install index at `$ZVCS_HOME/pkg/installed.toml` — the single
/// source of truth for what is installed.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct InstalledIndex {
    /// Schema version.
    pub version: u32,
    /// One entry per installed plugin, sorted by name for deterministic diffs.
    #[serde(default, rename = "package")]
    pub packages: Vec<InstalledPlugin>,
}

/// One installed plugin: identity, provenance, and everything dispatch needs to
/// reach it without re-resolving or loading anything else.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct InstalledPlugin {
    /// Plugin name (store key `name@version`).
    pub name: String,
    /// Installed version (`0.0.0` when the plugin declared none).
    pub version: String,
    /// Provenance: `github:owner/repo`, `git+URL`, or `path+file://DIR`.
    pub source: String,
    /// `"native"` or `"script"`.
    pub kind: String,
    /// SHA-256 of the extracted tree, `sha256-<hex>` (audit / change detection).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub integrity: String,
    /// Native: the cdylib filename inside the store dir (e.g. `libfoo.dylib`).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub lib: String,
    /// Subcommands this plugin adds — recorded at install time by loading it
    /// once (native) or by scanning for `git-<verb>` (script). This is what
    /// lets `git <verb>` find the owning plugin without loading any other.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub verbs: Vec<String>,
    /// Existing verbs this plugin REPLACES. Consulted before every command, so
    /// an empty list here keeps the hot path at a single failed `stat`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub overrides: Vec<String>,
    /// Script: directories inside the store dir holding the `git-*`
    /// executables, relative to the store dir (`"."` for the root).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub bin: Vec<String>,
}

impl InstalledIndex {
    /// Empty index stamped with the current schema version.
    pub fn new() -> InstalledIndex {
        InstalledIndex { version: 1, packages: Vec::new() }
    }

    /// Load the index from a [`Store`], or an empty index when it does not exist.
    pub fn load_from(store: &Store) -> PkgResult<InstalledIndex> {
        let path = store.root().join(INSTALLED_FILE);
        if !path.is_file() {
            return Ok(InstalledIndex::new());
        }
        let s = std::fs::read_to_string(&path)
            .map_err(|e| PkgError::Io(format!("read {}: {e}", path.display())))?;
        toml::from_str::<InstalledIndex>(&s)
            .map_err(|e| PkgError::Other(format!("parse {}: {}", path.display(), e.message())))
    }

    /// Write the index (packages sorted by name) under `store.root()`, then
    /// rewrite the two derived tables from it.
    pub fn save_to(&mut self, store: &Store) -> PkgResult<()> {
        self.packages.sort_by(|a, b| a.name.cmp(&b.name));
        let path = store.root().join(INSTALLED_FILE);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let body = toml::to_string_pretty(&self)
            .map_err(|e| PkgError::Other(format!("serialize {INSTALLED_FILE}: {e}")))?;
        std::fs::write(&path, format!("# znative — auto-generated. Do not edit.\n{body}"))
            .map_err(|e| PkgError::Io(format!("write {}: {e}", path.display())))?;
        self.write_side_tables(store)
    }

    /// Rewrite `verbs.tsv` / `overrides.tsv` from the index. A table with no
    /// rows is *removed* rather than written empty, so its absence stays the
    /// cheap answer for dispatch.
    pub fn write_side_tables(&self, store: &Store) -> PkgResult<()> {
        for (file, rows) in [
            (store.verbs_file(), self.rows(|p| &p.verbs)),
            (store.overrides_file(), self.rows(|p| &p.overrides)),
        ] {
            if rows.is_empty() {
                let _ = std::fs::remove_file(&file);
                continue;
            }
            std::fs::write(&file, rows)
                .map_err(|e| PkgError::Io(format!("write {}: {e}", file.display())))?;
        }
        Ok(())
    }

    /// `verb\tplugin\n` rows for one of the two verb lists.
    fn rows(&self, pick: fn(&InstalledPlugin) -> &Vec<String>) -> String {
        let mut out = String::new();
        for p in &self.packages {
            for v in pick(p) {
                out.push_str(v);
                out.push('\t');
                out.push_str(&p.name);
                out.push('\n');
            }
        }
        out
    }

    /// Find an installed plugin by name.
    pub fn find(&self, name: &str) -> Option<&InstalledPlugin> {
        self.packages.iter().find(|p| p.name == name)
    }

    /// Insert or replace the entry for `p.name`.
    pub fn upsert(&mut self, p: InstalledPlugin) {
        if let Some(slot) = self.packages.iter_mut().find(|e| e.name == p.name) {
            *slot = p;
        } else {
            self.packages.push(p);
        }
    }

    /// Remove the entry named `name`; returns it if present.
    pub fn remove(&mut self, name: &str) -> Option<InstalledPlugin> {
        let idx = self.packages.iter().position(|p| p.name == name)?;
        Some(self.packages.remove(idx))
    }
}

/// Recursively copy `src` into `dst`, skipping `.git/` and `target/` (VCS and
/// Rust build scratch) at any depth. File modes are preserved, which a script
/// plugin depends on — a `git-<verb>` that loses its execute bit in the store
/// is not runnable.
fn copy_dir_filtered(src: &Path, dst: &Path) -> PkgResult<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let name = entry.file_name();
        let name_s = name.to_string_lossy();
        if name_s == ".git" || name_s == "target" {
            continue;
        }
        let from = entry.path();
        let to = dst.join(&name);
        let ft = entry.file_type()?;
        if ft.is_dir() {
            copy_dir_filtered(&from, &to)?;
        } else if ft.is_symlink() {
            // Copy the resolved content: a dangling link in the store would
            // break the load, and plugins rarely ship links deliberately.
            if let Ok(bytes) = std::fs::read(&from) {
                std::fs::write(&to, bytes)?;
            }
        } else {
            std::fs::copy(&from, &to)
                .map_err(|e| PkgError::Io(format!("copy {}: {e}", from.display())))?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn tmp() -> PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static N: AtomicU64 = AtomicU64::new(0);
        let p = std::env::temp_dir().join(format!(
            "zvcs-znative-test-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn two_indexes_loaded_at_once_lose_one_of_the_plugins() {
        // The index is read-modify-write: `load_from`, `upsert`, `save_to`
        // rewrites `installed.toml` and both derived tables from what was in
        // memory. Two callers that load before either saves therefore cannot
        // both survive — the second save writes a list that never contained the
        // first plugin, and neither call fails.
        //
        // This is the shape, made deterministic. `pkg::commands` is what stops
        // real installs reaching it, by holding the registry lock across the
        // load and the save; without that the loss is a matter of timing rather
        // than of possibility, which is why the end-to-end suite cannot prove it
        // and this can.
        let store = Store::at(tmp().join("pkg"));
        store.ensure_layout().unwrap();

        let plugin = |name: &str, verb: &str| InstalledPlugin {
            name: name.into(),
            version: "0.1.0".into(),
            source: format!("path+file:///{name}"),
            kind: "native".into(),
            verbs: vec![verb.into()],
            ..Default::default()
        };

        // Both callers read the same empty index...
        let mut first = InstalledIndex::load_from(&store).unwrap();
        let mut second = InstalledIndex::load_from(&store).unwrap();

        // ...each adds its own plugin and writes the whole thing back.
        first.upsert(plugin("alpha", "alpha"));
        first.save_to(&store).unwrap();
        second.upsert(plugin("beta", "beta"));
        second.save_to(&store).unwrap();

        let back = InstalledIndex::load_from(&store).unwrap();
        assert_eq!(back.packages.len(), 1, "the shape changed — see this test's comment");
        assert!(back.find("beta").is_some(), "the later writer should be the survivor");
        assert!(back.find("alpha").is_none(), "the earlier writer survived a whole-file rewrite?");

        // And the derived table dispatch reads has lost the same plugin, which
        // is what makes the loss invisible: `git alpha` simply stops resolving.
        let verbs = std::fs::read_to_string(store.verbs_file()).unwrap_or_default();
        assert!(!verbs.contains("alpha"), "verbs.tsv kept a plugin the index dropped:\n{verbs}");
    }

    #[test]
    fn install_dir_skips_git_and_target() {
        let src = tmp();
        std::fs::write(src.join("git-hi"), b"#!/bin/sh\necho hi").unwrap();
        std::fs::create_dir_all(src.join(".git")).unwrap();
        std::fs::write(src.join(".git/HEAD"), b"ref").unwrap();
        std::fs::create_dir_all(src.join("target")).unwrap();
        std::fs::write(src.join("target/junk"), b"x").unwrap();
        let store = Store::at(tmp().join("pkg"));
        store.ensure_layout().unwrap();
        let dst = store.install_dir("a", "0.1.0", &src).unwrap();
        assert!(dst.join("git-hi").is_file());
        assert!(!dst.join(".git").exists());
        assert!(!dst.join("target").exists());
        let _ = std::fs::remove_dir_all(&src);
        let _ = std::fs::remove_dir_all(store.root());
    }

    #[test]
    fn index_round_trip_and_side_tables() {
        let store = Store::at(tmp().join("pkg"));
        store.ensure_layout().unwrap();
        let mut idx = InstalledIndex::new();
        idx.upsert(InstalledPlugin {
            name: "zed".into(),
            version: "1.0.0".into(),
            source: "github:o/zed".into(),
            kind: "script".into(),
            verbs: vec!["zed".into()],
            bin: vec![".".into()],
            ..Default::default()
        });
        idx.upsert(InstalledPlugin {
            name: "abc".into(),
            version: "0.1.0".into(),
            source: "github:o/abc".into(),
            kind: "native".into(),
            lib: "libabc.dylib".into(),
            verbs: vec!["abc".into()],
            overrides: vec!["blame".into()],
            ..Default::default()
        });
        idx.save_to(&store).unwrap();

        let back = InstalledIndex::load_from(&store).unwrap();
        assert_eq!(back.packages.len(), 2);
        // Sorted by name: abc before zed.
        assert_eq!(back.packages[0].name, "abc");
        assert_eq!(back.find("zed").unwrap().kind, "script");
        assert_eq!(back.find("abc").unwrap().verbs, vec!["abc".to_string()]);

        // Derived tables: one row per verb, both plugins present.
        let verbs = std::fs::read_to_string(store.verbs_file()).unwrap();
        assert_eq!(verbs, "abc\tabc\nzed\tzed\n");
        let overrides = std::fs::read_to_string(store.overrides_file()).unwrap();
        assert_eq!(overrides, "blame\tabc\n");
        let _ = std::fs::remove_dir_all(store.root());
    }

    #[test]
    fn an_index_with_no_overrides_leaves_no_override_table() {
        // The hot path's fast answer is the file's ABSENCE; an empty file would
        // turn every command's failed stat into a successful open + read.
        let store = Store::at(tmp().join("pkg"));
        store.ensure_layout().unwrap();
        std::fs::write(store.overrides_file(), "stale\tgone\n").unwrap();
        let mut idx = InstalledIndex::new();
        idx.upsert(InstalledPlugin {
            name: "a".into(),
            version: "0.1.0".into(),
            kind: "native".into(),
            verbs: vec!["a".into()],
            ..Default::default()
        });
        idx.save_to(&store).unwrap();
        assert!(!store.overrides_file().exists());
        assert!(store.verbs_file().exists());
        let _ = std::fs::remove_dir_all(store.root());
    }
}
