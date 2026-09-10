/// The error returned by [`encode_to_worktree()][super::encode_to_worktree()].
#[derive(Debug, thiserror::Error)]
#[expect(missing_docs)]
pub enum Error {
    #[error("Cannot convert input of {input_len} UTF-8 bytes to target encoding without overflowing")]
    Overflow { input_len: usize },
    #[error("Input was not UTF-8 encoded")]
    InputAsUtf8(#[from] std::str::Utf8Error),
    #[error("The character '{character}' could not be mapped to the {worktree_encoding}")]
    Unmappable {
        character: char,
        worktree_encoding: &'static str,
    },
}

pub(crate) mod function {
    use bstr::BStr;
    use encoding_rs::EncoderResult;

    use super::Error;
    use crate::worktree::utf;

    /// Encode `src_utf8`, which is assumed to be UTF-8 encoded, according to `worktree_encoding` for placement in the working directory,
    /// and write it to `buf`, possibly resizing it.
    /// Note that the encoding is always applied, there is no conditional even if `worktree_encoding` and the `src` encoding are the same.
    pub fn encode_to_worktree(
        src_utf8: &[u8],
        worktree_encoding: &'static encoding_rs::Encoding,
        buf: &mut Vec<u8>,
    ) -> Result<(), Error> {
        let mut encoder = worktree_encoding.new_encoder();
        let buf_len = encoder
            .max_buffer_length_from_utf8_if_no_unmappables(src_utf8.len())
            .ok_or(Error::Overflow {
                input_len: src_utf8.len(),
            })?;
        buf.clear();
        buf.resize(buf_len, 0);
        let src = std::str::from_utf8(src_utf8)?;
        let (res, read, written) = encoder.encode_from_utf8_without_replacement(src, buf, true);
        match res {
            EncoderResult::InputEmpty => {
                assert!(
                    buf_len >= written,
                    "encoding_rs estimates the maximum amount of bytes written correctly"
                );
                assert_eq!(read, src_utf8.len(), "input buffer should be fully consumed");
                buf.truncate(written);
            }
            EncoderResult::OutputFull => {
                unreachable!("we assure that the output buffer is big enough as per the encoder's estimate")
            }
            EncoderResult::Unmappable(c) => {
                return Err(Error::Unmappable {
                    worktree_encoding: worktree_encoding.name(),
                    character: c,
                });
            }
        }
        Ok(())
    }

    /// Port of `encode_to_worktree()` — `convert.c:478-501`, git v2.55.0.
    ///
    /// Encode `src_utf8` for placement at `rela_path` in the working tree under the
    /// `working-tree-encoding` named by `worktree_encoding`, writing the result to `buf`. Return
    /// `true` when `buf` holds the converted content, and `false` when the content was left as it
    /// stands — which is what git answers for an empty input and, importantly, for an encoding it
    /// could not apply.
    ///
    /// That second case is a diagnostic and not a failure: `reencode_string_len()` answering null
    /// reaches `error()`, which prints and returns, and the `return 0` on the next line hands the
    /// unmodified content to the rest of the pipeline (`convert.c:493-497`). Measured on git 2.55.0,
    /// `cat-file --filters` with `working-tree-encoding=NOSUCHENC` prints
    /// `error: failed to encode 'f.txt' from UTF-8 to NOSUCHENC`, emits the stored bytes and exits
    /// 0. A port that fails here instead loses the file.
    pub fn encode_to_worktree_by_name(
        rela_path: &BStr,
        src_utf8: &[u8],
        worktree_encoding: &BStr,
        buf: &mut Vec<u8>,
    ) -> bool {
        // `if (!enc || (src && !src_len)) return 0;` — convert.c:488.
        if src_utf8.is_empty() {
            return false;
        }
        if let Some(utf) = utf::to_worktree(worktree_encoding) {
            // `iconv` refuses input that is not valid in the source encoding, and the source
            // encoding here is always UTF-8.
            let Ok(src) = std::str::from_utf8(src_utf8) else {
                return failed(rela_path, worktree_encoding);
            };
            utf::encode(src, utf, buf);
            return true;
        }
        let Some(encoding) = crate::worktree::encoding::for_label(worktree_encoding).ok() else {
            return failed(rela_path, worktree_encoding);
        };
        match encode_to_worktree(src_utf8, encoding, buf) {
            Ok(()) => true,
            Err(_) => failed(rela_path, worktree_encoding),
        }
    }

    /// git's `error()` arm, whose message names the encodings in the order the conversion reads
    /// them: out of UTF-8, into the working-tree encoding — `convert.c:494-496`.
    fn failed(rela_path: &BStr, worktree_encoding: &BStr) -> bool {
        eprintln!("error: failed to encode '{rela_path}' from UTF-8 to {worktree_encoding}");
        false
    }
}
