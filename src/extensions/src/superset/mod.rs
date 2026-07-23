//! The zvcs superset — coordination verbs stock git structurally cannot have.
//!
//! This is the world's-first layer: not "git in Rust" (gitoxide already is that),
//! but a VCS that solves the N-concurrent-agent + submodule pain of the meta repo.
//! Each verb lives in its own module:
//!
//!   * [`zdaemon`] — per-repo coordinator: a FIFO queue/barrier replaces git's
//!     `index.lock` flock, so a contended writer waits its turn instead of
//!     failing. Hosts background reconcile threads.
//!   * [`zsync`]   — reconcile every submodule to its tracked mainline
//!     (`origin/main`/`origin/master`) and keep it *attached* — detached HEAD
//!     never happens. Fast-forward only; never touches a dirty worktree.
//!   * [`zbump`]   — forward-only submodule gitlink bumps: stage a submodule
//!     pointer only when the new SHA is a descendant of the recorded one.

pub mod attach;
pub mod claim;
pub mod dashed;
pub mod hooks;
pub mod ledger;
pub mod oplog;
pub mod select;
pub mod snapshot;
pub mod status;
pub mod zforeach;
pub mod zhook;
pub mod zstash;
pub mod zup;
pub mod zworktree;
pub mod queue;
mod reconcile;
pub mod repl;
pub mod watch;
mod zbump;
pub mod zdaemon;
mod zsync;

pub use attach::{ensure_attached, Attached};
pub use claim::{zclaim, zunclaim, zwho};
pub use dashed::zdashed;
pub use oplog::{zlog, zundo};
pub use snapshot::{zrestore, zsnapshot, zsnapshots};
pub use status::zstatus;
pub use zforeach::zforeach;
pub use zhook::zhook;
pub use zstash::{zstash, zstashes, zunstash};
pub use zup::zup;
pub use zworktree::zworktree;
pub use ledger::{zjob, zjobs, zreindex, zrepos};
pub use queue::{zcommit, zpush};
pub use repl::zrepl;
pub use reconcile::reconcile_tree;
pub use zbump::{zbump, zbump_run, BumpOutcome};
pub use zdaemon::zdaemon;
pub use zsync::{reconcile_repo, reconcile_repo_local, zsync};
