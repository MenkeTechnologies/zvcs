use crate::gitcomp::*;

/// `builtin_hook_options[]` (builtin/hook.c:198-202).
pub(super) const BUILTIN_HOOK_OPTIONS: &[Opt] = &[
    OPT_SUBCOMMAND("run"),
    OPT_SUBCOMMAND("list"),
];
