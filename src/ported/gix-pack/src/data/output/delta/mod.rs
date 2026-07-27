//! Delta compression for packs being written.
//!
//! Two halves, matching git's own split:
//!
//! * [`encode`] is `diff-delta.c` — given a base buffer and a target buffer,
//!   produce the delta byte stream that rebuilds the target.
//! * [`search`] is the delta-search half of `builtin/pack-objects.c` — decide
//!   *which* pairs are worth deltifying, honouring `pack.window`, `pack.depth`,
//!   `pack.windowMemory`, `pack.deltaCacheSize`, `pack.deltaCacheLimit` and
//!   `pack.threads`.
//!
//! Together they are what turns a pack of standalone objects into one the size
//! git writes. The read side already existed: [`crate::data::delta::apply`]
//! consumes exactly what [`encode::Index::create`] produces.

///
pub mod encode;
///
pub mod search;

pub use encode::Index;
pub use search::{Delta, Islands, Object, Options, find_deltas, name_hash};
