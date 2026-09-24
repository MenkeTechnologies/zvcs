//! `fsck.c`: consistency checks of a stack.

use crate::stack::Stack;

/// `enum reftable_fsck_error`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FsckError {
    /// `REFTABLE_FSCK_ERROR_TABLE_NAME`: a table's name does not follow
    /// `0x<min>-0x<max>-<random>.ref`.
    TableName,
}

/// `struct reftable_fsck_info`: one problem found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FsckInfo<'a> {
    /// What is wrong.
    pub error: FsckError,
    /// The message git reports.
    pub msg: &'static str,
    /// The offending table.
    pub path: &'a str,
}

/// `strtoull(ptr, &end, 16)` as `table_has_valid_name()` uses it: skip
/// whitespace, an optional sign, an optional `0x`, then hex digits. Returns
/// the rest of the input, or `None` where `errno` would be set (overflow).
fn skip_strtoull_hex(s: &[u8]) -> Option<&[u8]> {
    let mut p = 0;
    while p < s.len() && (s[p] == b' ' || (b'\t'..=b'\r').contains(&s[p])) {
        p += 1;
    }
    if p < s.len() && (s[p] == b'+' || s[p] == b'-') {
        p += 1;
    }
    if s.len() >= p + 3 && s[p] == b'0' && (s[p + 1] | 0x20) == b'x' && s[p + 2].is_ascii_hexdigit() {
        p += 2;
    }
    let digits_start = p;
    let mut val: u64 = 0;
    let mut overflow = false;
    while p < s.len() && s[p].is_ascii_hexdigit() {
        let d = u64::from((s[p] as char).to_digit(16).expect("hex digit"));
        match val.checked_mul(16).and_then(|v| v.checked_add(d)) {
            Some(v) => val = v,
            _ => overflow = true,
        }
        p += 1;
    }
    if overflow {
        return None;
    }
    // Without digits no conversion is performed and `end` is the input.
    Some(if p == digits_start { s } else { &s[p..] })
}

/// `table_has_valid_name()` (`fsck.c:6-41`).
fn table_has_valid_name(name: &str) -> bool {
    let mut ptr = name.as_bytes();
    for _ in 0..2 {
        let Some(rest) = skip_strtoull_hex(ptr) else { return false };
        let Some(rest) = rest.strip_prefix(b"-") else { return false };
        ptr = rest;
    }
    // `strtoul()`: `unsigned long` is 64 bits on the platforms zvcs targets.
    let Some(rest) = skip_strtoull_hex(ptr) else {
        return false;
    };
    rest == b".ref" || rest == b".log"
}

/// `reftable_fsck_check()` (`fsck.c:81-100`): run the table checks over every
/// table of `stack`, calling `verbose` with progress and `report` with each
/// problem. Returns whether any report returned `true` (an error).
pub fn check(stack: &Stack, mut report: impl FnMut(&FsckInfo<'_>) -> bool, mut verbose: impl FnMut(&str)) -> bool {
    let mut err = false;
    for t in stack.tables() {
        verbose(&format!("Checking table: {}", t.name()));
        // `table_check_name()` (`fsck.c:47-62`).
        if !table_has_valid_name(t.name()) {
            err |= report(&FsckInfo {
                error: FsckError::TableName,
                msg: "invalid reftable table name",
                path: t.name(),
            });
        }
    }
    err
}

#[cfg(test)]
mod tests {
    use super::table_has_valid_name;

    #[test]
    fn table_names() {
        assert!(table_has_valid_name("0x000000000001-0x000000000002-a1b2c3d4.ref"));
        assert!(table_has_valid_name("0x000000000001-0x000000000002-a1b2c3d4.log"));
        assert!(!table_has_valid_name("0x000000000001-0x000000000002-a1b2c3d4.lock"));
        assert!(!table_has_valid_name("tables.list"));
        assert!(!table_has_valid_name("0x1-0x2.ref"));
    }
}
