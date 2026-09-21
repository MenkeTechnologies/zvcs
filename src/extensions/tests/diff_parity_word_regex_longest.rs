//! `--word-diff` tokenizes with POSIX `regexec()`, which is leftmost-**longest**.
//!
//! `init_diff_words_data()` compiles the driver's `word_regex` with
//! `regcomp(…, REG_EXTENDED | REG_NEWLINE)` (diff.c:2355-2359) and
//! `find_word_boundaries()` calls `regexec_buf()` on it (diff.c:2288). POSIX picks
//! the *longest* of the alternatives that match at the earliest position; a
//! Perl-style engine picks the one written first. The built-in `cpp` driver
//! (userdiff.c:96-104) is where the two disagree loudest:
//!
//! ```text
//! "[a-zA-Z_][a-zA-Z0-9_]*"
//! "|[0-9][0-9.]*([Ee][-+]?[0-9]+)?[fFlLuU]*"   <- matches the bare `0` of `0xdead`
//! "|0[xXbB][0-9a-fA-F]+[lLuU]*"                <- matches all of `0xdead`
//! …
//! "|[-+*/<>%&^|=!]=|--|\\+\\+|<<=?|>>=?|&&|\\|\\||::|->\\*?|\\.\\*|<=>"
//! ```
//!
//! Leftmost-first splits `0xdead` into `0` + `xdead` and `<=>` into `<=` + `>`,
//! because the shorter branch is written first in both cases. git's own
//! `t/t4034/cpp/expect` records the longest-match answer.
//!
//! The same macros append `"|[^[:space:]]|[\xc0-\xff][\x80-\xbf]+"` (userdiff.c:22),
//! whose second alternative is a *byte* range in the C string literal — a UTF-8
//! lead byte and its continuation bytes — not the characters `\`, `x`, `c` and so
//! on. Under leftmost-longest that distinction stops being cosmetic: read as
//! characters the class covers ASCII and wins over every real branch.
//!
//! Every expectation below was read off stock git 2.55.0 on this fixture before it
//! was written down.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn run(dir: &Path, args: &[&str]) -> Output {
    Command::new(BIN)
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("LC_ALL", "C")
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@e.x")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@e.x")
        .env("GIT_AUTHOR_DATE", "1700000000 +0000")
        .env("GIT_COMMITTER_DATE", "1700000000 +0000")
        .env_remove("COLUMNS")
        .output()
        .unwrap()
}

fn git(dir: &Path, args: &[&str]) {
    let out = run(dir, args);
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// The last line of stdout, which is the single reworded line of the patch.
fn last_line(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).trim_end_matches('\n').rsplit('\n').next().unwrap().to_string()
}

fn fixture(tag: &str, pre: &str, post: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!("zvcs-wordlongest-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let repo = root.canonicalize().unwrap();

    git(&repo, &["init", "-q", "-b", "main"]);
    std::fs::write(repo.join(".gitattributes"), "* diff=cpp\n").unwrap();
    std::fs::write(repo.join("t.cpp"), pre).unwrap();
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-q", "-m", "c0"]);
    std::fs::write(repo.join("t.cpp"), post).unwrap();
    repo
}

/// Three separate leftmost-first traps on one line. Under leftmost-first the
/// rendering is `[-0-]{+0+}...` style noise — the shared `0`/`0b` prefixes and the
/// `<=` prefix all become their own words, so the change lands on the tail.
#[test]
fn the_cpp_driver_takes_the_longest_alternative() {
    let repo = fixture("cpp", "0xdead 0b1000 i<=j\n", "0xdeaf 0b1100 i<=>j\n");

    let o = run(&repo, &["diff", "--word-diff=plain", "--", "t.cpp"]);
    assert_eq!(last_line(&o), "[-0xdead 0b1000-]{+0xdeaf 0b1100+} i[-<=-]{+<=>+}j");

    // `--color-words` is the same tokenizer with a different frame.
    let o = run(
        &repo,
        &["-c", "color.diff=always", "diff", "--color-words", "--", "t.cpp"],
    );
    assert_eq!(
        last_line(&o),
        "\u{1b}[1;31m0xdead 0b1000\u{1b}[m\u{1b}[1;32m0xdeaf 0b1100\u{1b}[m\
\u{1b}[34m i\u{1b}[m\u{1b}[1;31m<=\u{1b}[m\u{1b}[1;32m<=>\u{1b}[m\u{1b}[34mj\u{1b}[m"
    );
}

/// `->*`, `.*`, `<<=` and `>>=` — and, with them, the spelling of the appended
/// `[\xc0-\xff][\x80-\xbf]+` branch. That branch is a *byte* range in
/// `userdiff.c`'s C string literal; read instead as the literal characters `\`,
/// `x`, `c`, `0`…`f` it becomes an ASCII class covering `0`-`\` and, under
/// leftmost-longest, swallows `<<b c>>` whole into one word.
#[test]
fn the_cpp_driver_takes_the_longest_operator() {
    let repo = fixture("ops", "b->v d.e a<<b c>>d\n", "b->*v d.*e a<<=b c>>=d\n");
    let o = run(&repo, &["diff", "--word-diff=plain", "--", "t.cpp"]);
    assert_eq!(
        last_line(&o),
        "b[-->-]{+->*+}v d[-.-]{+.*+}e a[-<<-]{+<<=+}b c[->>-]{+>>=+}d"
    );
}
