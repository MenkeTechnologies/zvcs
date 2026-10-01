use crate::gitcomp::*;

/// `options[]` (builtin/history.c:1205-1210, git 2.56).
pub(super) const HISTORY_OPTIONS: &[Opt] = &[
    OPT_SUBCOMMAND("drop"),
    OPT_SUBCOMMAND("fixup"),
    OPT_SUBCOMMAND("reword"),
    OPT_SUBCOMMAND("split"),
];
