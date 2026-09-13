use crate::gitcomp::*;

/// `options[]` (builtin/ls-remote.c:66-94).
pub(super) const LS_REMOTE_OPTIONS: &[Opt] = &[
    OPT__QUIET(),
    OPT_STRING("upload-pack"),
    option(Type::String, "exec", PARSE_OPT_HIDDEN),
    OPT_BIT("tags"),
    OPT_BIT("branches"),
    OPT_BIT_F("heads", PARSE_OPT_HIDDEN),
    OPT_BIT("refs"),
    OPT_BOOL("get-url"),
    OPT_REF_SORT(),
    OPT_SET_INT_F("exit-code", PARSE_OPT_NOCOMPLETE),
    OPT_BOOL("symref"),
    OPT_STRING_LIST("server-option"),
];
