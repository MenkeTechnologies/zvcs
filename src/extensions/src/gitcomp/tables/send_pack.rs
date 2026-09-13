use crate::gitcomp::*;

/// `options[]` (builtin/send-pack.c:187-213).
pub(super) const SEND_PACK_OPTIONS: &[Opt] = &[
    // OPT__VERBOSITY (parse-options.h:545-560)
    OPT_CALLBACK_F("verbose", PARSE_OPT_NOARG),
    OPT_CALLBACK_F("quiet", PARSE_OPT_NOARG),
    OPT_STRING("receive-pack"),
    OPT_STRING("exec"),
    OPT_STRING("remote"),
    OPT_BOOL("all"),
    OPT_BOOL("dry-run"),
    OPT_BOOL("mirror"),
    OPT_BOOL("force"),
    OPT_CALLBACK_F("signed", PARSE_OPT_OPTARG),
    OPT_STRING_LIST("push-option"),
    OPT_BOOL("progress"),
    OPT_BOOL("thin"),
    OPT_BOOL("atomic"),
    OPT_BOOL("stateless-rpc"),
    OPT_BOOL("stdin"),
    OPT_BOOL("helper-status"),
    OPT_CALLBACK_F("force-with-lease", PARSE_OPT_OPTARG),
    OPT_BOOL("force-if-includes"),
];
