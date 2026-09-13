use crate::gitcomp::*;

/// `verify_pack_options[]` (builtin/verify-pack.c:75-83).
pub(super) const VERIFY_PACK_OPTIONS: &[Opt] = &[
    OPT_BIT("verbose"),
    OPT_BIT("stat-only"),
    OPT_STRING("object-format"),
];
