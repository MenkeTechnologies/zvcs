#![allow(clippy::result_large_err)]
use crate::{bstr::BString, remote};

#[cfg(feature = "async-network-client")]
use gix_transport::client::async_io::Transport;
#[cfg(feature = "blocking-network-client")]
use gix_transport::client::blocking_io::Transport;

type ConfigureRemoteFn =
    Box<dyn FnMut(crate::Remote<'_>) -> Result<crate::Remote<'_>, Box<dyn std::error::Error + Send + Sync>>>;
#[cfg(any(feature = "async-network-client", feature = "blocking-network-client"))]
type ConfigureConnectionFn = Box<
    dyn FnMut(
        &mut remote::Connection<'_, '_, '_, Box<dyn Transport + Send>>,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>>,
>;

/// A utility to collect configuration on how to fetch from a remote and initiate a fetch operation. It will delete the newly
/// created repository on when dropped without successfully finishing a fetch.
#[must_use]
pub struct PrepareFetch {
    /// A freshly initialized repository which is owned by us, or `None` if it was handed to the user
    repo: Option<crate::Repository>,
    /// The name of the remote, which defaults to `origin` if not overridden.
    remote_name: Option<BString>,
    /// Additional config `values` that are applied in-memory before starting the fetch process.
    config_overrides: Vec<BString>,
    /// A function to configure a remote prior to fetching a pack.
    configure_remote: Option<ConfigureRemoteFn>,
    /// A function to configure a connection before using it.
    #[cfg(any(feature = "async-network-client", feature = "blocking-network-client"))]
    configure_connection: Option<ConfigureConnectionFn>,
    /// Options for preparing a fetch operation.
    #[cfg(any(feature = "async-network-client", feature = "blocking-network-client"))]
    fetch_options: remote::ref_map::Options,
    /// How to build the transport, notably which program to run in place of `git-upload-pack`.
    #[cfg(any(feature = "async-network-client", feature = "blocking-network-client"))]
    connect_options: remote::connect::Options,
    /// Protocol-v2 server options to send with every request.
    #[cfg(any(feature = "async-network-client", feature = "blocking-network-client"))]
    server_options: Vec<BString>,
    /// The url to clone from
    #[cfg_attr(not(feature = "blocking-network-client"), allow(dead_code))]
    url: gix_url::Url,
    /// How to handle shallow clones
    #[cfg_attr(not(feature = "blocking-network-client"), allow(dead_code))]
    shallow: remote::fetch::Shallow,
    /// The name of the reference to fetch. If `None`, the reference pointed to by `HEAD` will be checked out.
    #[cfg_attr(not(feature = "blocking-network-client"), allow(dead_code))]
    ref_name: Option<gix_ref::PartialName>,
    /// If `true`, drop removes the entire worktree. Otherwise leave it alone.
    remove_worktree_on_drop: bool,
    /// The object filter of a partial clone (git's `--filter`), if any.
    #[cfg_attr(not(feature = "blocking-network-client"), allow(dead_code))]
    filter: Option<gix_protocol::fetch::filter::Filter>,
}

/// The error returned by [`PrepareFetch::new()`].
#[derive(Debug, thiserror::Error)]
#[expect(missing_docs)]
pub enum Error {
    #[error(transparent)]
    Init(#[from] crate::init::Error),
    #[error(transparent)]
    CommitterOrFallback(#[from] crate::config::commit_signature::Error),
    #[error(transparent)]
    UrlParse(#[from] gix_url::parse::Error),
    #[error("Failed to turn a the relative file url \"{}\" into an absolute one", url.to_bstring())]
    CanonicalizeUrl {
        url: gix_url::Url,
        source: gix_path::realpath::Error,
    },
}

/// Instantiation
impl PrepareFetch {
    /// Create a new repository at `path` with `create_opts` which is ready to clone from `url`, possibly after making additional adjustments to
    /// configuration and settings.
    ///
    /// Note that this is merely a handle to perform the actual connection to the remote, and if any of it fails the freshly initialized repository
    /// will be removed automatically as soon as this instance drops.
    ///
    /// # Deviation
    ///
    /// Similar to `git`, a missing user name and email configuration is not terminal and we will fill it in with dummy values. However,
    /// instead of deriving values from the system, ours are hardcoded to indicate what happened.
    #[expect(
        clippy::result_large_err,
        reason = "will be removed once `gix-error` is used consistently"
    )]
    pub fn new<Url, E>(
        url: Url,
        path: impl AsRef<std::path::Path>,
        kind: crate::create::Kind,
        create_opts: crate::create::Options,
        open_opts: crate::open::Options,
    ) -> Result<Self, Error>
    where
        Url: TryInto<gix_url::Url, Error = E>,
        gix_url::parse::Error: From<E>,
    {
        Self::new_inner(
            url.try_into().map_err(gix_url::parse::Error::from)?,
            path.as_ref(),
            kind,
            create_opts,
            open_opts,
        )
    }

    #[expect(
        clippy::result_large_err,
        reason = "will be removed once `gix-error` is used consistently"
    )]
    fn new_inner(
        mut url: gix_url::Url,
        path: &std::path::Path,
        kind: crate::create::Kind,
        mut create_opts: crate::create::Options,
        open_opts: crate::open::Options,
    ) -> Result<Self, Error> {
        if create_opts.destination_must_be_empty.is_none() {
            create_opts.destination_must_be_empty = Some(true);
        }

        // Capture this before init_opts creates `.git`, otherwise the check below would see our own files.
        let remove_worktree_on_drop = match std::fs::read_dir(path) {
            Ok(mut entries) => entries.next().is_none(),
            // Non-existent destinations will be created by init_opts.
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => true,
            // If we can't verify emptiness, keep cleanup conservative and leave the destination untouched.
            Err(_) => false,
        };

        let mut repo = crate::ThreadSafeRepository::init_opts(path, kind, create_opts, open_opts)?.to_thread_local();
        // `builtin/clone.c:1057` records `absolute_pathdup(repo_name)`: the path made absolute
        // against the current directory and nothing else. Resolving it instead — symlinks, `.`,
        // `..` — writes a different string into `remote.<name>.url` and into the
        // `clone: from <url>` reflog message than stock git leaves behind, so `git clone ./src`
        // would record `<cwd>/src` where git records `<cwd>/./src`.
        url.absolutize(repo.options.current_dir_or_empty());
        repo.committer_or_set_generic_fallback()?;
        Ok(PrepareFetch {
            url,
            #[cfg(any(feature = "async-network-client", feature = "blocking-network-client"))]
            fetch_options: Default::default(),
            repo: Some(repo),
            config_overrides: Vec::new(),
            remote_name: None,
            configure_remote: None,
            #[cfg(any(feature = "async-network-client", feature = "blocking-network-client"))]
            configure_connection: None,
            #[cfg(any(feature = "async-network-client", feature = "blocking-network-client"))]
            connect_options: Default::default(),
            #[cfg(any(feature = "async-network-client", feature = "blocking-network-client"))]
            server_options: Vec::new(),
            shallow: remote::fetch::Shallow::NoChange,
            ref_name: None,
            remove_worktree_on_drop,
            filter: None,
        })
    }
}

/// A utility to collect configuration on how to perform a checkout into a working tree,
/// and when dropped without checking out successfully the fetched repository will be deleted from disk.
#[must_use]
#[cfg(feature = "worktree-mutation")]
#[derive(Debug)]
pub struct PrepareCheckout {
    /// A freshly initialized repository which is owned by us, or `None` if it was successfully checked out.
    pub(self) repo: Option<crate::Repository>,
    /// The name of the reference to check out. If `None`, the reference pointed to by `HEAD` will be checked out.
    pub(self) ref_name: Option<gix_ref::PartialName>,
    /// If `true`, drop removes the entire worktree. Otherwise leave it alone.
    pub(self) remove_worktree_on_drop: bool,
}

fn cleanup_clone_destination_on_drop(repo: &crate::Repository, remove_worktree_on_drop: bool) {
    let path_to_remove = if remove_worktree_on_drop {
        Some(repo.workdir().unwrap_or_else(|| repo.path()))
    } else {
        // The destination held pre-existing user files. Leave everything, including the `.git` we created,
        // so the user can inspect or clean up the partially cloned repository with Git tooling.
        None
    };
    if let Some(path_to_remove) = path_to_remove {
        std::fs::remove_dir_all(path_to_remove).ok();
    }
}

// This module encapsulates functionality that works with both feature toggles. Can be combined with `fetch`
// once async and clone are a thing.
#[cfg(any(feature = "async-network-client", feature = "blocking-network-client"))]
mod access_feat {
    use super::Transport;
    use crate::clone::PrepareFetch;

    /// Builder
    impl PrepareFetch {
        /// Set a callback to use for configuring the connection to use right before connecting to the remote.
        ///
        /// It is most commonly used for custom configuration.
        // TODO: tests
        pub fn configure_connection(
            mut self,
            f: impl FnMut(
                &mut crate::remote::Connection<'_, '_, '_, Box<dyn Transport + Send>>,
            ) -> Result<(), Box<dyn std::error::Error + Send + Sync>>
            + 'static,
        ) -> Self {
            self.configure_connection = Some(Box::new(f));
            self
        }

        /// Set additional options to adjust parts of the fetch operation that are not affected by the git configuration.
        pub fn with_fetch_options(mut self, opts: crate::remote::ref_map::Options) -> Self {
            self.fetch_options = opts;
            self
        }

        /// Adjust how the transport itself is built, e.g. to run a different program in place of
        /// `git-upload-pack` (git's `--upload-pack`).
        pub fn with_connect_options(mut self, opts: crate::remote::connect::Options) -> Self {
            self.connect_options = opts;
            self
        }

        /// Send `options` as protocol-v2 `server-option` lines with every request (git's `--server-option`).
        pub fn with_server_options(mut self, options: Vec<crate::bstr::BString>) -> Self {
            self.server_options = options;
            self
        }
    }
}

///
#[cfg(any(feature = "async-network-client-async-std", feature = "blocking-network-client"))]
pub mod fetch;

mod access;

///
#[cfg(feature = "worktree-mutation")]
pub mod checkout;
