//! `core.repositoryFormatVersion` is read with `git_config_int()`, i.e. `strtoimax(…, 0)`.
//!
//! Leading whitespace, a `+` sign, `0x` hex and a leading-zero octal all convert, so the repository
//! opens. zvcs read the value as a bare decimal and ended every command in
//! `Failed to load the git configuration` at exit 1 where git carries on.
//!
//! Stock git is the oracle (`support/stock_git.rs`).

#[path = "support/stock_git.rs"]
mod stock_git;
#[path = "support/twin.rs"]
mod twin;

use twin::Twin;

#[test]
fn the_version_is_parsed_with_base_zero() {
    let Some(t) = Twin::new("repo-format-strtoimax") else { return };
    for value in [" 1", "0x1", "01", "+1", "\t0"] {
        t.prepare(&["config", "core.repositoryFormatVersion", value]);
        for args in [&["status", "--short"][..], &["commit-graph", "write"]] {
            let (stock, zvcs) = t.run_in("work", args);
            assert_eq!(stock.code, 0, "{value:?} {args:?}: {stock:?}");
            assert_eq!(zvcs, stock, "core.repositoryFormatVersion={value:?} git {args:?}");
        }
    }
}

#[test]
fn a_value_strtoimax_cannot_consume_is_refused_alike() {
    for (i, value) in ["08", "1 ", "0x", "1k"].into_iter().enumerate() {
        let Some(t) = Twin::new(&format!("repo-format-bad-{i}")) else { return };
        t.prepare(&["config", "core.repositoryFormatVersion", value]);
        let (stock, zvcs) = t.run_in("work", &["status", "--short"]);
        assert_eq!(stock.code, 128, "{value:?}: {stock:?}");
        assert_eq!(zvcs, stock, "core.repositoryFormatVersion={value:?}");
    }
}
