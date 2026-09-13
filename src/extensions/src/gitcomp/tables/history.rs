use crate::gitcomp::*;

/// `options[]` (builtin/history.c:990-995).
pub(super) const HISTORY_OPTIONS: &[Opt] = &[
    OPT_SUBCOMMAND("fixup"),
    OPT_SUBCOMMAND("reword"),
    OPT_SUBCOMMAND("split"),
];
