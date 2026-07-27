//! An implementation of the `git` transport layer, abstracting over all of its [versions][Protocol].
//!
//! Use `client::blocking_io::connect()` or `client::async_io::connect()` to establish a connection.
//!
//! All git transports are supported, including `ssh`, `git`, `http` and `https`, as well as local repository paths.
//! ## Feature Flags
#![cfg_attr(
    all(doc, feature = "document-features"),
    doc = ::document_features::document_features!()
)]
#![cfg_attr(all(doc, feature = "document-features"), feature(doc_cfg))]
#![deny(missing_docs)]
#![forbid(unsafe_code)]

#[cfg(feature = "async-trait")]
pub use async_trait;
pub use bstr;
#[cfg(feature = "futures-io")]
pub use futures_io;
pub use gix_packetline as packetline;

/// The version of the way client and server communicate.
#[derive(Default, PartialEq, Eq, Debug, Hash, Ord, PartialOrd, Clone, Copy)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum Protocol {
    /// Version 0 is like V1, but doesn't show capabilities at all, at least when hosted without `git-daemon`.
    V0 = 0,
    /// Version 1 was the first one conceived, is stateful, and our implementation was seen to cause deadlocks. Prefer V2
    V1 = 1,
    /// A command-based and stateless protocol with clear semantics, and the one to use assuming the server isn't very old.
    /// This is the default.
    #[default]
    V2 = 2,
}

/// Restrict the IP addresses a connection may use, from git's `--ipv4`/`--ipv6`.
///
/// This is git's `enum transport_family` minus its `TRANSPORT_FAMILY_ALL`, which every API here
/// spells as `None` instead. The transports that open a socket themselves honour it directly, the
/// ones that spawn a program pass the equivalent flag on; `file://` has no socket and ignores it,
/// exactly as git's `connect.c` does.
#[derive(PartialEq, Eq, Debug, Hash, Ord, PartialOrd, Clone, Copy)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum AddressFamily {
    /// `TRANSPORT_FAMILY_IPV4`: only use IPv4 addresses (`AF_INET`).
    V4,
    /// `TRANSPORT_FAMILY_IPV6`: only use IPv6 addresses (`AF_INET6`).
    V6,
}

impl AddressFamily {
    /// The `ssh` command-line flag git's `push_ssh_options()` appends for this family.
    pub fn as_ssh_flag(&self) -> &'static str {
        match self {
            AddressFamily::V4 => "-4",
            AddressFamily::V6 => "-6",
        }
    }

    /// Whether `addr` belongs to this family, the check `getaddrinfo`'s `hints.ai_family` performs
    /// for git.
    pub fn matches(&self, addr: &std::net::SocketAddr) -> bool {
        match self {
            AddressFamily::V4 => addr.is_ipv4(),
            AddressFamily::V6 => addr.is_ipv6(),
        }
    }
}

/// The kind of service to invoke on the client or the server side.
#[derive(PartialEq, Eq, Debug, Hash, Ord, PartialOrd, Clone, Copy)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum Service {
    /// The service sending packs from a server to the client. Used for fetching pack data.
    UploadPack,
    /// The service receiving packs produced by the client, who sends a pack to the server.
    ReceivePack,
}

impl Service {
    /// Render this instance as a string recognized by the git transport layer, like `git-upload-pack`.
    pub fn as_str(&self) -> &'static str {
        match self {
            Service::ReceivePack => "git-receive-pack",
            Service::UploadPack => "git-upload-pack",
        }
    }

    /// Render this instance as a subcommand understood by the `git` program, like `upload-pack`.
    pub fn as_git_subcommand(&self) -> &'static str {
        self.as_str()
            .strip_prefix("git-")
            .expect("all services are 'git-*' subcommands")
    }
}

mod traits {
    use std::convert::Infallible;

    /// An error which can tell whether it's worth retrying to maybe succeed next time.
    pub trait IsSpuriousError: std::error::Error {
        /// Return `true` if retrying might result in a different outcome due to IO working out differently.
        fn is_spurious(&self) -> bool {
            false
        }
    }

    impl IsSpuriousError for Infallible {}

    impl IsSpuriousError for std::io::Error {
        fn is_spurious(&self) -> bool {
            // TODO: also include the new special Kinds (currently unstable)
            use std::io::ErrorKind::*;
            match self.kind() {
                Unsupported | WriteZero | InvalidInput | InvalidData | WouldBlock | AlreadyExists
                | AddrNotAvailable | NotConnected | Other | PermissionDenied | NotFound => false,
                Interrupted | UnexpectedEof | OutOfMemory | TimedOut | BrokenPipe | AddrInUse | ConnectionAborted
                | ConnectionReset | ConnectionRefused => true,
                _ => false,
            }
        }
    }
}
pub use traits::IsSpuriousError;

///
pub mod client;
