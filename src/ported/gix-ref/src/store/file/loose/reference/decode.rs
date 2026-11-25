use gix_hash::ObjectId;
use gix_object::bstr::BString;

use crate::{FullName, Target, parse::hex_hash, store_impl::file::loose::Reference};

enum MaybeUnsafeState {
    Id(ObjectId),
    UnvalidatedPath(BString),
}

/// The error returned by [`Reference::try_from_path()`].
#[derive(Debug, thiserror::Error)]
#[expect(missing_docs)]
pub enum Error {
    #[error("{content:?} could not be parsed")]
    Parse { content: BString },
    #[error("The path {path:?} to a symbolic reference within a ref file is invalid")]
    RefnameValidation {
        source: gix_validate::reference::name::Error,
        path: BString,
    },
}

impl TryFrom<MaybeUnsafeState> for Target {
    type Error = Error;

    fn try_from(v: MaybeUnsafeState) -> Result<Self, Self::Error> {
        Ok(match v {
            MaybeUnsafeState::Id(id) => Target::Object(id),
            MaybeUnsafeState::UnvalidatedPath(name) => {
                Target::Symbolic(match gix_validate::reference::name(name.as_ref()) {
                    Ok(_) => FullName(name),
                    Err(err) => {
                        return Err(Error::RefnameValidation {
                            source: err,
                            path: name,
                        });
                    }
                })
            }
        })
    }
}

impl Reference {
    /// Create a new reference named `name` from the loose reference file contents in `path_contents`,
    /// parsing object ids as `object_hash`.
    pub fn try_from_path(name: FullName, path_contents: &[u8], object_hash: gix_hash::Kind) -> Result<Self, Error> {
        Ok(Reference {
            name,
            target: parse(path_contents, object_hash)
                .map_err(|_| Error::Parse {
                    content: path_contents.into(),
                })?
                .try_into()?,
        })
    }
}

/// Parse the contents of a loose reference file.
///
/// A *symbolic* reference starts with `ref:`, may have whitespace before the
/// path, and returns [`MaybeUnsafeState::UnvalidatedPath`] with the path bytes up to the next NUL byte (just like Git), line ending, or the end of input. The path
/// is validated later when it is converted into a [`Target`].
///
/// A *direct* reference starts with a hexadecimal object id and returns
/// [`MaybeUnsafeState::Id`].
///
/// If neither reference form can be parsed, an error is returned.
fn parse(mut i: &[u8], object_hash: gix_hash::Kind) -> Result<MaybeUnsafeState, ()> {
    // `parse_loose_ref_contents()` (refs/files-backend.c:674-676): `ref:`, then
    // whatever git's `isspace()` skips (space, `\t`, `\n`, `\r`, ctype.c:20-22),
    // so `ref:refs/heads/main` and `ref:\trefs/heads/main` are symbolic too.
    if let Some(rest) = i.strip_prefix(b"ref:") {
        i = rest;
        while i.first().is_some_and(|b| matches!(b, b' ' | b'\t' | b'\n' | b'\r')) {
            i = &i[1..];
        }
        let path_end = i
            .iter()
            .position(|b| *b == b'\0' || *b == b'\r' || *b == b'\n')
            .unwrap_or(i.len());
        let path = i[..path_end].into();
        Ok(MaybeUnsafeState::UnvalidatedPath(path))
    } else {
        let hex = hex_hash(&mut i, object_hash)?;
        if i.first().is_some_and(u8::is_ascii_hexdigit) {
            return Err(());
        }
        Ok(MaybeUnsafeState::Id(ObjectId::from_hex(hex).expect("prior validation")))
    }
}
