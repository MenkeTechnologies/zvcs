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

/// The `WWW-Authenticate` values of the HTTP response `err` reports, in order, or nothing when `err` is not an
/// HTTP error response.
///
/// http.c's `fwrite_wwwauth()` (http.c:220-317) keeps them for the credential helper's `wwwauth[]` lines.
pub fn www_authenticate(err: &std::io::Error) -> Vec<String> {
    #[cfg(feature = "http-client")]
    if let Some(response) = err
        .get_ref()
        .and_then(|inner| inner.downcast_ref::<blocking_io::http::ErrorResponse>())
    {
        return response.www_authenticate.clone();
    }
    let _ = err;
    Vec::new()
}
