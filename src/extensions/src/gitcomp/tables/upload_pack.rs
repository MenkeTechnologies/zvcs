use crate::gitcomp::*;

/// `options[]` (builtin/upload-pack.c:32-43).
pub(super) const UPLOAD_PACK_OPTIONS: &[Opt] = &[
    OPT_BOOL("stateless-rpc"),
    OPT_HIDDEN_BOOL("http-backend-info-refs"),
    OPT_ALIAS("advertise-refs", "http-backend-info-refs"),
    OPT_BOOL("strict"),
    OPT_INTEGER("timeout"),
];
