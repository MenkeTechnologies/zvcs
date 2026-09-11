use std::{io::Read, path::PathBuf};

use crate::{
    FullNameRef,
    store_impl::{file, file::log},
};

impl file::Store {
    /// Returns true if a reflog exists for the given reference `name`.
    ///
    /// Please note that this method shouldn't be used to check if a log exists before trying to read it, but instead
    /// is meant to be the fastest possible way to determine if a log exists or not.
    /// If the caller needs to know if it's readable, try to read the log instead with a reverse or forward iterator.
    pub fn reflog_exists<'a, Name, E>(&self, name: Name) -> Result<bool, E>
    where
        Name: TryInto<&'a FullNameRef, Error = E>,
        crate::name::Error: From<E>,
    {
        Ok(self.reflog_path(name.try_into()?).is_file())
    }

    /// Return a reflog reverse iterator for the given fully qualified `name`, reading chunks from the back into the fixed buffer `buf`.
    ///
    /// The iterator will traverse log entries from most recent to oldest, reading the underlying file in chunks from the back.
    /// Return `Ok(None)` if no reflog exists.
    pub fn reflog_iter_rev<'a, 'b, Name, E>(
        &self,
        name: Name,
        buf: &'b mut [u8],
    ) -> Result<Option<log::iter::Reverse<'b, std::fs::File>>, Error>
    where
        Name: TryInto<&'a FullNameRef, Error = E>,
        crate::name::Error: From<E>,
    {
        let name: &FullNameRef = name.try_into().map_err(|err| Error::RefnameValidation(err.into()))?;
        let path = self.reflog_path(name);
        if path.is_dir() {
            return Ok(None);
        }
        match std::fs::File::open(&path) {
            Ok(file) => Ok(Some(log::iter::reverse(file, buf)?)),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(err) => Err(err.into()),
        }
    }

    /// Return a reflog forward iterator for the given fully qualified `name` and write its file contents into `buf`.
    ///
    /// The iterator will traverse log entries from oldest to newest.
    /// Return `Ok(None)` if no reflog exists.
    pub fn reflog_iter<'a, 'b, Name, E>(
        &self,
        name: Name,
        buf: &'b mut Vec<u8>,
    ) -> Result<Option<log::iter::Forward<'b>>, Error>
    where
        Name: TryInto<&'a FullNameRef, Error = E>,
        crate::name::Error: From<E>,
    {
        let name: &FullNameRef = name.try_into().map_err(|err| Error::RefnameValidation(err.into()))?;
        let path = self.reflog_path(name);
        match std::fs::File::open(&path) {
            Ok(mut file) => {
                buf.clear();
                if let Err(err) = file.read_to_end(buf) {
                    return if path.is_dir() { Ok(None) } else { Err(err.into()) };
                }
                Ok(Some(log::iter::forward(buf)))
            }
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
            #[cfg(windows)]
            Err(err) if err.kind() == std::io::ErrorKind::PermissionDenied => Ok(None),
            Err(err) => Err(err.into()),
        }
    }
}

impl file::Store {
    /// Implements the logic required to transform a fully qualified refname into its log name
    pub(crate) fn reflog_path(&self, name: &FullNameRef) -> PathBuf {
        let (base, rela_path) = self.reflog_base_and_relative_path(name);
        base.join(rela_path)
    }
}

///
pub mod create_or_update {
    use std::{
        borrow::Cow,
        io::Write,
        path::{Path, PathBuf},
    };

    use gix_hash::{ObjectId, oid};
    use gix_object::bstr::BStr;

    use crate::store_impl::{file, file::WriteReflog};

    impl file::Store {
        pub(crate) fn reflog_create_or_append(
            &self,
            name: &FullNameRef,
            previous_oid: Option<ObjectId>,
            new: &oid,
            committer: Option<gix_actor::SignatureRef<'_>>,
            message: &BStr,
            mut force_create_reflog: bool,
        ) -> Result<(), Error> {
            let (reflog_base, full_name) = self.reflog_base_and_relative_path(name);
            // `log_ref_setup()` (refs/files-backend.c:1859) has exactly one shape for all
            // three values of `log_all_ref_updates`, and the policy only decides whether the
            // file may be *created*:
            //
            // ```c
            // if (force_create || should_autocreate_reflog(log_refs_cfg, refname)) {
            //         if (raceproof_create_file(logfile, open_or_create_logfile, logfd)) …
            // } else {
            //         *logfd = open(logfile, O_APPEND | O_WRONLY);
            //         if (*logfd < 0) { if (errno == ENOENT || errno == EISDIR) ; … }
            // }
            // ```
            //
            // So `core.logAllRefUpdates = false` does not mean "no reflog writes", it means
            // "no *new* reflog files": an existing log keeps being appended to, because git
            // looks for the file before it consults the setting. That is why `HEAD`'s history
            // survives a repository that turns logging off, and it is the reason
            // [`WriteReflog::Disable`] takes the same path here as the other two.
            if self.write_reflog == WriteReflog::Always {
                force_create_reflog = true;
            }
            let mut options = std::fs::OpenOptions::new();
            options.append(true).read(false);
            let log_path = reflog_base.join(&full_name);

            // The two halves of `log_ref_setup()` differ in more than whether `O_CREAT` is set:
            // only the creating half is allowed to clear a directory out of the way, and only the
            // non-creating half treats `EISDIR` as "nothing to write here".
            let file_for_appending = if force_create_reflog || self.should_autocreate_reflog(&full_name) {
                let parent_dir = log_path.parent().expect("always with parent directory");
                gix_tempfile::create_dir::all(parent_dir, Default::default()).map_err(|err| {
                    Error::CreateLeadingDirectories {
                        source: err,
                        reflog_directory: parent_dir.to_owned(),
                    }
                })?;
                options.create(true);
                match options.open(&log_path) {
                    Ok(f) => Some(f),
                    Err(err) if err.kind() == std::io::ErrorKind::NotFound => None,
                    // `raceproof_create_file()` (refs/files-backend.c:1149-1158) answers `EISDIR`
                    // by removing the directory "if it is empty (and recursively any empty
                    // directories that it contains)" and calling the opener once more — once,
                    // because a process racing us to create directories is one we let win. A
                    // directory that still holds a file survives and the retry fails again.
                    //
                    // `is_dir()` rather than `ErrorKind::IsADirectory`: Windows reports a
                    // directory open as `PermissionDenied`, so the errno test would miss it.
                    Err(err) if log_path.is_dir() => gix_tempfile::remove_dir::empty_depth_first(log_path.clone())
                        .and_then(|()| options.open(&log_path))
                        .map(Some)
                        .map_err(|_| Error::Append {
                            source: err,
                            reflog_path: self.reflog_path(name),
                        })?,
                    Err(err) => {
                        return Err(Error::Append {
                            source: err,
                            reflog_path: log_path,
                        });
                    }
                }
            } else {
                // The other half (refs/files-backend.c:1887-1903) is a bare
                // `open(logfile, O_APPEND | O_WRONLY)` whose failure is inspected:
                //
                // ```c
                // if (errno == ENOENT || errno == EISDIR) {
                //         /*
                //          * The logfile doesn't already exist, but that is not an error;
                //          * it only means that we won't write log entries to it.
                //          */
                //         ;
                // } else { … goto error; }
                // ```
                //
                // So a directory sitting where the log would go is as quiet as a missing log,
                // and it is left standing: `core.logAllRefUpdates=false` updating a ref whose
                // `logs/` path is a directory exits 0, writes the ref, and does not touch the
                // directory — measured against git 2.55.0.
                match options.open(&log_path) {
                    Ok(f) => Some(f),
                    Err(err) if err.kind() == std::io::ErrorKind::NotFound => None,
                    Err(_err) if log_path.is_dir() => None,
                    Err(err) => {
                        return Err(Error::Append {
                            source: err,
                            reflog_path: log_path,
                        });
                    }
                }
            };

            if let Some(mut file) = file_for_appending {
                let committer = committer.ok_or(Error::MissingCommitter)?;
                // `ref_transaction_add_update()` stores `normalize_reflog_message(msg)`
                // (refs.c:1342), so by the time `log_ref_write_fd()` (refs/files-backend.c:1933)
                // writes it every run of whitespace is one space and the ends are trimmed.
                // Doing it here covers both callers of this function for the same reason
                // the C does it in one place: the reflog format separates the committer
                // from the message with a tab, and an unnormalized message can contain one.
                let message = crate::log::normalize_message(message);
                write!(file, "{} {} ", previous_oid.unwrap_or_else(|| new.kind().null()), new)
                    .and_then(|_| committer.trim().write_to(&mut file))
                    .and_then(|_| {
                        if !message.is_empty() {
                            // Written as bytes, not through `Display`: `log_ref_write_fd()`
                            // `fwrite`s the message, and a reflog line for a mail whose
                            // `Subject:` was Latin-1 (`git am` on a `charset=ISO-8859-1`
                            // patch) carries bytes that are not UTF-8. `BString`'s `Display`
                            // is lossy and would store U+FFFD in their place.
                            file.write_all(b"\t")
                                .and_then(|_| file.write_all(message.as_slice()))
                                .and_then(|_| file.write_all(b"\n"))
                        } else {
                            writeln!(file)
                        }
                    })
                    .map_err(|err| Error::Append {
                        source: err,
                        reflog_path: self.reflog_path(name),
                    })?;
            }
            Ok(())
        }

        /// `should_autocreate_reflog()` (refs.c:1056), which decides only whether a *missing*
        /// log may be created.
        ///
        /// ```c
        /// case LOG_REFS_NORMAL:
        ///         return starts_with(refname, "refs/heads/") ||
        ///                 starts_with(refname, "refs/remotes/") ||
        ///                 starts_with(refname, "refs/notes/") ||
        ///                 !strcmp(refname, "HEAD");
        /// ```
        ///
        /// `refs/worktree/` is deliberately absent: a worktree-private ref gets no log of its
        /// own, so `update-ref refs/worktree/pin HEAD` writes the ref and nothing else.
        fn should_autocreate_reflog(&self, full_name: &Path) -> bool {
            match self.write_reflog {
                WriteReflog::Always => true,
                WriteReflog::Disable => false,
                WriteReflog::Normal => {
                    full_name.starts_with("refs/heads/")
                        || full_name.starts_with("refs/remotes/")
                        || full_name.starts_with("refs/notes/")
                        || full_name == Path::new("HEAD")
                }
            }
        }

        /// Returns the base paths for all reflogs
        pub(in crate::store_impl::file) fn reflog_base_and_relative_path<'a>(
            &self,
            name: &'a FullNameRef,
        ) -> (PathBuf, Cow<'a, Path>) {
            let is_reflog = true;
            let (base, name) = self.to_base_dir_and_relative_name(name, is_reflog);
            (
                base.join("logs"),
                match &self.namespace {
                    None => gix_path::to_native_path_on_windows(name.as_bstr()),
                    Some(namespace) => gix_path::to_native_path_on_windows(
                        namespace.to_owned().into_namespaced_name(name).into_inner(),
                    ),
                },
            )
        }
    }

    #[cfg(test)]
    mod tests;

    mod error {
        use std::path::PathBuf;

        /// The error returned when creating or appending to a reflog
        #[derive(Debug, thiserror::Error)]
        #[expect(missing_docs)]
        pub enum Error {
            #[error("Could create one or more directories in {reflog_directory:?} to contain reflog file")]
            CreateLeadingDirectories {
                source: std::io::Error,
                reflog_directory: PathBuf,
            },
            #[error("Could not open reflog file at {reflog_path:?} for appending")]
            Append {
                source: std::io::Error,
                reflog_path: PathBuf,
            },
            #[error("reflog message must not contain newlines")]
            MessageWithNewlines,
            #[error("reflog messages need a committer which isn't set")]
            MissingCommitter,
        }
    }
    pub use error::Error;

    use crate::FullNameRef;
}

mod error {
    /// The error returned by [`crate::file::Store::reflog_iter()`].
    #[derive(Debug, thiserror::Error)]
    #[expect(missing_docs)]
    pub enum Error {
        #[error("The reflog name or path is not a valid ref name")]
        RefnameValidation(#[from] crate::name::Error),
        #[error("The reflog file could not read")]
        Io(#[from] std::io::Error),
    }
}
pub use error::Error;
