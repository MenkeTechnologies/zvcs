use crate::gitcomp::*;

/// `options[]` (builtin/credential-cache.c:153-159).
pub(super) const CREDENTIAL_CACHE_OPTIONS: &[Opt] = &[
    OPT_INTEGER("timeout"),
    OPT_STRING("socket"),
];
