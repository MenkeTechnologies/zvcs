use crate::gitcomp::*;

/// `options[]` (builtin/stash.c:2462-2477).
pub(super) const STASH_OPTIONS: &[Opt] = &[
    OPT_SUBCOMMAND("apply"),
    OPT_SUBCOMMAND("clear"),
    OPT_SUBCOMMAND("drop"),
    OPT_SUBCOMMAND("pop"),
    OPT_SUBCOMMAND("branch"),
    OPT_SUBCOMMAND("list"),
    OPT_SUBCOMMAND("show"),
    OPT_SUBCOMMAND("store"),
    OPT_SUBCOMMAND("create"),
    OPT_SUBCOMMAND("push"),
    OPT_SUBCOMMAND("export"),
    OPT_SUBCOMMAND("import"),
    OPT_SUBCOMMAND_F("save", PARSE_OPT_NOCOMPLETE),
];
