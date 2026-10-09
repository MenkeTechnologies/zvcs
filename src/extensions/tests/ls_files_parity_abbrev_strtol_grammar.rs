//! `git ls-files --abbrev=<n>` goes through `parse_opt_abbrev_cb()`: `strtol(arg, &end, 10)`
//! with its leading white space and sign, a negative or small value raised to the minimum,
//! trailing bytes and an empty value an error.
#![cfg(unix)]

#[path = "support/stock_git.rs"]
mod stock_git;
#[path = "support/twin_repo.rs"]
mod twin_repo;

#[test]
fn abbrev_value_follows_strtol() {
    let Some(stock) = stock_git::stock_git() else { return };
    let (s, z) = twin_repo::pair("ls-files-abbrev-strtol", stock);
    for value in [" 1", "  8", "\t6", "+5", "-3", "0", "7", "12", "99999999999999999999999999", "4x", "8 ", "", "0x10", "abc"] {
        let arg = format!("--abbrev={value}");
        let args = ["ls-files", "-s", arg.as_str()];
        assert_eq!(z.git(&args), s.git(&args), "{args:?}");
    }
}
