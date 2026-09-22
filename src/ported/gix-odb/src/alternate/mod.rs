//! A file with directories of other git object databases to use when reading objects.
//!
//! This inherently makes alternates read-only.
//!
//! An alternate file in `<git-dir>/objects/info/alternates` can look as follows:
//!
//! ```text
//! # a comment, empty lines are also allowed
//! # relative paths resolve relative to the object directory that lists them
//! ../path/relative/to/objects
//! /absolute/path/to/objects
//!
//! "/a/ansi-c-quoted/path/with/tabs\t/objects"
//! ```
//!
//! Ported from `odb_prepare_alternates()` (`odb.c:487-502`) and the
//! `odb_add_alternate_recursively()` / `odb_is_source_usable()` pair it drives
//! (`odb.c:56-100`, `odb.c:169-205`):
//!
//! ```c
//! void odb_prepare_alternates(struct object_database *odb)
//! {
//!         struct strvec sources = STRVEC_INIT;
//!
//!         if (odb->loaded_alternates)
//!                 return;
//!
//!         parse_alternates(odb->alternate_db, PATH_SEP, NULL, &sources);
//!         odb_source_read_alternates(odb->sources, &sources);
//!         for (size_t i = 0; i < sources.nr; i++)
//!                 odb_add_alternate_recursively(odb, sources.v[i], 0);
//!
//!         odb->loaded_alternates = 1;
//!
//!         strvec_clear(&sources);
//! }
//! ```
//!
//! Five properties of that assembly are observable, and this is the list of what
//! the previous stack-based traversal answered differently.
//!
//! * **`$GIT_ALTERNATE_OBJECT_DIRECTORIES` is a source of alternates**, read
//!   before the repository's own `info/alternates` and with `PATH_SEP` — `:` —
//!   for a separator. `odb->alternate_db` is that variable (`odb.c:1022`).
//! * **An entry that is not a directory is dropped**, not linked
//!   (`odb_is_source_usable()`, `odb.c:68-73`). The same check drops an entry
//!   whose path could not be normalized at all, which `parse_alternates()`
//!   already refused to emit.
//! * **A repeat is skipped, and so is a cycle** — `source_by_path` holds the
//!   primary object directory and every alternate linked so far, so "the common
//!   mistake of listing the same thing twice" and an alternate pointing back at
//!   its borrower are the same case and neither is an error (`odb.c:75-93`).
//! * **Nesting stops after five levels.** `if (sources.nr && depth + 1 > 5)`
//!   (`odb.c:194`) drops the whole level below, so a chain reached from the
//!   primary store contributes at most six object directories.
//! * **The order is a depth-first pre-order walk in file order**, because each
//!   entry is appended to `odb->sources` *before* its own alternates are read.
//!   That order reaches `git count-objects -v`'s `alternate:` lines and
//!   `git rev-list --alternate-refs`.

use std::{
    fs, io,
    path::{Path, PathBuf},
};

use gix_path::realpath::MAX_SYMLINKS;

///
pub mod parse;

/// Returned by [`resolve()`]
#[derive(thiserror::Error, Debug)]
#[expect(missing_docs)]
pub enum Error {
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error(transparent)]
    Realpath(#[from] gix_path::realpath::Error),
    #[error(transparent)]
    Parse(#[from] parse::Error),
}

/// `if (sources.nr && depth + 1 > 5)` (`odb.c:194`).
const MAX_DEPTH: usize = 5;

/// Given an `objects_directory`, resolve the alternate object directories it
/// borrows from into canonical paths, resolving relative paths with the help of
/// `current_dir`.
///
/// The answer is `odb->sources` minus the primary store: the environment's
/// alternates first, then the ones `objects/info/alternates` names, each
/// followed immediately by its own alternates. A repository with no alternates
/// answers an empty `Vec`, which is not an error, and neither is a listed
/// directory that is missing, repeated, cyclic or nested too deeply — each is
/// dropped exactly as git drops it.
pub fn resolve(objects_directory: PathBuf, current_dir: &Path) -> Result<Vec<PathBuf>, Error> {
    resolve_inner(objects_directory, current_dir, &mut Vec::new())
}

/// The `error()` calls the same walk makes, in the order git makes them, without
/// the object directories it resolves.
///
/// git prepares alternates lazily and re-prepares them whenever a lookup misses,
/// so the same line appears once for `log` or `cat-file`, several times for `gc`,
/// and not at all for a command that never reaches the object database.
/// Reproducing that count would mean hooking every object read; a caller that
/// wants the diagnostics asks for them once, before the command runs, and the
/// resolution itself stays silent — so the text is git's and the repetition is
/// not.
/// `$GIT_ALTERNATE_OBJECT_DIRECTORIES` is diagnosed separately, before any
/// command runs, so `include_environment` lets a caller that has already
/// reported those ask only about the ones `objects/info/alternates` chains
/// reach. git prints each line once per preparation whatever its source, so a
/// caller that reported the environment list must not repeat it here.
pub fn diagnose(
    objects_directory: PathBuf,
    current_dir: &Path,
    include_environment: bool,
) -> Vec<String> {
    let mut diagnostics = Vec::new();
    let _ = resolve_with(
        objects_directory,
        current_dir,
        include_environment,
        &mut diagnostics,
    );
    diagnostics
}

fn resolve_inner(
    objects_directory: PathBuf,
    current_dir: &Path,
    diagnostics: &mut Vec<String>,
) -> Result<Vec<PathBuf>, Error> {
    resolve_with(objects_directory, current_dir, true, diagnostics)
}

fn resolve_with(
    objects_directory: PathBuf,
    current_dir: &Path,
    include_environment: bool,
    diagnostics: &mut Vec<String>,
) -> Result<Vec<PathBuf>, Error> {
    let mut out = Vec::new();
    // `source_by_path` is seeded with the primary object directory, which is why
    // an alternate pointing back at the borrower is skipped rather than linked.
    let mut seen = vec![gix_path::realpath_opts(&objects_directory, current_dir, MAX_SYMLINKS)?];

    // `parse_alternates(odb->alternate_db, PATH_SEP, NULL, &sources)`: no
    // relative base, so a relative entry resolves against the current directory.
    let env = include_environment
        .then(|| std::env::var_os("GIT_ALTERNATE_OBJECT_DIRECTORIES"))
        .flatten()
        .and_then(|raw| gix_path::os_string_into_bstring(raw).ok());
    let mut sources = match &env {
        Some(raw) => parse::alternates(raw.as_slice(), b':', None, current_dir, diagnostics)?,
        None => Vec::new(),
    };
    // `odb_source_read_alternates(odb->sources, &sources)` appends to the same
    // list, so the environment's entries stay in front of the file's.
    sources.extend(read_alternates_of(&objects_directory, current_dir, diagnostics)?);

    for source in sources {
        add_alternate_recursively(source, 0, current_dir, &mut seen, &mut out, diagnostics)?;
    }
    Ok(out)
}

/// `odb_source_files_read_alternates()` (`odb/source-files.c:192-209`): the
/// `info/alternates` of one object directory, with that directory as the base a
/// relative entry resolves against.
///
/// A missing file is not an error — `strbuf_read_file()` failing only reaches
/// `warn_on_fopen_errors()`, which is silent for `ENOENT`.
fn read_alternates_of(
    object_dir: &Path,
    current_dir: &Path,
    diagnostics: &mut Vec<String>,
) -> Result<Vec<PathBuf>, Error> {
    match fs::read(object_dir.join("info").join("alternates")) {
        Ok(input) => Ok(parse::alternates(
            &input,
            b'\n',
            Some(object_dir),
            current_dir,
            diagnostics,
        )?),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(err) => Err(err.into()),
    }
}

/// `odb_add_alternate_recursively()` (`odb.c:169-205`).
fn add_alternate_recursively(
    source: PathBuf,
    depth: usize,
    current_dir: &Path,
    seen: &mut Vec<PathBuf>,
    out: &mut Vec<PathBuf>,
    diagnostics: &mut Vec<String>,
) -> Result<(), Error> {
    // `odb_is_source_usable()`: a path that has vanished, is the primary store,
    // or is already linked contributes nothing. `parse::alternates` has already
    // resolved `source`, so the comparison is between canonical paths as it is
    // in the C.
    if !source.is_dir() {
        // ```c
        // /* Detect cases where alternate disappeared */
        // if (!is_directory(path)) {
        //         error(_("object directory %s does not exist; "
        //                 "check .git/objects/info/alternates"), path);
        // ```
        // The message names `.git/objects/info/alternates` whether the entry
        // came from that file or from the environment, because
        // `odb_is_source_usable()` cannot tell them apart by the time it runs.
        diagnostics.push(format!(
            "object directory {} does not exist; check .git/objects/info/alternates",
            source.display()
        ));
        return Ok(());
    }
    // A repeat and a cycle are the same case, and git says nothing about either.
    if seen.contains(&source) {
        return Ok(());
    }
    seen.push(source.clone());
    // The entry is appended before its own alternates are read, which is what
    // makes the walk pre-order.
    out.push(source.clone());

    let sources = read_alternates_of(&source, current_dir, diagnostics)?;
    // The level below is dropped whole, not trimmed, once the nesting is too
    // deep.
    if sources.is_empty() {
        return Ok(());
    }
    if depth + 1 > MAX_DEPTH {
        diagnostics.push(format!(
            "{}: ignoring alternate object stores, nesting too deep",
            source.display()
        ));
        return Ok(());
    }
    for source in sources {
        add_alternate_recursively(source, depth + 1, current_dir, seen, out, diagnostics)?;
    }
    Ok(())
}
