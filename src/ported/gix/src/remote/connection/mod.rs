#[cfg(feature = "async-network-client")]
use gix_transport::client::async_io::Transport;
#[cfg(feature = "blocking-network-client")]
use gix_transport::client::blocking_io::Transport;

use crate::{Remote, bstr::BString, types::RemoteDetached};

/// A function that performs a given credential action, trying to obtain credentials for an operation that needs it.
pub type AuthenticateFn<'a> = Box<dyn FnMut(gix_credentials::helper::Action) -> gix_credentials::protocol::Result + 'a>;

/// A type to represent an ongoing connection to a remote host, typically with the connection already established.
///
/// It can be used to perform a variety of operations with the remote without worrying about protocol details,
/// much like a remote procedure call.
pub struct Connection<'remote, 'auth, 'repo, T>
where
    T: Transport,
{
    pub(crate) remote: &'remote Remote<'repo>,
    pub(crate) authenticate: Option<AuthenticateFn<'auth>>,
    pub(crate) transport_options: Option<Box<dyn std::any::Any>>,
    pub(crate) transport: gix_protocol::SendFlushOnDrop<T>,
    pub(crate) handshake: Option<gix_protocol::Handshake>,
    pub(crate) trace: bool,
    /// Protocol-v2 server options to send with every request made through this connection.
    pub(crate) server_options: Vec<BString>,
    /// Ask the server for no progress when fetching; see [`gix_protocol::fetch::Options::no_progress`].
    pub(crate) no_progress: bool,
    /// Where the server's sideband messages go when fetching; see [`gix_protocol::fetch::Options::sideband`].
    pub(crate) sideband: Option<gix_protocol::fetch::Sideband>,
}

/// Like [`Connection`], but without borrowing its remote or repository.
pub(crate) struct ConnectionDetached<'a, T>
where
    T: Transport,
{
    pub(crate) remote: RemoteDetached,
    pub(crate) authenticate: Option<AuthenticateFn<'a>>,
    pub(crate) transport_options: Option<Box<dyn std::any::Any>>,
    pub(crate) transport: gix_protocol::SendFlushOnDrop<T>,
    pub(crate) handshake: Option<gix_protocol::Handshake>,
    pub(crate) trace: bool,
    /// Protocol-v2 server options to send with every request made through this connection.
    pub(crate) server_options: Vec<BString>,
    pub(crate) no_progress: bool,
    pub(crate) sideband: Option<gix_protocol::fetch::Sideband>,
}

mod access;

///
pub mod ref_map;

///
pub mod fetch;

///
#[cfg(any(feature = "blocking-network-client", feature = "async-network-client"))]
pub mod negotiate_only;
