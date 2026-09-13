use crate::gitcomp::*;

/// `options[]` (builtin/remote.c:1939-1953).
pub(super) const REMOTE_OPTIONS: &[Opt] = &[
    OPT__VERBOSE(),
    OPT_SUBCOMMAND("add"),
    OPT_SUBCOMMAND("rename"),
    OPT_SUBCOMMAND_F("rm", PARSE_OPT_NOCOMPLETE),
    OPT_SUBCOMMAND("remove"),
    OPT_SUBCOMMAND("set-head"),
    OPT_SUBCOMMAND("set-branches"),
    OPT_SUBCOMMAND("get-url"),
    OPT_SUBCOMMAND("set-url"),
    OPT_SUBCOMMAND("show"),
    OPT_SUBCOMMAND("prune"),
    OPT_SUBCOMMAND("update"),
];
