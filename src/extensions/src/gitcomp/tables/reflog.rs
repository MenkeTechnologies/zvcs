use crate::gitcomp::*;

/// `options[]` (builtin/reflog.c:473-482).
pub(super) const REFLOG_OPTIONS: &[Opt] = &[
    OPT_SUBCOMMAND("show"),
    OPT_SUBCOMMAND("list"),
    OPT_SUBCOMMAND("exists"),
    OPT_SUBCOMMAND("write"),
    OPT_SUBCOMMAND("delete"),
    OPT_SUBCOMMAND("drop"),
    OPT_SUBCOMMAND("expire"),
];
