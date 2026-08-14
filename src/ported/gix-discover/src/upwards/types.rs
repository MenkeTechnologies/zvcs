use std::{env, ffi::OsStr, path::PathBuf};

/// The error returned by [`gix_discover::upwards()`][crate::upwards()].
#[derive(Debug, thiserror::Error)]
#[expect(missing_docs)]
pub enum Error {
    #[error("Could not obtain the current working directory")]
    CurrentDir(#[from] std::io::Error),
    #[error("Relative path \"{}\"tries to reach beyond root filesystem", directory.display())]
    InvalidInput { directory: PathBuf },
    #[error("Failed to access a directory, or path is not a directory: '{}'", .path.display())]
    InaccessibleDirectory { path: PathBuf },
    #[error("Could not find a git repository in '{}' or in any of its parents", .path.display())]
    NoGitRepository { path: PathBuf },
    #[error("Could not find a git repository in '{}' or in any of its parents within ceiling height of {}", .path.display(), .ceiling_height)]
    NoGitRepositoryWithinCeiling { path: PathBuf, ceiling_height: usize },
    #[error("Could not find a git repository in '{}' or in any of its parents within device limits below '{}'", .path.display(), .limit.display())]
    NoGitRepositoryWithinFs { path: PathBuf, limit: PathBuf },
    #[error("None of the passed ceiling directories prefixed the git-dir candidate, making them ineffective.")]
    NoMatchingCeilingDir,
    #[error("Could not find a trusted git repository in '{}' or in any of its parents, candidate at '{}' discarded", .path.display(), .candidate.display())]
    NoTrustedGitRepository {
        path: PathBuf,
        candidate: PathBuf,
        required: gix_sec::Trust,
    },
    #[error("Could not determine trust level for path '{}'.", .path.display())]
    CheckTrust {
        path: PathBuf,
        #[source]
        err: std::io::Error,
    },
}

/// How to obtain the trust level for a discovered repository.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub enum TrustPolicy {
    /// Determine trust from repository ownership and require it to be at least the given level.
    Required(gix_sec::Trust),
    /// Trust computation is skipped and the given trust level is assumed.
    Assume(gix_sec::Trust),
}

impl Default for TrustPolicy {
    fn default() -> Self {
        TrustPolicy::Required(gix_sec::Trust::Reduced)
    }
}

/// Options to help guide the [discovery][crate::upwards()] of repositories, along with their options
/// when instantiated.
pub struct Options<'a> {
    /// When discovering a repository, determine how trust should be obtained.
    ///
    /// This defaults to [`Required(Reduced)`][TrustPolicy::Required] as our default settings are geared towards avoiding abuse.
    /// Set it to `Required(Full)` to only see repositories that [are owned by the current user][gix_sec::Trust::from_path_ownership()],
    /// or [`TrustPolicy::Assume`] to skip trust computation and return the given trust level.
    pub trust: TrustPolicy,
    /// When discovering a repository, ignore any repositories that are located in these directories or any of their parents.
    ///
    /// These are made absolute and normalized before they are compared to the search directory, so a ceiling may be
    /// spelled relatively or contain `.` and `..` components and still match.
    ///
    /// Note that we ignore ceiling directories if the search directory is directly on top of one, which by default is an error
    /// if `match_ceiling_dir_or_error` is true, the default.
    pub ceiling_dirs: Vec<PathBuf>,
    /// Like [`ceiling_dirs`][Self::ceiling_dirs], but these are compared to the search directory *verbatim*, as the very
    /// bytes they are spelled with, without being normalized or made absolute first.
    ///
    /// This is what git does to the entries of `GIT_CEILING_DIRECTORIES` that follow an empty entry: an empty entry turns
    /// canonicalization off for everything after it (`canonicalize_ceiling_entry()` in `setup.c`), and the comparison that
    /// follows is a plain string prefix test (`longest_ancestor_length()` in `path.c`). An entry that only names an ancestor
    /// after normalization, like `/parent/child/..`, therefore does *not* stop the search there.
    ///
    /// [`apply_environment()`][Self::apply_environment()] is the only thing in this crate that fills it in, so ceilings
    /// set by hand keep [`ceiling_dirs`][Self::ceiling_dirs]'s normalizing behaviour unless they are put here deliberately.
    ///
    /// The two lists do not have to preserve their relative order, because the deepest ceiling out of both wins no matter
    /// where it was spelled - git likewise takes the *longest* ancestor over the whole list.
    pub ceiling_dirs_verbatim: Vec<PathBuf>,
    /// If true, default true, and `ceiling_dirs` is not empty, we expect at least one ceiling directory to
    /// contain our search dir or else there will be an error.
    pub match_ceiling_dir_or_error: bool,
    /// if `true` avoid crossing filesystem boundaries.
    /// Only supported on Unix-like systems.
    // TODO: test on Linux
    // TODO: Handle WASI once https://github.com/rust-lang/rust/issues/71213 is resolved
    pub cross_fs: bool,
    /// If true, limit discovery to `.git` directories.
    ///
    /// This  will fail to find typical bare repositories, but would find them if they happen to be named `.git`.
    /// Use this option if repos with worktrees are the only kind of repositories you are interested in for
    /// optimal discovery performance.
    pub dot_git_only: bool,
    /// If set, the _current working directory_ (absolute path) to use when resolving relative paths. Note that
    /// that this is merely an optimization for those who discover a lot of repositories in the same process.
    ///
    /// If unset, the current working directory will be obtained automatically.
    /// Note that the path here might or might not contained decomposed unicode, which may end up in a path
    /// relevant us, like the git-dir or the worktree-dir. However, when opening the repository, it will
    /// change decomposed unicode to precomposed unicode based on the value of `core.precomposeUnicode`, and we
    /// don't have to deal with that value here just yet.
    pub current_dir: Option<&'a std::path::Path>,
}

impl Default for Options<'_> {
    fn default() -> Self {
        Options {
            trust: TrustPolicy::default(),
            ceiling_dirs: vec![],
            ceiling_dirs_verbatim: vec![],
            match_ceiling_dir_or_error: true,
            cross_fs: false,
            dot_git_only: false,
            current_dir: None,
        }
    }
}

impl Options<'_> {
    /// Loads discovery options overrides from the environment.
    ///
    /// The environment variables are:
    /// - `GIT_CEILING_DIRECTORIES` for `ceiling_dirs`
    ///
    /// Note that `GIT_DISCOVERY_ACROSS_FILESYSTEM` for `cross_fs` is **not** read,
    /// as it requires parsing of `git-config` style boolean values.
    // TODO: test
    pub fn apply_environment(mut self) -> Self {
        let name = "GIT_CEILING_DIRECTORIES";
        if let Some(ceiling_dirs) = env::var_os(name) {
            let CeilingDirs { canonicalized, verbatim } = parse_ceiling_dirs(&ceiling_dirs);
            self.ceiling_dirs = canonicalized;
            self.ceiling_dirs_verbatim = verbatim;
        }
        self
    }
}

/// The two kinds of ceiling directory that `GIT_CEILING_DIRECTORIES` can hold, mirroring the two ways
/// git's `canonicalize_ceiling_entry()` can keep an entry.
#[derive(Default, Debug)]
pub(crate) struct CeilingDirs {
    /// Entries seen before the first empty entry, with their symlinks resolved.
    pub canonicalized: Vec<PathBuf>,
    /// Entries seen after the first empty entry, kept exactly as they were spelled.
    pub verbatim: Vec<PathBuf>,
}

/// Parse a byte-string of `:`-separated paths into [`CeilingDirs`].
/// On Windows, paths are separated by `;`.
/// Non-absolute paths are discarded.
///
/// To match git, all paths are canonicalized until an empty path is encountered. From there on entries are kept as-is,
/// which git's `canonicalize_ceiling_entry()` in `setup.c` does by returning early with a comment that says as much:
///
/// ```text
/// } else if (*empty_entry_found) {
///         /* Keep entry but do not canonicalize it */
///         return 1;
/// ```
///
/// Such an entry is never touched again, so it also never gets normalized - hence the separate list, whose entries are
/// compared verbatim later on.
pub(crate) fn parse_ceiling_dirs(ceiling_dirs: &OsStr) -> CeilingDirs {
    let mut empty_entry_found = false;
    let mut out = CeilingDirs::default();
    for ceiling_dir in std::env::split_paths(ceiling_dirs) {
        if ceiling_dir.as_os_str().is_empty() {
            empty_entry_found = true;
            continue;
        }

        // Only absolute paths are allowed
        if ceiling_dir.is_relative() {
            continue;
        }

        if empty_entry_found {
            out.verbatim.push(ceiling_dir);
        } else {
            let canonicalized = gix_path::realpath(&ceiling_dir).unwrap_or(ceiling_dir);
            out.canonicalized.push(canonicalized);
        }
    }
    out
}

#[cfg(test)]
mod tests {

    #[test]
    #[cfg(unix)]
    fn parse_ceiling_dirs_from_environment_format() -> std::io::Result<()> {
        use std::{fs, os::unix::fs::symlink};

        use super::*;

        // Setup filesystem
        let dir = tempfile::tempdir().expect("success creating temp dir");
        let direct_path = dir.path().join("direct");
        let symlink_path = dir.path().join("symlink");
        fs::create_dir(&direct_path)?;
        symlink(&direct_path, &symlink_path)?;

        // Parse & build ceiling dirs string
        let symlink_str = symlink_path.to_str().expect("symlink path is valid utf8");
        let ceiling_dir_string = format!("{symlink_str}:relative::{symlink_str}/..");
        let ceiling_dirs = parse_ceiling_dirs(OsStr::new(ceiling_dir_string.as_str()));

        assert_eq!(
            ceiling_dirs.canonicalized.len() + ceiling_dirs.verbatim.len(),
            2,
            "Relative path is discarded"
        );
        assert_eq!(
            ceiling_dirs.canonicalized,
            vec![symlink_path.canonicalize().expect("symlink path exists")],
            "Symlinks are resolved"
        );
        assert_eq!(
            ceiling_dirs.verbatim,
            vec![symlink_path.join("..")],
            "After an empty item entries are kept exactly as spelled - neither the symlink nor the `..` is resolved"
        );

        dir.close()
    }

    #[test]
    #[cfg(windows)]
    fn parse_ceiling_dirs_from_environment_format() -> std::io::Result<()> {
        use std::{fs, os::windows::fs::symlink_dir};

        use super::*;

        // Setup filesystem
        let dir = tempfile::tempdir().expect("success creating temp dir");
        let direct_path = dir.path().join("direct");
        let symlink_path = dir.path().join("symlink");
        fs::create_dir(&direct_path)?;
        symlink_dir(&direct_path, &symlink_path)?;

        // Parse & build ceiling dirs string
        let symlink_str = symlink_path.to_str().expect("symlink path is valid utf8");
        let ceiling_dir_string = format!("{};relative;;{}\\..", symlink_str, symlink_str);
        let ceiling_dirs = parse_ceiling_dirs(OsStr::new(ceiling_dir_string.as_str()));

        assert_eq!(
            ceiling_dirs.canonicalized.len() + ceiling_dirs.verbatim.len(),
            2,
            "Relative path is discarded"
        );
        assert_eq!(ceiling_dirs.canonicalized, vec![direct_path], "Symlinks are resolved");
        assert_eq!(
            ceiling_dirs.verbatim,
            vec![symlink_path.join("..")],
            "After an empty item entries are kept exactly as spelled - neither the symlink nor the `..` is resolved"
        );

        dir.close()
    }
}
