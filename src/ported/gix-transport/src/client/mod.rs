///
#[cfg(feature = "async-client")]
pub mod async_io;

mod traits;
pub use traits::TransportWithoutIO;

///
#[cfg(feature = "blocking-client")]
pub mod blocking_io;

///
pub mod capabilities;
#[doc(inline)]
pub use capabilities::Capabilities;

mod non_io_types;
pub use gix_sec::identity::Account;
pub use non_io_types::{Error, HangUp, MessageKind, WriteMode};

///
#[cfg(any(feature = "blocking-client", feature = "async-client"))]
pub mod git;

/// The first packet line of an advertisement, with an end of stream before it
/// turned into [`HangUp::InitialContact`].
pub(crate) fn initial_contact<T>(read: std::io::Result<T>) -> Result<T, Error> {
    read.map_err(|err| match err.kind() {
        std::io::ErrorKind::UnexpectedEof => Error::HungUp(HangUp::InitialContact),
        _ => Error::Io(err),
    })
}

/// A v2 capability line, with an end of stream before the flush packet turned
/// into [`HangUp::CapabilitiesFlush`].
pub(crate) fn capabilities_v2_line<T>(read: std::io::Result<T>) -> Result<T, Error> {
    read.map_err(|err| match err.kind() {
        std::io::ErrorKind::UnexpectedEof => Error::HungUp(HangUp::CapabilitiesFlush),
        _ => Error::Io(err),
    })
}
