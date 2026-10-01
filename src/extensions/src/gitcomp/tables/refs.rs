use crate::gitcomp::*;

/// `opts[]` (builtin/refs.c:395-405, git 2.56).
pub(super) const REFS_OPTIONS: &[Opt] = &[
    OPT_SUBCOMMAND("migrate"),
    OPT_SUBCOMMAND("verify"),
    OPT_SUBCOMMAND("list"),
    OPT_SUBCOMMAND("exists"),
    OPT_SUBCOMMAND("optimize"),
    OPT_SUBCOMMAND("create"),
    OPT_SUBCOMMAND("delete"),
    OPT_SUBCOMMAND("update"),
    OPT_SUBCOMMAND("rename"),
];
