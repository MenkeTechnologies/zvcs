use bstr::BStr;

use crate::protocol::context::Error;

mod write {
    use bstr::{BStr, BString};

    use crate::protocol::{Context, ContextOptions, context::serde::validate};

    impl Context {
        /// Write ourselves to `out` such that [`from_bytes()`][Self::from_bytes()] can decode it losslessly.
        pub fn write_to(&self, mut out: impl std::io::Write) -> std::io::Result<()> {
            use bstr::ByteSlice;
            fn write_key(out: &mut impl std::io::Write, key: &str, value: &BStr) -> std::io::Result<()> {
                out.write_all(key.as_bytes())?;
                out.write_all(b"=")?;
                out.write_all(value)?;
                out.write_all(b"\n")
            }
            let Context {
                options: ContextOptions { protect_protocol },
                protocol,
                host,
                path,
                username,
                password,
                oauth_refresh_token,
                password_expiry_utc,
                url,
                // We only decode quit and interpret it, but won't get to pass it on as it means to stop the
                // credential helper invocation chain.
                quit: _,
            } = self;
            // `url` has no counterpart in `credential_write()` — git splits a `url=`
            // it *reads* into the fields below and never writes one back — so it
            // stays first, where a helper reading it the way `credential_read()`
            // does (`credential_from_url()`, which resets every field) still ends up
            // overwritten by the explicit fields that follow.
            for (key, value) in [("url", url)] {
                if let Some(value) = value {
                    validate(key, value.as_slice().into(), *protect_protocol).map_err(std::io::Error::other)?;
                    write_key(&mut out, key, value.as_ref()).ok();
                }
            }
            // `credential_write()` (credential.c:423-428) writes the fields in this
            // exact order, `path` between `host` and `username`:
            //
            //     credential_write_item(c, fp, "protocol", c->protocol, 1);
            //     credential_write_item(c, fp, "host", c->host, 1);
            //     credential_write_item(c, fp, "path", c->path, 0);
            //     credential_write_item(c, fp, "username", c->username, 0);
            //     credential_write_item(c, fp, "password", c->password, 0);
            //     credential_write_item(c, fp, "oauth_refresh_token", …, 0);
            //
            // A helper that logs its input verbatim — the shape `credential.helper`
            // scripts take — reports the difference, so the order is observable.
            for (key, value) in [("protocol", protocol), ("host", host)] {
                if let Some(value) = value {
                    validate(key, value.as_str().into(), *protect_protocol).map_err(std::io::Error::other)?;
                    write_key(&mut out, key, value.as_bytes().as_bstr()).ok();
                }
            }
            if let Some(value) = path {
                validate("path", value.as_slice().into(), *protect_protocol).map_err(std::io::Error::other)?;
                write_key(&mut out, "path", value.as_ref()).ok();
            }
            for (key, value) in [
                ("username", username),
                ("password", password),
                ("oauth_refresh_token", oauth_refresh_token),
            ] {
                if let Some(value) = value {
                    validate(key, value.as_str().into(), *protect_protocol).map_err(std::io::Error::other)?;
                    write_key(&mut out, key, value.as_bytes().as_bstr()).ok();
                }
            }
            if let Some(value) = password_expiry_utc {
                let key = "password_expiry_utc";
                let value = value.to_string();
                validate(key, value.as_str().into(), *protect_protocol).map_err(std::io::Error::other)?;
                write_key(&mut out, key, value.as_bytes().as_bstr()).ok();
            }
            Ok(())
        }

        /// Like [`write_to()`][Self::write_to()], but writes infallibly into memory.
        pub fn to_bstring(&self) -> BString {
            let mut buf = Vec::<u8>::new();
            self.write_to(&mut buf).expect("infallible");
            buf.into()
        }
    }
}

///
pub mod decode {
    use bstr::{BString, ByteSlice};

    use crate::protocol::{Context, ContextOptions, context, context::serde::validate};

    /// The error returned by [`from_bytes()`][Context::from_bytes()].
    #[derive(Debug, thiserror::Error)]
    #[expect(missing_docs)]
    pub enum Error {
        #[error("Illformed UTF-8 in value of key {key:?}: {value:?}")]
        IllformedUtf8InValue { key: String, value: BString },
        #[error(transparent)]
        Encoding(#[from] context::Error),
        #[error("Invalid format in line {line:?}, expecting key=value")]
        Syntax { line: BString },
    }

    impl Context {
        /// Decode ourselves from `input` which is the format written by [`write_to()`][Self::write_to()].
        /// `options` control what to support during deserialization.
        ///
        /// A line without `=` is an error here; [`from_bytes_until_invalid()`][Self::from_bytes_until_invalid()]
        /// is the reading git's own helper cascade does.
        pub fn from_bytes(input: &[u8], options: ContextOptions) -> Result<Self, Error> {
            match Self::from_bytes_until_invalid(input, options)? {
                (ctx, None) => Ok(ctx),
                (_, Some(line)) => Err(Error::Syntax { line }),
            }
        }

        /// `credential_read()` (credential.c:313), which does not fail the run on a
        /// malformed line:
        ///
        /// ```c
        /// if (!value) {
        ///         warning("invalid credential line: %s", key);
        ///         strbuf_release(&line);
        ///         return -1;
        /// }
        /// ```
        ///
        /// It stops there and keeps every field it read before it, and its callers
        /// throw the `-1` away — `credential_fill()` never looks at what
        /// `credential_do()` returned — so a helper that prints one bad line still
        /// contributes what it printed first, and the next helper still runs.
        ///
        /// Returns the context read so far and the offending line, if there was one.
        pub fn from_bytes_until_invalid(
            input: &[u8],
            options: ContextOptions,
        ) -> Result<(Self, Option<BString>), Error> {
            let mut ctx = Context {
                options,
                ..Context::default()
            };
            let Context {
                options: _,
                protocol,
                host,
                path,
                username,
                password,
                oauth_refresh_token,
                password_expiry_utc,
                url,
                quit,
            } = &mut ctx;
            let mut invalid: Option<BString> = None;
            for line in input.lines().take_while(|line| !line.is_empty()) {
                let mut it = line.splitn(2, |b| *b == b'=');
                let (key, value) = match (
                    it.next().and_then(|k| k.to_str().ok()),
                    it.next().map(ByteSlice::as_bstr),
                ) {
                    (Some(key), Some(value)) => {
                        validate(key, value, options.protect_protocol)?;
                        (key, value.to_owned())
                    }
                    // git's `if (!value)`: warn, stop, keep what was read.
                    _ => {
                        invalid = Some(line.into());
                        break;
                    }
                };
                match key {
                    "protocol" | "host" | "username" | "password" | "oauth_refresh_token" => {
                        if !value.is_utf8() {
                            return Err(Error::IllformedUtf8InValue { key: key.into(), value });
                        }
                        let value = value.to_string();
                        *match key {
                            "protocol" => &mut *protocol,
                            "host" => host,
                            "username" => username,
                            "password" => password,
                            "oauth_refresh_token" => oauth_refresh_token,
                            _ => unreachable!("checked field names in match above"),
                        } = Some(value);
                    }
                    "password_expiry_utc" => {
                        *password_expiry_utc = value.to_str().ok().and_then(|value| value.parse().ok());
                    }
                    "url" => *url = Some(value),
                    "path" => *path = Some(value),
                    "quit" => {
                        *quit = gix_config_value::Boolean::try_from(value.as_bstr())
                            .ok()
                            .map(Into::into);
                    }
                    _ => {}
                }
            }
            Ok((ctx, invalid))
        }
    }
}

fn validate(key: &str, value: &BStr, protect_protocol: bool) -> Result<(), Error> {
    if key.contains('\0')
        || key.contains('\n')
        || key.contains('\r')
        || value.contains(&0)
        || value.contains(&b'\n')
        || (protect_protocol && value.contains(&b'\r'))
    {
        return Err(Error::Encoding {
            key: key.to_owned(),
            value: value.to_owned(),
        });
    }
    Ok(())
}
