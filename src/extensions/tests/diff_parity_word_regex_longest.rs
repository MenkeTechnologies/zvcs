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
//! Every command runs in a hermetic environment — its own `HOME`,
//! `GIT_CONFIG_GLOBAL=/dev/null`, `GIT_CONFIG_NOSYSTEM` — so no user colour or diff
//! setting reaches it. The fixed expectations are stock git's output there, and each
//! one is also checked live against stock git when one is installed.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

#[path = "support/stock_git.rs"]
mod stock_git;
use stock_git::stock_git;

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn run(bin: &str, dir: &Path, args: &[&str]) -> Output {
    Command::new(bin)
        .args(args)
        .current_dir(dir)
        .env("HOME", dir)
        .env("XDG_CONFIG_HOME", dir.join(".config"))
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
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

fn git(bin: &str, dir: &Path, args: &[&str]) {
    let out = run(bin, dir, args);
    assert!(
        out.status.success(),
        "{bin} {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// The last line of stdout, which is the single reworded line of the patch.
fn last_line(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).trim_end_matches('\n').rsplit('\n').next().unwrap().to_string()
}

fn fixture(bin: &str, tag: &str, pre: &str, post: &str) -> PathBuf {
    let side = if bin == BIN { "zvcs" } else { "stock" };
    let root = std::env::temp_dir()
        .join(format!("zvcs-wordlongest-{tag}-{side}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let repo = root.canonicalize().unwrap();

    git(bin, &repo, &["init", "-q", "-b", "main"]);
    std::fs::write(repo.join(".gitattributes"), "* diff=cpp\n").unwrap();
    std::fs::write(repo.join("t.cpp"), pre).unwrap();
    git(bin, &repo, &["add", "-A"]);
    git(bin, &repo, &["commit", "-q", "-m", "c0"]);
    std::fs::write(repo.join("t.cpp"), post).unwrap();
    repo
}

/// The reworded line zvcs prints for `args` over a `pre` → `post` change, checked
/// against stock git's on its own copy of the fixture when one is installed.
fn reworded(tag: &str, pre: &str, post: &str, args: &[&str]) -> String {
    let repo = fixture(BIN, tag, pre, post);
    let got = last_line(&run(BIN, &repo, args));
    let _ = std::fs::remove_dir_all(&repo);
    if let Some(stock) = stock_git() {
        let repo = fixture(stock, tag, pre, post);
        let want = last_line(&run(stock, &repo, args));
        let _ = std::fs::remove_dir_all(&repo);
        assert_eq!(got, want, "{args:?}: zvcs must print what stock prints");
    }
    got
}

/// Three separate leftmost-first traps on one line. Under leftmost-first the
/// rendering is `[-0-]{+0+}...` style noise — the shared `0`/`0b` prefixes and the
/// `<=` prefix all become their own words, so the change lands on the tail.
#[test]
fn the_cpp_driver_takes_the_longest_alternative() {
    let (pre, post) = ("0xdead 0b1000 i<=j\n", "0xdeaf 0b1100 i<=>j\n");
    assert_eq!(
        reworded("cpp", pre, post, &["diff", "--word-diff=plain", "--", "t.cpp"]),
        "[-0xdead 0b1000-]{+0xdeaf 0b1100+} i[-<=-]{+<=>+}j"
    );

    // `--color-words` is the same tokenizer with a different frame, in the
    // default `color.diff.old` / `color.diff.new` and uncoloured context.
    assert_eq!(
        reworded(
            "cpp-color",
            pre,
            post,
            &["-c", "color.diff=always", "diff", "--color-words", "--", "t.cpp"],
        ),
        "\u{1b}[31m0xdead 0b1000\u{1b}[m\u{1b}[32m0xdeaf 0b1100\u{1b}[m i\
\u{1b}[31m<=\u{1b}[m\u{1b}[32m<=>\u{1b}[mj"
    );
}

/// `->*`, `.*`, `<<=` and `>>=` — and, with them, the spelling of the appended
/// `[\xc0-\xff][\x80-\xbf]+` branch. That branch is a *byte* range in
/// `userdiff.c`'s C string literal; read instead as the literal characters `\`,
/// `x`, `c`, `0`…`f` it becomes an ASCII class covering `0`-`\` and, under
/// leftmost-longest, swallows `<<b c>>` whole into one word.
#[test]
fn the_cpp_driver_takes_the_longest_operator() {
    assert_eq!(
        reworded(
            "ops",
            "b->v d.e a<<b c>>d\n",
            "b->*v d.*e a<<=b c>>=d\n",
            &["diff", "--word-diff=plain", "--", "t.cpp"],
        ),
        "b[-->-]{+->*+}v d[-.-]{+.*+}e a[-<<-]{+<<=+}b c[->>-]{+>>=+}d"
    );
}
