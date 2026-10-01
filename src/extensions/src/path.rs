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
