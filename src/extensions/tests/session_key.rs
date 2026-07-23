//! `session_key()` must treat a set-but-EMPTY `ZVCS_SESSION` as unset — otherwise
//! every such shell collapses to the one key "", cross-releasing each other's
//! claims. Isolated in its own test binary/process so the env mutation can't race
//! other tests.

#[test]
fn empty_zvcs_session_falls_back_to_pid_key() {
    std::env::set_var("ZVCS_SESSION", "");
    assert!(zvcs::session_key().starts_with("pid-"), "empty ZVCS_SESSION must fall back to a pid key");

    std::env::set_var("ZVCS_SESSION", "   ");
    assert!(zvcs::session_key().starts_with("pid-"), "whitespace-only ZVCS_SESSION must fall back too");

    std::env::set_var("ZVCS_SESSION", "realsession");
    assert_eq!(zvcs::session_key(), "realsession", "a real ZVCS_SESSION must be used verbatim");

    std::env::remove_var("ZVCS_SESSION");
    assert!(zvcs::session_key().starts_with("pid-"), "unset ZVCS_SESSION must fall back to a pid key");
}
