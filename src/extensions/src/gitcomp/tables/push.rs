use crate::gitcomp::*;

/// `options[]` (builtin/push.c:681-718).
pub(super) const PUSH_OPTIONS: &[Opt] = &[
    // OPT__VERBOSITY (parse-options.h:545-560)
    OPT_CALLBACK_F("verbose", PARSE_OPT_NOARG),
    OPT_CALLBACK_F("quiet", PARSE_OPT_NOARG),
    OPT_STRING("repo"),
    OPT_BIT("all"),
    OPT_ALIAS("branches", "all"),
    OPT_BIT("mirror"),
    OPT_BOOL("delete"),
    OPT_BOOL("tags"),
    OPT_BIT("dry-run"),
    OPT_BIT("porcelain"),
    OPT_BIT("force"),
    OPT_CALLBACK_F("force-with-lease", PARSE_OPT_OPTARG | PARSE_OPT_LITERAL_ARGHELP),
    OPT_BIT("force-if-includes"),
    OPT_CALLBACK("recurse-submodules"),
    OPT_BOOL_F("thin", PARSE_OPT_NOCOMPLETE),
    OPT_STRING("receive-pack"),
    OPT_STRING("exec"),
    OPT_BIT("set-upstream"),
    OPT_BOOL("progress"),
    OPT_BIT("prune"),
    OPT_BIT("no-verify"),
    OPT_BIT("follow-tags"),
    OPT_CALLBACK_F("signed", PARSE_OPT_OPTARG),
    OPT_BIT("atomic"),
    OPT_STRING_LIST("push-option"),
    // OPT_IPVERSION (parse-options.h:630-634)
    OPT_SET_INT_F("ipv4", PARSE_OPT_NONEG),
    OPT_SET_INT_F("ipv6", PARSE_OPT_NONEG),
];
