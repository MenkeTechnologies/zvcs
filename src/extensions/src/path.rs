//! The parts of git's `path.c` that more than one command prints through.
//!
//! [`format_path`] is `format_path()` (`path.c:1582-1645`, v2.56.0), which 2.56
//! lifted out of `builtin/rev-parse.c`'s `print_path()` so that `git repo info`'s
//! `path.*` keys and `rev-parse`'s path options render a directory the same way.
//! [`relative_path`] is the textual `relative_path()` both of them bottom out in.

use std::path::{Path, PathBuf};

/// git's `enum path_format` (`path.h:268-280`, v2.56.0).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PathFormat {
    /// `PATH_FORMAT_UNMODIFIED`: the path exactly as given.
    Unmodified,
    /// `PATH_FORMAT_RELATIVE`: relative to `prefix`, or to the cwd when there is
    /// none, with both sides made absolute first.
    Relative,
    /// `PATH_FORMAT_RELATIVE_IF_SHARED`: [`relative_path`] on the two strings as
    /// they stand, which leaves the path whole when they share no root or there
    /// is no prefix.
    RelativeIfShared,
    /// `PATH_FORMAT_CANONICAL`: absolute and symlink-free.
    Canonical,
}

/// ```c
/// void format_path(struct strbuf *dest, const char *path,
///                  const char *prefix, enum path_format format)
/// ```
///
/// (`path.c:1582-1645`, v2.56.0.) `cwd` is the directory git stands in once
/// setup is done — what `xgetcwd()` returns there and what a relative `path` or
/// `prefix` is resolved against. This port does not `chdir()` the way git's
/// setup does, so the caller names that directory.
///
/// ```c
/// case PATH_FORMAT_RELATIVE:
///         if (!prefix)
///                 prefix = cwd = xgetcwd();
///         if (!is_absolute_path(path)) {
///                 strbuf_realpath_forgiving(&real_path, path, 1);
///                 path = real_path.buf;
///         }
///         if (!is_absolute_path(prefix)) {
///                 strbuf_realpath_forgiving(&real_prefix, prefix, 1);
///                 prefix = real_prefix.buf;
///         }
///         strbuf_addstr(dest, relative_path(path, prefix, &relative_buf));
/// case PATH_FORMAT_RELATIVE_IF_SHARED:
///         strbuf_addstr(dest, relative_path(path, prefix, &relative_buf));
/// case PATH_FORMAT_CANONICAL:
///         strbuf_realpath_forgiving(dest, path, 1);
/// ```
///
/// An absolute `path` or `prefix` is used as it is in the `RELATIVE` arm: only a
/// relative one is resolved.
pub fn format_path(path: &Path, prefix: Option<&str>, format: PathFormat, cwd: &Path) -> Vec<u8> {
    let bytes = |p: &Path| p.as_os_str().as_encoded_bytes().to_vec();
    match format {
        PathFormat::Unmodified => bytes(path),
        PathFormat::Relative => {
            let prefix = match prefix {
                Some(p) => PathBuf::from(p),
                None => cwd.to_path_buf(),
            };
            let path = match path.is_absolute() {
                true => path.to_path_buf(),
                false => realpath_forgiving_in(cwd, path),
            };
            let prefix = match prefix.is_absolute() {
                true => prefix,
                false => realpath_forgiving_in(cwd, &prefix),
            };
            relative_path(&bytes(&path), Some(&bytes(&prefix)))
        }
        PathFormat::RelativeIfShared => relative_path(&bytes(path), prefix.map(str::as_bytes)),
        PathFormat::Canonical => bytes(&realpath_forgiving_in(cwd, path)),
    }
}

/// `strbuf_realpath_forgiving(…, path, 1)` with git's cwd standing at `cwd`.
fn realpath_forgiving_in(cwd: &Path, path: &Path) -> PathBuf {
    crate::setup::realpath_forgiving(&cwd.join(path))
}

/// ```c
/// const char *relative_path(const char *in, const char *prefix, struct strbuf *sb)
/// ```
///
/// (`path.c:942-1037`, v2.56.0), byte for byte: an empty `in` is `./`, an empty (or NULL)
/// `prefix` returns `in` unchanged, and paths that do not share a root are also
/// returned unchanged. Otherwise the shared directory components are dropped and one
/// `../` is emitted per component of `prefix` that is left over.
pub fn relative_path(input: &[u8], prefix: Option<&[u8]>) -> Vec<u8> {
    let is_sep = |b: u8| b == b'/';
    let in_len = input.len();
    let prefix = prefix.unwrap_or(b"");
    let prefix_len = prefix.len();
    if in_len == 0 {
        return b"./".to_vec();
    }
    if prefix_len == 0 {
        return input.to_vec();
    }
    // `have_same_root()`: on a POSIX filesystem that is "both absolute or both
    // relative", since there is no drive prefix to compare.
    if input.starts_with(b"/") != prefix.starts_with(b"/") {
        return input.to_vec();
    }

    let (mut i, mut j) = (0usize, 0usize);
    let (mut prefix_off, mut in_off) = (0usize, 0usize);
    while i < prefix_len && j < in_len && prefix[i] == input[j] {
        if is_sep(prefix[i]) {
            while i < prefix_len && is_sep(prefix[i]) {
                i += 1;
            }
            while j < in_len && is_sep(input[j]) {
                j += 1;
            }
            prefix_off = i;
            in_off = j;
        } else {
            i += 1;
            j += 1;
        }
    }

    if i >= prefix_len && prefix_off < prefix_len {
        if j >= in_len {
            in_off = in_len;
        } else if is_sep(input[j]) {
            while j < in_len && is_sep(input[j]) {
                j += 1;
            }
            in_off = j;
        } else {
            i = prefix_off;
        }
    } else if j >= in_len && in_off < in_len && i < prefix_len && is_sep(prefix[i]) {
        while i < prefix_len && is_sep(prefix[i]) {
            i += 1;
        }
        in_off = in_len;
    }

    let rest = &input[in_off..];
    if i >= prefix_len {
        return if rest.is_empty() { b"./".to_vec() } else { rest.to_vec() };
    }

    let mut sb: Vec<u8> = Vec::with_capacity(rest.len());
    while i < prefix_len {
        if is_sep(prefix[i]) {
            sb.extend_from_slice(b"../");
            while i < prefix_len && is_sep(prefix[i]) {
                i += 1;
            }
            continue;
        }
        i += 1;
    }
    if !is_sep(prefix[prefix_len - 1]) {
        sb.extend_from_slice(b"../");
    }
    sb.extend_from_slice(rest);
    sb
}

/// The failing outcomes of `safe_create_leading_directories()` (`enum
/// scld_error`, path.h:246-252, v2.56.0).
#[derive(Debug)]
pub enum Scld {
    /// `SCLD_FAILED`: `mkdir()` refused a prefix for a reason other than the two below.
    Failed(std::io::Error),
    /// `SCLD_EXISTS`: a prefix exists and is not a directory; `errno` is `ENOTDIR`.
    Exists,
    /// `SCLD_VANISHED`: `mkdir()` answered `ENOENT`, a parent disappeared underneath.
    Vanished,
}

impl Scld {
    /// The `errno` a `die_errno()` after the call reports.
    pub fn errno(&self) -> std::io::Error {
        match self {
            Scld::Failed(err) => std::io::Error::from_raw_os_error(err.raw_os_error().unwrap_or(libc::EIO)),
            Scld::Exists => std::io::Error::from_raw_os_error(libc::ENOTDIR),
            Scld::Vanished => std::io::Error::from_raw_os_error(libc::ENOENT),
        }
    }
}

/// `safe_create_leading_directories()` (path.c, v2.56.0): create every missing
/// directory above the last component of `path`, leaving that component alone.
///
/// ```c
/// if (!stat(path, &st)) {
///         /* path exists */
///         if (!S_ISDIR(st.st_mode)) {
///                 errno = ENOTDIR;
///                 ret = SCLD_EXISTS;
///         }
/// } else if (mkdir(path, 0777)) {
///         if (errno == EEXIST && !stat(path, &st) && S_ISDIR(st.st_mode))
///                 ; /* somebody created it since we checked */
///         else if (errno == ENOENT)
///                 ret = SCLD_VANISHED;
///         else
///                 ret = SCLD_FAILED;
/// }
/// ```
pub fn safe_create_leading_directories(path: &str) -> Result<(), Scld> {
    let bytes = path.as_bytes();
    // `offset_1st_component()`: the leading separator of an absolute path is not
    // a component that can be created.
    let mut next = usize::from(bytes.first() == Some(&b'/'));
    while next < bytes.len() {
        let Some(offset) = bytes[next..].iter().position(|b| *b == b'/') else {
            break;
        };
        let slash = next + offset;
        // Skip a run of separators; a path that ends in them has no further
        // component to create.
        let mut after = slash + 1;
        while bytes.get(after) == Some(&b'/') {
            after += 1;
        }
        if after >= bytes.len() {
            break;
        }
        next = after;

        let prefix = &path[..slash];
        match std::fs::metadata(prefix) {
            Ok(meta) if meta.is_dir() => {}
            Ok(_) => return Err(Scld::Exists),
            Err(_) => match std::fs::create_dir(prefix) {
                Ok(()) => {}
                Err(err)
                    if err.kind() == std::io::ErrorKind::AlreadyExists
                        && std::fs::metadata(prefix).is_ok_and(|meta| meta.is_dir()) => {}
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Err(Scld::Vanished),
                Err(err) => return Err(Scld::Failed(err)),
            },
        }
    }
    Ok(())
}
