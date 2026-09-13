use crate::gitcomp::*;

/// `subcommand_opts[]` (builtin/config.c:1630-1639).
pub(super) const SUBCOMMAND_OPTS: &[Opt] = &[
    OPT_SUBCOMMAND("list"),
    OPT_SUBCOMMAND("get"),
    OPT_SUBCOMMAND("set"),
    OPT_SUBCOMMAND("unset"),
    OPT_SUBCOMMAND("rename-section"),
    OPT_SUBCOMMAND("remove-section"),
    OPT_SUBCOMMAND("edit"),
];
