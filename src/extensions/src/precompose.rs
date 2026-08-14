//! `compat/precompose_utf8.c` — macOS hands out decomposed filenames, and git
//! composes them back before anything looks at them.
//!
//! A file created as `é` on an Apple filesystem is stored as `e` followed by
//! U+0301 COMBINING ACUTE ACCENT (NFD). The shell completes that name from disk,
//! so it is the NFD form that arrives in `argv` — while the same name typed by
//! hand, or read out of a tree written on Linux, is the single-code-point NFC
//! form. Left alone the two never compare equal, and every path-taking command
//! sees a file that is simultaneously present and missing.
//!
//! git's answer is to convert at the two places the decomposed form enters:
//! `readdir()`, which is where `gix` already handles it (`gix-fs`'s
//! `precompose_unicode`), and `argv`, which is this module.
//!
//! Two properties of the C are worth keeping in view:
//!
//!   * **It is off unless configured on.** `precomposed_unicode` starts at `-1`
//!     and `precompose_string_if_needed()` returns the string untouched for
//!     anything that is not exactly `1`, so an absent `core.precomposeunicode`
//!     means no conversion. The key is normally present because
//!     `probe_utf8_pathname_composition()` (setup.c:2630) writes it into every
//!     repository `git init` creates on a filesystem that composes.
//!   * **The conversion is `iconv`'s, not NFC.** `UTF-8-MAC` is Apple's
//!     decomposed encoding, and decoding a name *as* it composes only what that
//!     encoding decomposes. Canonical singletons are left alone where a general
//!     NFC pass would fold them: U+2126 OHM SIGN stays U+2126 rather than
//!     becoming U+03A9. Calling the platform's `iconv` is therefore both the
//!     faithful port and the simpler one.
//!
//! The whole file is `#ifdef PRECOMPOSE_UNICODE`, which only macOS builds define;
//! `git-compat-util.h:170-179` supplies inlines that hand the input straight back
//! everywhere else. That split is reproduced here with `cfg(target_os = "macos")`,
//! so the Linux build contains neither the `iconv` link nor the config lookup.

/// git's `precompose_argv_prefix()` (compat/precompose_utf8.c:102-111), which
/// `run_builtin()` calls over the dispatched command's arguments once repository
/// setup has run and the config is readable (git.c:488).
///
/// Every argument is converted, not just the ones that name files: git makes no
/// distinction, which is why `git log --format='%He<U+0301>'` prints the composed
/// form under `core.precomposeunicode=true` and the decomposed one under `false`.
pub fn argv(args: &mut [String]) {
    imp::argv(args);
}

#[cfg(target_os = "macos")]
mod imp {
    /// `iconv(3)` as `precompose_utf8.c` uses it. Declared here rather than taken
    /// from `libc`, whose copies are deprecated for removal in 1.0 because not
    /// every platform it covers has the library; macOS always does, and this
    /// module only exists there.
    mod iconv {
        use libc::{c_char, c_int, c_void, size_t};

        pub type Cd = *mut c_void;

        #[link(name = "iconv")]
        extern "C" {
            pub fn iconv_open(tocode: *const c_char, fromcode: *const c_char) -> Cd;
            pub fn iconv(
                cd: Cd,
                inbuf: *mut *mut c_char,
                inbytesleft: *mut size_t,
                outbuf: *mut *mut c_char,
                outbytesleft: *mut size_t,
            ) -> size_t;
            pub fn iconv_close(cd: Cd) -> c_int;
        }
    }

    /// git's `has_non_ascii()` (precompose_utf8.c:22-42): one byte with the high
    /// bit set is enough to make a string worth converting. It is also the guard
    /// that keeps an all-ASCII command line off the config path entirely.
    fn has_non_ascii(s: &str) -> bool {
        s.bytes().any(|b| b & 0x80 != 0)
    }

    pub fn argv(args: &mut [String]) {
        if !args.iter().any(|a| has_non_ascii(a)) {
            return;
        }
        // `repo_config_get_bool(the_repository, "core.precomposeunicode", …)`,
        // which leaves `precomposed_unicode` at `-1` when the key is absent; only
        // an explicit true reaches the conversion. Read through the same cascade
        // git reads it through — the repository's merged config when there is
        // one, the system and per-user files when there is not.
        if crate::config::config_bool("core.precomposeunicode") != Some(true) {
            return;
        }
        for arg in args {
            if !has_non_ascii(arg) {
                continue;
            }
            if let Some(composed) = precompose(arg) {
                *arg = composed;
            }
        }
    }

    /// `reencode_string_iconv(in, strlen(in), iconv_open("UTF-8", "UTF-8-MAC"))`.
    ///
    /// `None` stands for every way the C gives up and keeps the original bytes: a
    /// descriptor it could not open, and a conversion that stopped on anything
    /// other than a full output buffer.
    fn precompose(s: &str) -> Option<String> {
        // SAFETY: two NUL-terminated encoding names, and the descriptor is closed
        // on both paths out below.
        let cd = unsafe { iconv::iconv_open(c"UTF-8".as_ptr(), c"UTF-8-MAC".as_ptr()) };
        if cd == (-1isize) as iconv::Cd {
            return None;
        }
        let converted = convert(cd, s.as_bytes());
        // SAFETY: `cd` is the descriptor just opened and is not used again.
        unsafe { iconv::iconv_close(cd) };
        String::from_utf8(converted?).ok()
    }

    /// The `while (1)` in `reencode_string_iconv()` (utf8.c): start the output the
    /// same size as the input and grow it whenever `iconv` reports `E2BIG`, giving
    /// up on any other error.
    fn convert(cd: iconv::Cd, input: &[u8]) -> Option<Vec<u8>> {
        // `iconv` takes the input by `char **` and advances it, so it needs a
        // buffer of its own rather than the `&str`'s bytes.
        let mut inbuf = input.to_vec();
        let mut in_ptr = inbuf.as_mut_ptr().cast::<libc::c_char>();
        let mut in_left = inbuf.len();
        let mut out = vec![0u8; input.len().max(1)];
        let mut written = 0usize;
        loop {
            // Recomputed each round because a grow may have moved the buffer.
            let mut out_ptr = unsafe { out.as_mut_ptr().add(written) }.cast::<libc::c_char>();
            let mut out_left = out.len() - written;
            // SAFETY: both pointers address their own live buffers and the two
            // lengths are the bytes remaining in each, which is `iconv`'s contract.
            let ret = unsafe {
                iconv::iconv(cd, &mut in_ptr, &mut in_left, &mut out_ptr, &mut out_left)
            };
            written = out.len() - out_left;
            if ret != usize::MAX {
                out.truncate(written);
                return Some(out);
            }
            if std::io::Error::last_os_error().raw_os_error() != Some(libc::E2BIG) {
                return None;
            }
            out.resize(out.len() * 2 + 32, 0);
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        /// The conversion itself, against the four shapes that distinguish it from
        /// both "do nothing" and "run a general NFC pass".
        ///
        /// Checked against the platform's own converter, which is the same one the
        /// C calls:
        ///
        /// ```text
        /// $ printf 'e\xcc\x81'     | iconv -f UTF-8-MAC -t UTF-8 | xxd  # c3a9
        /// $ printf '\xe2\x84\xa6'  | iconv -f UTF-8-MAC -t UTF-8 | xxd  # e284a6
        /// ```
        #[test]
        fn composes_the_way_utf8_mac_does() {
            // The case the whole file exists for: a combining mark folded in.
            assert_eq!(precompose("e\u{301}.txt").as_deref(), Some("é.txt"));
            // Already composed, and therefore a fixed point — an argument typed by
            // hand must survive a command line that also carries one from disk.
            assert_eq!(precompose("é.txt").as_deref(), Some("é.txt"));
            // Hangul composes too, by algorithm rather than by table.
            assert_eq!(precompose("\u{1100}\u{1161}").as_deref(), Some("가"));
            // A canonical singleton is *not* folded. A general NFC pass would
            // return U+03A9 here; `UTF-8-MAC` decomposes nothing into U+2126, so
            // composing leaves it alone. This is the assertion that fails if the
            // platform converter is ever swapped for a normalization crate.
            assert_eq!(precompose("\u{2126}").as_deref(), Some("\u{2126}"));
        }

        /// The output buffer starts at the input's length, and composing can only
        /// shrink a string — but a `\0`-free grow path still has to be exercised,
        /// so run a long mixed string through and check it end to end.
        #[test]
        fn converts_a_long_mixed_string_whole() {
            let input = "e\u{301}x".repeat(500);
            let expected = "éx".repeat(500);
            assert_eq!(precompose(&input).as_deref(), Some(expected.as_str()));
        }

        /// The `has_non_ascii()` guard, which is what decides whether the config
        /// is consulted at all.
        #[test]
        fn only_non_ascii_strings_are_candidates() {
            assert!(!has_non_ascii(""));
            assert!(!has_non_ascii("--format=%H"));
            assert!(has_non_ascii("e\u{301}"));
            assert!(has_non_ascii("é"));
        }
    }
}

#[cfg(not(target_os = "macos"))]
mod imp {
    /// `git-compat-util.h:170-174`: without `PRECOMPOSE_UNICODE` the argv form is
    /// an inline that returns the prefix and touches nothing. No other platform
    /// hands out decomposed names, so there is nothing to compose.
    pub fn argv(_args: &mut [String]) {}
}

#[cfg(test)]
mod tests {
    /// An ASCII command line is untouched on every target — the macOS build
    /// returns before it reads the config, and every other build is a no-op. This
    /// runs on Linux CI, where the conversion itself does not exist.
    #[test]
    fn an_ascii_command_line_is_never_rewritten() {
        let mut args = vec!["--oneline".to_string(), "-n".to_string(), "5".to_string()];
        super::argv(&mut args);
        assert_eq!(args, ["--oneline", "-n", "5"]);
    }
}
