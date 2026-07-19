//! git-compatible porcelain, served natively via the vendored gitoxide
//! crates. One module per subcommand, ported incrementally. Each is the
//! drop-in equivalent of the stock `git` subcommand of the same name, so
//! tools on PATH (RustRover, gh, cargo) see identical behavior against the
//! same on-disk `.git`.

mod rev_parse;
mod add;
mod blame;
mod branch;
mod cat_file;
mod checkout;
mod clone;
mod commit;
mod config;
mod describe;
mod diff;
mod fetch;
mod init;
mod log;
mod ls_files;
mod ls_tree;
mod merge;
mod mv;
mod pull;
mod push;
mod remote;
mod reset;
mod restore;
mod rev_list;
mod rm;
mod show;
mod stash;
mod status;
mod switch;
mod tag;

pub use rev_parse::rev_parse;
pub use add::add;
pub use blame::blame;
pub use branch::branch;
pub use cat_file::cat_file;
pub use checkout::checkout;
pub use clone::clone;
pub use commit::commit;
pub use config::config;
pub use describe::describe;
pub use diff::diff;
pub use fetch::fetch;
pub use init::init;
pub use log::log;
pub use ls_files::ls_files;
pub use ls_tree::ls_tree;
pub use merge::merge;
pub use mv::mv;
pub use pull::pull;
pub use push::push;
pub use remote::remote;
pub use reset::reset;
pub use restore::restore;
pub use rev_list::rev_list;
pub use rm::rm;
pub use show::show;
pub use stash::stash;
pub use status::status;
pub use switch::switch;
pub use tag::tag;
