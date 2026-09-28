//! Remotes defined outside the configuration: `$GIT_DIR/remotes/<name>` and
//! `$GIT_DIR/branches/<name>`.
//!
//! git still reads both (remote.c:326-429, compiled in unless
//! `WITH_BREAKING_CHANGES`), but only for a remote whose configuration names no
//! URL: `remotes_remote_get_1()` (remote.c:797-822) tries the `remotes/` file,
//! then the `branches/` file, before falling back to the name as a URL. Parsing
//! them needs the caller's diagnostics — the deprecation warning, the
//! `init.defaultBranch` advice — so the reader is installed by the embedding
//! program with [`set_reader()`]; without one, no legacy remote exists.

use std::sync::OnceLock;

use crate::bstr::{BStr, BString};

/// What a `remotes/` or `branches/` file contributes to a remote.
#[derive(Debug, Default, Clone)]
pub struct Remote {
    /// `URL:` lines (or the `branches/` file's URL), in file order.
    pub urls: Vec<BString>,
    /// `Pull:` lines, fetch refspecs appended after any configured ones.
    pub fetch: Vec<BString>,
    /// `Push:` lines, push refspecs appended after any configured ones.
    pub push: Vec<BString>,
}

/// The reader: the repository and the remote name, `None` when neither file
/// defines the remote.
pub type Reader = fn(&crate::Repository, &BStr) -> Option<Remote>;

static READER: OnceLock<Reader> = OnceLock::new();

/// Install the reader consulted by every remote lookup. The first call wins.
pub fn set_reader(reader: Reader) {
    let _ = READER.set(reader);
}

/// `valid_remote_nick()` (remote.c): only a name that could be a file under
/// `remotes/` is looked up there.
pub fn valid_remote_nick(name: &BStr) -> bool {
    !name.is_empty() && name != "." && name != ".." && !name.contains(&b'/')
}

/// The legacy definition of `name`, if the installed reader finds one.
pub(crate) fn read(repo: &crate::Repository, name: &BStr) -> Option<Remote> {
    if !valid_remote_nick(name) {
        return None;
    }
    READER.get().and_then(|reader| reader(repo, name))
}
