mod types;
pub use types::{Error, Options, TrustPolicy};

mod util;

pub(crate) mod function {
    use std::{borrow::Cow, ffi::OsStr, path::Path};

    use gix_sec::Trust;

    use super::{Error, Options, TrustPolicy};
    #[cfg(unix)]
    use crate::upwards::util::device_id;
    use crate::{
        DOT_GIT_DIR,
        is::git_with_metadata as is_git_with_metadata,
        is_git,
        upwards::util::{find_ceiling_height, shorten_path_with_cwd},
    };

    /// Find the location of the git repository directly in `directory` or in any of its parent directories and provide
    /// an associated Trust level by looking at the git directory's ownership, and control discovery using `options`.
    ///
    /// Fail if no valid-looking git repository could be found.
    // TODO: tests for trust-based discovery
    #[cfg_attr(not(unix), allow(unused_variables))]
    pub fn discover_opts(
        directory: &Path,
        Options {
            trust,
            ceiling_dirs,
            ceiling_dirs_verbatim,
            match_ceiling_dir_or_error,
            cross_fs,
            current_dir,
            dot_git_only,
        }: Options<'_>,
    ) -> Result<(crate::repository::Path, gix_sec::Trust), Error> {
        // Normalize the path so that `Path::parent()` _actually_ gives
        // us the parent directory. (`Path::parent` just strips off the last
        // path component, which means it will not do what you expect when
        // working with paths that contain '..'.)
        let cwd = current_dir.map_or_else(
            || {
                // The paths we return are relevant to the repository, but at this time it's impossible to know
                // what `core.precomposeUnicode` is going to be. Hence, the one using these paths will have to
                // transform the paths as needed, because we can't. `false` means to leave the obtained path as is.
                gix_fs::current_dir(false).map(Cow::Owned)
            },
            |cwd| Ok(Cow::Borrowed(cwd)),
        )?;
        #[cfg(windows)]
        let directory = dunce::simplified(directory);
        let dir = gix_path::normalize(directory.into(), cwd.as_ref()).ok_or_else(|| Error::InvalidInput {
            directory: directory.into(),
        })?;
        let dir_metadata = dir.metadata().map_err(|_| Error::InaccessibleDirectory {
            path: dir.to_path_buf(),
        })?;

        if !dir_metadata.is_dir() {
            return Err(Error::InaccessibleDirectory { path: dir.into_owned() });
        }
        let mut dir_made_absolute = !directory.is_absolute()
            && cwd
                .as_ref()
                .strip_prefix(dir.as_ref())
                .or_else(|_| dir.as_ref().strip_prefix(cwd.as_ref()))
                .is_ok();

        let filter_by_trust = |x: &Path| -> Result<Result<Trust, Trust>, Error> {
            match trust {
                TrustPolicy::Required(required) => {
                    let trust =
                        Trust::from_path_ownership(x).map_err(|err| Error::CheckTrust { path: x.into(), err })?;
                    Ok(if trust >= required { Ok(trust) } else { Err(required) })
                }
                TrustPolicy::Assume(trust) => Ok(Ok(trust)),
            }
        };

        let max_height = if !(ceiling_dirs.is_empty() && ceiling_dirs_verbatim.is_empty()) {
            let max_height = find_ceiling_height(&dir, &ceiling_dirs, &ceiling_dirs_verbatim, cwd.as_ref());
            if max_height.is_none() && match_ceiling_dir_or_error {
                return Err(Error::NoMatchingCeilingDir);
            }
            max_height
        } else {
            None
        };

        #[cfg(unix)]
        let initial_device = device_id(&dir_metadata);

        let mut cursor = dir.clone().into_owned();
        let mut current_height = 0;
        let mut cursor_metadata = Some(dir_metadata);
        'outer: loop {
            // The ceiling directory itself is *not* searched. `setup_git_directory_gently_1()` in
            // `setup.c` keeps `ceil_offset`, the length of the longest ceiling that is a proper
            // ancestor of the starting directory, and stops before it ever looks at that
            // directory:
            //
            //     while (--offset > ceil_offset && !is_dir_sep(dir->buf[offset]))
            //             ; /* continue */
            //     if (offset <= ceil_offset)
            //             return GIT_DIR_HIT_CEILING;
            //
            // `offset` there is the length of the parent that is about to be examined, so the
            // parent whose length *equals* the ceiling's — the ceiling itself — ends the search.
            // `max_height` is that same boundary expressed as the number of components between
            // the ceiling and the starting directory, hence `>=` rather than `>`.
            //
            // `find_ceiling_height()` only ever yields a height of at least one (git's
            // `longest_ancestor_length()` likewise requires `path[len] == '/'`, so a ceiling
            // equal to the starting directory does not match), which is what keeps the starting
            // directory itself searched no matter what the ceilings say.
            if max_height.is_some_and(|x| current_height >= x) {
                return Err(Error::NoGitRepositoryWithinCeiling {
                    path: dir.into_owned(),
                    ceiling_height: current_height,
                });
            }
            current_height += 1;

            #[cfg(unix)]
            if current_height != 0 && !cross_fs {
                let metadata = cursor_metadata.take().map_or_else(
                    || {
                        if cursor.as_os_str().is_empty() {
                            Path::new(".")
                        } else {
                            cursor.as_ref()
                        }
                        .metadata()
                        .map_err(|_| Error::InaccessibleDirectory { path: cursor.clone() })
                    },
                    Ok,
                )?;

                if device_id(&metadata) != initial_device {
                    return Err(Error::NoGitRepositoryWithinFs {
                        path: dir.into_owned(),
                        limit: cursor.clone(),
                    });
                }
                cursor_metadata = Some(metadata);
            }

            let mut cursor_metadata_backup = None;
            let started_as_dot_git = cursor.file_name() == Some(OsStr::new(DOT_GIT_DIR));
            let dir_manipulation = if dot_git_only { &[true] as &[_] } else { &[true, false] };
            for append_dot_git in dir_manipulation {
                if *append_dot_git && !started_as_dot_git {
                    cursor.push(DOT_GIT_DIR);
                    cursor_metadata_backup = cursor_metadata.take();
                }
                // `git` probes `<dir>/.git` before it probes `<dir>` itself, and the two hits mean
                // different things (`setup_git_directory_gently_1()` in `setup.c`):
                //
                // * a hit on `<dir>/.git` is `GIT_DIR_DISCOVERED`: `<dir>` is the work tree.
                // * a hit on `<dir>` itself is `GIT_DIR_BARE`: `<dir>` *becomes* `GIT_DIR` and there
                //   is no work tree, no matter what the directory is called. This is what makes
                //   `git log` work from inside a `.git` directory, and what makes `git status`
                //   there fail with "this operation must be run in a work tree".
                //
                // `dot_git_only` is not git's discovery - it deliberately looks for work trees only
                // and ignores bare repositories - so it keeps treating a `.git` cursor as a work
                // tree's git directory.
                let probed_dot_git_child = *append_dot_git && !started_as_dot_git;
                let cursor_is_the_git_dir = !probed_dot_git_child && !dot_git_only;
                if let Ok(kind) = match cursor_metadata.take() {
                    Some(metadata) => is_git_with_metadata(&cursor, metadata, &cwd),
                    None => is_git(&cursor),
                } {
                    match filter_by_trust(&cursor)? {
                        Ok(trust) => {
                            if cursor_is_the_git_dir {
                                // `GIT_DIR_BARE`: adopt the directory as-is. `shorten_path_with_cwd()`
                                // is not applicable here as it only knows how to shorten paths that
                                // end in `.git`.
                                break 'outer Ok((crate::repository::Path::Repository(cursor), trust));
                            }
                            // TODO: test this more, it definitely doesn't always find the shortest path to a directory
                            let path = if dir_made_absolute {
                                shorten_path_with_cwd(cursor, cwd.as_ref())
                            } else {
                                cursor
                            };
                            break 'outer Ok((
                                crate::repository::Path::from_dot_git_dir(path, kind, cwd.as_ref()).ok_or_else(
                                    || Error::InvalidInput {
                                        directory: directory.into(),
                                    },
                                )?,
                                trust,
                            ));
                        }
                        Err(required) => {
                            break 'outer Err(Error::NoTrustedGitRepository {
                                path: dir.into_owned(),
                                candidate: cursor,
                                required,
                            });
                        }
                    }
                }

                // Usually `.git` (started_as_dot_git == true) will be a git dir, but if not we can quickly skip over it.
                if *append_dot_git || started_as_dot_git {
                    cursor.pop();
                    if let Some(metadata) = cursor_metadata_backup.take() {
                        cursor_metadata = Some(metadata);
                    }
                }
            }
            if cursor.as_os_str().is_empty() || cursor.as_os_str() == OsStr::new(".") {
                cursor = cwd.to_path_buf();
                dir_made_absolute = true;
            }
            if !cursor.pop() {
                if dir_made_absolute
                    || matches!(
                        cursor.components().next(),
                        Some(std::path::Component::RootDir | std::path::Component::Prefix(_))
                    )
                {
                    break Err(Error::NoGitRepository { path: dir.into_owned() });
                } else {
                    dir_made_absolute = true;
                    debug_assert!(!cursor.as_os_str().is_empty());
                    // TODO: realpath or normalize? No test runs into this.
                    cursor = gix_path::normalize(cursor.clone().into(), cwd.as_ref())
                        .ok_or_else(|| Error::InvalidInput {
                            directory: cursor.clone(),
                        })?
                        .into_owned();
                }
            }
        }
    }

    /// Find the location of the git repository directly in `directory` or in any of its parent directories, and provide
    /// the trust level derived from Path ownership.
    ///
    /// Fail if no valid-looking git repository could be found.
    pub fn discover(directory: &Path) -> Result<(crate::repository::Path, gix_sec::Trust), Error> {
        discover_opts(directory, Default::default())
    }
}
