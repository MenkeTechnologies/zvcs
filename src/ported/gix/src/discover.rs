#![allow(clippy::result_large_err)]
use std::path::Path;

pub use gix_discover::*;

use crate::{
    ThreadSafeRepository,
    bstr::BString,
    config::tree::{Core, Key},
};

/// The error returned by [`crate::discover()`].
#[derive(Debug, thiserror::Error)]
#[expect(missing_docs)]
pub enum Error {
    #[error(transparent)]
    Discover(#[from] upwards::Error),
    #[error(transparent)]
    Open(#[from] crate::open::Error),
}

impl ThreadSafeRepository {
    /// Try to open a git repository in `directory` and search upwards through its parents until one is found,
    /// using default trust options which matters in case the found repository isn't owned by the current user.
    ///
    /// This is git's setup: `setup_git_directory_gently_1()` in `setup.c` reads `GIT_DIR` before it
    /// walks anywhere and returns `GIT_DIR_EXPLICIT` if it is set, and consults
    /// `GIT_CEILING_DIRECTORIES` and `GIT_DISCOVERY_ACROSS_FILESYSTEM` for the walk itself. Use
    /// [`discover_opts()`][Self::discover_opts()] to search without consulting the environment.
    pub fn discover(directory: impl AsRef<Path>) -> Result<Self, Error> {
        Self::discover_with_environment_overrides(directory)
    }

    /// Try to open a git repository in `directory` and search upwards through its parents until one is found,
    /// while applying `options`. Then use the `trust_map` to determine which of our own repository options to use
    /// for instantiations.
    ///
    /// Note that [trust overrides](crate::open::Options::with()) in the `trust_map` are not effective here and we will
    /// always override it with the determined trust value as per [gix_discover::upwards::Options::trust].
    /// This value, however, can be set to [assume a given trust level](gix_discover::upwards::TrustPolicy::Assume) to let
    /// callers control the trust level without re-determining it.
    pub fn discover_opts(
        directory: impl AsRef<Path>,
        options: upwards::Options<'_>,
        trust_map: gix_sec::trust::Mapping<crate::open::Options>,
    ) -> Result<Self, Error> {
        Self::discover_opts_with_work_tree(directory, options, trust_map, None)
    }

    /// [`discover_opts()`][Self::discover_opts()] with `work_tree_dir` naming the work tree outright,
    /// as `GIT_WORK_TREE` does.
    ///
    /// Both endings of git's discovery re-enter `setup_explicit_git_dir()` with the directory they
    /// found when that variable is set — `setup_discovered_git_dir()` at `setup.c:1216-1228` and
    /// `setup_bare_git_dir()` at `setup.c:1266-1274` — where the given work tree is installed before
    /// `core.bare` or `core.worktree` are consulted. So it replaces whatever the location implies,
    /// including the nothing that a bare repository implies.
    fn discover_opts_with_work_tree(
        directory: impl AsRef<Path>,
        options: upwards::Options<'_>,
        trust_map: gix_sec::trust::Mapping<crate::open::Options>,
        work_tree_dir: Option<std::path::PathBuf>,
    ) -> Result<Self, Error> {
        let _span = gix_trace::coarse!("ThreadSafeRepository::discover()");
        let (path, trust) = upwards_opts(directory.as_ref(), options)?;
        // Discovery landing on a git directory itself is git's `GIT_DIR_BARE`, and
        // `setup_bare_git_dir()` in `setup.c` turns the implicit work tree off for it: from inside
        // `<repo>/.git`, only `GIT_WORK_TREE` or `core.worktree` may still attach one.
        let implicit_work_tree = if matches!(path, gix_discover::repository::Path::Repository(_)) {
            crate::open::ImplicitWorkTree::None
        } else {
            crate::open::ImplicitWorkTree::ParentOfDotGitDir
        };
        let (git_dir, worktree_dir) = path.into_repository_and_work_tree_directories();
        let mut options = trust_map.into_value_by_level(trust);
        options.git_dir_trust = trust.into();
        options.implicit_work_tree = implicit_work_tree;
        // Note that we will adjust the `current_dir` later so it matches the value of `core.precomposeUnicode`.
        let current_dir = gix_fs::current_dir(false).map_err(upwards::Error::CurrentDir)?;
        // `set_git_work_tree()` (setup.c:1904) stores the real path of what it is handed, so a
        // relative work tree is resolved against the directory the search started in.
        let work_tree_dir = work_tree_dir
            .map(|wt| gix_path::normalize(wt.clone().into(), &current_dir).map_or(wt, std::borrow::Cow::into_owned));
        options.work_tree_is_explicit = work_tree_dir.is_some();
        options.current_dir = Some(current_dir);
        Self::open_from_paths(git_dir, work_tree_dir.or(worktree_dir), options).map_err(Into::into)
    }

    /// Try to open a git repository directly from the environment.
    /// If that fails, discover upwards from `directory` until one is found,
    /// while applying discovery options from the environment.
    ///
    /// For more, see [`ThreadSafeRepository::discover_with_environment_overrides_opts()`].
    pub fn discover_with_environment_overrides(directory: impl AsRef<Path>) -> Result<Self, Error> {
        Self::discover_with_environment_overrides_opts(
            directory,
            upwards::Options {
                // git turns a `GIT_CEILING_DIRECTORIES` that contains no ancestor of the search
                // directory into "no ceiling at all" (`ceil_offset = min_offset - 2` in
                // `setup_git_directory_gently_1()`), it does not refuse to search.
                match_ceiling_dir_or_error: false,
                ..Default::default()
            },
            Default::default(),
        )
    }

    /// Try to open a git repository directly from the environment, which reads `GIT_DIR`
    /// if it is set. If unset, discover upwards from `directory` until one is found,
    /// while applying `options` with overrides from the environment which includes:
    ///
    /// - `GIT_DISCOVERY_ACROSS_FILESYSTEM`
    /// - `GIT_CEILING_DIRECTORIES`
    ///
    /// Finally, use the `trust_map` to determine which of our own repository options to use
    /// based on the trust level of the effective repository directory.
    ///
    /// ### Note
    ///
    /// Consider to set [`match_ceiling_dir_or_error = false`](gix_discover::upwards::Options::match_ceiling_dir_or_error)
    /// to allow discovery if an outside environment variable sets non-matching ceiling directories for greater
    /// compatibility with Git.
    pub fn discover_with_environment_overrides_opts(
        directory: impl AsRef<Path>,
        mut options: upwards::Options<'_>,
        trust_map: gix_sec::trust::Mapping<crate::open::Options>,
    ) -> Result<Self, Error> {
        fn apply_additional_environment(mut opts: upwards::Options<'_>) -> upwards::Options<'_> {
            use crate::bstr::ByteVec;

            if let Some(cross_fs) = std::env::var_os("GIT_DISCOVERY_ACROSS_FILESYSTEM")
                .and_then(|v| Vec::from_os_string(v).ok().map(BString::from))
            {
                if let Ok(b) = gix_config::Boolean::try_from(cross_fs) {
                    opts.cross_fs = b.into();
                }
            }
            opts
        }

        if std::env::var_os("GIT_DIR").is_some() {
            return Self::open_with_environment_overrides(directory.as_ref(), trust_map).map_err(Error::Open);
        }

        options = apply_additional_environment(options.apply_environment());
        // `GIT_WORK_TREE` without `GIT_DIR`: git still walks upwards, then hands what it found to
        // `setup_explicit_git_dir()` (setup.c:1217, 1267), so the variable attaches a work tree to a
        // discovered repository — a bare one included.
        let work_tree_dir = std::env::var_os(Core::WORKTREE.the_environment_override()).map(std::path::PathBuf::from);
        Self::discover_opts_with_work_tree(directory, options, trust_map, work_tree_dir)
    }
}
