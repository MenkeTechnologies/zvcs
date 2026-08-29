//! `git config --get-regexp <name-regex>` — the key-pattern read.
//!
//! Output shape is git's: one `key value` line per matching entry, separated by
//! a **space** (not the `=` of `--list`), keys git-normalized (section and value
//! names lower-cased, subsection case preserved), file order preserved with
//! multivars repeated rather than collapsed. No match is exit 1 with no output;
//! an invalid ERE is `error: invalid key pattern: <pattern>` at exit 6.
//!
//! `gh` runs this on every push (`^remote\..*\.gh-resolved$` and
//! `^branch\.<name>\.(remote|merge|pushremote|gh-merge-base)$`), so the two
//! anchored-alternation cases below are the real-world regression guard.
//!
//! Every case reads with `--local` so the assertions depend only on the repo
//! config this test writes — never on the developer's global/system files, which
//! would otherwise leak into the merged snapshot and make expected output
//! machine-specific.

use std::path::Path;
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

/// Run the zvcs binary in `dir` and return the raw output.
fn zvcs(dir: &Path, args: &[&str]) -> Output {
    Command::new(BIN).args(args).current_dir(dir).output().expect("run zvcs git")
}

fn stdout_of(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn stderr_of(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

/// A repo whose LOCAL config carries a known set of entries: a multivar
/// (`remote.origin.fetch` twice), a subsection key, mixed-case names to prove
/// normalization, and the `branch.main.*` keys gh probes.
///
/// Named per test and per pid so concurrent test binaries never share a repo.
fn repo(tag: &str) -> std::path::PathBuf {
    let p = std::env::temp_dir().join(format!("zvcs-cfgre-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&p);
    std::fs::create_dir_all(&p).expect("mkdir fixture");
    assert!(zvcs(&p, &["init", "-q", "-b", "main"]).status.success(), "init failed");

    let config = p.join(".git").join("config");
    let mut text = std::fs::read_to_string(&config).unwrap_or_default();
    text.push_str(
        r#"
[remote "origin"]
	url = git@github.com:o/r.git
	fetch = +refs/heads/*:refs/remotes/origin/*
	fetch = +refs/tags/*:refs/tags/*
[branch "main"]
	remote = origin
	merge = refs/heads/main
[User]
	Name = Ada
"#,
    );
    std::fs::write(&config, text).expect("write config");
    p
}

#[test]
fn matches_keys_in_file_order_with_multivars_repeated() {
    let dir = repo("keys");
    let out = zvcs(&dir, &["config", "--local", "--get-regexp", r"^remote\."]);

    assert!(out.status.success(), "exit {:?}", out.status.code());
    assert_eq!(
        stdout_of(&out),
        "remote.origin.url git@github.com:o/r.git\n\
         remote.origin.fetch +refs/heads/*:refs/remotes/origin/*\n\
         remote.origin.fetch +refs/tags/*:refs/tags/*\n",
        "space-separated, file order, both multivar values"
    );
}

#[test]
fn key_names_are_normalized_to_lowercase() {
    let dir = repo("lowercase");
    let out = zvcs(&dir, &["config", "--local", "--get-regexp", r"^user\."]);

    // `[User] Name` is written mixed-case; git reports `user.name`.
    assert_eq!(stdout_of(&out), "user.name Ada\n");
}

#[test]
fn gh_push_probes_resolve() {
    let dir = repo("gh");

    // gh's remote-resolution probe: no such key here, so exit 1 and no output.
    let resolved =
        zvcs(&dir, &["config", "--local", "--get-regexp", r"^remote\..*\.gh-resolved$"]);
    assert_eq!(resolved.status.code(), Some(1), "unmatched pattern is exit 1");
    assert!(stdout_of(&resolved).is_empty());

    // gh's branch probe: an anchored alternation that must match two of four.
    let branch = zvcs(
        &dir,
        &[
            "config",
            "--local",
            "--get-regexp",
            r"^branch\.main\.(remote|merge|pushremote|gh-merge-base)$",
        ],
    );
    assert!(branch.status.success());
    assert_eq!(stdout_of(&branch), "branch.main.remote origin\nbranch.main.merge refs/heads/main\n");
}

#[test]
fn name_only_drops_the_value_half() {
    let dir = repo("nameonly");
    let out =
        zvcs(&dir, &["config", "--local", "--name-only", "--get-regexp", r"^branch\."]);

    assert!(out.status.success());
    assert_eq!(stdout_of(&out), "branch.main.remote\nbranch.main.merge\n");
}

#[test]
fn invalid_key_pattern_is_exit_6() {
    let dir = repo("badpat");
    let out = zvcs(&dir, &["config", "--local", "--get-regexp", "["]);

    assert_eq!(out.status.code(), Some(6), "git reports a bad key pattern as exit 6");
    assert_eq!(String::from_utf8_lossy(&out.stderr), "error: invalid key pattern: [\n");
    assert!(stdout_of(&out).is_empty());
}

/// A repo whose local config carries one multivar with three values, for the
/// `<value-pattern>` reads.
fn multivar_repo(tag: &str) -> std::path::PathBuf {
    let p = std::env::temp_dir().join(format!("zvcs-cfgvp-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&p);
    std::fs::create_dir_all(&p).expect("mkdir fixture");
    assert!(zvcs(&p, &["init", "-q", "-b", "main"]).status.success(), "init failed");
    for v in ["one", "two", "three"] {
        assert!(zvcs(&p, &["config", "--add", "a.b", v]).status.success(), "add {v}");
    }
    p
}

#[test]
fn value_pattern_selects_which_values_are_read() {
    let dir = multivar_repo("select");

    // `--get` reports the LAST value that survives the filter, not the first.
    let last = zvcs(&dir, &["config", "--local", "--get", "a.b", "t"]);
    assert!(last.status.success());
    assert_eq!(stdout_of(&last), "three\n");

    // `--get-all` reports every survivor, in file order.
    let all = zvcs(&dir, &["config", "--local", "--get-all", "a.b", "t"]);
    assert_eq!(stdout_of(&all), "two\nthree\n");

    // A leading `!` inverts the match.
    let inverted = zvcs(&dir, &["config", "--local", "--get", "a.b", "!t"]);
    assert_eq!(stdout_of(&inverted), "one\n");

    // Selecting nothing is exit 1 with no output — git does not distinguish it
    // from an absent key.
    let none = zvcs(&dir, &["config", "--local", "--get", "a.b", "zz"]);
    assert_eq!(none.status.code(), Some(1));
    assert!(stdout_of(&none).is_empty());
}

#[test]
fn get_regexp_narrows_by_value_too() {
    let dir = multivar_repo("regexp");

    let out = zvcs(&dir, &["config", "--local", "--get-regexp", r"^a\.", "tw"]);

    assert!(out.status.success());
    assert_eq!(stdout_of(&out), "a.b two\n", "key pattern AND value pattern both apply");
}

#[test]
fn an_invalid_value_pattern_is_exit_6() {
    let dir = multivar_repo("badvalue");

    let out = zvcs(&dir, &["config", "--local", "--get", "a.b", "["]);

    assert_eq!(out.status.code(), Some(6));
    assert_eq!(String::from_utf8_lossy(&out.stderr), "error: invalid pattern: [\n");
}

/// A repo with one multivar, one boolean, one number and one path, for the
/// display-flag and type-conversion reads.
fn typed_repo(tag: &str) -> std::path::PathBuf {
    let p = std::env::temp_dir().join(format!("zvcs-cfgtype-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&p);
    std::fs::create_dir_all(&p).expect("mkdir fixture");
    assert!(zvcs(&p, &["init", "-q", "-b", "main"]).status.success(), "init failed");
    let seeds: [&[&str]; 4] = [
        &["config", "--local", "a.b", "one"],
        &["config", "--local", "--add", "a.b", "two"],
        &["config", "--local", "sec.flag", "yes"],
        &["config", "--local", "num.n", "1k"],
    ];
    for args in seeds {
        assert!(zvcs(&p, args).status.success(), "seed {args:?}");
    }
    p
}

#[test]
fn type_canonicalizes_and_rejects() {
    let dir = typed_repo("type");

    // git's boolean grammar: `yes` prints as `true`.
    let b = zvcs(&dir, &["config", "--local", "--type=bool", "sec.flag"]);
    assert_eq!(stdout_of(&b), "true\n");
    // The legacy spelling is the same flag.
    let legacy = zvcs(&dir, &["config", "--local", "--bool", "sec.flag"]);
    assert_eq!(stdout_of(&legacy), "true\n");
    // `1k` is 1024 under git's integer grammar.
    let n = zvcs(&dir, &["config", "--local", "--type=int", "num.n"]);
    assert_eq!(stdout_of(&n), "1024\n");

    // A value that is not of the requested type is fatal, and the message names
    // the FIRST offending value even though `--get` would return the last.
    let bad = zvcs(&dir, &["config", "--local", "--get", "--type=bool", "a.b"]);
    assert_eq!(bad.status.code(), Some(128));
    assert_eq!(
        String::from_utf8_lossy(&bad.stderr),
        "fatal: bad boolean config value 'one' for 'a.b'\n"
    );
}

#[test]
fn show_origin_and_scope_prefix_the_output() {
    let dir = typed_repo("origin");

    let list = zvcs(&dir, &["config", "--local", "--list", "--show-scope"]);
    assert!(
        stdout_of(&list).lines().all(|l| l.starts_with("local\t")),
        "every line of a --local list is in the local scope: {:?}",
        stdout_of(&list)
    );

    // A get prints the VALUE only — the prefix is added, the key is not.
    let get = zvcs(&dir, &["config", "--local", "--get", "--show-origin", "a.b"]);
    assert_eq!(stdout_of(&get), "file:.git/config\ttwo\n");
}

#[test]
fn null_separates_records_with_nul() {
    let dir = typed_repo("null");

    let out = zvcs(&dir, &["config", "--local", "--list", "--null"]);
    let text = String::from_utf8_lossy(&out.stdout);

    assert!(text.contains("a.b\none\0"), "key NL value NUL: {text:?}");
    assert!(!text.contains("a.b=one"), "the `=` form is not used under --null");
}

#[test]
fn replace_all_collapses_matching_values() {
    let dir = typed_repo("replace");

    assert!(zvcs(&dir, &["config", "--local", "--replace-all", "a.b", "ZZ"]).status.success());
    let all = zvcs(&dir, &["config", "--local", "--get-all", "a.b"]);
    assert_eq!(stdout_of(&all), "ZZ\n", "both values collapse into one");

    // With a pattern, only the values it selects are replaced.
    assert!(zvcs(&dir, &["config", "--local", "--add", "a.b", "keepme"]).status.success());
    assert!(zvcs(&dir, &["config", "--local", "--replace-all", "a.b", "QQ", "keep"]).status.success());
    let after = zvcs(&dir, &["config", "--local", "--get-all", "a.b"]);
    assert_eq!(stdout_of(&after), "ZZ\nQQ\n");
}

#[test]
fn sections_rename_and_remove() {
    let dir = typed_repo("sections");

    assert!(zvcs(&dir, &["config", "--local", "--rename-section", "sec", "newsec"]).status.success());
    let moved = zvcs(&dir, &["config", "--local", "--get", "newsec.flag"]);
    assert_eq!(stdout_of(&moved), "yes\n", "values survive the rename verbatim");

    assert!(zvcs(&dir, &["config", "--local", "--remove-section", "newsec"]).status.success());
    let gone = zvcs(&dir, &["config", "--local", "--get", "newsec.flag"]);
    assert_eq!(gone.status.code(), Some(1));

    let missing = zvcs(&dir, &["config", "--local", "--remove-section", "nosuch"]);
    assert_eq!(missing.status.code(), Some(128));
    assert_eq!(String::from_utf8_lossy(&missing.stderr), "fatal: no such section: nosuch\n");
}

#[test]
fn get_colorbool_answers_through_the_exit_code() {
    let dir = typed_repo("colorbool");

    // With the tty-ness stated, git PRINTS the answer and exits 0 either way.
    let stated = zvcs(&dir, &["config", "--local", "--get-colorbool", "color.ui", "true"]);
    assert!(stated.status.success());
    assert_eq!(stdout_of(&stated), "true\n");

    // With it omitted and stdout captured (not a terminal), the answer is the
    // exit code alone — no output.
    let probed = zvcs(&dir, &["config", "--local", "--get-colorbool", "color.ui"]);
    assert_eq!(probed.status.code(), Some(1));
    assert!(stdout_of(&probed).is_empty());
}

/// A standalone config file with URL-specific `http` subsections, plus an
/// included file, for `--get-urlmatch` and `--includes`.
fn url_config(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("zvcs-cfgurl-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("mkdir fixture");
    std::fs::write(dir.join("sub.cfg"), "[inc]\n\tfrom = included\n").expect("sub");
    std::fs::write(
        dir.join("main.cfg"),
        format!(
            "[include]\n\tpath = {}/sub.cfg\n\
             [http]\n\tsslVerify = true\n\tcookieFile = /tmp/generic\n\
             [http \"https://example.com\"]\n\tsslVerify = false\n\
             [http \"https://example.com/path\"]\n\tcookieFile = /tmp/path\n\
             [http \"https://user@example.com/path\"]\n\tcookieFile = /tmp/user\n",
            dir.display()
        ),
    )
    .expect("main");
    dir
}

#[test]
fn urlmatch_picks_the_most_specific_subsection() {
    let dir = url_config("urlmatch");
    let cfg = "main.cfg";

    // A longer matching path beats the generic entry.
    let path = zvcs(&dir, &["config", "-f", cfg, "--get-urlmatch", "http.cookieFile", "https://example.com/path/deeper"]);
    assert_eq!(stdout_of(&path), "/tmp/path\n");

    // A pattern naming a user beats one that does not, for the same path.
    let user = zvcs(&dir, &["config", "-f", cfg, "--get-urlmatch", "http.cookieFile", "https://user@example.com/path"]);
    assert_eq!(stdout_of(&user), "/tmp/user\n");

    // A URL matching no pattern still gets the section's generic value.
    let generic = zvcs(&dir, &["config", "-f", cfg, "--get-urlmatch", "http.cookieFile", "https://other.example.org"]);
    assert_eq!(stdout_of(&generic), "/tmp/generic\n");

    // A whole-section query prints `section.key value` for every key.
    let section = zvcs(&dir, &["config", "-f", cfg, "--get-urlmatch", "http", "https://example.com"]);
    assert_eq!(stdout_of(&section), "http.cookiefile /tmp/generic\nhttp.sslverify false\n");

    // A key no candidate defines is exit 1.
    let missing = zvcs(&dir, &["config", "-f", cfg, "--get-urlmatch", "http.nosuch", "https://example.com"]);
    assert_eq!(missing.status.code(), Some(1));
}

#[test]
fn includes_are_followed_only_when_asked() {
    let dir = url_config("includes");

    let off = zvcs(&dir, &["config", "-f", "main.cfg", "--get", "inc.from"]);
    assert_eq!(off.status.code(), Some(1), "include.path is not followed by default here");

    let on = zvcs(&dir, &["config", "-f", "main.cfg", "--includes", "--get", "inc.from"]);
    assert!(on.status.success());
    assert_eq!(stdout_of(&on), "included\n");
}

#[test]
fn edit_runs_the_configured_editor_on_the_target_file() {
    let dir = std::env::temp_dir().join(format!("zvcs-cfgedit-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("mkdir");
    assert!(zvcs(&dir, &["init", "-q", "-b", "main"]).status.success());

    // A scripted "editor" that appends a section, so the effect is observable
    // without a terminal.
    let out = Command::new(BIN)
        .args(["config", "--local", "--edit"])
        .current_dir(&dir)
        .env("GIT_EDITOR", r#"printf '[edited]\n\tby = zvcs\n' >>"#)
        .output()
        .expect("run zvcs git");

    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let read_back = zvcs(&dir, &["config", "--local", "--get", "edited.by"]);
    assert_eq!(stdout_of(&read_back), "zvcs\n", "the editor's write landed in the target file");
}

/// `git config get --regexp` and `git config --get-regexp` share `get_value()`
/// but disagree on display: the subcommand form is a *value* reader with a
/// regexp key (`GET_VALUE_KEY_REGEXP`, builtin/config.c:511-516), printing the
/// last match alone unless `--all` asks for every one, while the legacy option
/// prints `key value` for all of them.
#[test]
fn the_get_subcommand_regexp_prints_values_and_the_legacy_option_prints_pairs() {
    let dir = repo("sub-vs-legacy");
    let pattern = r"^branch\.main\.";

    let last = zvcs(&dir, &["config", "get", "--local", "--regexp", pattern]);
    assert!(last.status.success(), "get --regexp failed");
    assert_eq!(stdout_of(&last), "refs/heads/main\n", "only the last value, no key");

    let all = zvcs(&dir, &["config", "get", "--local", "--all", "--regexp", pattern]);
    assert_eq!(stdout_of(&all), "origin\nrefs/heads/main\n", "every value, in file order");

    let named = zvcs(&dir, &["config", "get", "--local", "--all", "--show-names", "--regexp", pattern]);
    assert_eq!(
        stdout_of(&named),
        "branch.main.remote origin\nbranch.main.merge refs/heads/main\n",
        "--show-names restores the keys the legacy form always prints"
    );

    let legacy = zvcs(&dir, &["config", "--local", "--get-regexp", pattern]);
    assert_eq!(stdout_of(&legacy), stdout_of(&named), "legacy is --all --show-names");
}

/// `get_value()` lower-cases the *pattern* before compiling it — the section
/// name up to the first `.` and the variable name back from the last one, with
/// the subsection between them untouched (builtin/config.c:563-569). Regexp
/// metacharacters ride along, so `USER\.NAME` still matches `user.name`.
#[test]
fn a_key_pattern_is_lower_cased_the_way_a_key_is() {
    let dir = repo("pattern-case");

    // Without --regexp the name is a literal key, and key lookup folds case
    // itself — so the same spelling finds the entry.
    let upper = zvcs(&dir, &["config", "get", "--local", "USER.NAME"]);
    assert_eq!(stdout_of(&upper), "Ada\n", "a literal key lookup folds case too");

    let matched = zvcs(&dir, &["config", "get", "--local", "--regexp", r"^USER\.NAME$"]);
    assert!(matched.status.success(), "lower-cased pattern should match");
    assert_eq!(stdout_of(&matched), "Ada\n");

    // The subsection keeps its case, so a pattern that lower-cases it misses.
    let sub = zvcs(&dir, &["config", "get", "--local", "--regexp", r"^REMOTE\.origin\.URL$"]);
    assert_eq!(stdout_of(&sub), "git@github.com:o/r.git\n", "section and name fold, subsection does not");
    let folded = zvcs(&dir, &["config", "get", "--local", "--regexp", r"^remote\.ORIGIN\.url$"]);
    assert_eq!(folded.status.code(), Some(1), "a folded subsection is a different key");
}

/// The `get` subcommand's own option table makes `--name-only` and `--default`
/// unconditional (builtin/config.c:1082-1097); only the legacy form restricts
/// them to `--list`/`--get-regexp` and `--get`. An empty regexp result with a
/// `--default` prints the default, formatted the same way a stored value is.
#[test]
fn the_get_subcommand_accepts_name_only_and_default_with_a_regexp() {
    let dir = repo("sub-display-opts");

    let names = zvcs(&dir, &["config", "get", "--local", "--name-only", "--all", "--regexp", r"^branch\."]);
    assert!(names.status.success(), "--name-only under `get` must not be a usage error");
    assert_eq!(stdout_of(&names), "\n\n", "values omitted, keys not asked for: two empty lines");

    let default = zvcs(&dir, &["config", "get", "--local", "--default", "fallback", "--regexp", r"^zz\."]);
    assert!(default.status.success(), "--default under `get` must not be a usage error");
    assert_eq!(stdout_of(&default), "fallback\n");
}

/// The URL a `--get-urlmatch` compares is `url_normalize()`'s output, not the
/// string as typed: the scheme and host fold to lower case, a default port
/// disappears, `.`/`..` segments resolve, and `%XX` escapes are unescaped where
/// they need not be there and upper-cased where they do. Two spellings of one
/// URL must therefore pick the same entry.
#[test]
fn urlmatch_compares_normalized_urls() {
    let dir = url_config("urlmatch-normalize");
    let cfg = dir.join("norm.cfg");
    std::fs::write(
        &cfg,
        "[http]\n\tcookieFile = generic\n\
         [http \"https://example.com/a/b\"]\n\tcookieFile = deep\n\
         [http \"https://*.example.com\"]\n\tcookieFile = wild\n",
    )
    .expect("write norm.cfg");
    let cfg = cfg.to_str().expect("utf-8 fixture path");

    let same = |url: &str| {
        let out = zvcs(&dir, &["config", "-f", cfg, "--get-urlmatch", "http.cookieFile", url]);
        stdout_of(&out)
    };

    // :443 is https's default, `.`/`..` resolve, and case folds — all four of
    // these normalize to the same URL as the `/a/b` pattern.
    assert_eq!(same("https://example.com/a/b"), "deep\n");
    assert_eq!(same("https://example.com:443/a/b"), "deep\n");
    assert_eq!(same("HTTPS://EXAMPLE.COM/a/./b"), "deep\n");
    assert_eq!(same("https://example.com/a/x/../b/"), "deep\n");
    // %62 is 'b', which needs no escape, so it unescapes before the comparison.
    assert_eq!(same("https://example.com/a/%62"), "deep\n");

    // A `*` in a pattern's host matches one whole component, and nothing else:
    // `sub.example.com` matches, `example.com` does not (there is no component
    // for the `*` to consume) and neither does a two-label subdomain.
    assert_eq!(same("https://sub.example.com/"), "wild\n");
    assert_eq!(same("https://example.com/"), "generic\n");
    assert_eq!(same("https://a.b.example.com/"), "generic\n");

    // A port that is not the default is part of the comparison, so it stops the
    // otherwise-matching pattern.
    assert_eq!(same("https://example.com:8443/a/b"), "generic\n");
}

/// `url_normalize()` names each way a URL can be malformed, and
/// `cmd_config_get_urlmatch()` prints that message and nothing else.
#[test]
fn urlmatch_refuses_a_malformed_url_in_gits_own_words() {
    let dir = url_config("urlmatch-refusals");
    let cfg = dir.join("norm.cfg");
    std::fs::write(&cfg, "[http]\n\tcookieFile = generic\n").expect("write norm.cfg");
    let cfg = cfg.to_str().expect("utf-8 fixture path");

    for (url, message) in [
        ("not-a-url", "invalid URL scheme name or missing '://' suffix"),
        ("1http://example.com/", "invalid URL scheme name or missing '://' suffix"),
        ("http://", "missing host and scheme is not 'file:'"),
        // Only a `file:` URL with no host at all reaches this one; `file:` with
        // a host and a port is a perfectly ordinary URL.
        ("file://:80/x", "a 'file:' URL may not have a port number"),
        ("http://ex ample.com/", "invalid characters in host name"),
        ("http://example.com:99999/", "invalid port number"),
        ("http://example.com:0/", "invalid port number"),
        ("http://example.com/%zz", "invalid %XX escape sequence"),
        ("http://example.com/../x", "invalid '..' path segment"),
    ] {
        let out = zvcs(&dir, &["config", "-f", cfg, "--get-urlmatch", "http.cookieFile", url]);
        assert_eq!(out.status.code(), Some(128), "{url}");
        assert_eq!(stderr_of(&out), format!("fatal: {message}\n"), "{url}");
    }
}
