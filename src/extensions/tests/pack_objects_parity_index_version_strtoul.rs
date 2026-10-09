//! `--index-version` is read with `strtoul()`, which skips leading whitespace and reads the
//! `,<offset>` tail in base 0 (`0x` hex, leading-`0` octal). The text after the number must be
//! empty and the offset must stay below 2^31, otherwise `bad index version`; a version above 2 is
//! `unsupported index version`.
#![cfg(unix)]

#[path = "support/stock_git.rs"]
mod stock_git;
#[path = "support/twin_repo.rs"]
mod twin_repo;

#[test]
fn leading_whitespace_and_a_base_zero_offset_are_read_like_strtoul() {
    let Some(stock) = stock_git::stock_git() else { return };
    let (s, z) = twin_repo::pair("pack-objects-index-version", stock);
    for value in [
        " 1", " 2", "\t2", "\n1", " +2", "1,0x10", "2,010", "2,0x7fffffff", "1,0x80000000", "1,0x", "0x2",
        "1,", "1,x", "2,-1", " ", "  3", "-1",
    ] {
        let flag = format!("--index-version={value}");
        let args = ["pack-objects", "--revs", "no-such-dir/pack", flag.as_str()];
        let want = s.git(&args);
        assert_eq!(z.git(&args), want, "{value:?}");
    }
    // The accepted ones get past the option and fail on the unwritable destination instead.
    for value in [" 1", "\t2", "1,0x10", "2,010"] {
        let flag = format!("--index-version={value}");
        let want = s.git(&["pack-objects", "--revs", "no-such-dir/pack", flag.as_str()]);
        assert!(want.stderr.contains("unable to rename temporary file"), "{value:?}: {want:?}");
    }
}
