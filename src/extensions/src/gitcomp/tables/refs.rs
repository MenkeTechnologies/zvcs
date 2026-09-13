use crate::gitcomp::*;

/// `opts[]` (builtin/refs.c:193-200).
pub(super) const REFS_OPTIONS: &[Opt] = &[
    OPT_SUBCOMMAND("migrate"),
    OPT_SUBCOMMAND("verify"),
    OPT_SUBCOMMAND("list"),
    OPT_SUBCOMMAND("exists"),
    OPT_SUBCOMMAND("optimize"),
];
