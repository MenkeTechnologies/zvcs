//! zvcs plugin package manager (`git znative`) — a global package manager for
//! **native** (Rust cdylib) plugins and **script** plugins (a repo of
//! `git-<verb>` executables).
//!
//! Ported from the zshrs package manager (`zshrs/src/extensions/pkg/`), which
//! was itself ported from strykelang's. The store model carries over unchanged
//! — one content-addressed global store under `$ZVCS_HOME/pkg/`, no per-project
//! manifest or lockfile, `installed.toml` as the single source of truth — and
//! the shell-specific half is retargeted: a plugin contributes git subcommands
//! rather than functions, aliases and `fpath` entries.
//!
//! The one structural difference from zshrs is forced by the process model. A
//! shell loads every plugin once into a process that then lives for hours;
//! `git` is a fresh process per command, so nothing may be loaded eagerly. The
//! installer therefore records, at install time, which verbs each plugin owns
//! ([`store::InstalledPlugin::verbs`] / [`store::InstalledPlugin::overrides`])
//! and mirrors them into two flat side tables, [`store::VERBS_FILE`] and
//! [`store::OVERRIDES_FILE`]. Dispatch consults those, and `dlopen`s exactly
//! the one plugin that owns the verb being run — a machine with no plugins
//! installed pays two failed `stat`s and nothing else.
//!
//! Surface:
//! - [`manifest`] — a plugin's optional `znative.toml` (`[plugin]`/`[native]`/
//!   `[script]`); auto-detected when absent.
//! - [`store`]    — `$ZVCS_HOME/pkg/{store,cache,git,bin}/` + `installed.toml`.
//! - [`resolver`] — a source spec (`owner/repo`, `git+URL`, `path:DIR`) into a
//!   staged directory ready to install.
//! - [`commands`] — `add`/`remove`/`list`/`info`/`load`/`update`/`gc`/`clean`.

pub mod commands;
pub mod manifest;
pub mod resolver;
pub mod store;

/// Result alias used throughout the package manager. Errors are stringly-typed
/// (one user-facing diagnostic per failure path), emitted to stderr as
/// `znative: <reason>` with exit code 1.
pub type PkgResult<T> = Result<T, PkgError>;

/// Errors emitted by the package manager. `Display` produces the one-line
/// reason (no `znative:` prefix — the verb adds it).
#[derive(Debug)]
pub enum PkgError {
    /// File I/O — read/write/create/copy.
    Io(String),
    /// Manifest parse error (bad TOML in a plugin's `znative.toml`).
    Manifest(String),
    /// Resolver error — unknown source form, clone/build failure.
    Resolve(String),
    /// The plugin kind could not be determined (no `znative.toml`, no
    /// `git-*` executable, no cdylib/`Cargo.toml`).
    Unknown(String),
    /// Generic runtime error.
    Other(String),
}

impl std::fmt::Display for PkgError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PkgError::Io(s)
            | PkgError::Manifest(s)
            | PkgError::Resolve(s)
            | PkgError::Unknown(s)
            | PkgError::Other(s) => write!(f, "{s}"),
        }
    }
}

impl From<std::io::Error> for PkgError {
    fn from(e: std::io::Error) -> Self {
        PkgError::Io(e.to_string())
    }
}

/// Deterministic SHA-256 of a directory tree, `sha256-<hex>`. Ported from the
/// zshrs package manager's `store_integrity` (strykelang's
/// `integrity_for_directory` before that). Files are walked in sorted order so
/// the hash is stable regardless of filesystem iteration; each file contributes
/// `<relpath>\0F\0<len>\n<bytes>\n`, symlinks their target. Recorded in the
/// install index for change detection and audit.
pub fn store_integrity(root: &std::path::Path) -> PkgResult<String> {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    let mut entries: Vec<std::path::PathBuf> = Vec::new();
    fn walk(
        root: &std::path::Path,
        cur: &std::path::Path,
        out: &mut Vec<std::path::PathBuf>,
    ) -> PkgResult<()> {
        for entry in std::fs::read_dir(cur)? {
            let entry = entry?;
            let path = entry.path();
            let meta = entry.metadata()?;
            if meta.is_dir() && !meta.file_type().is_symlink() {
                walk(root, &path, out)?;
            } else {
                out.push(path.strip_prefix(root).unwrap_or(&path).to_path_buf());
            }
        }
        Ok(())
    }
    walk(root, root, &mut entries)?;
    entries.sort();
    for rel in &entries {
        let abs = root.join(rel);
        let meta = std::fs::symlink_metadata(&abs)?;
        let rel_s = rel.to_string_lossy();
        if meta.file_type().is_symlink() {
            let target = std::fs::read_link(&abs)?;
            hasher.update(rel_s.as_bytes());
            hasher.update(b"\0L\0");
            hasher.update(target.to_string_lossy().as_bytes());
            hasher.update(b"\n");
        } else if meta.is_file() {
            let bytes = std::fs::read(&abs)?;
            hasher.update(rel_s.as_bytes());
            hasher.update(b"\0F\0");
            hasher.update(bytes.len().to_string().as_bytes());
            hasher.update(b"\n");
            hasher.update(&bytes);
            hasher.update(b"\n");
        }
    }
    // sha2 0.11 hands back a `hybrid_array::Array`, which has no `LowerHex`
    // (0.10's `GenericArray` did), so the digest is rendered a byte at a time.
    let mut out = String::with_capacity(7 + 64);
    out.push_str("sha256-");
    for b in hasher.finalize() {
        out.push_str(&format!("{b:02x}"));
    }
    Ok(out)
}
