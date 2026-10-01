//! 2.56 added a built-in `swift` userdiff driver (userdiff.c:365-374). Before it,
//! `diff=swift` named no driver and fell back to the default funcname rule
//! (first column alphabetic) and the whitespace-run word splitter.
//!
//! The funcname pattern accepts leading indentation, `@attr(...)` attributes and
//! lowercase modifiers ahead of `func`/`init`/`subscript`/type keywords, so a
//! method or initialiser nested in a class names its own hunk. The word regex
//! splits identifiers, `0x`/`0o`/`0b` literals with `_`, and the Swift operators
//! (`..<`, `...`, `??`, `<<=`) out of a run of punctuation.
//!
//! Every expectation was read off stock git 2.56.0 on this fixture.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn run(dir: &Path, args: &[&str]) -> Output {
    Command::new(BIN)
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("LC_ALL", "C")
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@e.x")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@e.x")
        .env_remove("COLUMNS")
        .output()
        .unwrap()
}

fn git(dir: &Path, args: &[&str]) {
    let out = run(dir, args);
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
}

fn fixture(tag: &str, pre: &str, post: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!("zvcs-swift-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let repo = root.canonicalize().unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    std::fs::write(repo.join(".gitattributes"), "*.swift diff=swift\n").unwrap();
    std::fs::write(repo.join("t.swift"), pre).unwrap();
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-q", "-m", "c0"]);
    std::fs::write(repo.join("t.swift"), post).unwrap();
    repo
}

const PRE: &str = "import Foundation

@MainActor
public final class Widget: NSObject {
    var a = 1
    var b = 2
    var c = 3

    @objc(doThing:) private func doThing(x: Int) -> Int {
        let y = x + 1
        let z = y * 2
        let w = z - 3
        return 0x1F + 0b101 + 1_000.5e3
    }

    init?(name: String) {
        self.a = 1
        self.b = 2
        self.c = 3
    }
    subscript<T>(i: Int) -> T { fatalError() }
}
struct S {
    let one = 1
    let two = 2
    let three = 3
}
";

#[test]
fn nested_methods_and_initialisers_name_their_hunks() {
    let post = PRE
        .replace("var c = 3", "var c = 4")
        .replace("return 0x1F", "return 0x2F")
        .replace("self.c = 3", "self.c = 4")
        .replace("let three = 3", "let three = x ?? 3");
    let repo = fixture("funcname", PRE, &post);
    let out = run(&repo, &["diff", "-U1", "--", "t.swift"]);
    let headers: Vec<String> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter(|l| l.starts_with("@@"))
        .map(str::to_string)
        .collect();
    assert_eq!(
        headers,
        [
            "@@ -6,3 +6,3 @@ public final class Widget: NSObject {",
            "@@ -12,3 +12,3 @@ @objc(doThing:) private func doThing(x: Int) -> Int {",
            "@@ -18,3 +18,3 @@ init?(name: String) {",
            "@@ -25,3 +25,3 @@ struct S {",
        ]
    );
    let _ = std::fs::remove_dir_all(&repo);
}

#[test]
fn the_word_regex_splits_swift_operators_and_literals() {
    let repo = fixture(
        "words",
        "let r = a..<b ?? c<<=d + 0x1F_FF\n",
        "let r = a...b ?? c<<d + 0x1F_FE\n",
    );
    let out = run(&repo, &["diff", "--word-diff=plain", "--", "t.swift"]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_eq!(
        stdout.trim_end().rsplit('\n').next().unwrap(),
        "let r = a[-..<-]{+...+}b ?? c[-<<=-]{+<<+}d + [-0x1F_FF-]{+0x1F_FE+}"
    );
    let _ = std::fs::remove_dir_all(&repo);
}
