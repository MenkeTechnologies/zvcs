//! `get_oid_with_context_1()`'s path arm (`object-name.c:1730-1871`) — the half of
//! the object-name grammar that reads a *path* out of a tree or the index rather
//! than navigating the commit graph.
//!
//! Two jobs live here, and git does them in two different functions:
//!
//! * **Resolution.** `<rev>:./<path>`, `:./<path>` and `:<n>:./<path>` are
//!   relative to the current directory, not to the top of the work tree, and git
//!   rewrites them through `resolve_relative_path()` → `prefix_path()` before
//!   anything looks the path up. gitoxide's revspec parser has no such step, so
//!   [`canonical_paths`] performs it and hands the parser the root-relative
//!   spelling it does understand.
//! * **Diagnosis.** When the lookup fails, `verify_filename()` gives the operand a
//!   second pass with `GET_OID_ONLY_TO_DIE` set, and `diagnose_invalid_oid_path()`
//!   / `diagnose_invalid_index_path()` say *why* — which is a far more specific
//!   message than the `ambiguous argument …` fallback. [`misspelt_object_name`] is
//!   that second pass.
//!
//! The two are separate on purpose: `git rev-parse main:nosuch` never resolves, so
//! only the diagnosis runs, while `git rev-parse main:./base.txt` resolves and the
//! diagnosis is never reached.

use std::path::Path;

/// `file_exists()` as git asks it here. `setup_git_directory()` has already
/// chdir'd to the top of the work tree by the time any of this runs, so every
/// bare path in `diagnose_invalid_*_path()` is root-relative — which is why
/// stock, run from `sub/`, answers `git rev-parse main:s.txt` with
/// `path 'sub/s.txt' exists, but not 's.txt'` rather than the on-disk message
/// the process's own cwd would produce.
fn exists_at_root(repo: &gix::Repository, path: &str) -> bool {
    match repo.workdir() {
        Some(root) => root.join(path).symlink_metadata().is_ok(),
        None => Path::new(path).symlink_metadata().is_ok(),
    }
}

/// [`crate::setup::prefix`], slash-terminated the way `prefix_path()` wants it.
fn prefix(repo: &gix::Repository) -> Option<String> {
    Some(format!("{}/", crate::setup::prefix(repo)?.to_str()?))
}

/// `normalize_path_copy()` (`path.c`): collapse `.` and `..` textually, without
/// touching the filesystem. Returns `None` when a `..` would climb above the
/// root, which is `prefix_path_gently()`'s failure and `prefix_path()`'s `die()`.
fn normalize(joined: &str) -> Option<String> {
    let mut out: Vec<&str> = Vec::new();
    for part in joined.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                out.pop()?;
            }
            other => out.push(other),
        }
    }
    Some(out.join("/"))
}

/// `resolve_relative_path()` (`object-name.c:1702-1714`): a path that starts with
/// `./` or `../` is relative to the current directory and gets rewritten against
/// the prefix; anything else is already root-relative and is left alone.
///
/// ```c
/// static char *resolve_relative_path(struct repository *r, const char *rel)
/// {
///         if (!starts_with(rel, "./") && !starts_with(rel, "../"))
///                 return NULL;
///         if (r != the_repository || !is_inside_work_tree(the_repository))
///                 die(_("relative path syntax can't be used outside working tree"));
///         return prefix_path(the_repository, startup_info->prefix, …, rel);
/// }
/// ```
///
/// The three answers are all separately observable:
///
/// * `Ok(None)` — not a relative spelling, nothing to do.
/// * `Ok(Some(path))` — rewritten. From `sub/`, `:../base.txt` becomes
///   `:base.txt` and `main:./s.txt` becomes `main:sub/s.txt`.
/// * `Err(msg)` — `prefix_path()`'s `die()`. Stock 2.55.0 from `sub/`:
///   `git rev-parse :../../escape` →
///   `fatal: '../../escape' is outside repository at '<toplevel>'`.
pub fn resolve_relative_path(repo: &gix::Repository, rel: &str) -> Result<Option<String>, String> {
    if !rel.starts_with("./") && !rel.starts_with("../") {
        return Ok(None);
    }
    let Some(workdir) = repo.workdir() else {
        return Err("relative path syntax can't be used outside working tree".into());
    };
    let joined = match prefix(repo) {
        Some(p) => format!("{p}{rel}"),
        None => rel.to_string(),
    };
    match normalize(&joined) {
        Some(path) => Ok(Some(path)),
        None => {
            let top = std::fs::canonicalize(workdir).unwrap_or_else(|_| workdir.to_owned());
            Err(format!("'{rel}' is outside repository at '{}'", top.display()))
        }
    }
}

/// How `get_oid_with_context_1()` splits an operand, before any lookup happens.
#[derive(Debug, PartialEq, Eq)]
pub enum Split<'a> {
    /// `:/<text>` — the oneline search, which is not a path operand at all.
    Oneline,
    /// `:<path>` or `:<stage>:<path>`, with the stage git decoded.
    ///
    /// Note that only `0`–`3` are stages: `:4:f` is `stage 0` over the *path*
    /// `4:f`, which is why stock says `path '4:base.txt' does not exist`.
    Index { stage: u8, path: &'a str },
    /// `<rev>:<path>`, split at the first `:` outside `@{…}`/`^{…}`.
    Tree { rev: &'a str, path: &'a str },
    /// No path arm — an ordinary revision.
    Rev,
}

/// `get_oid_with_context_1()`'s dispatch (`object-name.c:1758-1831`), decoded
/// without touching the object database.
///
/// The `<rev>:<path>` split is *not* a plain `find(':')`: git walks the name
/// tracking `@{`/`^{` bracket depth, so `main@{2}:f` and `main^{/x}:f` split at
/// the right colon. It stops at the first unbracketed `:`, which is why
/// `main:base.txt^{tree}` is the path `base.txt^{tree}` in `main` rather than a
/// peel of `main:base.txt`.
pub fn split(name: &str) -> Split<'_> {
    if let Some(rest) = name.strip_prefix(':') {
        if rest.len() > 1 && rest.starts_with('/') {
            return Split::Oneline;
        }
        let b = rest.as_bytes();
        // `if (namelen < 3 || name[2] != ':' || name[1] < '0' || '3' < name[1])`
        if b.len() >= 2 && b[1] == b':' && b[0].is_ascii_digit() && b[0] <= b'3' {
            return Split::Index { stage: b[0] - b'0', path: &rest[2..] };
        }
        return Split::Index { stage: 0, path: rest };
    }
    let bytes = name.as_bytes();
    let mut depth = 0usize;
    let mut i = 0usize;
    while i < bytes.len() {
        let c = bytes[i];
        if (c == b'@' || c == b'^') && bytes.get(i + 1) == Some(&b'{') {
            i += 1;
            depth += 1;
        } else if depth > 0 && c == b'}' {
            depth -= 1;
        } else if depth == 0 && c == b':' {
            return Split::Tree { rev: &name[..i], path: &name[i + 1..] };
        }
        i += 1;
    }
    Split::Rev
}

/// `spec` with any `./`/`../` path arm rewritten root-relative, so a revspec
/// parser that only understands root-relative paths can resolve it.
///
/// This is the *resolution* half of the module: git performs the rewrite inside
/// `get_oid_with_context_1()` before consulting the index or the tree, so
/// `git rev-parse :./base.txt` and `git rev-parse main:./base.txt` both succeed
/// in stock 2.55.0 from the top of the work tree, and from `sub/` they name
/// `sub/base.txt`.
///
/// A `..` that climbs out of the work tree is `prefix_path()`'s `die()`, returned
/// here as `Err` so the caller can report it verbatim instead of resolving.
pub fn canonical_paths<'a>(
    repo: &gix::Repository,
    spec: &'a str,
) -> Result<std::borrow::Cow<'a, str>, String> {
    let (head_len, path) = match split(spec) {
        Split::Oneline | Split::Rev => return Ok(std::borrow::Cow::Borrowed(spec)),
        Split::Index { path, .. } | Split::Tree { path, .. } => (spec.len() - path.len(), path),
    };
    match resolve_relative_path(repo, path)? {
        None => Ok(std::borrow::Cow::Borrowed(spec)),
        Some(rewritten) => Ok(std::borrow::Cow::Owned(format!("{}{rewritten}", &spec[..head_len]))),
    }
}

/// `maybe_die_on_misspelt_object_name()` (`object-name.c:1880-1889`): the message
/// `die_verify_filename()` prefers over the generic `ambiguous argument …`, or
/// `None` when git has nothing more specific to say.
///
/// git reaches this by re-running `get_oid_with_context_1()` with
/// `GET_OID_ONLY_TO_DIE`, which is why the operand is resolved *twice* on a
/// failure — and why `git rev-parse main^{blob}` prints its
/// `expected blob type` line twice while `git cat-file -t main^{blob}`, which
/// never calls `verify_filename()`, prints it once.
///
/// `only_to_die` also switches off the `:/<text>` oneline branch
/// (`if (!only_to_die && namelen > 2 && name[1] == '/')`), so a `:/nosuchmsg`
/// that found no commit falls through to the index lookup, fails there, and is
/// then spared a message by `die_verify_filename()`'s magic-pathspec guard.
pub fn misspelt_object_name(repo: &gix::Repository, name: &str) -> Option<String> {
    match split(name) {
        Split::Rev => None,
        Split::Oneline | Split::Index { .. } => {
            let (stage, path) = match split(name) {
                Split::Index { stage, path } => (stage, path),
                // `only_to_die` suppresses the oneline branch, so `:/foo` is read
                // as the index path `/foo` at stage 0.
                _ => (0u8, &name[1..]),
            };
            let path = match resolve_relative_path(repo, path) {
                Ok(Some(rewritten)) => std::borrow::Cow::Owned(rewritten),
                Ok(None) => std::borrow::Cow::Borrowed(path),
                Err(msg) => return Some(msg),
            };
            // `if (only_to_die && name[1] && name[1] != '/')` — an empty operand
            // and the oneline spelling get no diagnosis.
            let after_colon = name.as_bytes().get(1).copied();
            if after_colon.is_none_or(|b| b == b'/') {
                return None;
            }
            diagnose_invalid_index_path(repo, stage, &path)
        }
        Split::Tree { rev, path } => {
            let tree = crate::objname::resolve_quiet(repo, rev)?;
            let path = match resolve_relative_path(repo, path) {
                Ok(Some(rewritten)) => std::borrow::Cow::Owned(rewritten),
                Ok(None) => std::borrow::Cow::Borrowed(path),
                Err(msg) => return Some(msg),
            };
            if tree_entry(repo, tree, &path).is_some() {
                return None;
            }
            diagnose_invalid_oid_path(repo, &path, tree, rev)
        }
    }
}

/// `diagnose_invalid_oid_path()` (`object-name.c:1610-1642`).
///
/// The prefix half of the C — the `path '<full>' exists, but not '<rel>'` hint —
/// only fires when the operand was *not* already rewritten by
/// `resolve_relative_path()`, i.e. when the user spelled a bare `f.txt` from a
/// subdirectory and meant `./f.txt`. That is the case reproduced below.
fn diagnose_invalid_oid_path(
    repo: &gix::Repository,
    filename: &str,
    tree: gix::ObjectId,
    object_name: &str,
) -> Option<String> {
    if exists_at_root(repo, filename) {
        return Some(format!("path '{filename}' exists on disk, but not in '{object_name}'"));
    }
    if let Some(pre) = prefix(repo) {
        let fullname = format!("{pre}{filename}");
        if tree_entry(repo, tree, &fullname).is_some() {
            return Some(format!(
                "path '{fullname}' exists, but not '{filename}'\n\
                 hint: Did you mean '{object_name}:{fullname}' aka '{object_name}:./{filename}'?"
            ));
        }
    }
    Some(format!("path '{filename}' does not exist in '{object_name}'"))
}

/// `diagnose_invalid_index_path()` (`object-name.c:1645-1699`), in git's order:
/// wrong stage first, then the relative/absolute confusion, then on-disk, then
/// missing entirely.
fn diagnose_invalid_index_path(repo: &gix::Repository, stage: u8, filename: &str) -> Option<String> {
    let index = repo.index_or_empty().ok()?;
    let entry_stage = |path: &str| {
        index
            .entries()
            .iter()
            .find(|e| e.path(&index) == path.as_bytes())
            .map(|e| e.stage_raw() as u8)
    };

    if let Some(found) = entry_stage(filename) {
        return Some(format!(
            "path '{filename}' is in the index, but not at stage {stage}\n\
             hint: Did you mean ':{found}:{filename}'?"
        ));
    }
    if let Some(pre) = prefix(repo) {
        let fullname = format!("{pre}{filename}");
        if let Some(found) = entry_stage(&fullname) {
            return Some(format!(
                "path '{fullname}' is in the index, but not '{filename}'\n\
                 hint: Did you mean ':{found}:{fullname}' aka ':{found}:./{filename}'?"
            ));
        }
    }
    if exists_at_root(repo, filename) {
        return Some(format!("path '{filename}' exists on disk, but not in the index"));
    }
    Some(format!("path '{filename}' does not exist (neither on disk nor in the index)"))
}

/// `get_tree_entry()`: the id `path` names inside the tree `tree_ish` peels to,
/// or `None`. An empty path is the tree itself, which is what git's
/// `<rev>:` spelling means.
fn tree_entry(repo: &gix::Repository, tree_ish: gix::ObjectId, path: &str) -> Option<gix::ObjectId> {
    let tree = repo.find_object(tree_ish).ok()?.peel_to_tree().ok()?;
    if path.is_empty() {
        return Some(tree.id);
    }
    Some(tree.lookup_entry_by_path(path).ok()??.oid().to_owned())
}

/// Not used for lookups — kept so the module's join rule has one spelling.
#[cfg(test)]
fn join(prefix: &str, rel: &str) -> Option<String> {
    normalize(&format!("{prefix}{rel}"))
}

#[cfg(test)]
mod tests {
    use super::{join, split, Split};

    /// `normalize_path_copy()`'s three answers, which decide whether a `./`/`../`
    /// operand resolves at all.
    #[test]
    fn normalize_collapses_and_refuses_escapes() {
        assert_eq!(join("sub/", "./s.txt").as_deref(), Some("sub/s.txt"));
        assert_eq!(join("sub/", "../base.txt").as_deref(), Some("base.txt"));
        assert_eq!(join("", "./base.txt").as_deref(), Some("base.txt"));
        assert_eq!(join("sub/", "./../base.txt").as_deref(), Some("base.txt"));
        assert_eq!(join("sub/", "../../escape"), None);
        assert_eq!(join("", "../escape"), None);
    }

    /// The dispatch in `get_oid_with_context_1()`, one case per branch. Every one
    /// of these is a different stock message when the lookup fails, so a wrong
    /// split is a wrong diagnosis.
    #[test]
    fn split_matches_get_oid_with_context() {
        assert_eq!(split("main"), Split::Rev);
        assert_eq!(split("main:base.txt"), Split::Tree { rev: "main", path: "base.txt" });
        // The scan stops at the *first* unbracketed colon, so the peel suffix is
        // part of the path — stock: `path 'base.txt^{tree}' does not exist in 'main'`.
        assert_eq!(
            split("main:base.txt^{tree}"),
            Split::Tree { rev: "main", path: "base.txt^{tree}" }
        );
        // …but a colon *inside* a brace is skipped.
        assert_eq!(split("main@{1}:f"), Split::Tree { rev: "main@{1}", path: "f" });
        assert_eq!(split("main^{/a:b}:f"), Split::Tree { rev: "main^{/a:b}", path: "f" });
        assert_eq!(split(":base.txt"), Split::Index { stage: 0, path: "base.txt" });
        assert_eq!(split(":0:base.txt"), Split::Index { stage: 0, path: "base.txt" });
        assert_eq!(split(":3:base.txt"), Split::Index { stage: 3, path: "base.txt" });
        // Only 0..3 are stages; `4` is part of the path, which is why stock says
        // `path '4:base.txt' does not exist`.
        assert_eq!(split(":4:base.txt"), Split::Index { stage: 0, path: "4:base.txt" });
        assert_eq!(split(":./base.txt"), Split::Index { stage: 0, path: "./base.txt" });
        assert_eq!(split(":/c1"), Split::Oneline);
        // `namelen > 2` — a bare `:/` is the index path `/`, not a search.
        assert_eq!(split(":/"), Split::Index { stage: 0, path: "/" });
    }
}

/// `die_verify_filename()` (`setup.c:202-225`), minus the two `die()` calls that
/// are the caller's to make: the specific message git prefers over
/// `ambiguous argument '<arg>': …`, or `None` when it has nothing better.
///
/// ```c
/// /*
///  * Saying "'(icase)foo' does not exist in the index" when the
///  * user gave us ":(icase)foo" is just stupid.  A magic pathspec
///  * begins with a colon and is followed by a non-alnum; do not
///  * let maybe_die_on_misspelt_object_name() even trigger.
///  */
/// if (!(arg[0] == ':' && !isalnum(arg[1])))
///         maybe_die_on_misspelt_object_name(r, arg, prefix);
/// ```
///
/// The guard is why `:./nosuch` gets the generic message from a subdirectory
/// while `:nosuch` gets `path 'nosuch' does not exist (neither on disk nor in
/// the index)` — the leading `.` is not alphanumeric, so the diagnosis is never
/// attempted for the relative spelling.
pub fn verify_filename_diagnosis(repo: &gix::Repository, arg: &str) -> Option<String> {
    let b = arg.as_bytes();
    if b.first() == Some(&b':') && !b.get(1).is_some_and(u8::is_ascii_alphanumeric) {
        return None;
    }
    misspelt_object_name(repo, arg)
}

/// `prefix_path()`'s `die()`, for an operand whose `./`/`../` path arm climbs out
/// of the work tree — or `None` when there is no such arm.
///
/// This one fires *inside* `get_oid_with_context_1()`, not from
/// `die_verify_filename()`, so it is due before the operand is echoed and it is
/// not subject to the magic-pathspec guard in [`verify_filename_diagnosis`].
/// Stock 2.55.0 from `sub/`: `git rev-parse :../../escape` prints nothing on
/// stdout and `fatal: '../../escape' is outside repository at '<toplevel>'` on
/// stderr, where the same command with `:./nosuch` echoes the operand first.
pub fn relative_path_fatal(repo: &gix::Repository, spec: &str) -> Option<String> {
    let path = match split(spec) {
        Split::Oneline | Split::Rev => return None,
        Split::Index { path, .. } | Split::Tree { path, .. } => path,
    };
    resolve_relative_path(repo, path).err()
}

/// `relative_path()` (`path.c`), the renderer behind
/// `write_name_quoted_relative()`: `name` seen from the directory `prefix`
/// names, with `../` for every level that has to be climbed.
///
/// Both operands are repository-root-relative and `prefix` carries its trailing
/// `/` — git's own `prefix`. A name inside the prefix loses it (`src/lib.rs`
/// under `src/` is `lib.rs`); a name outside it gains the climb
/// (`README.md` under `src/` is `../README.md`, and under `nested/deep/` it is
/// `../../README.md`). An empty `prefix` returns the name unchanged, and an
/// empty `name` is git's `"./"`.
///
/// This is what separates `ls-files -- :(top)` run from a subdirectory from a
/// plain prefix strip: the pathspec reaches outside the current directory, and
/// git names those entries relative to where the user is standing rather than
/// from the repository root.
pub fn relative_path(name: &[u8], prefix: &[u8]) -> Vec<u8> {
    if name.is_empty() {
        return b"./".to_vec();
    }
    if prefix.is_empty() {
        return name.to_vec();
    }
    let is_sep = |b: u8| b == b'/';

    // The shared leading directories, counted in whole components: `i`/`j` walk
    // in step and the `*_off` marks trail the last component boundary crossed.
    let (mut i, mut j) = (0usize, 0usize);
    let (mut prefix_off, mut in_off) = (0usize, 0usize);
    while i < prefix.len() && j < name.len() && prefix[i] == name[j] {
        if is_sep(prefix[i]) {
            while i < prefix.len() && is_sep(prefix[i]) {
                i += 1;
            }
            while j < name.len() && is_sep(name[j]) {
                j += 1;
            }
            prefix_off = i;
            in_off = j;
        } else {
            i += 1;
            j += 1;
        }
    }

    if i >= prefix.len() && prefix_off < prefix.len() {
        // `prefix` is a prefix of `name` and does not end in a separator.
        if j >= name.len() {
            in_off = name.len();
        } else if is_sep(name[j]) {
            while j < name.len() && is_sep(name[j]) {
                j += 1;
            }
            in_off = j;
        } else {
            // `name` only *starts* like the prefix: `a/bbb` under `a/b`.
            i = prefix_off;
        }
    } else if j >= name.len() || (i < prefix.len() && is_sep(prefix[i])) {
        while i < prefix.len() && is_sep(prefix[i]) {
            i += 1;
        }
    } else {
        i = prefix_off;
    }

    let rest = &name[in_off..];
    if i >= prefix.len() {
        return if rest.is_empty() { b"./".to_vec() } else { rest.to_vec() };
    }

    let mut out = Vec::with_capacity(rest.len() + 8);
    while i < prefix.len() {
        if is_sep(prefix[i]) {
            out.extend_from_slice(b"../");
            while i < prefix.len() && is_sep(prefix[i]) {
                i += 1;
            }
            continue;
        }
        i += 1;
    }
    if !is_sep(prefix[prefix.len() - 1]) {
        out.extend_from_slice(b"../");
    }
    out.extend_from_slice(rest);
    out
}

#[cfg(test)]
mod relative_path_tests {
    use super::relative_path;

    /// The four shapes `ls-files -- :(top)` produces from a subdirectory, each
    /// measured against git 2.55.0 in the `awkward-paths` fixture.
    #[test]
    fn names_are_written_relative_to_the_prefix() {
        assert_eq!(relative_path(b"src/lib.rs", b"src/"), b"lib.rs");
        assert_eq!(relative_path(b"README.md", b"src/"), b"../README.md");
        assert_eq!(relative_path(b"nested/deep/path.txt", b"src/"), b"../nested/deep/path.txt");
        assert_eq!(relative_path(b"src/lib.rs", b"nested/deep/"), b"../../src/lib.rs");
    }

    /// No prefix leaves the name alone, an empty name is `"./"`, and a name that
    /// diverges from the prefix mid-component keeps only the components the two
    /// agreed on.
    ///
    /// That last one is `relative_path()`'s own quirk and is asserted as the C
    /// computes it, not as a climb: with `prefix = "a/b/"` and `in = "a/bbb/c"`
    /// the comparison loop stops at `b` vs `bb` having already crossed `a/`, the
    /// `is_dir_sep(prefix[i])` branch skips the rest of the prefix, and what is
    /// returned is `in + in_off` — `bbb/c`.
    #[test]
    fn edges() {
        assert_eq!(relative_path(b"a/b", b""), b"a/b");
        assert_eq!(relative_path(b"", b"src/"), b"./");
        assert_eq!(relative_path(b"a/bbb/c", b"a/b/"), b"bbb/c");
    }
}
