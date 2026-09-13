use crate::gitcomp::*;

/// `builtin_help_options[]` (builtin/help.c:66-98).
pub(super) const BUILTIN_HELP_OPTIONS: &[Opt] = &[
    OPT_CMDMODE("all"),
    OPT_BOOL("external-commands"),
    OPT_BOOL("aliases"),
    OPT_HIDDEN_BOOL("exclude-guides"),
    OPT_SET_INT("man"),
    OPT_SET_INT("web"),
    OPT_SET_INT("info"),
    OPT__VERBOSE(),
    OPT_CMDMODE("guides"),
    OPT_CMDMODE("user-interfaces"),
    OPT_CMDMODE("developer-interfaces"),
    OPT_CMDMODE("config"),
    OPT_CMDMODE_F("config-for-completion", PARSE_OPT_HIDDEN),
    OPT_CMDMODE_F("config-sections-for-completion", PARSE_OPT_HIDDEN),
    OPT_CMDMODE_F("aliases-for-completion", PARSE_OPT_HIDDEN),
];
