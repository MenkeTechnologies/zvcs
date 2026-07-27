//! The object filter-spec grammar of a partial clone or fetch, as understood by `git`'s
//! `list-objects-filter-options.c` and transmitted verbatim as the `filter <spec>` argument of the
//! `fetch` command.
//!
//! Parsing exists so that an invalid spec is rejected locally with `git`'s own message, before a
//! connection is made, and so that callers can tell a *blob-less* clone (which needs a promisor
//! remote to stay usable) from one that merely trims history.

/// The error produced when a filter-spec cannot be parsed.
///
/// Its `Display` is byte-for-byte the message `git` prints, so callers can pass it straight through
/// to the user.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[expect(missing_docs)]
pub enum Error {
    #[error("invalid filter-spec '{spec}'")]
    Invalid { spec: String },
    #[error("expected 'tree:<depth>'")]
    ExpectedTreeDepth,
    #[error("'{value}' for 'object:type=<type>' is not a valid object type")]
    InvalidObjectType { value: String },
    #[error("sparse:path filters support has been dropped")]
    SparsePathDropped,
    #[error("expected something after combine:")]
    EmptyCombine,
    #[error("must escape char in sub-filter-spec: '{character}'")]
    ReservedCharacter { character: char },
}

/// A parsed filter-spec.
///
/// The variants are `git`'s `enum list_objects_filter_choice`, minus the `LOFC_DISABLED` sentinel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Spec {
    /// `blob:none` - omit every blob.
    BlobNone,
    /// `blob:limit=<n>` - omit blobs larger than `n` bytes.
    BlobLimit {
        /// The size limit in bytes, with `git`'s `k`/`m`/`g` suffixes already applied.
        limit: u64,
    },
    /// `tree:<depth>` - omit trees (and thus blobs) deeper than `depth`.
    TreeDepth {
        /// The maximum tree depth to transfer, where `0` omits all trees.
        depth: u64,
    },
    /// `sparse:oid=<blob-ish>` - keep only what the sparse-checkout patterns in that blob select.
    SparseOid {
        /// The name of the blob holding the patterns, resolved by the *server*.
        blob: String,
    },
    /// `object:type=<type>` - keep only objects of that type.
    ObjectType {
        /// One of `blob`, `tree`, `commit` or `tag`.
        kind: gix_object::Kind,
    },
    /// `combine:<spec>+<spec>…` - the intersection of two or more filters.
    Combine {
        /// The sub-filters, in the order given.
        specs: Vec<Spec>,
    },
}

impl Spec {
    /// Return `true` if this filter can withhold blobs, meaning the repository it produces is only
    /// usable with a promisor remote to fetch them back on demand.
    pub fn omits_blobs(&self) -> bool {
        match self {
            Spec::BlobNone
            | Spec::BlobLimit { .. }
            | Spec::TreeDepth { .. }
            | Spec::SparseOid { .. }
            | Spec::ObjectType { .. } => true,
            Spec::Combine { specs } => specs.iter().any(Spec::omits_blobs),
        }
    }
}

/// A validated filter-spec together with the exact text that is put on the wire and recorded in
/// `remote.<name>.partialclonefilter`.
///
/// This is `git`'s `struct list_objects_filter_options`, reduced to the two fields that leave the
/// process: the `choice` and the `filter_spec` string.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Filter {
    /// What the spec selects.
    pub choice: Spec,
    /// The spec as `expand_list_objects_filter_spec()` renders it: the text the user gave, except that
    /// `blob:limit=` is rewritten with its size suffix resolved to a byte count.
    spec: String,
}

impl Filter {
    /// The spec text to send as `filter <spec>` and to store in the configuration.
    pub fn as_str(&self) -> &str {
        &self.spec
    }
}

impl std::str::FromStr for Filter {
    type Err = Error;

    fn from_str(spec: &str) -> Result<Self, Self::Err> {
        let choice = parse(spec)?;
        // `expand_list_objects_filter_spec()` rewrites exactly one variant and passes every other spec
        // through unchanged - verified against git 2.55.0, which records `blob:limit=1k` as
        // `blob:limit=1024` but leaves `tree:1k` and `combine:…` alone.
        let spec = match &choice {
            Spec::BlobLimit { limit } => format!("blob:limit={limit}"),
            _ => spec.to_owned(),
        };
        Ok(Filter { choice, spec })
    }
}

/// Return `true` if a server with these `capabilities` accepts a `filter` argument on its `fetch` command.
///
/// A server that does not is not an error - `git`'s `transport.c` warns
/// `filtering not recognized by server, ignoring` and transfers everything - so callers that want to
/// reproduce that warning have to ask before fetching.
#[cfg(any(feature = "blocking-client", feature = "async-client"))]
pub fn is_supported(version: gix_transport::Protocol, capabilities: &gix_transport::client::Capabilities) -> bool {
    crate::Command::Fetch
        .default_features(version, capabilities)
        .iter()
        .any(|(name, _)| *name == "filter")
}

/// Characters `git`'s `has_reserved_character()` refuses in a `combine:` sub-spec because they would
/// have to be percent-encoded to survive the `+`-separated list.
const RESERVED_NON_WS: &str = "~`!@#$^&*()[]{}\\;'\",<>?";

/// Parse `spec` the way `gently_parse_list_objects_filter()` does.
pub fn parse(spec: &str) -> Result<Spec, Error> {
    let invalid = || Error::Invalid { spec: spec.to_owned() };

    if spec == "blob:none" {
        Ok(Spec::BlobNone)
    } else if let Some(v) = spec.strip_prefix("blob:limit=") {
        parse_ulong(v).map(|limit| Spec::BlobLimit { limit }).ok_or_else(invalid)
    } else if let Some(v) = spec.strip_prefix("tree:") {
        // Note the asymmetry with `blob:limit=`, which `git` shares: a malformed depth is its own
        // message rather than the generic "invalid filter-spec".
        parse_ulong(v)
            .map(|depth| Spec::TreeDepth { depth })
            .ok_or(Error::ExpectedTreeDepth)
    } else if let Some(v) = spec.strip_prefix("sparse:oid=") {
        Ok(Spec::SparseOid { blob: v.to_owned() })
    } else if spec.starts_with("sparse:path=") {
        Err(Error::SparsePathDropped)
    } else if let Some(v) = spec.strip_prefix("object:type=") {
        object_kind(v)
            .map(|kind| Spec::ObjectType { kind })
            .ok_or_else(|| Error::InvalidObjectType { value: v.to_owned() })
    } else if let Some(v) = spec.strip_prefix("combine:") {
        parse_combine(v)
    } else {
        Err(invalid())
    }
}

/// `parse_combine_filter()`: `+`-separated, percent-decoded sub-specs.
fn parse_combine(specs: &str) -> Result<Spec, Error> {
    if specs.is_empty() {
        return Err(Error::EmptyCombine);
    }
    specs
        .split('+')
        .map(|sub| {
            let decoded = percent_decode(sub);
            if let Some(character) = decoded.chars().find(|c| *c <= ' ' || RESERVED_NON_WS.contains(*c)) {
                return Err(Error::ReservedCharacter { character });
            }
            parse(&decoded)
        })
        .collect::<Result<Vec<_>, _>>()
        .map(|specs| Spec::Combine { specs })
}

/// `strbuf_add_percentdecode()` restricted to what a sub-filter-spec may contain: `%XX` becomes the
/// byte it names, and a truncated or non-hex escape is left alone, as `git` does.
fn percent_decode(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let bytes = input.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let hex = (i + 2 < bytes.len())
            .then(|| std::str::from_utf8(&bytes[i + 1..i + 3]).ok())
            .flatten()
            .filter(|_| bytes[i] == b'%')
            .and_then(|hex| u8::from_str_radix(hex, 16).ok());
        match hex {
            Some(byte) => {
                out.push(byte as char);
                i += 3;
            }
            None => {
                out.push(bytes[i] as char);
                i += 1;
            }
        }
    }
    out
}

/// `git_parse_ulong()`: a decimal number with an optional `k`, `m` or `g` scale suffix.
/// Anything else - a sign, trailing junk, an overflow - is a parse failure.
fn parse_ulong(input: &str) -> Option<u64> {
    let (digits, scale) = match input.as_bytes().last() {
        Some(b'k' | b'K') => (&input[..input.len() - 1], 1024),
        Some(b'm' | b'M') => (&input[..input.len() - 1], 1024 * 1024),
        Some(b'g' | b'G') => (&input[..input.len() - 1], 1024 * 1024 * 1024),
        _ => (input, 1),
    };
    digits
        .parse::<u64>()
        .ok()
        .filter(|_| digits.bytes().all(|b| b.is_ascii_digit()))
        .and_then(|n| n.checked_mul(scale))
}

/// `type_from_string_gently()` for the four object types a filter may name.
fn object_kind(input: &str) -> Option<gix_object::Kind> {
    Some(match input {
        "blob" => gix_object::Kind::Blob,
        "tree" => gix_object::Kind::Tree,
        "commit" => gix_object::Kind::Commit,
        "tag" => gix_object::Kind::Tag,
        _ => return None,
    })
}

