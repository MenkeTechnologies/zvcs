use crate::gitcomp::*;

/// `options[]` (builtin/receive-pack.c:2627-2635).
pub(super) const RECEIVE_PACK_OPTIONS: &[Opt] = &[
    OPT__QUIET(),
    OPT_HIDDEN_BOOL("skip-connectivity-check"),
    OPT_HIDDEN_BOOL("stateless-rpc"),
    OPT_HIDDEN_BOOL("http-backend-info-refs"),
    OPT_ALIAS("advertise-refs", "http-backend-info-refs"),
    OPT_HIDDEN_BOOL("reject-thin-pack-for-testing"),
];
