//! Promisor remotes: the half of *partial clone* that makes the objects a clone skipped reachable again.
//!
//! A partial clone deliberately arrives without some objects, and records the remote it came from as a
//! *promisor* - a remote that promised to hand those objects over later. Reading one of them makes the
//! repository go back and fetch it on demand.
//!
//! See git's `Documentation/technical/partial-clone.adoc` and `promisor-remote.c`. This module is the
//! port of the client half: [`remotes()`] is `promisor_remote_init()`, and the hook installed by
//! [`install_hook()`] plays the role of `promisor_remote_get_direct()`, which `oid_object_info_extended()`
//! calls when an object is missing locally.

use crate::bstr::ByteSlice;

/// A remote that a partial clone may fall back on for objects it does not have.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Remote {
    /// The name of the remote, as in `remote.<name>.url`.
    pub name: String,
    /// The filter that was in effect when the partial clone was made, from `remote.<name>.partialclonefilter`.
    ///
    /// It is repeated on every lazy fetch so the remote doesn't start sending objects that were skipped
    /// on purpose, exactly as git's `--filter=blob:none` on the fetch it spawns.
    pub filter: Option<String>,
}

/// The promisor remotes configured for `repo`, in configuration order.
///
/// Two spellings are recognized, both of which `git` still reads:
///
/// * `remote.<name>.promisor = true` - what `partial_clone_register()` writes today.
/// * `extensions.partialClone = <name>` - the original, single-remote form.
pub fn remotes(repo: &crate::Repository) -> Vec<Remote> {
    let config = &repo.config.resolved;
    let filter_of = |name: &str| {
        config
            .string_by("remote", Some(crate::bstr::BStr::new(name)), "partialclonefilter")
            .map(|v| v.to_str_lossy().into_owned())
    };

    let mut remotes: Vec<Remote> = config
        .sections_by_name("remote")
        .into_iter()
        .flatten()
        .filter_map(|section| {
            let name = section.header().subsection_name()?.to_str().ok()?;
            config
                .boolean_by("remote", Some(crate::bstr::BStr::new(name)), "promisor")
                .ok()
                .flatten()
                .filter(|promisor| *promisor)
                .map(|_| Remote {
                    name: name.to_owned(),
                    filter: filter_of(name),
                })
        })
        .collect();

    if let Some(name) = config
        .string("extensions.partialClone")
        .map(|v| v.to_str_lossy().into_owned())
        .filter(|name| !name.is_empty())
    {
        if !remotes.iter().any(|r| r.name == name) {
            let filter = filter_of(&name);
            remotes.push(Remote { name, filter });
        }
    }
    remotes
}

/// Give `repo`'s object database a way to fetch objects it doesn't have from a promisor remote.
///
/// Does nothing when no promisor remote is configured, which is the case for every ordinary repository.
///
/// The hook re-opens the repository from `git_dir` when it fires rather than capturing this one: the
/// object database outlives the `Repository` handle, and capturing it would keep the two alive through
/// each other. git achieves the same separation by running the fetch in a subprocess.
#[cfg(feature = "blocking-network-client")]
pub(crate) fn install_hook(repo: &crate::Repository) {
    install_hook_for(repo, remotes(repo));
}

/// Like [`install_hook()`], but for promisor remotes the caller knows about.
///
/// A clone needs this: its object database is opened before `remote.<name>.promisor` is written, so
/// the checkout that immediately follows would otherwise find no way to obtain the blobs the filter
/// left behind.
#[cfg(feature = "blocking-network-client")]
pub(crate) fn install_hook_for(repo: &crate::Repository, promisors: Vec<Remote>) {
    if promisors.is_empty() {
        return;
    }
    let git_dir = repo.git_dir().to_owned();
    let open_options = repo.options.clone();
    repo.objects.store_ref().set_promisor(Box::new(move |ids| {
        let Ok(repo) = crate::open_opts(&git_dir, open_options.clone()) else {
            return false;
        };
        promisors
            .iter()
            .any(|promisor| fetch(&repo, promisor, ids).unwrap_or(false))
    }));
}

/// Obtain every object in `ids` that is not present locally, in a single fetch from the promisor remote.
///
/// Returns `true` if a fetch was made. This is the prefetch git performs in `check_updates()` before
/// writing a worktree: without it, each missing blob would cost its own round trip.
#[cfg(feature = "blocking-network-client")]
pub fn prefetch(repo: &crate::Repository, ids: impl IntoIterator<Item = gix_hash::ObjectId>) -> bool {
    use gix_object::Exists;

    let store = repo.objects.store_ref();
    if !store.has_promisor() {
        return false;
    }
    let mut missing: Vec<_> = ids.into_iter().filter(|id| !repo.objects.exists(id)).collect();
    missing.sort_unstable();
    missing.dedup();
    store.fetch_from_promisor(&missing)
}

/// The error produced by [`fetch()`].
#[cfg(feature = "blocking-network-client")]
#[derive(Debug, thiserror::Error)]
#[expect(missing_docs)]
pub enum Error {
    #[error(transparent)]
    FindRemote(#[from] crate::remote::find::existing::Error),
    #[error(transparent)]
    Connect(#[from] crate::remote::connect::Error),
    #[error(transparent)]
    Handshake(#[from] gix_protocol::handshake::Error),
    #[error(transparent)]
    Transport(#[from] gix_protocol::transport::client::Error),
    #[error(transparent)]
    ConfigureTransport(#[from] Box<dyn std::error::Error + Send + Sync + 'static>),
    #[error(transparent)]
    GatherTransportConfig(#[from] crate::config::transport::Error),
    #[error(transparent)]
    Fetch(#[from] gix_protocol::fetch::Error),
    #[error("Failed to mark the pack of a lazy fetch as promisor at \"{}\"", path.display())]
    WritePromisorFile {
        path: std::path::PathBuf,
        source: std::io::Error,
    },
}

/// Fetch exactly the objects named by `ids` from `promisor` into `repo`, returning `true` if a pack arrived.
///
/// This is git's
/// `git -c fetch.negotiationAlgorithm=noop fetch <remote> --no-tags --no-write-fetch-head --recurse-submodules=no --filter=<spec> --stdin`,
/// as `promisor-remote.c`'s `fetch_objects()` spawns it, done in-process:
///
/// * no negotiation - the wants are sent with `done` in a single round, which is what `noop` amounts to,
/// * no tags,
/// * no reference is updated, so there is no `FETCH_HEAD` and no tracking ref to write,
/// * the same filter the partial clone used, so the remote doesn't re-send what was skipped on purpose,
/// * the resulting pack is marked `.promisor`, because it too may lack objects.
#[cfg(feature = "blocking-network-client")]
pub fn fetch(repo: &crate::Repository, promisor: &Remote, ids: &[gix_hash::ObjectId]) -> Result<bool, Error> {
    use std::sync::atomic::AtomicBool;

    if ids.is_empty() {
        return Ok(false);
    }
    let remote = repo.find_remote(promisor.name.as_str())?;
    let mut con = remote.connect(crate::remote::Direction::Fetch)?;

    let mut credentials_storage;
    let url = con.transport.inner.to_url();
    let remote_name = crate::bstr::BStr::new(promisor.name.as_str());
    let transport_options = repo.transport_options(url.as_ref(), Some(remote_name))?;
    let authenticate = match con.authenticate.as_mut() {
        Some(f) => f,
        None => {
            credentials_storage = con.configured_credentials_for_current_url();
            &mut credentials_storage
        }
    };
    if let Some(config) = transport_options.as_ref() {
        con.transport.inner.configure(&**config)?;
    }

    // No `ls-refs`: protocol v2 lets the `fetch` command stand on its own, and we know the object ids
    // already. A v1 server sends its advertisement as part of the handshake, which is harmless here.
    let mut progress = crate::progress::Discard;
    let mut handshake = gix_protocol::handshake(
        &mut con.transport.inner,
        gix_transport::Service::UploadPack,
        authenticate,
        Vec::new(),
        &mut progress,
    )?;

    let write_pack_options = gix_pack::bundle::write::Options {
        thread_limit: None,
        index_version: gix_pack::index::Version::V2,
        iteration_mode: gix_pack::data::input::Mode::Verify,
        object_hash: repo.object_hash(),
        alloc_limit_bytes: repo.config.alloc_limit_bytes,
        compression: repo.config.loose_compression,
    };
    let should_interrupt = AtomicBool::new(false);
    let mut write_pack_bundle = None;
    let mut negotiate = WantOnly { ids };

    let outcome = gix_protocol::fetch(
        &mut negotiate,
        |reader, progress, should_interrupt| -> Result<bool, gix_pack::bundle::write::Error> {
            write_pack_bundle = Some(gix_pack::Bundle::write_to_directory(
                reader,
                Some(&repo.objects.store_ref().path().join("pack")),
                progress,
                should_interrupt,
                Some(Box::new(repo.objects.clone())),
                write_pack_options,
            )?);
            Ok(true)
        },
        &mut progress,
        &should_interrupt,
        gix_protocol::fetch::Context {
            handshake: &mut handshake,
            transport: &mut con.transport.inner,
            user_agent: repo.config.user_agent_tuple(),
            trace_packetlines: con.trace,
            server_options: Vec::new(),
        },
        gix_protocol::fetch::Options {
            shallow_file: repo.shallow_file(),
            shallow: &gix_protocol::fetch::Shallow::NoChange,
            tags: gix_protocol::fetch::Tags::None,
            reject_shallow_remote: false,
            filter: promisor.filter.as_deref(),
        },
    )?;

    if matches!(handshake.server_protocol_version, gix_protocol::transport::Protocol::V2) {
        gix_protocol::indicate_end_of_interaction(&mut con.transport.inner, con.trace).ok();
    }

    let Some(bundle) = write_pack_bundle.filter(|_| outcome.is_some()) else {
        return Ok(false);
    };
    // `write_promisor_file()` with no sought refs, i.e. an empty marker - the same file git's lazy
    // fetch leaves behind. The `.keep` file may go, as the objects are needed right now and nothing
    // is going to collect a pack we are about to read from.
    if let Some(index_path) = bundle.index_path.as_deref() {
        let path = index_path.with_extension("promisor");
        std::fs::write(&path, "").map_err(|err| Error::WritePromisorFile { path, source: err })?;
    }
    if let Some(keep_path) = bundle.keep_path.as_deref() {
        std::fs::remove_file(keep_path).ok();
    }
    Ok(bundle.index.num_objects > 0)
}

/// A [negotiator](gix_protocol::fetch::Negotiate) that asks for a fixed set of objects and nothing else.
///
/// It is git's `fetch.negotiationAlgorithm=noop`, which is what `fetch_objects()` forces for a promisor
/// fetch: sending `have` lines would be pointless, because what we are missing has nothing to do with
/// which commits we already share.
#[cfg(feature = "blocking-network-client")]
struct WantOnly<'a> {
    ids: &'a [gix_hash::ObjectId],
}

#[cfg(feature = "blocking-network-client")]
impl gix_protocol::fetch::Negotiate for WantOnly<'_> {
    fn mark_complete_and_common_ref(
        &mut self,
    ) -> Result<gix_protocol::fetch::negotiate::Action, gix_protocol::fetch::negotiate::Error> {
        Ok(gix_protocol::fetch::negotiate::Action::MustNegotiate {
            remote_ref_target_known: Vec::new(),
        })
    }

    fn add_wants(&mut self, arguments: &mut gix_protocol::fetch::Arguments, _remote_ref_target_known: &[bool]) -> bool {
        for id in self.ids {
            arguments.want(id);
        }
        true
    }

    fn one_round(
        &mut self,
        _state: &mut gix_protocol::fetch::negotiate::one_round::State,
        _arguments: &mut gix_protocol::fetch::Arguments,
        _previous_response: Option<&gix_protocol::fetch::Response>,
    ) -> Result<(gix_protocol::fetch::negotiate::Round, bool), gix_protocol::fetch::negotiate::Error> {
        Ok((
            gix_protocol::fetch::negotiate::Round {
                haves_sent: 0,
                in_vain: 0,
                haves_to_send: 0,
                previous_response_had_at_least_one_in_common: false,
            },
            true,
        ))
    }
}
