//! `git diff --no-index` with `--[no-]indent-heuristic` and `--no-ext-diff`.
//!
//! Both are on `add_diff_options()`'s table, which `diff_no_index()` parses
//! (diff-no-index.c:372), and zvcs refused them with `unsupported option` — so
//! `diff --indent-heuristic --diff-algorithm=2m --no-index ...` stopped on the
//! first instead of reaching stock's `diff-algorithm` error.
//!
//! `XDF_INDENT_HEURISTIC` comes from `diff_setup()` copying
//! `diff_indent_heuristic` (diff.c:57, 291, 5143), which `git_diff_ui_config()`
//! has read before the no-index split, and the flag overrides it. The no-index
//! body always ran the heuristic. `--no-ext-diff` clears the `allow_external`
//! bit `cmd_diff()` set (builtin/diff.c:511).
//!
//! Expectations measured from stock git 2.56.0 under the same environment.

use std::path::PathBuf;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

struct Fixture {
    root: PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

impl Fixture {
    /// No repository. `o`→`n` inserts a block between two others; the added
    /// group can slide by one block boundary, so the heuristic decides where.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir()
            .join(format!("zvcs-no-index-indent-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("o"), "  {\n    a();\n  }\n\n  {\n    c();\n  }\n").unwrap();
        std::fs::write(
            root.join("n"),
            "  {\n    a();\n  }\n\n  {\n    b();\n  }\n\n  {\n    c();\n  }\n",
        )
        .unwrap();
        Fixture { root }
    }

    fn run(&self, args: &[&str]) -> (String, String, i32) {
        let out = Command::new(BIN)
            .args(args)
            .current_dir(&self.root)
            .env("HOME", &self.root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CEILING_DIRECTORIES", self.root.parent().unwrap())
            .env("LC_ALL", "C")
            .env_remove("GIT_EXTERNAL_DIFF")
            .output()
            .unwrap();
        (
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
            out.status.code().expect("no signal"),
        )
    }

    /// The hunk lines of a run that differed and wrote nothing to stderr.
    fn hunk(&self, args: &[&str]) -> String {
        let (out, err, code) = self.run(args);
        assert_eq!((err.as_str(), code), ("", 1), "{args:?}");
        out.lines().skip(4).map(|l| format!("{l}\n")).collect()
    }
}

const HEURISTIC: &str = "@@ -2,6 +2,10 @@
     a();
   }
 
+  {
+    b();
+  }
+
   {
     c();
   }
";

const PLAIN: &str = "@@ -3,5 +3,9 @@
   }
 
   {
+    b();
+  }
+
+  {
     c();
   }
";

#[test]
fn indent_heuristic_flag_and_config_steer_the_slider() {
    let f = Fixture::new("slide");
    let diff = |pre: &[&str], post: &[&str]| {
        f.hunk(&[pre, &["diff", "--no-index"], post, &["o", "n"]].concat())
    };
    assert_eq!(diff(&[], &[]), HEURISTIC);
    assert_eq!(diff(&[], &["--indent-heuristic"]), HEURISTIC);
    assert_eq!(diff(&[], &["--no-indent-heuristic"]), PLAIN);
    assert_eq!(diff(&[], &["--no-indent-heuristic", "--indent-heuristic"]), HEURISTIC);
    assert_eq!(diff(&["-c", "diff.indentHeuristic=false"], &[]), PLAIN);
    assert_eq!(diff(&["-c", "diff.indentHeuristic=false"], &["--indent-heuristic"]), HEURISTIC);
    assert_eq!(diff(&[], &["--no-ext-diff"]), HEURISTIC);
}

/// Accepted options let a later bad value be the error stock reports.
#[test]
fn accepted_options_reach_the_later_error() {
    let f = Fixture::new("order");
    for flag in ["--indent-heuristic", "--no-ext-diff"] {
        let (out, err, code) = f.run(&["diff", "--no-index", flag, "--diff-algorithm=2m", "o", "n"]);
        assert_eq!(
            (out.as_str(), err.as_str(), code),
            ("", "error: option diff-algorithm accepts \"myers\", \"minimal\", \"patience\" and \"histogram\"\n", 129),
            "{flag}"
        );
    }
}
