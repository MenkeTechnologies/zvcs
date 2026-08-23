//!
#![allow(clippy::empty_docs)]
use gix_ref::file::ReferenceExt;

use crate::{
    Reference,
    bstr::{BStr, BString, ByteSlice, ByteVec},
};

impl Reference<'_> {
    /// Return a platform for obtaining iterators over reference logs.
    pub fn log_iter(&self) -> gix_ref::file::log::iter::Platform<'_, '_> {
        self.inner.log_iter(&self.repo.refs)
    }

    /// Return true if a reflog is present for this reference.
    pub fn log_exists(&self) -> bool {
        self.inner.log_exists(&self.repo.refs)
    }
}

/// Generate a message typical for git commit logs based on the given `operation`, commit `message` and `num_parents` of the commit.
///
/// The message is cut the way `update_head_with_reflog()` (sequencer.c:1259-1295)
/// cuts it — `nl = strchr(msg->buf, '\n')` and everything up to and including that
/// newline, or the whole message plus one when it has none. That is the *first
/// line*, not the folded `%s` subject: a message whose subject runs across two
/// lines with no blank line between them shows as `line one line two` in
/// `git log --format=%s` and as `commit: line one` in the reflog. Using
/// `MessageRef::summary()` here put the whole folded paragraph in the reflog.
///
/// The result then goes through [`gix_ref::log::normalize_message`], git's
/// `copy_reflog_msg()` — every reflog message is normalized in
/// `ref_transaction_add_update()` (refs.c:1342) — which collapses the whitespace
/// runs and trims the ends, so the trailing newline added above disappears and a
/// message with no first line leaves the bare `commit:`.
pub fn message(operation: &str, message: &BStr, num_parents: usize) -> BString {
    let mut out = BString::from(operation);
    if let Some(commit_type) = commit_type_by_parents(num_parents) {
        out.push_str(b" (");
        out.extend_from_slice(commit_type.as_bytes());
        out.push_byte(b')');
    }
    out.push_str(b": ");
    match message.find_byte(b'\n') {
        Some(nl) => out.extend_from_slice(&message[..=nl]),
        None => {
            out.extend_from_slice(message);
            out.push_byte(b'\n');
        }
    }
    gix_ref::log::normalize_message(out.as_bstr())
}

pub(crate) fn commit_type_by_parents(count: usize) -> Option<&'static str> {
    Some(match count {
        0 => "initial",
        1 => return None,
        _two_or_more => "merge",
    })
}
