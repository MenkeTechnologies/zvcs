//! Worktree encodings are powered by the `encoding_rs` crate, which has a narrower focus than the `iconv` library. Thus this implementation
//! is inherently more limited but will handle the common cases.
//!
//! Note that for encoding to legacy formats, [additional normalization steps](https://docs.rs/encoding_rs/0.8.32/encoding_rs/#preparing-text-for-the-encoders)
//! can be taken, which we do not yet take unless there is specific examples or problems to solve.
//!
//! The UTF-16 and UTF-32 family is the exception `encoding_rs` cannot serve at all — it folds the
//! byte-order variants onto one value and encodes all of them as UTF-8 — so [`utf`] carries those
//! by hand, along with the byte-order-mark rules git applies around them.

///
pub mod encoding;

pub(crate) mod utf;

///
pub mod encode_to_git;
pub use encode_to_git::function::{encode_to_git, encode_to_git_by_name};

///
pub mod encode_to_worktree;
pub use encode_to_worktree::function::{encode_to_worktree, encode_to_worktree_by_name};
