use crate::gitcomp::*;

/// `options[]` (builtin/notes.c:1134-1148).
pub(super) const NOTES_OPTIONS: &[Opt] = &[
    OPT_STRING("ref"),
    OPT_SUBCOMMAND("list"),
    OPT_SUBCOMMAND("add"),
    OPT_SUBCOMMAND("copy"),
    OPT_SUBCOMMAND("append"),
    OPT_SUBCOMMAND("edit"),
    OPT_SUBCOMMAND("show"),
    OPT_SUBCOMMAND("merge"),
    OPT_SUBCOMMAND("remove"),
    OPT_SUBCOMMAND("prune"),
    OPT_SUBCOMMAND("get-ref"),
];
