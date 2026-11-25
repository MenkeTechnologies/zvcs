/// The error returned by [`fetch()`](crate::fetch()).
#[derive(Debug, thiserror::Error)]
#[expect(missing_docs)]
pub enum Error {
    #[error("Could not decode server reply")]
    FetchResponse(#[from] crate::fetch::response::Error),
    #[error(transparent)]
    Negotiate(#[from] crate::fetch::negotiate::Error),
    #[error(transparent)]
    Client(#[from] crate::transport::client::Error),
    #[error("Server lack feature {feature:?}: {description}")]
    MissingServerFeature {
        feature: &'static str,
        description: &'static str,
    },
    #[error("Could not write 'shallow' file to incorporate remote updates after fetching")]
    WriteShallowFile(#[from] gix_shallow::write::Error),
    #[error("Could not read 'shallow' file to send current shallow boundary")]
    ReadShallowFile(#[from] gix_shallow::read::Error),
    #[error("'shallow' file could not be locked in preparation for writing changes")]
    LockShallowFile(#[from] gix_lock::acquire::Error),
    #[error("Receiving objects from shallow remotes is prohibited due to the value of `clone.rejectShallow`")]
    RejectShallowRemote,
    #[error("Failed to consume the pack sent by the remote")]
    ConsumePack(#[source] Box<dyn std::error::Error + Send + Sync + 'static>),
    #[error("Failed to read remaining bytes in stream")]
    ReadRemainingBytes(#[source] std::io::Error),
    /// `send_request()` could not write a v0/v1 request over a connection that persists:
    /// `die_errno(_("unable to write to remote"))` (fetch-pack.c:254-264). A caller under `fetch`'s
    /// `SIGPIPE` ignore (builtin/fetch.c:2494-2498) reports it as such; one with `SIGPIPE` at its
    /// default was already killed by the signal the `write()` raised.
    #[error("unable to write to remote")]
    WriteRequest(#[source] std::io::Error),
}

impl Error {
    /// Route a failed write of a v0/v1 request on a persistent connection to [`Error::WriteRequest`].
    pub(crate) fn from_request_write(err: crate::transport::client::Error, stateless: bool) -> Self {
        match err {
            crate::transport::client::Error::Io(io) if !stateless => Error::WriteRequest(io),
            other => Error::Client(other),
        }
    }
}

impl crate::transport::IsSpuriousError for Error {
    fn is_spurious(&self) -> bool {
        match self {
            Error::FetchResponse(err) => err.is_spurious(),
            Error::Client(err) => err.is_spurious(),
            _ => false,
        }
    }
}
