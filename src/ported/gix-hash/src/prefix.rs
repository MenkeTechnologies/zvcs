use std::cmp::Ordering;

use crate::{ObjectId, Prefix, oid};

/// The error returned by [`Prefix::new()`].
#[derive(Debug, thiserror::Error)]
#[expect(missing_docs)]
pub enum Error {
    #[error(
        "The minimum hex length of a short object id is {}, got {hex_len}",
        Prefix::MIN_HEX_LEN
    )]
    TooShort { hex_len: usize },
    #[error("An object of kind {object_kind} cannot be larger than {} in hex, but {hex_len} was requested", object_kind.len_in_hex())]
    TooLong { object_kind: crate::Kind, hex_len: usize },
}

///
pub mod from_hex {
    /// The error returned by [`Prefix::from_hex`][super::Prefix::from_hex()].
    #[derive(Debug, Eq, PartialEq, thiserror::Error)]
    #[expect(missing_docs)]
    pub enum Error {
        #[error(
            "The minimum hex length of a short object id is {}, got {hex_len}",
            super::Prefix::MIN_HEX_LEN
        )]
        TooShort { hex_len: usize },
        #[error("An id cannot be larger than {} chars in hex, but {hex_len} was requested", crate::Kind::longest().len_in_hex())]
        TooLong { hex_len: usize },
        #[error("Invalid hex character")]
        Invalid,
    }
}

/// `bytes` extended with zeros to exactly `len` items, the way `git` sees an object id: a
/// `hash[GIT_MAX_RAWSZ]` array whose tail past the algorithm's length is zero-filled.
fn zero_padded(bytes: &[u8], len: usize) -> impl Iterator<Item = u8> + '_ {
    bytes.iter().copied().chain(std::iter::repeat(0)).take(len)
}

/// The byte at `idx`, or the zero `git` would have read from the padding past a shorter hash.
fn byte_or_zero(bytes: &[u8], idx: usize) -> u8 {
    bytes.get(idx).copied().unwrap_or(0)
}

impl Prefix {
    /// The smallest allowed prefix length below which chances for collisions are too high even in small repositories.
    pub const MIN_HEX_LEN: usize = 4;

    /// Create a new instance by taking a full `id` as input and truncating it to `hex_len`.
    ///
    /// For instance, with `hex_len` of 7 the resulting prefix is 3.5 bytes, or 3 bytes and 4 bits
    /// wide, with all other bytes and bits set to zero.
    pub fn new(id: &oid, hex_len: usize) -> Result<Self, Error> {
        if hex_len > id.kind().len_in_hex() {
            Err(Error::TooLong {
                object_kind: id.kind(),
                hex_len,
            })
        } else if hex_len < Self::MIN_HEX_LEN {
            Err(Error::TooShort { hex_len })
        } else {
            let mut prefix = ObjectId::null(id.kind());
            let b = prefix.as_mut_slice();
            let copy_len = hex_len.div_ceil(2);
            b[..copy_len].copy_from_slice(&id.as_bytes()[..copy_len]);
            if hex_len % 2 == 1 {
                b[hex_len / 2] &= 0xf0;
            }

            Ok(Prefix { bytes: prefix, hex_len })
        }
    }

    /// Returns the prefix as object id.
    ///
    /// Note that it may be deceptive to use given that it looks like a full
    /// object id, even though its post-prefix bytes/bits are set to zero.
    pub fn as_oid(&self) -> &oid {
        &self.bytes
    }

    /// Return the amount of hexadecimal characters that are set in the prefix.
    ///
    /// This gives the prefix a granularity of 4 bits.
    pub fn hex_len(&self) -> usize {
        self.hex_len
    }

    /// Provided with candidate id which is a full hash, determine how this prefix compares to it,
    /// only looking at the prefix bytes, ignoring everything behind that.
    ///
    /// The prefix may be *wider* than the candidate: [`Kind::from_hex_len()`][crate::Kind::from_hex_len()]
    /// answers `Sha256` for any hex length of 41 to 64, so a user-typed 41-character name yields a
    /// 32-byte prefix that is compared against 20-byte SHA-1 object ids. `git` reaches the same
    /// situation and reads straight through it: an object id there is a fixed-width, zero-filled
    /// `hash[GIT_MAX_RAWSZ]` array, so `match_hash()` sees zeros past the algorithm's own length.
    /// `oid` is a variable-length slice instead, so the padding has to be supplied here. Reading a
    /// too-wide prefix as "not equal" instead would be wrong, not merely safe: `git` resolves a
    /// 40-hex object id followed by zeros as that very object.
    pub fn cmp_oid(&self, candidate: &oid) -> Ordering {
        let common_len = self.hex_len / 2;
        let prefix = self.bytes.as_bytes();
        let candidate = candidate.as_bytes();

        prefix[..common_len]
            .iter()
            .copied()
            .cmp(zero_padded(candidate, common_len))
            .then(if self.hex_len % 2 == 1 {
                prefix[common_len].cmp(&(byte_or_zero(candidate, common_len) & 0xf0))
            } else {
                Ordering::Equal
            })
    }

    /// This prefix narrowed to `hex_len` hexadecimal characters, or a copy of `self` when it is
    /// already no wider than that.
    ///
    /// Pack lookups need this: `git` admits an object name of up to 64 hex characters whatever the
    /// repository's own hash is (`init_object_disambiguation()` bounds it by `GIT_MAX_HEXSZ`), but
    /// then clamps it to `hash_algo->hexsz` before scanning a pack index, so the characters past
    /// the repository's own hash width are simply dropped there. The loose path does not clamp,
    /// which is why the two disagree for a name like `<40-hex>f`.
    pub fn truncated(&self, hex_len: usize) -> Result<Self, Error> {
        if hex_len >= self.hex_len {
            Ok(*self)
        } else {
            Self::new(self.as_oid(), hex_len)
        }
    }

    /// Create an instance from the given hexadecimal prefix `value`, e.g. `35e77c16` would yield a `Prefix` with `hex_len()` = 8.
    /// Note that the minimum hex length is `4` - use [`Self::from_hex_nonempty()`].
    pub fn from_hex(value: &str) -> Result<Self, from_hex::Error> {
        let hex_len = value.len();
        if hex_len < Self::MIN_HEX_LEN {
            return Err(from_hex::Error::TooShort { hex_len });
        }
        Self::from_hex_nonempty(value)
    }

    /// Create an instance from the given hexadecimal prefix `value`, e.g. `35e` would yield a `Prefix` with `hex_len()` = 3.
    /// Note that this function supports all non-empty hex input - for a more typical implementation, use [`Self::from_hex()`].
    pub fn from_hex_nonempty(value: &str) -> Result<Self, from_hex::Error> {
        let hex_len = value.len();

        if hex_len > crate::Kind::longest().len_in_hex() {
            return Err(from_hex::Error::TooLong { hex_len });
        } else if hex_len == 0 {
            return Err(from_hex::Error::TooShort { hex_len });
        }

        let src = if value.len() % 2 == 0 {
            let mut out = Vec::from_iter(std::iter::repeat_n(0, value.len() / 2));
            faster_hex::hex_decode(value.as_bytes(), &mut out).map(move |_| out)
        } else {
            // TODO(perf): do without heap allocation here.
            let mut buf = [0u8; crate::Kind::longest().len_in_hex()];
            buf[..value.len()].copy_from_slice(value.as_bytes());
            buf[value.len()] = b'0';
            let src = &buf[..=value.len()];
            let mut out = Vec::from_iter(std::iter::repeat_n(0, src.len() / 2));
            faster_hex::hex_decode(src, &mut out).map(move |_| out)
        }
        .map_err(|e| match e {
            faster_hex::Error::InvalidChar | faster_hex::Error::Overflow => from_hex::Error::Invalid,
            faster_hex::Error::InvalidLength(_) => panic!("This is already checked"),
        })?;

        let mut bytes = ObjectId::null(crate::Kind::from_hex_len(value.len()).expect("hex-len is already checked"));
        let dst = bytes.as_mut_slice();
        let copy_len = src.len();
        dst[..copy_len].copy_from_slice(&src);

        Ok(Prefix { bytes, hex_len })
    }
}

/// Create an instance from the given hexadecimal prefix, e.g. `35e77c16` would yield a `Prefix`
/// with `hex_len()` = 8.
impl TryFrom<&str> for Prefix {
    type Error = from_hex::Error;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        Prefix::from_hex(value)
    }
}

impl std::fmt::Display for Prefix {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.bytes.to_hex_with_len(self.hex_len).fmt(f)
    }
}

impl From<ObjectId> for Prefix {
    fn from(oid: ObjectId) -> Self {
        Prefix {
            bytes: oid,
            hex_len: oid.kind().len_in_hex(),
        }
    }
}
