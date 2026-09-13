//! The ported `struct option` arrays, one module per builtin source file
//! (`builtin/<name>.c`), and the registry that names which builtins answer.

use super::Builtin;

mod add;

/// `commands[]` (git.c:529-685) order, restricted to the builtins whose table
/// is ported. `run_setup` is the entry's `RUN_SETUP` bit.
pub(super) const BUILTINS: &[Builtin] = &[
    Builtin { name: "add", run_setup: true, options: &[add::BUILTIN_ADD_OPTIONS] },
    Builtin { name: "stage", run_setup: true, options: &[add::BUILTIN_ADD_OPTIONS] },
];
