//! `parse_alternates()` (`odb.c:102-167`): one separated list of alternate
//! object directories, turned into the absolute paths git will link.
//!
//! ```c
//! while (*string) {
//!         const char *end;
//!
//!         strbuf_reset(&buf);
//!         strbuf_reset(&pathbuf);
//!
//!         if (*string == '#') {
//!                 /* comment; consume up to next separator */
//!                 end = strchrnul(string, sep);
//!         } else if (*string == '"' && !unquote_c_style(&buf, string, &end)) {
//!                 /*
//!                  * quoted path; unquote_c_style has copied the
//!                  * data for us and set "end". Broken quoting (e.g.,
//!                  * an entry that doesn't end with a quote) falls
//!                  * back to the unquoted case below.
//!                  */
//!         } else {
//!                 /* normal, unquoted path */
//!                 end = strchrnul(string, sep);
//!                 strbuf_add(&buf, string, end - string);
//!         }
//!
//!         if (*end)
//!                 end++;
//!         string = end;
//!
//!         if (!buf.len)
//!                 continue;
//!
//!         if (!is_absolute_path(buf.buf) && relative_base) {
//!                 strbuf_realpath(&pathbuf, relative_base, 1);
//!                 strbuf_addch(&pathbuf, '/');
//!         }
//!         strbuf_addbuf(&pathbuf, &buf);
//!
//!         strbuf_reset(&buf);
//!         if (!strbuf_realpath(&buf, pathbuf.buf, 0)) {
//!                 error(_("unable to normalize alternate object path: %s"),
//!                       pathbuf.buf);
//!                 continue;
//!         }
//!
//!         /*
//!          * The trailing slash after the directory name is given by
//!          * this function at the end. Remove duplicates.
//!          */
//!         while (buf.len && buf.buf[buf.len - 1] == '/')
//!                 strbuf_setlen(&buf, buf.len - 1);
//!
//!         strvec_push(out, buf.buf);
//! }
//! ```
//!
//! Three properties of that loop are observable and reproduced here.
//!
//! * **Only the unquoted arm stops at the separator.** A quoted entry ends at
//!   its closing quote, so it may hold a separator of its own; quoting that does
//!   not parse falls back to the unquoted arm rather than failing the list.
//! * **A relative entry is joined to `relative_base`, and `relative_base` is the
//!   object directory that listed it** — not the repository the walk started in.
//!   With no base (the `GIT_ALTERNATE_OBJECT_DIRECTORIES` form) the path is left
//!   relative and `strbuf_realpath` resolves it against the current directory.
//! * **Every entry is normalized, and one that cannot be is dropped.** Symlinks
//!   are followed, `..` is folded, and trailing slashes are trimmed, which is why
//!   `git count-objects -v` prints an absolute, symlink-free `alternate:` line
//!   whatever the file said.

use std::{
    borrow::Cow,
    path::{Path, PathBuf},
};

use gix_object::bstr::ByteSlice;
use gix_path::realpath::MAX_SYMLINKS;

/// Returned as part of [`crate::alternate::Error::Parse`]
#[derive(thiserror::Error, Debug)]
#[expect(missing_docs)]
pub enum Error {
    #[error("Could not obtain an object path for the alternate directory '{}'", String::from_utf8_lossy(.0))]
    PathConversion(Vec<u8>),
    #[error("Could not unquote alternate path")]
    Unquote(#[from] gix_quote::ansi_c::undo::Error),
}

/// `parse_alternates(string, sep, relative_base, out)`.
///
/// `relative_base` is git's argument of the same name: the object directory a
/// relative entry is resolved against, or `None` for the environment form.
/// `current_dir` is what `strbuf_realpath` resolves a still-relative path
/// against.
///
/// An entry that cannot be normalized is dropped, as `continue` does after
/// git's `error()`.
pub(crate) fn alternates(
    input: &[u8],
    sep: u8,
    relative_base: Option<&Path>,
    current_dir: &Path,
    diagnostics: &mut Vec<String>,
) -> Result<Vec<PathBuf>, Error> {
    let mut out = Vec::new();
    let mut at = 0;
    while at < input.len() {
        let (entry, next): (Cow<'_, [u8]>, usize) = if input[at] == b'#' {
            // A comment contributes nothing and runs to the next separator.
            (Cow::Borrowed(&[][..]), strchrnul(input, sep, at))
        } else if input[at] == b'"' {
            match gix_quote::ansi_c::undo(input[at..].as_bstr()) {
                Ok((text, consumed)) => (Cow::Owned(text.into_owned().into()), at + consumed),
                // "Broken quoting … falls back to the unquoted case below."
                Err(_) => {
                    let end = strchrnul(input, sep, at);
                    (Cow::Borrowed(&input[at..end]), end)
                }
            }
        } else {
            let end = strchrnul(input, sep, at);
            (Cow::Borrowed(&input[at..end]), end)
        };
        // `if (*end) end++;` — step over the separator only when there was one.
        at = if next < input.len() { next + 1 } else { next };

        // `if (!buf.len) continue;` — which is why an empty
        // `GIT_ALTERNATE_OBJECT_DIRECTORIES` is silent rather than an error.
        if entry.is_empty() {
            continue;
        }
        let entry = gix_path::try_from_byte_slice(&entry)
            .map_err(|_| Error::PathConversion(entry.to_vec()))?;
        let joined = match relative_base {
            Some(base) if entry.is_relative() => {
                // `strbuf_realpath(&pathbuf, relative_base, 1)` is the dying
                // form; a base that will not resolve is used as written and
                // fails the normalization below like any other bad path.
                gix_path::realpath_opts(base, current_dir, MAX_SYMLINKS)
                    .unwrap_or_else(|_| base.to_owned())
                    .join(entry)
            }
            _ => entry.to_owned(),
        };
        // `strbuf_realpath(&buf, pathbuf.buf, 0)` plus the trailing-slash trim.
        let resolved = gix_path::realpath_opts(&joined, current_dir, MAX_SYMLINKS)
            .ok()
            // `strbuf_realpath_1()` (`abspath.c:128-137`) lstats each component
            // and errors out "unless this was the last component", so exactly one
            // missing trailing name is tolerated. `realpath_opts` resolves
            // symbolically and never stops for a missing component, so the
            // condition is restored here: it is the *parent* that has to exist.
            .filter(|resolved| resolved.parent().is_none_or(|p| p.exists()));
        let Some(resolved) = resolved else {
            diagnostics.push(format!(
                "unable to normalize alternate object path: {}",
                joined.display()
            ));
            continue;
        };
        out.push(resolved);
    }
    Ok(out)
}

/// `strchrnul(string, sep)`: the offset of the next `sep` at or after `from`, or
/// the end of the string.
fn strchrnul(input: &[u8], sep: u8, from: usize) -> usize {
    input[from..]
        .iter()
        .position(|b| *b == sep)
        .map_or(input.len(), |off| from + off)
}
