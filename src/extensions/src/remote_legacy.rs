//! `$GIT_DIR/remotes/<name>` and `$GIT_DIR/branches/<name>`: remotes defined
//! outside the configuration, which git still reads (remote.c:326-429) for a
//! remote whose configuration names no URL.
//!
//! [`install`] hands [`read`] to the vendored remote lookup
//! ([`gix::remote::legacy`]), so `fetch`, `push`, `ls-remote` and `pull` see
//! the remote; `git remote` and `push`, which read `remote.*` themselves, ask
//! [`lookup`]. git parses each remote once per process (`remote_state` keeps
//! it), and with it prints the deprecation warning once; the cache here keeps
//! that true however often the lookup runs.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;

use gix::bstr::{BStr, BString, ByteSlice};
use gix::remote::legacy::Remote;

/// Register the reader with the vendored lookup.
pub fn install() {
    gix::remote::legacy::set_reader(read);
}

/// The legacy definition of `name`, for callers that read the remote's
/// configuration themselves and have found no URL there.
pub(crate) fn lookup(repo: &gix::Repository, name: &BStr) -> Option<Remote> {
    gix::remote::legacy::valid_remote_nick(name).then(|| read(repo, name)).flatten()
}

type Cache = HashMap<(PathBuf, BString), Option<Remote>>;
static CACHE: Mutex<Option<Cache>> = Mutex::new(None);

/// The `remotes/` file, else the `branches/` file, for `name` — each read at
/// most once per repository and name.
fn read(repo: &gix::Repository, name: &BStr) -> Option<Remote> {
    let key = (repo.common_dir().to_owned(), name.to_owned());
    if let Some(cached) = CACHE.lock().ok()?.get_or_insert_with(HashMap::new).get(&key) {
        return cached.clone();
    }
    let found = read_remotes_file(repo, name).or_else(|| read_branches_file(repo, name));
    CACHE.lock().ok()?.get_or_insert_with(HashMap::new).insert(key, found.clone());
    found
}

/// `warn_about_deprecated_remote_type()` (remote.c:334-347).
fn warn_deprecated(kind: &str, name: &BStr) {
    eprintln!(
        "warning: reading remote from \"{kind}/{name}\", which is nominated for removal.\n\
         \n\
         If you still use the \"remotes/\" directory it is recommended to\n\
         migrate to config-based remotes:\n\
         \n\
         \tgit remote rename {name} {name}\n\
         \n\
         If you cannot, please let us know why you still need to use it by\n\
         sending an e-mail to <git@vger.kernel.org>."
    );
}

/// `fopen_or_warn()`: a missing file is silently absent, anything else that
/// stops the read is a warning.
fn open(path: &std::path::Path) -> Option<Vec<u8>> {
    match std::fs::read(path) {
        Ok(bytes) => Some(bytes),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => {
            eprintln!(
                "warning: unable to access '{}': {}",
                path.display(),
                crate::external::strerror(&e)
            );
            None
        }
    }
}

/// `read_remotes_file()` (remote.c:349-379): `URL:`, `Push:` and `Pull:` lines,
/// each value with its leading whitespace skipped.
fn read_remotes_file(repo: &gix::Repository, name: &BStr) -> Option<Remote> {
    let bytes = open(&repo.common_dir().join("remotes").join(gix::path::from_bstr(name)))?;
    warn_deprecated("remotes", name);
    let mut remote = Remote::default();
    // `strbuf_getline()` then `strbuf_rtrim()`.
    for line in bytes.lines() {
        let line = line.trim_end();
        let value = |prefix: &[u8]| line.strip_prefix(prefix).map(|v| BString::from(v.trim_start()));
        if let Some(v) = value(b"URL:") {
            remote.urls.push(v);
        } else if let Some(v) = value(b"Push:") {
            remote.push.push(v);
        } else if let Some(v) = value(b"Pull:") {
            remote.fetch.push(v);
        }
    }
    Some(remote)
}

/// `read_branches_file()` (remote.c:381-428): one `<url>[#<branch>]` line. The
/// branch (default: `repo_default_branch_name()`) is fetched into
/// `refs/heads/<name>`, and `HEAD` pushes to it. An empty line defines nothing,
/// though the warning has been printed by then.
fn read_branches_file(repo: &gix::Repository, name: &BStr) -> Option<Remote> {
    let bytes = open(&repo.common_dir().join("branches").join(gix::path::from_bstr(name)))?;
    warn_deprecated("branches", name);
    // `strbuf_getline_lf()` then `strbuf_trim()`.
    let line = bytes.split(|b| *b == b'\n').next().unwrap_or_default().trim();
    if line.is_empty() {
        return None;
    }
    let (url, branch) = match line.find_byte(b'#') {
        Some(at) => (&line[..at], line[at + 1..].to_str_lossy().into_owned()),
        None => (line, crate::refname::repo_default_branch_name(repo, false)),
    };
    Some(Remote {
        urls: vec![url.into()],
        fetch: vec![format!("refs/heads/{branch}:refs/heads/{name}").into()],
        push: vec![format!("HEAD:refs/heads/{branch}").into()],
    })
}
