//! `git znative` subcommand implementations. Ported from the zshrs package
//! manager's `pkg/commands.rs`: install a plugin into the global store, record
//! it in `installed.toml`, and register the verbs it provides.
//!
//! The one retarget worth naming is `load`. In a shell, `znative load` puts the
//! plugin into the running process — that is the whole point of the command,
//! and a `.zshrc` calls it on every startup. `git` has no such process to load
//! into, so here `load` **verifies and registers**: it installs the source if it
//! is not in the store yet, loads the library once to confirm it initialises,
//! and rewrites the derived verb tables dispatch reads. It stays idempotent and
//! zero-network for an already-installed plugin, so a dotfiles bootstrap can
//! carry one `git znative load owner/repo` line per plugin exactly as a
//! `.zshrc` does.

use std::path::Path;

use super::manifest::{scan_verbs, PluginKind, PluginManifest};
use super::store::{InstalledIndex, InstalledPlugin, Store};
use super::{resolver, PkgError, PkgResult};

/// `git znative add <SOURCE>` — resolve, install into the store, record, and
/// register.
pub fn add(spec: &str) -> PkgResult<()> {
    let store = Store::user_default();
    store.ensure_layout()?;

    let staged = resolver::resolve(spec, &store)?;
    let manifest = PluginManifest::load(&staged.dir)?;
    let meta = manifest.as_ref().map(|m| &m.plugin);
    let name = meta
        .map(|m| m.name.clone())
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| staged.name.clone());
    let version =
        meta.map(|m| m.version.clone()).filter(|v| !v.is_empty()).unwrap_or_else(|| "0.0.0".into());
    let description = meta.map(|m| m.description.clone()).unwrap_or_default();
    let kind = PluginKind::detect(&staged.dir, manifest.as_ref())?;

    // A native plugin may need a build step before its cdylib exists at all.
    // The artifact is placed into the store below rather than staged back into
    // the source tree: a `path:` install must not leave a build product behind
    // in somebody's working copy.
    let built = match &kind {
        PluginKind::Native(spec) => Some(prepare_native(&staged.dir, spec, &name)?),
        PluginKind::Script(_) => None,
    };

    // Copy the loadable subset into the content-addressed store, then the
    // cdylib alongside it (the tree copy skips `target/`, where cargo left it).
    let store_path = store.install_dir(&name, &version, &staged.dir)?;
    if let Some(built) = &built {
        let dst = store_path.join(built.file_name().unwrap_or_default());
        if !dst.exists() {
            std::fs::copy(built, &dst)
                .map_err(|e| PkgError::Io(format!("stage cdylib into the store: {e}")))?;
        }
    }
    // After the cdylib lands, so the hash covers what will actually be loaded.
    let integrity = super::store_integrity(&store_path)?;

    let mut entry = InstalledPlugin {
        name: name.clone(),
        version: version.clone(),
        source: staged.source.clone(),
        integrity,
        ..Default::default()
    };
    match &kind {
        PluginKind::Native(_) => {
            entry.kind = "native".into();
            entry.lib = find_cdylib(&store_path)
                .ok_or_else(|| PkgError::Resolve(format!("{name}: no cdylib after build")))?;
            // Load it once, from the store copy, to learn what it registers.
            // This is both the verb discovery pass and the only honest proof
            // that the plugin initialises at all.
            let loaded = crate::plugin_host::load(&store_path.join(&entry.lib).to_string_lossy())
                .map_err(PkgError::Resolve)?;
            entry.verbs = loaded.verbs;
            entry.overrides = loaded.overrides;
            if entry.verbs.is_empty() && entry.overrides.is_empty() {
                return Err(PkgError::Resolve(format!("{name}: plugin registered no verbs")));
            }
        }
        PluginKind::Script(s) => {
            entry.kind = "script".into();
            entry.bin = s.bin.clone();
            // Re-scan against the STORE copy: the recorded verbs must name
            // executables that exist where dispatch will look for them.
            entry.verbs = if s.verbs.is_empty() {
                scan_verbs(&store_path, &entry.bin)
            } else {
                s.verbs.clone()
            };
            if entry.verbs.is_empty() {
                return Err(PkgError::Resolve(format!("{name}: no git-<verb> executable in the store copy")));
            }
        }
    }

    // The index and its two derived tables are rewritten whole, so two installs
    // in flight at once leave only the later one's plugin: six concurrent
    // installs left one plugin installed and reported success six times. The
    // lock covers the conflict check too — two plugins claiming one verb must
    // not both pass it.
    let _lock = crate::superset::registry::lock("pkg");
    let mut index = InstalledIndex::load_from(&store)?;
    reject_verb_conflicts(&index, &entry)?;
    index.upsert(entry.clone());
    index.save_to(&store)?;

    // Clean the clone scratch — the store copy is authoritative.
    if staged.source.starts_with("github:") || staged.source.starts_with("git+") {
        let _ = std::fs::remove_dir_all(&staged.dir);
    }

    let desc = if description.is_empty() { String::new() } else { format!(" — {description}") };
    println!("znative: added {name}@{version} ({}){desc}", entry.kind);
    println!("znative: verbs: {}", verb_summary(&entry));
    Ok(())
}

/// Refuse an install whose verbs collide with a DIFFERENT plugin's, or whose
/// added verb is already a built-in one. Reinstalling the same plugin is fine —
/// that is what `update` does.
fn reject_verb_conflicts(index: &InstalledIndex, entry: &InstalledPlugin) -> PkgResult<()> {
    for verb in &entry.verbs {
        if crate::dispatch::is_verb(verb) {
            return Err(PkgError::Other(format!(
                "{}: verb '{verb}' is a built-in git command; a plugin must override it \
                 explicitly, not add it",
                entry.name
            )));
        }
    }
    for verb in &entry.overrides {
        if !crate::dispatch::is_verb(verb) {
            return Err(PkgError::Other(format!(
                "{}: verb '{verb}' does not exist, so there is nothing to override",
                entry.name
            )));
        }
    }
    for other in &index.packages {
        if other.name == entry.name {
            continue;
        }
        for verb in entry.verbs.iter().chain(&entry.overrides) {
            if other.verbs.contains(verb) || other.overrides.contains(verb) {
                return Err(PkgError::Other(format!(
                    "{}: verb '{verb}' is already provided by plugin '{}'",
                    entry.name, other.name
                )));
            }
        }
    }
    Ok(())
}

/// `verbs` plus `overrides` rendered for the one-line install summary.
fn verb_summary(entry: &InstalledPlugin) -> String {
    let mut parts: Vec<String> = entry.verbs.clone();
    parts.extend(entry.overrides.iter().map(|v| format!("{v} (override)")));
    if parts.is_empty() {
        "none".into()
    } else {
        parts.join(" ")
    }
}

/// `git znative remove <NAME>` — drop the store copy and the index row.
pub fn remove(name: &str) -> PkgResult<()> {
    let store = Store::user_default();
    // Held across load and save, as in `add`: a removal that races an install
    // would otherwise write back an index that never saw the new plugin.
    let _lock = crate::superset::registry::lock("pkg");
    let mut index = InstalledIndex::load_from(&store)?;
    let Some(entry) = index.remove(name) else {
        return Err(PkgError::Other(format!("{name} is not installed")));
    };
    let dir = store.package_dir(&entry.name, &entry.version);
    if dir.is_dir() {
        std::fs::remove_dir_all(&dir)
            .map_err(|e| PkgError::Io(format!("remove {}: {e}", dir.display())))?;
    }
    index.save_to(&store)?;
    println!("znative: removed {name}");
    Ok(())
}

/// `git znative list` — one line per installed plugin.
pub fn list() -> PkgResult<()> {
    let store = Store::user_default();
    let index = InstalledIndex::load_from(&store)?;
    if index.packages.is_empty() {
        println!("znative: no plugins installed");
        return Ok(());
    }
    for p in &index.packages {
        println!("{:<24} {:<10} {:<7} {}", p.name, p.version, p.kind, p.source);
    }
    Ok(())
}

/// `git znative info <NAME>` — the full record for one plugin.
pub fn info(name: &str) -> PkgResult<()> {
    let store = Store::user_default();
    let index = InstalledIndex::load_from(&store)?;
    let Some(p) = index.find(name) else {
        return Err(PkgError::Other(format!("{name} is not installed")));
    };
    println!("name       {}", p.name);
    println!("version    {}", p.version);
    println!("kind       {}", p.kind);
    println!("source     {}", p.source);
    println!("store      {}", store.package_dir(&p.name, &p.version).display());
    if !p.integrity.is_empty() {
        println!("integrity  {}", p.integrity);
    }
    if !p.lib.is_empty() {
        println!("lib        {}", p.lib);
    }
    if !p.verbs.is_empty() {
        println!("verbs      {}", p.verbs.join(" "));
    }
    if !p.overrides.is_empty() {
        println!("overrides  {}", p.overrides.join(" "));
    }
    if !p.bin.is_empty() {
        println!("bin        {}", p.bin.join(" "));
    }
    Ok(())
}

/// `git znative load [SOURCE]` — install if the source is not in the store yet,
/// then verify the plugin loads and refresh the derived verb tables. With no
/// argument, every installed plugin. Zero network for anything already stored.
pub fn load(name: Option<&str>) -> PkgResult<()> {
    let store = Store::user_default();
    let index = InstalledIndex::load_from(&store)?;
    match name {
        Some(n) => {
            // 1. Already installed under this name → verify from the store.
            if let Some(entry) = index.find(n) {
                return verify(&store, entry);
            }
            // 2. `n` is a SOURCE spec — is a plugin from that source installed?
            //    The index keys on the source label, since a repo's basename
            //    usually differs from its `znative.toml` plugin name.
            if let Some(label) = resolver::source_label(n) {
                if let Some(entry) = index.packages.iter().find(|p| p.source == label) {
                    return verify(&store, entry);
                }
                // 3. Not in the store yet → install on first use.
                return add(n);
            }
            Err(PkgError::Other(format!("{n} is not installed")))
        }
        None => {
            let mut errs = Vec::new();
            for p in &index.packages {
                if let Err(e) = verify(&store, p) {
                    errs.push(format!("{}: {e}", p.name));
                }
            }
            // The tables are derived state; rebuild them even if one plugin is
            // broken, so a single bad entry cannot strand the healthy ones.
            index.write_side_tables(&store)?;
            if errs.is_empty() {
                Ok(())
            } else {
                Err(PkgError::Other(errs.join("; ")))
            }
        }
    }
}

/// Confirm an installed plugin is still loadable from the store: a native one
/// is `dlopen`ed and initialised, a script one has its executables checked.
fn verify(store: &Store, p: &InstalledPlugin) -> PkgResult<()> {
    let dir = store.package_dir(&p.name, &p.version);
    match p.kind.as_str() {
        "native" => {
            let lib = dir.join(&p.lib);
            let loaded = crate::plugin_host::load(&lib.to_string_lossy()).map_err(PkgError::Resolve)?;
            // A plugin that renamed its verbs between installs would otherwise
            // keep answering under the old ones.
            if loaded.verbs != p.verbs || loaded.overrides != p.overrides {
                return Err(PkgError::Other(format!(
                    "{}: registered verbs changed since install ({}); re-run `git znative update {}`",
                    p.name,
                    verb_summary(&InstalledPlugin {
                        verbs: loaded.verbs,
                        overrides: loaded.overrides,
                        ..Default::default()
                    }),
                    p.name
                )));
            }
            let _ = crate::plugin_host::unload(&p.name);
            Ok(())
        }
        "script" => {
            let missing: Vec<&String> = p
                .verbs
                .iter()
                .filter(|v| {
                    let bins: Vec<String> =
                        if p.bin.is_empty() { vec![".".into()] } else { p.bin.clone() };
                    !bins.iter().any(|b| dir.join(b).join(format!("git-{v}")).is_file())
                })
                .collect();
            if missing.is_empty() {
                Ok(())
            } else {
                Err(PkgError::Other(format!(
                    "{}: missing executable(s) in the store: {}",
                    p.name,
                    missing.iter().map(|v| format!("git-{v}")).collect::<Vec<_>>().join(" ")
                )))
            }
        }
        other => Err(PkgError::Other(format!("{}: unknown plugin kind '{other}'", p.name))),
    }
}

/// `git znative update [NAME]` — re-resolve and reinstall from the recorded
/// source (one plugin, or all of them).
pub fn update(name: Option<&str>) -> PkgResult<()> {
    let store = Store::user_default();
    let index = InstalledIndex::load_from(&store)?;
    let targets: Vec<String> = match name {
        Some(n) => vec![n.to_string()],
        None => index.packages.iter().map(|p| p.name.clone()).collect(),
    };
    for n in targets {
        let Some(p) = index.find(&n) else {
            return Err(PkgError::Other(format!("{n} is not installed")));
        };
        add(&source_to_spec(&p.source))?;
    }
    Ok(())
}

/// Convert a recorded provenance label back into an `add` spec.
fn source_to_spec(source: &str) -> String {
    match source.strip_prefix("path+file://") {
        Some(rest) => format!("path:{rest}"),
        // `github:owner/repo` and `git+URL` are already valid `add` specs.
        None => source.to_string(),
    }
}

/// Recursive byte size of a directory tree (0 if unreadable).
fn dir_size(path: &Path) -> u64 {
    let mut total: u64 = 0;
    let Ok(entries) = std::fs::read_dir(path) else { return 0 };
    for entry in entries.flatten() {
        let Ok(meta) = entry.metadata() else { continue };
        if meta.is_dir() {
            total += dir_size(&entry.path());
        } else if meta.is_file() {
            total += meta.len();
        }
    }
    total
}

/// `git znative gc [--dry-run]` — remove every `store/<name>@<version>/`
/// directory not pinned by `installed.toml` (orphans left by old versions or
/// failed installs), plus the `git/` clone scratch.
pub fn gc(dry_run: bool) -> PkgResult<()> {
    let store = Store::user_default();
    let index = InstalledIndex::load_from(&store)?;
    let pinned: std::collections::HashSet<String> =
        index.packages.iter().map(|p| format!("{}@{}", p.name, p.version)).collect();

    let mut freed: u64 = 0;
    let mut count: usize = 0;

    // 1. Orphan store/<name>@<version> directories.
    if let Ok(entries) = std::fs::read_dir(store.store_dir()) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if entry.path().is_dir() && !pinned.contains(&name) {
                let bytes = dir_size(&entry.path());
                if dry_run {
                    println!("znative gc: would remove {name} ({} KB)", kb(bytes));
                } else {
                    std::fs::remove_dir_all(entry.path())
                        .map_err(|e| PkgError::Io(format!("remove {name}: {e}")))?;
                    println!("znative gc: removed {name} ({} KB)", kb(bytes));
                }
                freed += bytes;
                count += 1;
            }
        }
    }

    // 2. git/ clone scratch — the store holds the copied working tree, so the
    //    clone is dead weight after install.
    let git = store.git_dir();
    let git_bytes = dir_size(&git);
    if git_bytes > 0 {
        if dry_run {
            println!("znative gc: would clear git cache ({} KB)", kb(git_bytes));
        } else {
            let _ = std::fs::remove_dir_all(&git);
            println!("znative gc: cleared git cache ({} KB)", kb(git_bytes));
        }
        freed += git_bytes;
        count += 1;
    }

    if count == 0 {
        println!("znative gc: nothing to collect");
    } else {
        let verb = if dry_run { "would free" } else { "freed" };
        println!("znative gc: {verb} {} KB total", kb(freed));
    }
    Ok(())
}

/// `git znative clean` — clear the scratch directories (`git/`, `cache/`,
/// `bin/`) that installs accumulate but that nothing loads from. The store and
/// the index are left intact.
pub fn clean() -> PkgResult<()> {
    let store = Store::user_default();
    let mut freed: u64 = 0;
    for d in [store.git_dir(), store.cache_dir(), store.bin_dir()] {
        if d.exists() {
            freed += dir_size(&d);
            std::fs::remove_dir_all(&d)
                .map_err(|e| PkgError::Io(format!("remove {}: {e}", d.display())))?;
        }
    }
    println!("znative clean: cleared {} KB of scratch", kb(freed));
    Ok(())
}

/// Rounded kilobytes, as the size reports print them.
fn kb(bytes: u64) -> u64 {
    bytes.div_ceil(1024)
}

/// Produce a native plugin's cdylib and return the path to it. A
/// `lib*.{dylib,so}` already at the tree root is used as is; otherwise
/// `cargo build --release` runs and the artifact is taken from `target/release`.
/// Nothing is written into the source tree — the caller copies the result into
/// the store, so installing from a local path leaves the working copy clean.
fn prepare_native(
    dir: &Path,
    spec: &super::manifest::NativeSpec,
    name: &str,
) -> PkgResult<std::path::PathBuf> {
    if let Some(prebuilt) = find_cdylib(dir) {
        return Ok(dir.join(prebuilt)); // shipped as a binary; nothing to build.
    }
    let has_cargo = dir.join("Cargo.toml").is_file();
    let want_build = spec.build.unwrap_or(has_cargo);
    if !want_build {
        return Err(PkgError::Resolve(format!(
            "{name}: native plugin has no prebuilt cdylib and build is disabled"
        )));
    }
    if !has_cargo {
        return Err(PkgError::Resolve(format!(
            "{name}: native plugin has neither a cdylib nor a Cargo.toml to build"
        )));
    }
    // Release, deliberately: this is a third-party artifact that will be
    // `dlopen`ed on every invocation of its verbs, not a local dev build.
    let out = std::process::Command::new("cargo")
        .current_dir(dir)
        .arg("build")
        .arg("--release")
        .output()
        .map_err(|e| PkgError::Resolve(format!("cargo build: {e} (is cargo installed?)")))?;
    if !out.status.success() {
        return Err(PkgError::Resolve(format!(
            "{name}: cargo build failed:\n{}",
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    let rel = dir.join("target").join("release");
    let built = find_cdylib(&rel).ok_or_else(|| {
        PkgError::Resolve(format!(
            "{name}: cargo build produced no cdylib in {} (need crate-type=[\"cdylib\"])",
            rel.display()
        ))
    })?;
    Ok(rel.join(built))
}

/// Find a `lib*.{dylib,so}` filename directly inside `dir`.
fn find_cdylib(dir: &Path) -> Option<String> {
    let suffix = std::env::consts::DLL_SUFFIX; // .dylib / .so
    let rd = std::fs::read_dir(dir).ok()?;
    for entry in rd.flatten() {
        let n = entry.file_name().to_string_lossy().into_owned();
        if n.ends_with(suffix) && n.starts_with(std::env::consts::DLL_PREFIX) {
            return Some(n);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_recorded_source_round_trips_into_an_add_spec() {
        assert_eq!(source_to_spec("github:o/r"), "github:o/r");
        assert_eq!(source_to_spec("git+https://x/y.git"), "git+https://x/y.git");
        assert_eq!(source_to_spec("path+file:///tmp/p"), "path:/tmp/p");
    }

    #[test]
    fn adding_a_builtin_verb_is_refused() {
        // A plugin may REPLACE `status`, but adding it would leave two owners
        // of one name with no rule for which wins.
        let index = InstalledIndex::new();
        let entry = InstalledPlugin {
            name: "x".into(),
            verbs: vec!["status".into()],
            ..Default::default()
        };
        let err = reject_verb_conflicts(&index, &entry).unwrap_err().to_string();
        assert!(err.contains("built-in git command"), "{err}");
    }

    #[test]
    fn two_plugins_cannot_own_one_verb() {
        let mut index = InstalledIndex::new();
        index.upsert(InstalledPlugin {
            name: "first".into(),
            verbs: vec!["shiny".into()],
            ..Default::default()
        });
        let entry = InstalledPlugin {
            name: "second".into(),
            verbs: vec!["shiny".into()],
            ..Default::default()
        };
        let err = reject_verb_conflicts(&index, &entry).unwrap_err().to_string();
        assert!(err.contains("already provided by plugin 'first'"), "{err}");
        // Reinstalling the SAME plugin over itself is what `update` does.
        let same = InstalledPlugin {
            name: "first".into(),
            verbs: vec!["shiny".into()],
            ..Default::default()
        };
        assert!(reject_verb_conflicts(&index, &same).is_ok());
    }

    #[test]
    fn an_override_of_a_verb_that_does_not_exist_is_refused() {
        // Otherwise the row lands in `overrides.tsv` and is never consulted:
        // dispatch only reaches the override hook for a verb it already serves.
        let index = InstalledIndex::new();
        let entry = InstalledPlugin {
            name: "x".into(),
            overrides: vec!["nosuchverb".into()],
            ..Default::default()
        };
        let err = reject_verb_conflicts(&index, &entry).unwrap_err().to_string();
        assert!(err.contains("nothing to override"), "{err}");
    }

    #[test]
    fn an_override_of_a_builtin_verb_is_allowed() {
        let index = InstalledIndex::new();
        let entry = InstalledPlugin {
            name: "x".into(),
            overrides: vec!["blame".into()],
            ..Default::default()
        };
        assert!(reject_verb_conflicts(&index, &entry).is_ok());
    }
}
