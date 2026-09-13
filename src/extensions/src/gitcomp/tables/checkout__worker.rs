use crate::gitcomp::*;

/// `checkout_worker_options[]` (builtin/checkout--worker.c:126-130).
pub(super) const CHECKOUT_WORKER_OPTIONS: &[Opt] = &[
    OPT_STRING("prefix"),
];
