//! `reftable-error.h` and `error.c`: the library's error codes and their messages.

/// A reftable error, one per negative code of `enum reftable_error`.
///
/// The [`Display`](std::fmt::Display) text is `reftable_error_str()` (`error.c:14-45`),
/// which git prints verbatim in messages like `reftable: transaction failure: %s`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, thiserror::Error)]
pub enum Error {
    /// `-1`, the generic failure.
    #[error("general error")]
    General,
    /// `REFTABLE_IO_ERROR`: unexpected file system behavior.
    #[error("I/O error")]
    Io,
    /// `REFTABLE_FORMAT_ERROR`: format inconsistency on reading data.
    #[error("corrupt reftable file")]
    Format,
    /// `REFTABLE_NOT_EXIST_ERROR`: a table file listed in the stack does not exist.
    #[error("file does not exist")]
    NotExist,
    /// `REFTABLE_LOCK_ERROR`: trying to access locked data.
    #[error("data is locked")]
    Lock,
    /// `REFTABLE_API_ERROR`: misuse of the API.
    #[error("misuse of the reftable API")]
    Api,
    /// `REFTABLE_ZLIB_ERROR`: decompression error.
    #[error("zlib failure")]
    Zlib,
    /// `REFTABLE_EMPTY_TABLE_ERROR`: wrote a table without blocks.
    #[error("wrote empty table")]
    EmptyTable,
    /// `REFTABLE_REFNAME_ERROR`: invalid ref name.
    #[error("invalid refname")]
    Refname,
    /// `REFTABLE_ENTRY_TOO_BIG_ERROR`: an entry does not fit into a block.
    #[error("entry too large")]
    EntryTooBig,
    /// `REFTABLE_OUTDATED_ERROR`: trying to write out-of-date data.
    #[error("data concurrently modified")]
    Outdated,
    /// `REFTABLE_OUT_OF_MEMORY_ERROR`: an allocation failed.
    #[error("out of memory")]
    OutOfMemory,
}

impl Error {
    /// The C code of this error, as `enum reftable_error` defines it.
    pub fn code(self) -> i32 {
        match self {
            Error::General => -1,
            Error::Io => -2,
            Error::Format => -3,
            Error::NotExist => -4,
            Error::Lock => -5,
            Error::Api => -6,
            Error::Zlib => -7,
            Error::EmptyTable => -8,
            Error::Refname => -10,
            Error::EntryTooBig => -11,
            Error::Outdated => -12,
            Error::OutOfMemory => -13,
        }
    }
}

impl From<std::io::Error> for Error {
    fn from(_: std::io::Error) -> Self {
        Error::Io
    }
}

/// The result type of this crate.
pub type Result<T> = std::result::Result<T, Error>;
