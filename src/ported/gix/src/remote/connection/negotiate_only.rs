//! git's `git fetch --negotiate-only`: find out which commits the remote has in common with us,
//! without asking it for a single object.
//!
//! This is a fetch request that carries no `want` at all. `wait-for-done` keeps the server in the
//! acknowledgement phase instead of letting it decide it is `ready` and start a pack, so the exchange
//! ends when we run out of `have`s to offer rather than when the server has heard enough.
//!
//! There is no `ls-refs` beforehand: with nothing to want, there is nothing to map, which is why the
//! `have`s are the plain ancestry of the requested tips rather than the pruned set a normal fetch
//! would send.
//!
//! ### Deviation
//!
//! Stock git 2.55.0 sends one more request than it has `have`s for whenever a requested tip is left
//! unacknowledged, and that empty request is what makes `upload-pack` start a `packfile` section.
//! Its reader is still looking for `acknowledgments`, so the command dies with
//! `fatal: expected 'acknowledgments', received 'packfile'` and exit code 128 — for example under
//! `--negotiation-restrict='refs/heads/*'` against a remote that only shares one of those branches.
//! That is a defect rather than a contract: the documented job of the option is to print the common
//! ancestors, so this implementation stops once the negotiator is exhausted, prints them, and exits
//! zero.
use gix_protocol::fetch::negotiate;
#[cfg(feature = "async-network-client")]
use gix_transport::client::async_io::Transport;
#[cfg(feature = "blocking-network-client")]
use gix_transport::client::blocking_io::Transport;

use crate::{
    Progress,
    config::cache::util::ApplyLeniency,
    config::tree::Fetch,
    remote::{Connection, connection::ConnectionDetached},
};

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
    Response(#[from] gix_protocol::fetch::response::Error),
    #[error("The server does not support the 'wait-for-done' capability")]
    WaitForDoneUnsupported,
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
    /// them in the order it acknowledged them.
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
        if handshake.server_protocol_version != gix_transport::Protocol::V2
            || !handshake
                .capabilities
                .capability("fetch")
                .and_then(|c| c.supports("wait-for-done"))
                .unwrap_or(false)
        {
            return Err(Error::WaitForDoneUnsupported);
        }

        let mut features = fetch.default_features(handshake.server_protocol_version, &handshake.capabilities);
        features.push(repo.config.user_agent_tuple());
        features.extend(gix_protocol::command::server_options(
            &handshake.capabilities,
            &self.server_options,
        ));
        let mut arguments =
            gix_protocol::fetch::Arguments::new(handshake.server_protocol_version, features, self.trace);
        arguments.add_feature("wait-for-done");

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
        // Without a ref-map there is nothing to mark complete, so the tips are all the negotiator ever
        // learns about — which is why a `--negotiate-only` run walks further back than a fetch would.
        for id in restrictions.tips.iter().flatten() {
            negotiator.add_tip(*id, &mut graph)?;
        }
        for id in &restrictions.always_have {
            negotiator.in_common_with_remote(*id, &mut graph)?;
        }

        let is_stateless =
            arguments.is_stateless(!self.transport.inner.connection_persists_across_multiple_requests());
        let mut state = negotiate::one_round::State::new(is_stateless);
        let mut previous_response = None::<gix_protocol::fetch::Response>;
        let mut common = Vec::new();
        loop {
            let (round, is_done) = negotiate::one_round(
                negotiator.as_mut(),
                &mut graph,
                &mut state,
                &mut arguments,
                previous_response.as_ref(),
                &restrictions.always_have,
            )?;
            // Nothing left to offer. Sending an empty request would only ask the server to build a
            // pack, which is the one thing this operation must not do.
            if round.haves_sent == 0 && restrictions.always_have.is_empty() {
                break;
            }
            let mut reader = arguments.send(&mut self.transport.inner, false).await?;
            let response = gix_protocol::fetch::Response::from_line_reader(
                handshake.server_protocol_version,
                &mut reader,
                false,
                true,
            )
            .await?;
            drop(reader);
            for ack in response.acknowledgements() {
                if let gix_protocol::fetch::response::Acknowledgement::Common(id) = ack {
                    common.push(*id);
                }
            }
            previous_response = Some(response);
            if is_done {
                break;
            }
        }
        gix_protocol::indicate_end_of_interaction(&mut self.transport.inner, self.trace)
            .await
            .ok();
        // git prints them in `oidset` iteration order, which is a hash order it never promises; a
        // stable one is the closest thing to it that can be relied on.
        common.sort();
        common.dedup();
        Ok(common)
    }
}
