pub(crate) mod function {
    use bstr::ByteSlice;
    use gix_error::ValidationError;

    use crate::{IdentityRef, SignatureRef};

    /// Parse a signature from the bytes input `i`, and change it to point to the unparsed bytes afterwards.
    pub fn decode<'a>(i: &mut &'a [u8]) -> Result<SignatureRef<'a>, ValidationError> {
        let identity = identity(i)?;
        if i.first() == Some(&b' ') {
            *i = &i[1..];
        }

        let time_len = i.iter().position(|b| !is_time_byte(*b)).unwrap_or(i.len());
        let (time, rest) = i.split_at(time_len);
        *i = rest;
        // SAFETY: The parser validated that there are only ASCII characters with `is_time_byte()`.
        #[expect(unsafe_code)]
        let time = unsafe { std::str::from_utf8_unchecked(time) };

        Ok(SignatureRef {
            name: identity.name,
            email: identity.email,
            time,
        })
    }

    /// Parse an identity from the bytes input `i` (like `name <email>`).
    pub fn identity<'a>(i: &mut &'a [u8]) -> Result<IdentityRef<'a>, ValidationError> {
        let eol_idx = i.find_byte(b'\n').unwrap_or(i.len());
        let right_delim_idx = i[..eol_idx]
            .rfind_byte(b'>')
            .ok_or_else(|| ValidationError::new("Closing '>' not found"))?;
        let i_name_and_email = &i[..right_delim_idx];
        let skip_from_right = i_name_and_email.iter().rev().take_while(|b| **b == b'>').count();
        let left_delim_idx = i_name_and_email
            .find_byte(b'<')
            .ok_or_else(|| ValidationError::new("Opening '<' not found"))?;
        let skip_from_left = i[left_delim_idx..].iter().take_while(|b| **b == b'<').count();
        let mut name = i[..left_delim_idx].as_bstr();
        name = name.strip_suffix(b" ").unwrap_or(name).as_bstr();

        let email = i
            .get(left_delim_idx + skip_from_left..right_delim_idx - skip_from_right)
            .ok_or_else(|| ValidationError::new("Skipped parts run into each other"))?
            .as_bstr();
        *i = i.get(right_delim_idx + 1..).unwrap_or(&[]);
        Ok(IdentityRef { name, email })
    }

    /// The bytes an ident's date field may hold, which has to be at least every
    /// byte `split_ident_line()` (ident.c:324-341) is willing to walk over: the
    /// digits and sign it reads, plus whatever its `isspace()` skips between
    /// them. C's `isspace()` is six bytes — space, `\t`, `\n`, `\v`, `\f`, `\r` —
    /// and the vertical tab is not an academic entry: git's own
    /// `t4212-log-corrupt.sh` writes a committer date of a lone `\v` on purpose,
    /// because `strtoumax()` treats it as whitespace while git's `isspace()`
    /// wrapper does not. Leaving it out here does not make that date invalid, it
    /// makes the whole commit object undecodable, so `git log` fails where git
    /// prints an empty date.
    ///
    /// `\n` stays out: it ends the ident line, and consuming it would run the
    /// date field into the next header.
    fn is_time_byte(b: u8) -> bool {
        matches!(b, b'+' | b'-' | b'0'..=b'9' | b' ' | b'\t' | 0x0b | 0x0c | b'\r')
    }
}
pub use function::identity;

#[cfg(test)]
mod tests {
    mod parse_signature {
        use gix_error::ValidationError;

        use crate::SignatureRef;

        fn decode(mut i: &[u8]) -> Result<(&[u8], SignatureRef<'_>), ValidationError> {
            SignatureRef::from_bytes_consuming(&mut i).map(|signature| (i, signature))
        }

        fn signature(name: &'static str, email: &'static str, time: &'static str) -> SignatureRef<'static> {
            SignatureRef {
                name: name.into(),
                email: email.into(),
                time,
            }
        }

        #[test]
        fn tz_minus() {
            let actual = decode(b"Sebastian Thiel <byronimo@gmail.com> 1528473343 -0230")
                .expect("parse to work")
                .1;
            assert_eq!(
                actual,
                signature("Sebastian Thiel", "byronimo@gmail.com", "1528473343 -0230")
            );
            assert_eq!(actual.seconds(), 1528473343);
            assert_eq!(
                actual.time().expect("valid"),
                gix_date::Time {
                    seconds: 1528473343,
                    offset: -9000,
                }
            );
        }

        #[test]
        fn tz_plus() {
            assert_eq!(
                decode(b"Sebastian Thiel <byronimo@gmail.com> 1528473343 +0230")
                    .expect("parse to work")
                    .1,
                signature("Sebastian Thiel", "byronimo@gmail.com", "1528473343 +0230")
            );
        }

        #[test]
        fn email_with_space() {
            assert_eq!(
                decode(b"Sebastian Thiel <\tbyronimo@gmail.com > 1528473343 +0230")
                    .expect("parse to work")
                    .1,
                signature("Sebastian Thiel", "\tbyronimo@gmail.com ", "1528473343 +0230")
            );
        }

        #[test]
        fn negative_offset_0000() {
            assert_eq!(
                decode(b"Sebastian Thiel <byronimo@gmail.com> 1528473343 -0000")
                    .expect("parse to work")
                    .1,
                signature("Sebastian Thiel", "byronimo@gmail.com", "1528473343 -0000")
            );
        }

        #[test]
        fn negative_offset_double_dash() {
            assert_eq!(
                decode(b"name <name@example.com> 1288373970 --700")
                    .expect("parse to work")
                    .1,
                signature("name", "name@example.com", "1288373970 --700")
            );
        }

        #[test]
        fn empty_name_and_email() {
            assert_eq!(
                decode(b" <> 12345 -1215").expect("parse to work").1,
                signature("", "", "12345 -1215")
            );
        }

        #[test]
        fn invalid_signature() {
            assert_eq!(
                decode(b"hello < 12345 -1215")
                    .expect_err("parse fails as > is missing")
                    .to_string(),
                "Closing '>' not found"
            );
        }

        #[test]
        fn invalid_time() {
            assert_eq!(
                decode(b"hello <> abc -1215").expect("parse to work").1,
                signature("hello", "", "")
            );
        }
    }
}
