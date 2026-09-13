use crate::gitcomp::*;

/// `options[]` (builtin/bundle.c:241-247).
pub(super) const BUNDLE_OPTIONS: &[Opt] = &[
    OPT_SUBCOMMAND("create"),
    OPT_SUBCOMMAND("verify"),
    OPT_SUBCOMMAND("list-heads"),
    OPT_SUBCOMMAND("unbundle"),
];
