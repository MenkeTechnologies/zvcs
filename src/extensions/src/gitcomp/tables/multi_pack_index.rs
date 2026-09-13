use crate::gitcomp::*;

/// `builtin_multi_pack_index_options[]` (builtin/multi-pack-index.c:411-418).
pub(super) const BUILTIN_MULTI_PACK_INDEX_OPTIONS: &[Opt] = &[
    OPT_SUBCOMMAND("repack"),
    OPT_SUBCOMMAND("write"),
    OPT_SUBCOMMAND("compact"),
    OPT_SUBCOMMAND("verify"),
    OPT_SUBCOMMAND("expire"),
];

/// `common_opts[]` (builtin/multi-pack-index.c:96-104).
pub(super) const COMMON_OPTS: &[Opt] = &[
    OPT_CALLBACK("object-dir"),
    OPT_BIT("progress"),
];
