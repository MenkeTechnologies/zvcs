use crate::gitcomp::*;

/// `builtin_url_parse_options[]` (builtin/url-parse.c:14-18).
pub(super) const BUILTIN_URL_PARSE_OPTIONS: &[Opt] = &[
    OPT_STRING("component"),
];
