use crate::gitcomp::*;

/// `prune_packed_options[]` (builtin/prune-packed.c:17-23).
pub(super) const PRUNE_PACKED_OPTIONS: &[Opt] = &[
    OPT_BIT("dry-run"),
    OPT_NEGBIT("quiet"),
];
