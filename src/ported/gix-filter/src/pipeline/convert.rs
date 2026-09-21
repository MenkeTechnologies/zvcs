use std::{io::Read, path::Path};

use bstr::{BStr, ByteSlice};

use crate::{
    Pipeline, driver, eol, ident,
    pipeline::{EolConversion, WriteObject, util::Configuration},
    worktree,
};

///
pub mod configuration {
    use bstr::BString;

    /// Errors related to the configuration of filter attributes.
    #[derive(Debug, thiserror::Error)]
    #[expect(missing_docs)]
    pub enum Error {
        #[error("The encoding named '{name}' isn't available")]
        UnknownEncoding { name: BString },
        #[error("Encodings must be names, like UTF-16, and cannot be booleans.")]
        InvalidEncoding,
    }
}

///
pub mod to_git {
    use bstr::BString;

    /// A function that fills `buf` `fn(&mut buf)` with the data stored in the index of the file that should be converted.
    pub type IndexObjectFn<'a> = dyn FnMut(&mut Vec<u8>) -> Result<Option<()>, gix_object::find::Error> + 'a;

    /// The error returned by [Pipeline::convert_to_git()][super::Pipeline::convert_to_git()].
    #[derive(Debug, thiserror::Error)]
    #[expect(missing_docs)]
    pub enum Error {
        #[error(transparent)]
        Eol(#[from] crate::eol::convert_to_git::Error),
        #[error(transparent)]
        Worktree(#[from] crate::worktree::encode_to_git::Error),
        #[error(transparent)]
        Driver(#[from] crate::driver::apply::Error),
        #[error(transparent)]
        Configuration(#[from] super::configuration::Error),
        #[error("Copy of driver process output to memory failed")]
        ReadProcessOutputToBuffer(#[from] std::io::Error),
        /// Port of `die(_("%s: clean filter '%s' failed"))` — convert.c:1441. `apply_filter()`
        /// answers 0 for every way a clean filter can fail to deliver content — the driver has no
        /// `clean` command at all (convert.c:1021), the long-running process does not advertise the
        /// `clean` capability (convert.c:832), it answered `error`/`abort` (convert.c:927-933), or
        /// the single-file program exited non-zero (convert.c:697) — and a `required` driver turns
        /// every one of them into this one message.
        #[error("{rela_path}: clean filter '{driver}' failed")]
        RequiredCleanFilterFailed { rela_path: BString, driver: BString },
        #[error("Could not allocate buffer")]
        OutOfMemory(#[from] std::collections::TryReserveError),
    }
}

///
pub mod to_worktree {
    use bstr::BString;

    /// The error returned by [Pipeline::convert_to_worktree()][super::Pipeline::convert_to_worktree()].
    #[derive(Debug, thiserror::Error)]
    #[expect(missing_docs)]
    pub enum Error {
        /// Port of `die(_("%s: smudge filter %s failed"))` — convert.c:1518. git quotes the driver
        /// name in the clean message and leaves it bare here; that asymmetry is git's own text.
        #[error("{rela_path}: smudge filter {driver} failed")]
        RequiredSmudgeFilterFailed { rela_path: BString, driver: BString },
        #[error(transparent)]
        Ident(#[from] crate::ident::apply::Error),
        #[error(transparent)]
        Eol(#[from] crate::eol::convert_to_worktree::Error),
        #[error(transparent)]
        Worktree(#[from] crate::worktree::encode_to_worktree::Error),
        #[error(transparent)]
        Driver(#[from] crate::driver::apply::Error),
        #[error(transparent)]
        Configuration(#[from] super::configuration::Error),
    }
}

/// Access
impl Pipeline {
    /// Convert a `src` stream (to be found at `rela_path`) to a representation suitable for storage in `git`
    /// based on the `attributes` at `rela_path` which is passed as first argument..
    /// When converting to `crlf`, and depending on the configuration, `index_object` might be called to obtain the index
    /// version of `src` if available. It can return `Ok(None)` if this information isn't available.
    pub fn convert_to_git<R>(
        &mut self,
        mut src: R,
        rela_path: &Path,
        attributes: &mut dyn FnMut(&BStr, &mut gix_attributes::search::Outcome),
        index_object: &mut to_git::IndexObjectFn<'_>,
    ) -> Result<ToGitOutcome<'_, R>, to_git::Error>
    where
        R: std::io::Read,
    {
        let bstr_rela_path = gix_path::to_unix_separators_on_windows(gix_path::into_bstr(rela_path));
        let Configuration {
            driver,
            digest,
            _attr_digest: _,
            encoding,
            apply_ident_filter,
        } = Configuration::at_path(
            bstr_rela_path.as_ref(),
            &self.options.drivers,
            &mut self.attrs,
            attributes,
            self.options.eol_config,
        )?;

        let mut in_src_buffer = false;
        // `if (!(conv_flags & CONV_EOL_KEEP_CRLF))` — convert.c:1455. With the flag set the
        // end-of-line half of the conversion is skipped whole, so nothing here may estimate that
        // it would run either.
        let convert_eol = self.options.eol_conversion == EolConversion::Apply;
        // this is just an approximation, but it's as good as it gets without reading the actual input.
        let would_convert_eol = convert_eol
            && eol::convert_to_git(
            b"\r\n",
            digest,
            &mut self.bufs.dest,
            &mut |_| Ok(None),
            eol::convert_to_git::Options {
                round_trip_check: None,
                config: self.options.eol_config,
            },
        )?;

        if let Some(driver) = driver {
            // `convert_to_git()` hands `apply_filter()` a buffer it still owns, so a filter that
            // answers 0 — it failed, or it never ran — leaves `dst` untouched and the original
            // content is what the rest of the conversion sees (convert.c:1436-1445). Buffering
            // `src` up front keeps that fallback available once `apply()` has consumed the reader,
            // and it is what lets a `required` driver's failure be named with its path below.
            self.bufs.clear();
            src.read_to_end(&mut self.bufs.src)?;
            in_src_buffer = true;

            let filtered = {
                let mut original = self.bufs.src.as_slice();
                match self.processes.apply(
                    driver,
                    &mut original,
                    driver::Operation::Clean,
                    self.context.with_path(bstr_rela_path.as_ref()),
                ) {
                    Ok(Some(mut read)) => {
                        if !driver.required && !apply_ident_filter && encoding.is_none() && !would_convert_eol {
                            // Note that this is not typically a benefit in terms of saving memory as most filters
                            // aren't expected to make the output file larger. It's more about who is waiting for the filter's
                            // output to arrive, which won't be us now. For `git-lfs` it definitely won't matter though.
                            // A `required` driver cannot take this exit: its failure has to be seen here to be
                            // reported as git reports it, and only reading the output to its end reveals one.
                            return Ok(ToGitOutcome::Process(read));
                        }
                        let mut filtered = Vec::new();
                        read.read_to_end(&mut filtered).ok().map(|_| filtered)
                    }
                    // Every one of these is `apply_filter()` answering 0: no `clean` command, a
                    // process without the `clean` capability, or a driver that failed. Whatever
                    // explains it is already on stderr, printed where git prints it.
                    Ok(None) | Err(_) => None,
                }
            };
            match filtered {
                Some(filtered) => self.bufs.src = filtered,
                // `if (!ret && ca.drv && ca.drv->required)` — convert.c:1441. Without `required`
                // the buffered original stands in for the filter's output, which is exactly what
                // git's untouched `dst` leaves behind.
                None if driver.required => {
                    return Err(to_git::Error::RequiredCleanFilterFailed {
                        rela_path: bstr_rela_path.into_owned(),
                        driver: driver.name.clone(),
                    });
                }
                None => {}
            }
        }
        if !in_src_buffer && (apply_ident_filter || encoding.is_some() || would_convert_eol) {
            self.bufs.clear();
            src.read_to_end(&mut self.bufs.src)?;
            in_src_buffer = true;
        }

        if let Some(encoding) = &encoding {
            // `if (die_on_error && check_roundtrip(enc))` — convert.c:452. The check is not only
            // reported differently when the blob is not being stored, it is not run at all:
            // "the round trip check is only performed if content is written to Git"
            // (convert.c:441-443), since content nobody keeps cannot lose anything.
            let round_trip = match crate::worktree::encoding::for_label(encoding.as_bstr()) {
                // git's `check_roundtrip()` (convert.c:347-383) looks the name up in
                // `core.checkRoundtripEncoding`; the list arrives here already resolved, so the
                // name is resolved to meet it. A name `encoding_rs` cannot resolve is not in the
                // list either.
                Ok(resolved)
                    if self.options.write_object == WriteObject::Yes
                        && self.options.encodings_with_roundtrip_check.contains(&resolved) =>
                {
                    worktree::encode_to_git::RoundTripCheck::Fail
                }
                _ => worktree::encode_to_git::RoundTripCheck::Skip,
            };
            if worktree::encode_to_git_by_name(
                bstr_rela_path.as_ref(),
                &self.bufs.src,
                encoding.as_ref(),
                &mut self.bufs.dest,
                round_trip,
                self.options.write_object,
            )? {
                self.bufs.swap();
            }
        }

        if convert_eol
            && eol::convert_to_git(
                &self.bufs.src,
                digest,
                &mut self.bufs.dest,
                &mut |buf| index_object(buf),
                eol::convert_to_git::Options {
                    round_trip_check: self.options.crlf_roundtrip_check.to_eol_roundtrip_check(rela_path),
                    config: self.options.eol_config,
                },
            )?
        {
            self.bufs.swap();
        }

        if apply_ident_filter && ident::undo(&self.bufs.src, &mut self.bufs.dest)? {
            self.bufs.swap();
        }
        Ok(if in_src_buffer {
            ToGitOutcome::Buffer(&self.bufs.src)
        } else {
            ToGitOutcome::Unchanged(src)
        })
    }

    /// Convert a `src` buffer located at `rela_path` (in the index) from what's in `git` to the worktree representation,
    /// asking for `attributes` with `rela_path` as first argument to configure the operation automatically.
    /// `can_delay` defines if long-running processes can delay their response, and if they *choose* to the caller has to
    /// specifically deal with it by interacting with the [`driver_state`][Pipeline::driver_state_mut()] directly.
    ///
    /// The reason `src` is a buffer is to indicate that `git` generally doesn't do well streaming data, so it should be small enough
    /// to be performant while being held in memory. This is typically the case, especially if `git-lfs` is used as intended.
    pub fn convert_to_worktree<'input>(
        &mut self,
        src: &'input [u8],
        rela_path: &BStr,
        attributes: &mut dyn FnMut(&BStr, &mut gix_attributes::search::Outcome),
        can_delay: driver::apply::Delay,
    ) -> Result<ToWorktreeOutcome<'input, '_>, to_worktree::Error> {
        let Configuration {
            driver,
            digest,
            _attr_digest: _,
            encoding,
            apply_ident_filter,
        } = Configuration::at_path(
            rela_path,
            &self.options.drivers,
            &mut self.attrs,
            attributes,
            self.options.eol_config,
        )?;

        let mut bufs = self.bufs.use_foreign_src(src);
        let (src, dest) = bufs.src_and_dest();
        if apply_ident_filter && ident::apply(src, self.options.object_hash, dest)? {
            bufs.swap();
        }

        let (src, dest) = bufs.src_and_dest();
        if eol::convert_to_worktree(src, digest, dest, self.options.eol_config)? {
            bufs.swap();
        }

        if let Some(encoding) = &encoding {
            let (src, dest) = bufs.src_and_dest();
            if worktree::encode_to_worktree_by_name(rela_path, src, encoding.as_ref(), dest) {
                bufs.swap();
            }
        }

        if let Some(driver) = driver {
            let (mut src, _dest) = bufs.src_and_dest();
            match self.processes.apply_delayed(
                driver,
                &mut src,
                driver::Operation::Smudge,
                can_delay,
                self.context.with_path(rela_path),
            ) {
                Ok(Some(driver::apply::MaybeDelayed::Immediate(mut read))) if driver.required => {
                    // `apply_single_file_filter()` collects the whole result before it can tell
                    // whether the filter succeeded (convert.c:728-742), so a `required` smudge
                    // driver's failure has to be seen here rather than handed to the caller as a
                    // stream that fails halfway through — git names the path and the driver
                    // (convert.c:1517-1518) and nothing else does.
                    let mut filtered = Vec::new();
                    if read.read_to_end(&mut filtered).is_err() {
                        return Err(to_worktree::Error::RequiredSmudgeFilterFailed {
                            rela_path: rela_path.to_owned(),
                            driver: driver.name.clone(),
                        });
                    }
                    drop(read);
                    let (_src, dest) = bufs.src_and_dest();
                    *dest = filtered;
                    bufs.swap();
                    return Ok(ToWorktreeOutcome::Buffer(bufs.src));
                }
                Ok(Some(maybe_delayed)) => return Ok(ToWorktreeOutcome::Process(maybe_delayed)),
                // `apply_filter()` answering 0 — see the `clean` side. The content already in
                // `bufs` is the fallback, and `required` turns the failure into git's message
                // (`if (!ret_filter && ca->drv && ca->drv->required)`, convert.c:1517-1518).
                Ok(None) | Err(_) if driver.required => {
                    return Err(to_worktree::Error::RequiredSmudgeFilterFailed {
                        rela_path: rela_path.to_owned(),
                        driver: driver.name.clone(),
                    });
                }
                Ok(None) | Err(_) => {}
            }
        }

        Ok(match bufs.ro_src {
            Some(src) => ToWorktreeOutcome::Unchanged(src),
            None => ToWorktreeOutcome::Buffer(bufs.src),
        })
    }
}

/// The result of a conversion with zero or more filters to be stored in git.
pub enum ToGitOutcome<'pipeline, R> {
    /// The original input wasn't changed and the reader is still available for consumption.
    Unchanged(R),
    /// An external filter (and only that) was applied and its results *have to be consumed*.
    Process(Box<dyn std::io::Read + 'pipeline>),
    /// A reference to the result of one or more filters of which one didn't support streaming.
    ///
    /// This can happen if an `eol`, `working-tree-encoding` or `ident` filter is applied, possibly on top of an external filter.
    Buffer(&'pipeline [u8]),
}

/// The result of a conversion with zero or more filters.
///
/// ### Panics
///
/// If `std::io::Read` is used on it and the output is delayed, a panic will occur. The caller is responsible for either disallowing delayed
/// results or if allowed, handle them. Use [`is_delayed()][Self::is_delayed()].
pub enum ToWorktreeOutcome<'input, 'pipeline> {
    /// The original input wasn't changed and the original buffer is present
    Unchanged(&'input [u8]),
    /// A reference to the result of one or more filters of which one didn't support streaming.
    ///
    /// This can happen if an `eol`, `working-tree-encoding` or `ident` filter is applied, possibly on top of an external filter.
    Buffer(&'pipeline [u8]),
    /// An external filter (and only that) was applied and its results *have to be consumed*. Note that the output might be delayed,
    /// which requires special handling to eventually receive it.
    Process(driver::apply::MaybeDelayed<'pipeline>),
}

impl ToWorktreeOutcome<'_, '_> {
    /// Return true if this outcome is delayed. In that case, one isn't allowed to use [`Read`] or cause a panic.
    pub fn is_delayed(&self) -> bool {
        matches!(
            self,
            ToWorktreeOutcome::Process(driver::apply::MaybeDelayed::Delayed(_))
        )
    }

    /// Returns `true` if the input buffer was actually changed, or `false` if it is returned directly.
    pub fn is_changed(&self) -> bool {
        !matches!(self, ToWorktreeOutcome::Unchanged(_))
    }

    /// Return a buffer if we contain one, or `None` otherwise.
    ///
    /// This method is useful only if it's clear that no driver is available, which may cause a stream to be returned and not a buffer.
    pub fn as_bytes(&self) -> Option<&[u8]> {
        match self {
            ToWorktreeOutcome::Unchanged(b) | ToWorktreeOutcome::Buffer(b) => Some(b),
            ToWorktreeOutcome::Process(_) => None,
        }
    }

    /// Return a stream to read the drivers output from, if possible.
    ///
    /// Note that this is only the case if the driver process was applied last *and* didn't delay its output.
    pub fn as_read(&mut self) -> Option<&mut (dyn std::io::Read + '_)> {
        match self {
            ToWorktreeOutcome::Process(driver::apply::MaybeDelayed::Delayed(_))
            | ToWorktreeOutcome::Unchanged(_)
            | ToWorktreeOutcome::Buffer(_) => None,
            ToWorktreeOutcome::Process(driver::apply::MaybeDelayed::Immediate(read)) => Some(read),
        }
    }
}

impl std::io::Read for ToWorktreeOutcome<'_, '_> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        match self {
            ToWorktreeOutcome::Unchanged(b) => b.read(buf),
            ToWorktreeOutcome::Buffer(b) => b.read(buf),
            ToWorktreeOutcome::Process(driver::apply::MaybeDelayed::Delayed(_)) => {
                panic!("BUG: must not try to read delayed output")
            }
            ToWorktreeOutcome::Process(driver::apply::MaybeDelayed::Immediate(r)) => r.read(buf),
        }
    }
}

impl<R> std::io::Read for ToGitOutcome<'_, R>
where
    R: std::io::Read,
{
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        match self {
            ToGitOutcome::Unchanged(r) => r.read(buf),
            ToGitOutcome::Process(r) => r.read(buf),
            ToGitOutcome::Buffer(r) => r.read(buf),
        }
    }
}

impl<'a, R> ToGitOutcome<'a, R>
where
    R: std::io::Read,
{
    /// If we contain a buffer, and not a stream, return it.
    pub fn as_bytes(&self) -> Option<&'a [u8]> {
        match self {
            ToGitOutcome::Unchanged(_) | ToGitOutcome::Process(_) => None,
            ToGitOutcome::Buffer(b) => Some(b),
        }
    }

    /// Return a stream to read the drivers output from. This is only possible if there is only a driver, and no other filter.
    pub fn as_read(&mut self) -> Option<&mut (dyn std::io::Read + '_)> {
        match self {
            ToGitOutcome::Process(read) => Some(read),
            ToGitOutcome::Unchanged(read) => Some(read),
            ToGitOutcome::Buffer(_) => None,
        }
    }

    /// Returns `true` if the input buffer was actually changed, or `false` if it is returned directly.
    pub fn is_changed(&self) -> bool {
        !matches!(self, ToGitOutcome::Unchanged(_))
    }
}
