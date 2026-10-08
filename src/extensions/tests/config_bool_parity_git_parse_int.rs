//! Boolean configuration values against stock git: `git_parse_maybe_bool()` falls back to
//! `git_parse_int()`, so a number is read the way `strtoimax()` base 0 reads it (hex, octal,
//! a `k`/`m`/`g` unit) and non-zero is true; one `git_parse_int()` refuses is a fatal.
#![cfg(unix)]

#[path = "support/stock_git.rs"]
mod stock_git;
#[path = "support/twin_repo.rs"]
mod twin_repo;

#[test]
fn integer_spellings_of_a_boolean() {
    let Some(stock) = stock_git::stock_git() else { return };
    let (s, z) = twin_repo::pair("bool-int", stock);
    for side in [&s, &z] {
        side.write("a", "dirty\n");
    }
    for value in [
        "0x10", "0X1f", "010", "08", "0x0", "00", "1k", "0k", "1g", "2g", "-1", "+1", " 7", "1 ", "0x", "1kk",
        "99999999999999999999", "2147483647", "2147483648", "yes", "off", "",
    ] {
        // `advice.statusHints` is read with `git_config_bool()` and shows or hides the
        // `(use "git add" …)` hints of a status; a value it cannot read is fatal.
        let kv = format!("advice.statusHints={value}");
        let args = ["-c", kv.as_str(), "status"];
        assert_eq!(z.git(&args), s.git(&args), "{value:?}");
    }
}
