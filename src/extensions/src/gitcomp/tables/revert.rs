//! `run_sequencer()` (builtin/revert.c:110-172), shared by `cmd_revert()` and
//! `cmd_cherry_pick()`: `base_options[]`, concatenated with the action's own
//! `cp_extra[]` (builtin/revert.c:149-170), is what `parse_options()` receives
//! (builtin/revert.c:172).
use crate::gitcomp::*;

/// `base_options[]` (builtin/revert.c:120-147).
pub(super) const BASE_OPTIONS: &[Opt] = &[
    OPT_CMDMODE("quit"),
    OPT_CMDMODE("continue"),
    OPT_CMDMODE("abort"),
    OPT_CMDMODE("skip"),
    OPT_CLEANUP(),
    OPT_BOOL("no-commit"),
    OPT_BOOL("edit"),
    OPT_NOOP_NOARG(NULL),
    OPT_BOOL("signoff"),
    OPT_CALLBACK("mainline"),
    OPT_RERERE_AUTOUPDATE(),
    OPT_STRING("strategy"),
    OPT_STRVEC("strategy-option"),
    option(Type::String, "gpg-sign", PARSE_OPT_OPTARG),
];

/// The `REPLAY_PICK` `cp_extra[]` (builtin/revert.c:151-161).
pub(super) const CP_EXTRA_PICK: &[Opt] = &[
    OPT_BOOL(NULL),
    OPT_BOOL("ff"),
    OPT_BOOL("allow-empty"),
    OPT_BOOL("allow-empty-message"),
    OPT_BOOL("keep-redundant-commits"),
    OPT_CALLBACK_F("empty", PARSE_OPT_NONEG),
];

/// The `REPLAY_REVERT` `cp_extra[]` (builtin/revert.c:164-168).
pub(super) const CP_EXTRA_REVERT: &[Opt] = &[
    OPT_BOOL("reference"),
];
