use crate::gitcomp::*;

/// `builtin_log_options[]` (builtin/log.c:280-303).
pub(super) const BUILTIN_LOG_OPTIONS: &[Opt] = &[
    OPT__QUIET(),
    OPT_BOOL("source"),
    OPT_BOOL("use-mailmap"),
    // `#ifndef WITH_BREAKING_CHANGES` (builtin/log.c:284-287); stock 2.55.0 is
    // built without it.
    OPT_HIDDEN_BOOL("i-still-use-this"),
    OPT_ALIAS("mailmap", "use-mailmap"),
    OPT_CALLBACK_F("clear-decorations", PARSE_OPT_NOARG | PARSE_OPT_NONEG),
    OPT_STRING_LIST("decorate-refs"),
    OPT_STRING_LIST("decorate-refs-exclude"),
    OPT_CALLBACK_F("decorate", PARSE_OPT_OPTARG),
    OPT_CALLBACK(NULL),
];

/// `builtin_format_patch_options[]` (builtin/log.c:2006-2096).
pub(super) const BUILTIN_FORMAT_PATCH_OPTIONS: &[Opt] = &[
    OPT_CALLBACK_F("numbered", PARSE_OPT_NOARG),
    OPT_CALLBACK_F("no-numbered", PARSE_OPT_NOARG | PARSE_OPT_NONEG),
    OPT_BOOL("signoff"),
    OPT_BOOL("stdout"),
    OPT_BOOL("cover-letter"),
    OPT_STRING("commit-list-format"),
    OPT_BOOL("numbered-files"),
    OPT_STRING("suffix"),
    OPT_INTEGER("start-number"),
    OPT_STRING("reroll-count"),
    OPT_INTEGER("filename-max-length"),
    OPT_CALLBACK_F("rfc", PARSE_OPT_OPTARG),
    OPT_STRING("cover-from-description"),
    OPT_FILENAME("description-file"),
    OPT_CALLBACK_F("subject-prefix", PARSE_OPT_NONEG),
    OPT_CALLBACK_F("output-directory", PARSE_OPT_NONEG),
    OPT_CALLBACK_F("keep-subject", PARSE_OPT_NOARG | PARSE_OPT_NONEG),
    OPT_BOOL("no-binary"),
    OPT_BOOL("zero-commit"),
    OPT_BOOL("ignore-if-in-upstream"),
    OPT_SET_INT_F("no-stat", PARSE_OPT_NONEG),
    OPT_GROUP(),
    OPT_CALLBACK("add-header"),
    OPT_STRING_LIST("to"),
    OPT_STRING_LIST("cc"),
    OPT_CALLBACK_F("from", PARSE_OPT_OPTARG),
    OPT_STRING("in-reply-to"),
    OPT_CALLBACK_F("attach", PARSE_OPT_OPTARG),
    OPT_CALLBACK_F("inline", PARSE_OPT_OPTARG | PARSE_OPT_NONEG),
    OPT_CALLBACK_F("thread", PARSE_OPT_OPTARG),
    OPT_STRING("signature"),
    OPT_CALLBACK_F("base", 0),
    OPT_FILENAME("signature-file"),
    OPT__QUIET(),
    OPT_BOOL("progress"),
    OPT_CALLBACK("interdiff"),
    OPT_STRING("range-diff"),
    OPT_INTEGER("creation-factor"),
    OPT_BOOL("force-in-body-from"),
];

/// `options[]` (builtin/log.c:2749-2753).
pub(super) const CHERRY_OPTIONS: &[Opt] = &[
    OPT__ABBREV(),
    OPT__VERBOSE(),
];
