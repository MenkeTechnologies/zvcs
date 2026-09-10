/// Whether or not to perform round-trip checks.
#[derive(Debug, Copy, Clone)]
pub enum RoundTripCheck {
    /// Assure that we can losslessly convert the UTF-8 result back to the original encoding or fail with an error.
    Fail,
    /// Do not check if the encoding is round-trippable.
    Skip,
}

/// The error returned by [`encode_to_git()][super::encode_to_git()].
#[derive(Debug, thiserror::Error)]
#[expect(missing_docs)]
pub enum Error {
    #[error("Cannot convert input of {input_len} bytes to UTF-8 without overflowing")]
    Overflow { input_len: usize },
    #[error("The input was malformed and could not be decoded as '{encoding}'")]
    Malformed { encoding: &'static str },
    #[error("Encoding from '{src_encoding}' to '{dest_encoding}' and back is not the same")]
    RoundTrip {
        src_encoding: &'static str,
        dest_encoding: &'static str,
    },
    /// git's message for a `reencode_string_len()` that answered null — `convert.c:423`. It covers
    /// both an encoding the platform does not have and input that is not valid in it, as `iconv`
    /// reports the two the same way.
    #[error("failed to encode '{rela_path}' from {encoding} to UTF-8")]
    Unavailable {
        rela_path: bstr::BString,
        encoding: bstr::BString,
    },
    /// `validate_encoding()`'s first arm — `convert.c:282-283`.
    #[error("BOM is prohibited in '{rela_path}' if encoded as {encoding}")]
    ProhibitedBom {
        rela_path: bstr::BString,
        encoding: bstr::BString,
    },
    /// `validate_encoding()`'s second arm — `convert.c:302-303`.
    #[error("BOM is required in '{rela_path}' if encoded as {encoding}")]
    MissingBom {
        rela_path: bstr::BString,
        encoding: bstr::BString,
    },
}

pub(crate) mod function {
    use bstr::{BStr, ByteSlice};
    use encoding_rs::DecoderResult;

    use super::{Error, RoundTripCheck};
    use crate::worktree::utf;

    /// Decode `src` according to `src_encoding` to `UTF-8` for storage in git and place it in `buf`.
    /// Note that the encoding is always applied, there is no conditional even if `src_encoding` already is `UTF-8`.
    pub fn encode_to_git(
        src: &[u8],
        src_encoding: &'static encoding_rs::Encoding,
        buf: &mut Vec<u8>,
        round_trip: RoundTripCheck,
    ) -> Result<(), Error> {
        let mut decoder = src_encoding.new_decoder_with_bom_removal();
        let buf_len = decoder
            .max_utf8_buffer_length_without_replacement(src.len())
            .ok_or(Error::Overflow { input_len: src.len() })?;
        buf.clear();
        buf.resize(buf_len, 0);
        let (res, read, written) = decoder.decode_to_utf8_without_replacement(src, buf, true);
        match res {
            DecoderResult::InputEmpty => {
                assert!(
                    buf_len >= written,
                    "encoding_rs estimates the maximum amount of bytes written correctly"
                );
                assert_eq!(read, src.len(), "input buffer should be fully consumed");
                buf.truncate(written);
            }
            DecoderResult::OutputFull => {
                unreachable!("we assure that the output buffer is big enough as per the encoder's estimate")
            }
            DecoderResult::Malformed(_, _) => {
                return Err(Error::Malformed {
                    encoding: src_encoding.name(),
                });
            }
        }

        match round_trip {
            RoundTripCheck::Fail => {
                // SAFETY: we trust `encoding_rs` to output valid UTF-8 only if we ask it to.
                #[expect(unsafe_code)]
                let str = unsafe { std::str::from_utf8_unchecked(buf) };
                let (should_equal_src, _actual_encoding, _had_errors) = src_encoding.encode(str);
                if should_equal_src != src {
                    return Err(Error::RoundTrip {
                        src_encoding: src_encoding.name(),
                        dest_encoding: "UTF-8",
                    });
                }
            }
            RoundTripCheck::Skip => {}
        }
        Ok(())
    }

    /// Port of `encode_to_git()` — `convert.c:387-476`, git v2.55.0.
    ///
    /// Decode `src`, which came from `rela_path` in the working tree under the
    /// `working-tree-encoding` named by `src_encoding`, into the UTF-8 git stores, writing it to
    /// `buf`. Return `true` when `buf` holds the converted content and `false` when the content was
    /// left as it stands, which git answers for an empty input (`convert.c:398`).
    ///
    /// ### Deviation
    ///
    /// git decides between `die()` and `error()`-and-carry-on by whether `CONV_WRITE_OBJECT` is set,
    /// which is a property of the caller and not of the pipeline. Every arm here is an error, which
    /// is the `CONV_WRITE_OBJECT` behaviour — the one `add` and `hash-object` ask for.
    pub fn encode_to_git_by_name(
        rela_path: &BStr,
        src: &[u8],
        src_encoding: &BStr,
        buf: &mut Vec<u8>,
        round_trip: RoundTripCheck,
    ) -> Result<bool, Error> {
        // `if (!enc || (src && !src_len)) return 0;` — convert.c:398.
        if src.is_empty() {
            return Ok(false);
        }
        validate_encoding(rela_path, src_encoding, src)?;

        let unavailable = || Error::Unavailable {
            rela_path: rela_path.to_owned(),
            encoding: src_encoding.to_owned(),
        };
        if let Some(utf) = utf::to_git(src_encoding) {
            utf::decode(src, utf, buf).map_err(|_| unavailable())?;
            if let RoundTripCheck::Fail = round_trip {
                let decoded = std::str::from_utf8(buf).expect("decoding writes UTF-8 by construction");
                let mut re_src = Vec::with_capacity(src.len());
                utf::encode(decoded, utf, &mut re_src);
                if re_src != src {
                    return Err(Error::RoundTrip {
                        src_encoding: "UTF-16/UTF-32",
                        dest_encoding: "UTF-8",
                    });
                }
            }
            return Ok(true);
        }

        let encoding = crate::worktree::encoding::for_label(src_encoding).map_err(|_| unavailable())?;
        encode_to_git(src, encoding, buf, round_trip)?;
        Ok(true)
    }

    /// Port of `validate_encoding()` — `convert.c:269-319`.
    ///
    /// Only a name that starts with `UTF` is examined, "as UTF?? can be an alias for UTF-??"
    /// (`convert.c:274`). The advice git prints alongside spells the encoding the file should have
    /// been given instead: for a byte-order-fixed name it drops the last two characters to name the
    /// marked form, and for a bare one it offers both fixed forms.
    fn validate_encoding(rela_path: &BStr, enc: &BStr, data: &[u8]) -> Result<(), Error> {
        let Some(stripped) = utf::strip_utf(enc) else {
            return Ok(());
        };
        let stripped = stripped.to_str_lossy();
        if utf::has_prohibited_utf_bom(enc, data) {
            let marked = &stripped[..stripped.len().saturating_sub("BE".len())];
            eprintln!(
                "hint: The file '{rela_path}' contains a byte order mark (BOM). \
                 Please use UTF-{marked} as working-tree-encoding."
            );
            return Err(Error::ProhibitedBom {
                rela_path: rela_path.to_owned(),
                encoding: enc.to_owned(),
            });
        }
        if utf::is_missing_required_utf_bom(enc, data) {
            eprintln!(
                "hint: The file '{rela_path}' is missing a byte order mark (BOM). \
                 Please use UTF-{stripped}BE or UTF-{stripped}LE (depending on the byte order) \
                 as working-tree-encoding."
            );
            return Err(Error::MissingBom {
                rela_path: rela_path.to_owned(),
                encoding: enc.to_owned(),
            });
        }
        Ok(())
    }
}
