use std::path::PathBuf;

use gix_hash::ObjectId;
use gix_object::bstr::{BStr, BString, ByteSlice};
use memmap2::Mmap;

use crate::{FullNameRef, Namespace, file, transaction::RefEdit};

#[derive(Debug)]
enum Backing {
    /// The buffer is loaded entirely in memory, along with the `offset` to the first record past the header.
    InMemory(Vec<u8>),
    /// The buffer is mapping the file on disk, along with the offset to the first record past the header
    Mapped(Mmap),
}

/// A buffer containing a packed-ref file that is either memory mapped or fully in-memory depending on a cutoff.
///
/// The buffer is guaranteed to be sorted as per the packed-ref rules which allows some operations to be more efficient.
#[derive(Debug)]
pub struct Buffer {
    data: Backing,
    /// The hash kind to expect when parsing packed references.
    object_hash: gix_hash::Kind,
    /// The offset to the first record, how many bytes to skip past the header
    offset: usize,
    /// The path from which we were loaded
    path: PathBuf,
}

struct Edit {
    inner: RefEdit,
    peeled: Option<ObjectId>,
}

/// A transaction for editing packed references
pub(crate) struct Transaction {
    buffer: Option<file::packed::SharedBufferSnapshot>,
    edits: Option<Vec<Edit>>,
    lock: Option<gix_lock::File>,
    // It just has to be kept alive, hence no reads
    closed_lock: Option<gix_lock::Marker>,
    precompose_unicode: bool,
    /// The namespace to use when preparing or writing refs
    namespace: Option<Namespace>,
}

/// A reference as parsed from the `packed-refs` file
#[derive(Debug, PartialEq, Eq)]
pub struct Reference<'a> {
    /// The validated full name of the reference.
    pub name: &'a FullNameRef,
    /// The target object id of the reference, hex encoded.
    pub target: &'a BStr,
    /// The fully peeled object id, hex encoded, that the ref is ultimately pointing to
    /// i.e. when all indirections are removed.
    pub object: Option<&'a BStr>,
}

impl Reference<'_> {
    /// Decode the target as object
    pub fn target(&self) -> ObjectId {
        gix_hash::ObjectId::from_hex(self.target).expect("parser validation")
    }

    /// Decode the object this reference is ultimately pointing to. Note that this is
    /// the [`target()`][Reference::target()] if this is not a fully peeled reference like a tag.
    pub fn object(&self) -> ObjectId {
        self.object.map_or_else(
            || self.target(),
            |id| ObjectId::from_hex(id).expect("parser validation"),
        )
    }
}

/// An iterator over references in a packed refs file
pub struct Iter<'a> {
    /// The position at which to parse the next reference
    cursor: &'a [u8],
    /// The hash kind to expect when parsing packed references.
    object_hash: gix_hash::Kind,
    /// The next line, starting at 1
    current_line: usize,
    /// If set, references returned will match the prefix, the first failed match will stop all iteration.
    prefix: Option<BString>,
    /// The `packed-refs` file the buffer came from, to name in the diagnostic for a record that will not parse.
    path: PathBuf,
}

mod decode;

///
pub mod iter;

///
pub mod buffer;

///
pub mod find;

///
pub mod transaction;

/// A `packed-refs` record git refuses to parse, carrying everything git's
/// `die_invalid_line()` needs to name it.
///
/// git checks a record's shape in four places — `sort_snapshot()`
/// (refs/packed-backend.c:349-351), `verify_buffer_safe()` (:460-463), the
/// lookup in `packed_read_raw_ref()` (:747-748) and the iterator in
/// `next_record()` (:802-805, :832-836) — and all four end in
/// `die_invalid_line()` (:257-268), which prints the offending record and exits
/// 128. A record with no line ending at all gets a different sentence from
/// `die_unterminated_line()` (:248-255). Both truncate to 75 bytes plus `...`
/// once the line reaches 80. Line numbers are git v2.39.0-rc2, whose
/// `packed-backend.c` is unchanged here through v2.55.0 as measured against the
/// installed binary.
///
/// This type only builds the *message*; it is not itself a `die()`. The caller
/// decides whether git dies here, which is the difference between the lookup
/// path (dies) and a caller that never reads this record at all.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvalidLine {
    /// git's `refs->path`: the `packed-refs` file the record came from.
    pub path: PathBuf,
    /// The offending record without its line ending, truncated to the 80 bytes
    /// that are all git looks at to decide how to print it.
    pub line: BString,
    /// Whether the record ended in a newline. git has a different sentence for
    /// one that does not.
    pub terminated: bool,
}

impl InvalidLine {
    /// Build the diagnostic for the record starting at the front of `rest`,
    /// which runs to the end of the `packed-refs` buffer just as git's
    /// `p, eof - p` pair does.
    pub(crate) fn at_record(path: PathBuf, rest: &[u8]) -> Self {
        let path = without_cur_dir(path);
        let (line, terminated) = match rest.find_byte(b'\n') {
            Some(eol) => (&rest[..eol], true),
            None => (rest, false),
        };
        InvalidLine {
            path,
            line: line[..line.len().min(80)].into(),
            terminated,
        }
    }

    /// The record git would have died on, if `err` reports one.
    ///
    /// Four different errors in this crate carry it — the eager check at
    /// [`Buffer::open()`][crate::packed::Buffer::open()], the lookup, the iterator, and the
    /// iterator seen through [`LooseThenPacked`][crate::file::iter::LooseThenPacked] — because
    /// git reaches `die_invalid_line()` from four different places. A caller that wants to die
    /// the way git dies walks its error chain with this and does not have to know which one it
    /// was.
    pub fn in_error<'a>(err: &'a (dyn std::error::Error + 'static)) -> Option<&'a Self> {
        use crate::{file::iter::loose_then_packed, packed};
        if let Some(packed::buffer::open::Error::InvalidLine(line)) = err.downcast_ref() {
            return Some(line);
        }
        if let Some(packed::find::Error::Parse { line }) = err.downcast_ref() {
            return Some(line);
        }
        if let Some(packed::iter::Error::Reference { line, .. }) = err.downcast_ref() {
            return Some(line);
        }
        if let Some(loose_then_packed::Error::PackedReference { line, .. }) = err.downcast_ref() {
            return Some(line);
        }
        None
    }

    /// The same, for a record located by `offset` into the whole `buffer`.
    pub(crate) fn at_offset(path: PathBuf, buffer: &[u8], offset: usize) -> Self {
        Self::at_record(path, decode::record_at_offset(buffer, offset))
    }
}

/// Drop the `.` components a path picks up when the repository was opened by a
/// relative path, so `./.git/packed-refs` reads as `.git/packed-refs`.
///
/// git has nothing to drop: `setup_git_directory()` leaves `$GIT_DIR` as the
/// plain `.git` it found after chdir'ing to the top of the work tree, and
/// `refs->path` is built from that, so its message says `.git/packed-refs`. The
/// path is otherwise left exactly as it came — an absolute git dir prints
/// absolute in git too.
fn without_cur_dir(path: PathBuf) -> PathBuf {
    use std::path::Component;
    if !path.components().any(|c| c == Component::CurDir) {
        return path;
    }
    let stripped: PathBuf = path.components().filter(|c| *c != Component::CurDir).collect();
    if stripped.as_os_str().is_empty() { path } else { stripped }
}

impl std::fmt::Display for InvalidLine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let what = if self.terminated { "unexpected" } else { "unterminated" };
        let path = self.path.display();
        if self.line.len() < 80 {
            write!(f, "{what} line in {path}: {}", self.line)
        } else {
            write!(f, "{what} line in {path}: {}...", self.line[..75].as_bstr())
        }
    }
}

#[cfg(test)]
mod invalid_line_tests {
    use super::InvalidLine;

    fn rendered(rest: &[u8]) -> String {
        InvalidLine::at_record(std::path::PathBuf::from(".git/packed-refs"), rest).to_string()
    }

    /// `die_invalid_line()` prints the record up to its newline
    /// (refs/packed-backend.c:265, git v2.39.0-rc2).
    #[test]
    fn a_terminated_record_is_named_up_to_its_newline() {
        assert_eq!(
            rendered(b"not a ref line\n0000 refs/heads/next\n"),
            "unexpected line in .git/packed-refs: not a ref line"
        );
    }

    /// No newline anywhere in what is left means `die_unterminated_line()`
    /// (refs/packed-backend.c:260-261), a different sentence.
    #[test]
    fn a_record_without_a_newline_is_unterminated_instead() {
        assert_eq!(
            rendered(b"11111111"),
            "unterminated line in .git/packed-refs: 11111111"
        );
    }

    /// `eol - p < 80` keeps the whole line; 80 and up truncate to `%.75s...`
    /// (refs/packed-backend.c:264-267). Both sentences share the rule
    /// (:251-254), so both boundaries are worth pinning.
    #[test]
    fn eighty_bytes_is_where_truncation_starts() {
        let line = |n: usize| "x".repeat(n);
        assert_eq!(
            rendered(format!("{}\n", line(79)).as_bytes()),
            format!("unexpected line in .git/packed-refs: {}", line(79)),
            "79 bytes still print in full"
        );
        assert_eq!(
            rendered(format!("{}\n", line(80)).as_bytes()),
            format!("unexpected line in .git/packed-refs: {}...", line(75)),
            "80 bytes is the first length git abbreviates"
        );
        assert_eq!(
            rendered(line(200).as_bytes()),
            format!("unterminated line in .git/packed-refs: {}...", line(75)),
            "an unterminated line abbreviates by the same rule"
        );
    }

    /// git's `$GIT_DIR` is the plain `.git` it was set up with, so its message
    /// never carries a `./` this port's relative repository path can pick up.
    #[test]
    fn a_leading_dot_component_is_not_part_of_the_path_git_prints() {
        assert_eq!(
            InvalidLine::at_record(std::path::PathBuf::from("./.git/packed-refs"), b"bogus\n")
                .to_string(),
            "unexpected line in .git/packed-refs: bogus"
        );
    }
}
