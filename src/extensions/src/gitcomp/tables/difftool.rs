use crate::gitcomp::*;

/// `builtin_difftool_options[]` (builtin/difftool.c:729-753).
pub(super) const BUILTIN_DIFFTOOL_OPTIONS: &[Opt] = &[
    OPT_BOOL("gui"),
    OPT_BOOL("dir-diff"),
    OPT_SET_INT_F("no-prompt", PARSE_OPT_NONEG),
    OPT_SET_INT_F("prompt", PARSE_OPT_NONEG | PARSE_OPT_HIDDEN),
    OPT_BOOL("symlinks"),
    OPT_STRING("tool"),
    OPT_BOOL("tool-help"),
    OPT_BOOL("trust-exit-code"),
    OPT_STRING("extcmd"),
    OPT_BOOL("no-index"),
];
