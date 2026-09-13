use crate::gitcomp::*;

/// `builtin_gc_options[]` (builtin/gc.c:863-892).
pub(super) const BUILTIN_GC_OPTIONS: &[Opt] = &[
    OPT__QUIET(),
    option(Type::String, "prune", PARSE_OPT_OPTARG),
    OPT_BOOL("cruft"),
    OPT_UNSIGNED("max-cruft-size"),
    OPT_BOOL("aggressive"),
    OPT_BOOL_F("auto", PARSE_OPT_NOCOMPLETE),
    OPT_BOOL("detach"),
    OPT_BOOL_F("force", PARSE_OPT_NOCOMPLETE),
    OPT_BOOL("keep-largest-pack"),
    OPT_STRING("expire-to"),
    OPT_HIDDEN_BOOL("skip-foreground-tasks"),
];

/// `builtin_maintenance_options[]` (builtin/gc.c:3523-3531).
pub(super) const BUILTIN_MAINTENANCE_OPTIONS: &[Opt] = &[
    OPT_SUBCOMMAND("run"),
    OPT_SUBCOMMAND("start"),
    OPT_SUBCOMMAND("stop"),
    OPT_SUBCOMMAND("register"),
    OPT_SUBCOMMAND("unregister"),
    OPT_SUBCOMMAND("is-needed"),
];
