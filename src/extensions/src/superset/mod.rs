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

pub mod analytics;
pub mod attach;
pub mod banner;
pub mod claim;
pub mod dashed;
pub mod doctor;
pub mod gitls;
pub mod hooks;
pub mod ledger;
pub mod lscolors;
pub mod manpage;
pub mod oplog;
pub mod select;
pub mod shell;
pub mod snapshot;
pub mod status;
pub mod trigger;
pub mod zforeach;
pub mod zhook;
pub mod zstash;
pub mod zup;
pub mod zworktree;
pub mod query;
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
pub use doctor::zdoctor;
pub use oplog::{zlog, zundo};
pub use snapshot::{zrestore, zsnapshot, zsnapshots};
pub use gitls::zls;
pub use shell::{
    zcat, zcd, zcp, zecho, zenv, zln, zmkdir, zmv, zpwd, zrm, ztouch, zunset,
};
pub use status::zstatus;
pub use trigger::{ztrigger, zwatch};
pub use zforeach::zforeach;
pub use zhook::zhook;
pub use zstash::{zstash, zstashes, zunstash};
pub use zup::zup;
pub use zworktree::zworktree;
pub use ledger::{zjob, zjobs, zreindex, zrepos};
pub use analytics::{zahead, zauthors, zbehind, zconflicts, zgrep, zhot};
pub use query::{zage, zbranches, zdirty, zheads, zpull, zremotes, zsize, ztags};
pub use queue::{zcommit, zpush};
pub use repl::zrepl;
pub use reconcile::reconcile_tree;
pub use zbump::{zbump, zbump_run, BumpOutcome};
pub use zdaemon::zdaemon;
pub use zsync::{reconcile_repo, reconcile_repo_local, zsync};
