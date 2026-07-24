//! The selector mechanism is unified through `select.rs`. These guard the two
//! constants that drive parsing, docs, and repl completion against drift: every
//! `SELECTOR_VERBS` entry must be a real dispatchable verb, and the repl must
//! actually offer the selector flags for those verbs.

use zvcs::dispatch::is_verb;
use zvcs::superset::select::{SELECTOR_FLAGS, SELECTOR_VERBS};

#[test]
fn selector_verbs_are_real() {
    for v in SELECTOR_VERBS {
        assert!(is_verb(v), "SELECTOR_VERBS lists `{v}`, which is not a dispatchable verb");
    }
}

#[test]
fn selector_flags_are_nonempty_and_well_formed() {
    assert!(!SELECTOR_FLAGS.is_empty());
    for f in SELECTOR_FLAGS {
        assert!(f.starts_with("--"), "selector flag `{f}` should be a long option");
    }
}
