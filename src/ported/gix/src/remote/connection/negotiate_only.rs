//! git's `git fetch --negotiate-only`: find out which commits the remote has in common with us,
//! without asking it for a single object.
//!
//! This is `negotiate_using_fetch()` (fetch-pack.c:2226-2331): `fetch` requests that carry no
//! `want` at all, only `wait-for-done` and the `have`s. `wait-for-done` keeps the server in the
//! acknowledgement phase instead of letting it decide it is `ready` and start a pack.
//!
//! There is no `ls-refs` beforehand: with nothing to want, there is nothing to map, which is why the
//! `have`s are the plain ancestry of the requested tips rather than the pruned set a normal fetch
//! would send.
//!
//! The rounds end when a request went out with no new `have` (or after MAX_IN_VAIN unacknowledged
//! ones once something was acknowledged), or when every tip reaches an acknowledged commit. The
//! request with no new `have` is still sent, and `upload-pack` answers a request that has neither
//! `have`s nor `done` with a `packfile` section, so a tip the remote shares nothing with ends the run
//! in `fatal: expected 'acknowledgments', received 'packfile'`, as it does in git.
use std::collections::HashSet;

use gix_protocol::fetch::negotiate;
#[cfg(feature = "async-network-client")]
use gix_transport::client::async_io::{ExtendedBufRead, Transport, TransportV2Ext};
#[cfg(feature = "blocking-network-client")]
use gix_transport::client::blocking_io::{ExtendedBufRead, Transport, TransportV2Ext};

use crate::{
    Progress,
    config::cache::util::ApplyLeniency,
    config::tree::Fetch,
    remote::{Connection, connection::ConnectionDetached},
};

/// `INITIAL_FLUSH` (fetch-pack.c:272): the haves in the first round.
const INITIAL_FLUSH: usize = 16;
/// `MAX_IN_VAIN` (fetch-pack.c:67): haves sent without an ACK after which the rounds stop.
const MAX_IN_VAIN: usize = 256;

/// The error returned by [`Connection::negotiate_only()`].
#[derive(Debug, thiserror::Error)]
#[expect(missing_docs)]
pub enum Error {
    #[error(transparent)]
    Handshake(#[from] gix_protocol::handshake::Error),
    #[error(transparent)]
    Transport(#[from] gix_protocol::transport::client::Error),
    #[error(transparent)]
    ConfigureTransport(Box<dyn std::error::Error + Send + Sync + 'static>),
    #[error("Could not read configuration")]
    Config(#[from] crate::config::Error),
    #[error(transparent)]
    ConfigureCredentials(#[from] crate::config::credential_helpers::Error),
    #[error(transparent)]
    Negotiate(#[from] negotiate::Error),
    #[error("Could not walk the commit graph to find what we have")]
    Graph(#[from] gix_negotiate::Error),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error("--negotiate-only requires protocol v2")]
    RequiresV2,
    #[error("server does not support wait-for-done")]
    WaitForDoneUnsupported,
    /// A `die()` of the response reader, its message exactly as git words it.
    #[error("{0}")]
    Protocol(String),
}

impl gix_protocol::transport::IsSpuriousError for Error {
    fn is_spurious(&self) -> bool {
        match self {
            Error::Transport(err) => err.is_spurious(),
            Error::Handshake(err) => err.is_spurious(),
            _ => false,
        }
    }
}

impl<T> Connection<'_, '_, '_, T>
where
    T: Transport,
{
    /// Ask the remote which of the commits reachable from `restrictions` it already has, and return
    /// them in the order it acknowledged them, repeats included.
    ///
    /// Nothing is fetched and no ref is touched.
    #[gix_protocol::maybe_async::maybe_async]
    pub async fn negotiate_only(
        self,
        progress: impl Progress,
        restrictions: negotiate::Restrictions,
    ) -> Result<Vec<gix_hash::ObjectId>, Error> {
        let repo = self.remote.repo;
        self.into_detached().negotiate_only(repo, progress, restrictions).await
    }
}

impl<T> ConnectionDetached<'_, T>
where
    T: Transport,
{
    #[gix_protocol::maybe_async::maybe_async]
    pub(crate) async fn negotiate_only(
        mut self,
        repo: &crate::Repository,
        mut progress: impl Progress,
        restrictions: negotiate::Restrictions,
    ) -> Result<Vec<gix_hash::ObjectId>, Error> {
        let _span = gix_trace::coarse!("remote::Connection::negotiate_only()");
        let mut credentials_storage;
        let url = self.transport.inner.to_url();
        let authenticate = match self.authenticate.as_mut() {
            Some(f) => f,
            None => {
                credentials_storage = self.configured_credentials_for_current_url(repo);
                &mut credentials_storage
            }
        };
        if self.transport_options.is_none() {
            self.transport_options = repo
                .transport_options(url.as_ref(), self.remote.name().map(crate::remote::Name::as_bstr))
                .map_err(|err| Error::ConfigureTransport(Box::new(err)))?;
        }
        if let Some(config) = self.transport_options.as_ref() {
            self.transport
                .inner
                .configure(&**config)
                .map_err(Error::ConfigureTransport)?;
        }
        let handshake = gix_protocol::handshake(
            &mut self.transport.inner,
            gix_transport::Service::UploadPack,
            authenticate,
            Vec::new(),
            &mut progress,
        )
        .await?;

        let fetch = gix_protocol::Command::Fetch;
        // Only a v2 server that advertises `wait-for-done` will stay in the acknowledgement phase for
        // a request without wants; anything else would answer with a pack we never asked for.
        // `fetch_refs_via_pack()` (transport.c:532-548) refuses the two cases separately,
        // then closes the connection at `cleanup:` without the flush `disconnect_git()`
        // would send (transport.c:574-580): the service reads EOF where it expected wants.
        let refusal = if handshake.server_protocol_version != gix_transport::Protocol::V2 {
            Some(Error::RequiresV2)
        } else if !handshake
            .capabilities
            .capability("fetch")
            .and_then(|c| c.supports("wait-for-done"))
            .unwrap_or(false)
        {
            Some(Error::WaitForDoneUnsupported)
        } else {
            None
        };
        if let Some(refusal) = refusal {
            // The `warning()` comes before that close, and the service's own complaint about the
            // EOF follows it once dropping the transport waits for the child.
            eprintln!("warning: {refusal}");
            self.transport.skip_end_of_interaction();
            return Err(refusal);
        }

        // `write_command_and_capabilities()` (connect.c) writes the same capability lines as any
        // other `fetch`; of the arguments, `negotiate_using_fetch()` writes only `wait-for-done`
        // and the haves (fetch-pack.c:2270-2276) — no `thin-pack`, `ofs-delta` or `done`.
        let mut features = fetch.default_features(handshake.server_protocol_version, &handshake.capabilities);
        features.push(repo.config.user_agent_tuple());
        features.extend(gix_protocol::command::server_options(
            &handshake.capabilities,
            &self.server_options,
        ));

        let mut negotiator = repo
            .config
            .resolved
            .string(Fetch::NEGOTIATION_ALGORITHM)
            .map(|n| Fetch::NEGOTIATION_ALGORITHM.try_into_negotiation_algorithm(n))
            .transpose()
            .with_leniency(repo.config.lenient_config)
            .map_err(crate::config::Error::from)?
            .unwrap_or(gix_negotiate::Algorithm::Consecutive)
            .into_negotiator();
        let cache = repo.commit_graph_if_enabled().ok().flatten();
        let mut graph = repo.revision_graph(cache.as_ref());
        // `mark_tips()` (fetch-pack.c:2246): without a ref-map there is nothing to mark complete, so
        // the tips are all the negotiator ever learns about.
        let tips: &[gix_hash::ObjectId] = restrictions.tips.as_deref().unwrap_or_default();
        for id in tips {
            negotiator.add_tip(*id, &mut graph)?;
        }
        // `add_haves()` calls `have_sent()` on each `--negotiation-include` commit as it writes it
        // (fetch-pack.c:1351-1364), which marks it common for the walk; the first round does so
        // before its first `next()`, so marking them here is the same thing.
        for id in &restrictions.always_have {
            negotiator.in_common_with_remote(*id, &mut graph)?;
        }

        let mut haves_to_send = INITIAL_FLUSH;
        let mut in_vain = 0;
        let mut seen_ack = false;
        let mut last_iteration = false;
        // `acked_commits`, in the order the ACKs arrived; the caller prints them as git's oidset
        // iterates them.
        let mut acked = Vec::new();
        // The commits that carry fetch-pack's COMMON flag (fetch-pack.c:2295-2305).
        let mut common = HashSet::new();
        while !last_iteration {
            let mut args = vec![crate::bstr::BString::from("wait-for-done")];
            // `add_haves()` (fetch-pack.c:1343-1376): the `--negotiation-include` commits lead every
            // request, then up to `haves_to_send` from the negotiator.
            for id in &restrictions.always_have {
                args.push(format!("have {id}").into());
            }
            let mut haves_added = 0;
            while let Some(id) = negotiator.next_have(&mut graph) {
                args.push(format!("have {}", id?).into());
                haves_added += 1;
                if haves_added >= haves_to_send {
                    break;
                }
            }
            haves_to_send = gix_negotiate::window_size(true, Some(haves_to_send));
            in_vain += haves_added;
            if haves_added == 0 || (seen_ack && in_vain >= MAX_IN_VAIN) {
                last_iteration = true;
            }

            let mut reader = self.transport.inner.invoke(
                fetch.as_str(),
                features.iter().filter(|(_, v)| v.is_some()).cloned(),
                Some(args.into_iter()),
                self.trace,
            )?;
            reader.reset(gix_transport::Protocol::V2);
            // `process_section_header(&reader, "acknowledgments", 0)` (fetch-pack.c:1464-1485).
            let header = match reader.peek_data_line().await {
                Some(Ok(Ok(line))) => Some(String::from_utf8_lossy(line).trim_end_matches('\n').to_owned()),
                Some(Ok(Err(err))) => return Err(Error::Protocol(err.to_string())),
                Some(Err(err)) => return Err(err.into()),
                None => None,
            };
            match header.as_deref() {
                Some("acknowledgments") => {
                    let mut line = String::new();
                    reader.readline_str(&mut line).await?;
                }
                Some(other) => {
                    return Err(Error::Protocol(format!(
                        "expected 'acknowledgments', received '{other}'"
                    )));
                }
                None => return Err(Error::Protocol("expected 'acknowledgments'".into())),
            }
            // `process_ack()` (fetch-pack.c:1487-1541), called until it reports no further ACK.
            let mut received_ready = false;
            let mut line = String::new();
            loop {
                line.clear();
                if reader.readline_str(&mut line).await? == 0 {
                    break;
                }
                let line = line.trim_end_matches('\n');
                if line == "NAK" {
                    continue;
                }
                if let Some(arg) = line.strip_prefix("ACK ") {
                    let hex_len = repo.object_hash().len_in_hex();
                    if let Some(id) = arg
                        .get(..hex_len)
                        .and_then(|hex| gix_hash::ObjectId::from_hex(hex.as_bytes()).ok())
                    {
                        negotiator.in_common_with_remote(id, &mut graph)?;
                        if repo.find_commit(id).is_ok() {
                            common.insert(id);
                        }
                        in_vain = 0;
                        seen_ack = true;
                        acked.push(id);
                    }
                    continue;
                }
                if line == "ready" {
                    received_ready = true;
                    continue;
                }
                return Err(Error::Protocol(format!("unexpected acknowledgment line: '{line}'")));
            }
            let at_delimiter = reader.stopped_at() == Some(gix_transport::client::MessageKind::Delimiter);
            if received_ready && !at_delimiter {
                return Err(Error::Protocol("expected packfile to be sent after 'ready'".into()));
            }
            if !received_ready && at_delimiter {
                return Err(Error::Protocol(
                    "expected no other sections to be sent after no 'ready'".into(),
                ));
            }
            if received_ready {
                return Err(Error::Protocol("unexpected 'ready' from remote".into()));
            }
            drop(reader);
            if can_all_from_reach_with_flag(repo, tips, &common) {
                last_iteration = true;
            }
        }
        // `fetch_refs_via_pack()` closes the connection at `cleanup:` without a flush
        // (transport.c:574-580).
        self.transport.skip_end_of_interaction();
        Ok(acked)
    }
}

/// `can_all_from_reach_with_flag(&nt_object_array, COMMON, REACH_SCRATCH, 0, min_generation)`
/// (commit-reach.c:955-1055): whether every tip, peeled, reaches a commit in `common`.
///
/// The DFS from each tip in turn shares one visited set, and RESULT spreads one level up the stack
/// as the walk unwinds. A tip that is not a commit is passed over; one that cannot be read fails
/// the check. git also stops below `min_generation`, the lowest generation among the acknowledged
/// commits; generations only fall along ancestry, so no commit below it can reach one of them and
/// leaving the cut-off out changes nothing but the time spent.
fn can_all_from_reach_with_flag(
    repo: &crate::Repository,
    tips: &[gix_hash::ObjectId],
    common: &HashSet<gix_hash::ObjectId>,
) -> bool {
    let mut visited = HashSet::<gix_hash::ObjectId>::new();
    let mut result = HashSet::<gix_hash::ObjectId>::new();
    let mut list = Vec::new();
    for tip in tips {
        let Ok(object) = repo.find_object(*tip).and_then(|o| o.peel_tags_to_end().map_err(Into::into)) else {
            return false;
        };
        if object.kind != crate::object::Kind::Commit {
            continue;
        }
        list.push(object.id);
    }
    let parents_of = |id: gix_hash::ObjectId| -> Option<Vec<gix_hash::ObjectId>> {
        let commit = repo.find_commit(id).ok()?;
        Some(commit.parent_ids().map(crate::Id::detach).collect())
    };
    let reaches = |id: &gix_hash::ObjectId, result: &HashSet<gix_hash::ObjectId>| common.contains(id) || result.contains(id);
    for tip in list {
        visited.insert(tip);
        let mut stack = vec![tip];
        while let Some(&top) = stack.last() {
            if reaches(&top, &result) {
                stack.pop();
                if let Some(&below) = stack.last() {
                    result.insert(below);
                }
                continue;
            }
            let mut descended = false;
            for parent in parents_of(top).unwrap_or_default() {
                if reaches(&parent, &result) {
                    result.insert(top);
                }
                if visited.insert(parent) {
                    if repo.find_commit(parent).is_err() {
                        continue;
                    }
                    stack.push(parent);
                    descended = true;
                    break;
                }
            }
            if !descended {
                stack.pop();
            }
        }
        if !reaches(&tip, &result) {
            return false;
        }
    }
    true
}
