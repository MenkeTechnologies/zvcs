//! `fast-import`'s crash report, as a test comparing two runs has to see it.
//!
//! Once `die_nicely` is installed (builtin/fast-import.c:4245), every `die()`
//! ends with `fast-import: dumping crash report to .git/fast_import_crash_<pid>`
//! (fast-import.c:444-463), and the pid differs between any two runs — so the
//! zvcs and stock lines can only be compared once it is taken out.
//!
//! Each file includes this with
//!
//! ```ignore
//! #[path = "support/crash_report.rs"]
//! mod crash_report;
//! ```

const FILE_PREFIX: &str = "fast_import_crash_";

/// `stderr` with the pid after every `fast_import_crash_` replaced by `<pid>`.
pub fn without_pid(stderr: &str) -> String {
    let mut out = String::with_capacity(stderr.len());
    let mut rest = stderr;
    while let Some(at) = rest.find(FILE_PREFIX) {
        let (head, tail) = rest.split_at(at + FILE_PREFIX.len());
        out.push_str(head);
        out.push_str("<pid>");
        rest = tail.trim_start_matches(|c: char| c.is_ascii_digit());
    }
    out.push_str(rest);
    out
}
