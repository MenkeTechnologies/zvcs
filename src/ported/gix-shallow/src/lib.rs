//! [Read](read()) and [write](write()) shallow files, while performing typical operations on them.
//!
//! ## Examples
//!
//! ```
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let first = gix_hash::ObjectId::from_hex(b"1111111111111111111111111111111111111111")?;
//! let second = gix_hash::ObjectId::from_hex(b"2222222222222222222222222222222222222222")?;
//! # let dir = tempfile::tempdir()?;
//! # let shallow_file = dir.path().join("shallow");
//! # std::fs::write(&shallow_file, format!("{first}\n"))?;
//!
//! let shallow = gix_shallow::read(&shallow_file)?.expect("a shallow boundary");
//! let lock = gix_lock::File::acquire_to_update_resource(
//!     &shallow_file,
//!     gix_lock::acquire::Fail::Immediately,
//!     None,
//! )?;
//! gix_shallow::write(lock, Some(shallow), &[gix_shallow::Update::Shallow(second)])?;
//!
//! let ids = gix_shallow::read(&shallow_file)?.unwrap().into_iter().collect::<Vec<_>>();
//! assert_eq!(ids, vec![first, second]);
//! # Ok(()) }
//! ```
#![deny(missing_docs)]
#![forbid(unsafe_code)]

/// An instruction on how to
#[derive(PartialEq, Eq, Debug, Hash, Ord, PartialOrd, Clone, Copy)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum Update {
    /// Shallow the given `id`.
    Shallow(gix_hash::ObjectId),
    /// Don't shallow the given `id` anymore.
    Unshallow(gix_hash::ObjectId),
}

/// Return a list of shallow commits as unconditionally read from `shallow_file`.
///
/// The list of shallow commits represents the shallow boundary, beyond which we are lacking all (parent) commits.
/// Note that the list is never empty, as `Ok(None)` is returned in that case indicating the repository
/// isn't a shallow clone.
pub fn read(shallow_file: &std::path::Path) -> Result<Option<nonempty::NonEmpty<gix_hash::ObjectId>>, read::Error> {
    use bstr::ByteSlice;
    let buf = match std::fs::read(shallow_file) {
        Ok(buf) => buf,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(err.into()),
    };

    let mut commits = buf
        .lines()
        .map(|line| gix_hash::ObjectId::from_hex(hex_prefix(line)))
        .collect::<Result<Vec<_>, _>>()?;

    commits.sort();
    Ok(nonempty::NonEmpty::from_vec(commits))
}

/// The leading run of hex digits in one `shallow` line.
///
/// `is_repository_shallow()` (`shallow.c:87-92`) reads each line with
/// `fgets` — so the buffer still carries its newline — and decodes it with
/// `get_oid_hex()`:
///
/// ```c
/// while (fgets(buf, sizeof(buf), fp)) {
///         struct object_id oid;
///         if (get_oid_hex(buf, &oid))
///                 die("bad shallow line: %s", buf);
///         register_shallow(r, &oid);
/// }
/// ```
///
/// `get_oid_hex()` consumes exactly `the_hash_algo->hexsz` characters and never
/// looks at what follows, which is what lets a line keep its newline — and any
/// other trailing bytes. Measured against git 2.55.0, a boundary written with a
/// trailing space still grafts:
///
/// ```text
/// $ printf '%s \n' "$boundary" > .git/shallow
/// $ git log --oneline
/// 4a3fb3cba9 c5
/// 46d54d6a68 c4
/// ```
///
/// Decoding the whole line instead rejects it, and the repository silently stops
/// being shallow — which then fails on the first parent it does not have.
///
/// Trimming at the first non-hex byte reproduces that for every suffix git's
/// leniency actually covers. It differs only for a suffix that is *itself* hex
/// and carries the line past the hash length, which no writer of this file
/// produces.
fn hex_prefix(line: &[u8]) -> &[u8] {
    let end = line
        .iter()
        .position(|b| !b.is_ascii_hexdigit())
        .unwrap_or(line.len());
    &line[..end]
}

///
pub mod write {
    pub(crate) mod function {
        use std::io::Write;

        use super::Error;
        use crate::Update;

        /// Write the [previously obtained](crate::read()) (possibly non-existing) `shallow_commits` to the shallow `file`
        /// after applying all `updates`.
        ///
        /// If this leaves the list of shallow commits empty, the file is removed.
        ///
        /// ### Deviation
        ///
        /// Git also prunes the set of shallow commits while writing, we don't until we support some sort of pruning.
        pub fn write(
            mut file: gix_lock::File,
            shallow_commits: Option<nonempty::NonEmpty<gix_hash::ObjectId>>,
            updates: &[Update],
        ) -> Result<(), Error> {
            let mut shallow_commits = shallow_commits.map(Vec::from).unwrap_or_default();
            for update in updates {
                match update {
                    Update::Shallow(id) => {
                        shallow_commits.push(*id);
                    }
                    Update::Unshallow(id) => shallow_commits.retain(|oid| oid != id),
                }
            }
            if shallow_commits.is_empty() {
                if let Err(err) = std::fs::remove_file(file.resource_path()) {
                    if err.kind() != std::io::ErrorKind::NotFound {
                        return Err(err.into());
                    }
                }
                drop(file);
                return Ok(());
            }
            shallow_commits.sort();
            let mut buf = Vec::<u8>::new();
            for commit in shallow_commits {
                commit.write_hex_to(&mut buf).map_err(Error::Io)?;
                buf.push(b'\n');
            }
            file.write_all(&buf).map_err(Error::Io)?;
            file.flush().map_err(Error::Io)?;
            file.commit()?;
            Ok(())
        }
    }

    /// The error returned by [`write()`](crate::write()).
    #[derive(Debug, thiserror::Error)]
    #[expect(missing_docs)]
    pub enum Error {
        #[error(transparent)]
        Commit(#[from] gix_lock::commit::Error<gix_lock::File>),
        #[error("Could not remove an empty shallow file")]
        RemoveEmpty(#[from] std::io::Error),
        #[error("Failed to write object id to shallow file")]
        Io(std::io::Error),
    }
}
pub use write::function::write;

///
pub mod read {
    /// The error returned by [`read`](crate::read()).
    #[derive(Debug, thiserror::Error)]
    #[expect(missing_docs)]
    pub enum Error {
        #[error("Could not open shallow file for reading")]
        Io(#[from] std::io::Error),
        #[error("Could not decode a line in shallow file as hex-encoded object hash")]
        DecodeHash(#[from] gix_hash::decode::Error),
    }
}
