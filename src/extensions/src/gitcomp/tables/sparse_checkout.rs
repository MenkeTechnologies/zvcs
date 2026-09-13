use crate::gitcomp::*;

/// `builtin_sparse_checkout_options[]` (builtin/sparse-checkout.c:1188-1198).
pub(super) const BUILTIN_SPARSE_CHECKOUT_OPTIONS: &[Opt] = &[
    OPT_SUBCOMMAND("list"),
    OPT_SUBCOMMAND("init"),
    OPT_SUBCOMMAND("set"),
    OPT_SUBCOMMAND("add"),
    OPT_SUBCOMMAND("reapply"),
    OPT_SUBCOMMAND("clean"),
    OPT_SUBCOMMAND("disable"),
    OPT_SUBCOMMAND("check-rules"),
];
