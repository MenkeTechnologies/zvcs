use crate::config;

/// The error returned by [`receive()`](super::Prepare::receive()).
// TODO: remove unused variants
#[derive(Debug, thiserror::Error)]
#[expect(missing_docs)]
pub enum Error {
    #[error(transparent)]
    Fetch(#[from] gix_protocol::fetch::Error),
    #[error("The value to configure pack threads should be 0 to auto-configure or the amount of threads to use")]
    PackThreads(#[from] config::unsigned_integer::Error),
    #[error("The value to configure the pack index version should be 1 or 2")]
    PackIndexVersion(#[from] config::key::GenericError),
    #[error("Cannot fetch from a remote that uses {remote} while local repository uses {local} for object hashes")]
    IncompatibleObjectHash {
        local: gix_hash::Kind,
        remote: gix_hash::Kind,
    },
    #[error(transparent)]
    LoadAlternates(#[from] gix_odb::store::load_index::Error),
    #[error(transparent)]
    Client(#[from] gix_protocol::transport::client::Error),
    #[error(transparent)]
    UpdateRefs(#[from] super::refs::update::Error),
    #[error("Failed to remove .keep file at \"{}\"", path.display())]
    RemovePackKeepFile {
        path: std::path::PathBuf,
        source: std::io::Error,
    },
    #[error("None of the refspec(s) {} matched any of the {num_remote_refs} refs on the remote", refspecs.iter().map(|r| r.to_ref().instruction().to_bstring().to_string()).collect::<Vec<_>>().join(", "))]
    NoMapping {
        refspecs: Vec<gix_refspec::RefSpec>,
        num_remote_refs: usize,
    },
    #[error("Could not obtain configuration to learn if shallow remotes should be rejected")]
    RejectShallowRemoteConfig(#[from] config::boolean::Error),
    #[error(transparent)]
    NegotiationAlgorithmConfig(#[from] config::key::GenericErrorWithValue),
    #[error("Could not decide what to do with the shallow boundary the remote sent")]
    UpdateShallow(#[from] super::shallow::Error),
    #[error("Failed to mark the pack of a partial fetch as promisor at \"{}\"", path.display())]
    WritePromisorFile {
        path: std::path::PathBuf,
        source: std::io::Error,
    },
    /// The connectivity check could not be carried out. Every variant names the connectivity check
    /// itself, except the object-database one, which is passed through verbatim so a local failure
    /// like a too-small slotmap keeps its own reason instead of being dressed up as a fetch problem.
    #[error(transparent)]
    CheckConnected(#[from] super::connected::Error),
    /// The remote's answer left out objects the refs it offered need, so no ref may be stored.
    ///
    /// git's `store_updated_refs()` turns this into `error: <url> did not send all necessary
    /// objects` and abandons the fetch before its ref transaction.
    #[error("did not send all necessary objects")]
    NotConnected,
    /// The pack stream ended before `get_pack()` could take all of it, which git dies of (fetch-pack.c:960-1100).
    /// What it dies with depends on which child `get_pack()` chose, so the details are handed to the caller.
    #[error("the pack stream ended early")]
    PackStream(Box<PackStream>),
}

impl gix_protocol::transport::IsSpuriousError for Error {
    fn is_spurious(&self) -> bool {
        match self {
            Error::Fetch(err) => err.is_spurious(),
            Error::Client(err) => err.is_spurious(),
            _ => false,
        }
    }
}

/// How the sideband stream of a pack ended short of what `get_pack()` needed (fetch-pack.c:960-1100).
///
/// `sideband_demux()` runs `recv_sideband()` over the response, which stops at a flush, a delimiter or a
/// response-end packet alike (sideband.c:333-346), and at the end of the stream with
/// `fetch-pack: unexpected disconnect while reading sideband packet` (sideband.c:325-331), already printed by the time
/// this is returned. Its output is the pack's child's input, which then sees the end of its input.
#[derive(Debug)]
pub struct PackStream {
    /// The protocol version the pack was received with.
    pub protocol: gix_protocol::transport::Protocol,
    /// The first pack bytes, up to the 12 of the header `read_pack_header()` reads.
    pub header: Vec<u8>,
    /// Every pack byte that arrived, kept for a pack whose header announces fewer objects than the
    /// `fetch.unpackLimit` in effect, which `get_pack()` gives to `unpack-objects`.
    pub retained: Option<Vec<u8>>,
    /// The demultiplexer reached the end of the stream rather than one of the packets that end it, or the remote
    /// sent a band-3 message, which `finish_async()` reports as `error in sideband demultiplexer`
    /// (fetch-pack.c:1098-1099).
    pub disconnected: bool,
    /// The pack could not be read to its end, the child's failure.
    pub consumer_failed: bool,
    /// A protocol v2 response over a stateless connection whose pack ended at the response-end packet, so
    /// `do_check_stateless_delimiter()` reads the end of the stream in its place (fetch-pack.c:1847).
    pub response_end_consumed: bool,
    /// The pack that was written when the child succeeded.
    pub bundle: Option<gix_pack::bundle::write::Outcome>,
}
