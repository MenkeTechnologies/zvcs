use std::{any::Any, borrow::Cow, error::Error, io::Write};

use bstr::{BStr, BString, ByteVec};

use crate::{
    Protocol, Service,
    client::{
        self,
        blocking_io::{RequestWriter, SetServiceResponse},
        capabilities::blocking_recv::Handshake,
        git::{self, ConnectionState},
    },
    packetline::{
        PacketLineRef,
        blocking_io::{StreamingPeekableIter, Writer},
    },
};

/// A TCP connection to either a `git` daemon or a spawned `git` process.
///
/// When connecting to a daemon, additional context information is sent with the first line of the handshake. Otherwise that
/// context is passed using command line arguments to a [spawned `git` process][crate::client::blocking_io::file::SpawnProcessOnDemand].
pub struct Connection<R, W> {
    pub(in crate::client) writer: W,
    pub(in crate::client) line_provider: StreamingPeekableIter<R>,
    pub(in crate::client) state: ConnectionState,
}

impl<R, W> Connection<R, W> {
    /// Optionally set the URL to be returned when asked for it if `Some` or calculate a default for `None`.
    ///
    /// The URL is required as parameter for authentication helpers which are called in transports
    /// that support authentication. Even though plain git transports don't support that, this
    /// may well be the case in custom transports.
    pub fn custom_url(mut self, url: Option<BString>) -> Self {
        self.state.custom_url = url;
        self
    }

    /// Return the inner reader and writer
    pub fn into_inner(self) -> (R, W) {
        (self.line_provider.into_inner(), self.writer)
    }
}

impl<R, W> client::TransportWithoutIO for Connection<R, W>
where
    R: std::io::Read,
    W: std::io::Write,
{
    fn to_url(&self) -> Cow<'_, BStr> {
        self.state.custom_url.as_ref().map_or_else(
            || {
                let mut possibly_lossy_url = self.state.path.clone();
                possibly_lossy_url.insert_str(0, "file://");
                Cow::Owned(possibly_lossy_url)
            },
            |url| Cow::Borrowed(url.as_ref()),
        )
    }

    fn connection_persists_across_multiple_requests(&self) -> bool {
        true
    }

    fn configure(&mut self, _config: &dyn Any) -> Result<(), Box<dyn Error + Send + Sync + 'static>> {
        Ok(())
    }
}

impl<R, W> client::blocking_io::Transport for Connection<R, W>
where
    R: std::io::Read,
    W: std::io::Write,
{
    fn handshake<'a>(
        &mut self,
        service: Service,
        extra_parameters: &'a [(&'a str, Option<&'a str>)],
    ) -> Result<SetServiceResponse<'_>, client::Error> {
        if self.state.mode == git::ConnectMode::Daemon {
            let mut line_writer = Writer::new(&mut self.writer);
            line_writer.enable_binary_mode();
            line_writer.write_all(&git::message::connect(
                service,
                self.state.desired_version,
                &self.state.path,
                self.state.virtual_host.as_ref(),
                extra_parameters,
            ))?;
            line_writer.flush()?;
        }

        let Handshake {
            capabilities,
            refs,
            protocol: actual_protocol,
        } = Handshake::from_lines_with_version_detection(&mut self.line_provider)?;
        Ok(SetServiceResponse {
            actual_protocol,
            capabilities,
            refs,
        })
    }

    fn request(
        &mut self,
        write_mode: client::WriteMode,
        on_into_read: client::MessageKind,
        trace: bool,
    ) -> Result<RequestWriter<'_>, client::Error> {
        Ok(RequestWriter::new_from_bufread(
            &mut self.writer,
            Box::new(self.line_provider.as_read_without_sidebands()),
            write_mode,
            on_into_read,
            trace,
        ))
    }
}

impl<R, W> Connection<R, W>
where
    R: std::io::Read,
    W: std::io::Write,
{
    /// Create a connection from the given `read` and `write`, asking for `desired_version` as preferred protocol
    /// and the transfer of the repository at `repository_path`.
    ///
    /// `virtual_host` along with a port to which to connect to, while `mode` determines the kind of endpoint to connect to.
    /// If `trace` is `true`, all packetlines received or sent will be passed to the facilities of the `gix-trace` crate.
    pub fn new(
        read: R,
        write: W,
        desired_version: Protocol,
        repository_path: impl Into<BString>,
        virtual_host: Option<(impl Into<String>, Option<u16>)>,
        mode: git::ConnectMode,
        trace: bool,
    ) -> Self {
        Connection {
            writer: write,
            line_provider: StreamingPeekableIter::new(read, &[PacketLineRef::Flush], trace),
            state: ConnectionState {
                path: repository_path.into(),
                virtual_host: virtual_host.map(|(h, p)| (h.into(), p)),
                desired_version,
                custom_url: None,
                mode,
            },
        }
    }
    pub(crate) fn new_for_spawned_process(
        reader: R,
        writer: W,
        desired_version: Protocol,
        repository_path: impl Into<BString>,
        trace: bool,
    ) -> Self {
        Self::new(
            reader,
            writer,
            desired_version,
            repository_path,
            None::<(&str, _)>,
            git::ConnectMode::Process,
            trace,
        )
    }
}

///
pub mod connect {
    use std::net::{TcpStream, ToSocketAddrs};

    use bstr::BString;

    use super::Connection;
    use crate::client::git;

    /// The error used in [`connect()`].
    #[derive(Debug, thiserror::Error)]
    #[expect(missing_docs)]
    pub enum Error {
        #[error("An IO error occurred when connecting to the server")]
        Io(#[from] std::io::Error),
        #[error("Could not parse {host:?} as virtual host with format <host>[:port]")]
        VirtualHostInvalid { host: String },
        #[error("Unable to look up {host}:{port} with an {family} address")]
        NoAddressOfFamily {
            host: String,
            port: u16,
            family: &'static str,
        },
    }

    impl crate::IsSpuriousError for Error {
        fn is_spurious(&self) -> bool {
            match self {
                Error::Io(err) => err.is_spurious(),
                _ => false,
            }
        }
    }

    fn parse_host(input: String) -> Result<(String, Option<u16>), Error> {
        let mut tokens = input.splitn(2, ':');
        Ok(match (tokens.next(), tokens.next()) {
            (Some(host), None) => (host.to_owned(), None),
            (Some(host), Some(port)) => (
                host.to_owned(),
                Some(port.parse().map_err(|_| Error::VirtualHostInvalid { host: input })?),
            ),
            _ => unreachable!("we expect at least one token, the original string"),
        })
    }

    /// Connect to a git daemon running on `host` and optionally `port` and a repository at `path`.
    ///
    /// Use `desired_version` to specify a preferred protocol to use, knowing that it can be downgraded by a server not supporting it.
    /// If `trace` is `true`, all packetlines received or sent will be passed to the facilities of the `gix-trace` crate.
    /// `address_family` restricts the addresses that may be used, like git's `--ipv4`/`--ipv6`.
    pub fn connect(
        host: &str,
        path: BString,
        desired_version: crate::Protocol,
        port: Option<u16>,
        trace: bool,
        address_family: Option<crate::AddressFamily>,
    ) -> Result<Connection<TcpStream, TcpStream>, Error> {
        let vhost_port = port;
        let port = port.unwrap_or(9418);
        // git's `git_tcp_connect_sock()` narrows the resolution with `hints.ai_family` and then walks
        // every remaining address, connecting to the first one that answers; only when all of them
        // failed does it report an error. Both halves matter: without the walk a host whose first
        // address is unreachable never connects.
        let candidates: Vec<_> = (host, port)
            .to_socket_addrs()?
            .filter(|addr| address_family.is_none_or(|family| family.matches(addr)))
            .collect();
        let mut last_err = None;
        let mut stream = None;
        for addr in &candidates {
            match TcpStream::connect_timeout(addr, std::time::Duration::from_secs(5)) {
                Ok(s) => {
                    stream = Some(s);
                    break;
                }
                Err(err) => last_err = Some(err),
            }
        }
        let read = match (stream, last_err) {
            (Some(s), _) => s,
            (None, Some(err)) => return Err(err.into()),
            // Resolution succeeded but nothing of the requested family came back, which is what
            // `getaddrinfo()` turns into a lookup failure for git.
            (None, None) => {
                return Err(Error::NoAddressOfFamily {
                    host: host.to_owned(),
                    port,
                    family: match address_family {
                        Some(crate::AddressFamily::V4) => "IPv4",
                        Some(crate::AddressFamily::V6) => "IPv6",
                        None => "IP",
                    },
                })
            }
        };
        let write = read.try_clone()?;
        let vhost = std::env::var("GIT_OVERRIDE_VIRTUAL_HOST")
            .ok()
            .map(parse_host)
            .transpose()?
            .unwrap_or_else(|| (host.to_owned(), vhost_port));
        Ok(Connection::new(
            read,
            write,
            desired_version,
            path,
            Some(vhost),
            git::ConnectMode::Daemon,
            trace,
        ))
    }
}

pub use connect::connect;
