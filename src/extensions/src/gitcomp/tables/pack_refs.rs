use crate::gitcomp::*;

/// `opts[]` (pack-refs.c:27-36), which `pack_refs_core()` passes to
/// `parse_options()` (pack-refs.c:38) for `cmd_pack_refs()`
/// (builtin/pack-refs.c).
pub(super) const OPTS: &[Opt] = &[
    OPT_BOOL("all"),
    OPT_BIT("prune"),
    OPT_BIT("auto"),
    OPT_STRING_LIST("include"),
    OPT_STRING_LIST("exclude"),
];
