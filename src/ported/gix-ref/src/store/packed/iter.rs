use std::path::PathBuf;

use gix_object::bstr::{BString, ByteSlice};

use crate::store_impl::{packed, packed::decode};

/// packed-refs specific functionality
impl packed::Buffer {
    /// Return an iterator of references stored in this packed refs buffer, ordered by reference name.
    ///
    /// # Note
    ///
    /// There is no namespace support in packed iterators. It can be emulated using `iter_prefixed(…)`.
    pub fn iter(&self) -> Result<packed::Iter<'_>, packed::iter::Error> {
        packed::Iter::new_at(self.as_ref(), self.object_hash, self.path.clone())
    }

    /// Return an iterator yielding only references matching the given prefix, ordered by reference name.
    pub fn iter_prefixed(&self, prefix: BString) -> Result<packed::Iter<'_>, packed::iter::Error> {
        let first_record_with_prefix = self.binary_search_by(prefix.as_bstr()).unwrap_or_else(|(_, pos)| pos);
        packed::Iter::new_with_prefix(
            &self.as_ref()[first_record_with_prefix..],
            self.object_hash,
            Some(prefix),
            self.path.clone(),
        )
    }
}

impl<'a> Iterator for packed::Iter<'a> {
    type Item = Result<packed::Reference<'a>, Error>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.cursor.is_empty() {
            return None;
        }

        let start = self.cursor;
        match decode::reference(&mut self.cursor, self.object_hash) {
            Ok(reference) => {
                self.current_line += 1;
                if let Some(ref prefix) = self.prefix {
                    if !reference.name.as_bstr().starts_with_str(prefix) {
                        self.cursor = &[];
                        return None;
                    }
                }
                Some(Ok(reference))
            }
            Err(_) => {
                self.cursor = start;
                let (failed_line, after_first) = split_line(self.cursor);

                // git checks the reference line and its peeled line in two separate
                // places — `next_record()` at refs/packed-backend.c:802-805 and again
                // at :832-836 (v2.39.0-rc2) — so a record whose `<hash> <name>` line is
                // fine but whose `^<hash>` line is not names the peeled line. Decoding
                // the record as a whole, as this parser does, cannot tell the two apart
                // on its own, so re-read the first line alone to find out which it was.
                let mut first = failed_line;
                let first_line_parses =
                    decode::reference(&mut first, self.object_hash).is_ok() && first.is_empty();
                let (bad_line, next_cursor, line_offset) =
                    if first_line_parses && after_first.first() == Some(&b'^') {
                        let (peeled, rest) = split_line(after_first);
                        (peeled, rest, 1)
                    } else {
                        (failed_line, after_first, 0)
                    };

                self.cursor = next_cursor;
                let line_number = self.current_line + line_offset;
                self.current_line = line_number + 1;

                Some(Err(Error::Reference {
                    // git hands `die_invalid_line()` the record start and the
                    // rest of the buffer and lets it find the line ending, which
                    // is what tells an unexpected line from an unterminated one.
                    line: packed::InvalidLine::at_record(self.path.clone(), bad_line),
                    line_number,
                }))
            }
        }
    }
}

/// Split `input` after its first line ending, or into all of it and nothing when
/// there is none.
fn split_line(input: &[u8]) -> (&[u8], &[u8]) {
    input
        .find_byte(b'\n')
        .map_or((input, &[][..]), |pos| input.split_at(pos + 1))
}

impl<'a> packed::Iter<'a> {
    /// Return a new iterator after successfully parsing the possibly existing first line of the given `packed` refs buffer,
    /// parsing object ids as `object_hash`.
    pub fn new(packed: &'a [u8], object_hash: gix_hash::Kind) -> Result<Self, Error> {
        Self::new_at(packed, object_hash, PathBuf::new())
    }

    /// Like [`new()`][Self::new()], but naming the `packed-refs` file the buffer came from so a
    /// record that will not parse can be reported the way git's `die_invalid_line()` reports it.
    pub fn new_at(packed: &'a [u8], object_hash: gix_hash::Kind, path: PathBuf) -> Result<Self, Error> {
        Self::new_with_prefix(packed, object_hash, None, path)
    }

    /// Returns an iterator whose references will only match `prefix`.
    ///
    /// It assumes that the underlying `packed` buffer is indeed sorted and parses object ids as `object_hash`.
    pub(in crate::store_impl::packed) fn new_with_prefix(
        packed: &'a [u8],
        object_hash: gix_hash::Kind,
        prefix: Option<BString>,
        path: PathBuf,
    ) -> Result<Self, Error> {
        if packed.is_empty() {
            Ok(packed::Iter {
                cursor: packed,
                object_hash,
                prefix,
                current_line: 1,
                path,
            })
        } else if packed[0] == b'#' {
            let mut input = packed;
            decode::header(&mut input).map_err(|_| Error::Header {
                invalid_first_line: packed.lines().next().unwrap_or(packed).into(),
            })?;
            let refs = input;
            Ok(packed::Iter {
                cursor: refs,
                object_hash,
                prefix,
                current_line: 2,
                path,
            })
        } else {
            Ok(packed::Iter {
                cursor: packed,
                object_hash,
                prefix,
                current_line: 1,
                path,
            })
        }
    }
}

mod error {
    use gix_object::bstr::BString;

    use crate::store_impl::packed;

    /// The error returned by [`Iter`][super::packed::Iter],
    #[derive(Debug, thiserror::Error)]
    #[expect(missing_docs)]
    pub enum Error {
        #[error("The header existed but could not be parsed: {invalid_first_line:?}")]
        Header { invalid_first_line: BString },
        /// A record that will not parse. git dies here — `next_record()` reaches
        /// `die_invalid_line()` (refs/packed-backend.c:802-805 and :832-836,
        /// v2.39.0-rc2) — so `line` carries everything *that* message needs as
        /// well, for a caller that has decided to die the way git does. This
        /// crate's own wording stays this crate's.
        #[error("Invalid reference in line {line_number}: {:?}", line.line)]
        Reference {
            line: packed::InvalidLine,
            line_number: usize,
        },
    }
}

pub use error::Error;
