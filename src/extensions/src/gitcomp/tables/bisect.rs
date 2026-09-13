use crate::gitcomp::*;

/// `options[]` (builtin/bisect.c:1452-1464).
pub(super) const BISECT_OPTIONS: &[Opt] = &[
    OPT_SUBCOMMAND("reset"),
    OPT_SUBCOMMAND("terms"),
    OPT_SUBCOMMAND("start"),
    OPT_SUBCOMMAND("next"),
    OPT_SUBCOMMAND("log"),
    OPT_SUBCOMMAND("replay"),
    OPT_SUBCOMMAND("skip"),
    OPT_SUBCOMMAND("visualize"),
    OPT_SUBCOMMAND("view"),
    OPT_SUBCOMMAND("run"),
];
