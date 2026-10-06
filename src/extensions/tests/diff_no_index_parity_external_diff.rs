//! `diff --no-index` never ran an external diff program.
//!
//! `cmd_diff()` raises `allow_external` before it branches to the no-index path
//! (builtin/diff.c:511), so `GIT_EXTERNAL_DIFF`, `diff.external` and a path's
//! `diff.<driver>.command` replace `builtin_diff()` there as everywhere else
//! (`run_diff_cmd()`, diff.c:4916-4951). A no-index side has no valid object id,
//! so `prepare_temp_file()` hands the program the path itself named by the null
//! id, a missing side as `/dev/null . .`, and a symlink as a temporary file
//! holding its target; the post-image name and `fill_metainfo()`'s header follow
//! because the two names always differ (`run_external_diff()`, diff.c:4756-4814).
//! zvcs printed its own patch, refused `--ext-diff`, and under `-w` dropped the
//! raw row of a pair whose program cannot report "no change" (diff.c:4774-4777).
//!
//! Expectations measured from stock git 2.56.0 under the same environment.

use std::path::PathBuf;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

/// Prints its arguments bracketed and the two path counters.
const SHOW: &str = "#!/bin/sh\nprintf '[%s]' \"$@\"; printf ' c=%s t=%s\\n' \"$GIT_DIFF_PATH_COUNTER\" \"$GIT_DIFF_PATH_TOTAL\"\n";

const NULL: &str = "0000000000000000000000000000000000000000";

struct Fixture {
    root: PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

impl Fixture {
    /// `d1` and `d2` hold a modification (`f`), a deletion (`old`), an addition
    /// (`new`), a rename (`r` -> `rr`) and a symlink retarget (`l`); `ws1`/`ws2`
    /// differ in trailing whitespace alone.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir()
            .join(format!("zvcs-no-index-ext-diff-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        for d in ["d1", "d2"] {
            std::fs::create_dir_all(root.join(d)).unwrap();
        }
        let write = |p: &str, s: &str| std::fs::write(root.join(p), s).unwrap();
        write("show.sh", SHOW);
        write("d1/f", "1\n");
        write("d2/f", "2\n");
        write("d2/new", "n\n");
        write("d1/old", "o\n");
        write("d1/r", "line1\nline2\nline3\nline4\n");
        write("d2/rr", "line1\nline2\nline3\nline4x\n");
        write("ws1", "x\n");
        write("ws2", "x \n");
        std::os::unix::fs::symlink("tgt", root.join("d1/l")).unwrap();
        std::os::unix::fs::symlink("tgt2", root.join("d2/l")).unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(root.join("show.sh"), std::fs::Permissions::from_mode(0o755)).unwrap();
        Fixture { root }
    }

    fn run(&self, env: &[(&str, &str)], args: &[&str]) -> (String, String, i32) {
        let out = Command::new(BIN)
            .args(args)
            .current_dir(&self.root)
            .env("HOME", &self.root)
            .env("GIT_CEILING_DIRECTORIES", &self.root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env_remove("GIT_EXTERNAL_DIFF")
            .env_remove("GIT_EXTERNAL_DIFF_TRUST_EXIT_CODE")
            .envs(env.iter().copied())
            .env("LC_ALL", "C")
            .output()
            .unwrap();
        (
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
            out.status.code().expect("no signal"),
        )
    }
}

/// The argument group a regular-file pair gets, with its trailing header.
fn group(name: &str, other: &str, msg: &str, n: u32, total: u32) -> String {
    format!("[{name}][{name}][{NULL}][100644][{other}][{NULL}][100644][{other}][{msg}] c={n} t={total}\n")
}

#[test]
fn every_pair_of_a_directory_walk_goes_to_the_program() {
    let f = Fixture::new("walk");
    let (out, err, code) = f.run(&[("GIT_EXTERNAL_DIFF", "./show.sh")], &["diff", "--no-index", "d1", "d2"]);
    assert_eq!((err.as_str(), code), ("", 1));
    let lines: Vec<&str> = out.split_inclusive(" t=5\n").collect();
    assert_eq!(lines.len(), 5, "{out}");
    assert_eq!(lines[0], group("d1/f", "d2/f", "index d00491f..0cfbf08 100644\n", 1, 5));
    // The symlink sides are temporary files named after the link, each holding
    // its target and named by the null id and `S_IFLNK`.
    assert!(lines[1].starts_with("[d1/l]["), "{}", lines[1]);
    assert!(
        lines[1].ends_with(&format!(
            "/l][{NULL}][120000][d2/l][index c7e58fc..91e4cef 120000\n] c=2 t=5\n"
        )),
        "{}",
        lines[1]
    );
    assert_eq!(
        lines[2],
        format!("[/dev/null][/dev/null][.][.][d2/new][{NULL}][100644][d2/new][index 0000000..8ba3a16\n] c=3 t=5\n")
    );
    assert_eq!(
        lines[3],
        format!("[d1/old][d1/old][{NULL}][100644][/dev/null][.][.][/dev/null][index 13e7564..0000000\n] c=4 t=5\n")
    );
    assert_eq!(
        lines[4],
        group(
            "d1/r",
            "d2/rr",
            "similarity index 72%\nrename from d1/r\nrename to d2/rr\nindex 84275f9..85bd6ea 100644\n",
            5,
            5
        )
    );
}

#[test]
fn the_flags_and_the_configuration_select_the_program() {
    let f = Fixture::new("select");
    let env = [("GIT_EXTERNAL_DIFF", "./show.sh")];
    let ext = group("d1/f", "d2/f", "index d00491f..0cfbf08 100644\n", 1, 1);
    assert_eq!(f.run(&env, &["diff", "--no-index", "--ext-diff", "d1/f", "d2/f"]), (ext.clone(), String::new(), 1));
    assert_eq!(
        f.run(&env, &["diff", "--no-index", "--no-ext-diff", "d1/f", "d2/f"]).0,
        "diff --git a/d1/f b/d2/f\nindex d00491f..0cfbf08 100644\n--- a/d1/f\n+++ b/d2/f\n@@ -1 +1 @@\n-1\n+2\n"
    );
    // `fill_metainfo()` opens its lines with the line prefix; the program's own
    // output carries none.
    assert_eq!(
        f.run(&env, &["diff", "--no-index", "--line-prefix=> ", "d1/f", "d2/f"]).0,
        group("d1/f", "d2/f", "> index d00491f..0cfbf08 100644\n", 1, 1)
    );
    // `diff.external`; the stat is still the port's own, and the separator
    // between the two blocks is written as usual.
    assert_eq!(
        f.run(&[], &["-c", "diff.external=./show.sh", "diff", "--no-index", "--stat", "-p", "d1/f", "d2/f"]),
        (format!(" {{d1 => d2}}/f | 2 +-\n 1 file changed, 1 insertion(+), 1 deletion(-)\n\n{ext}"), String::new(), 1)
    );
    assert_eq!(
        f.run(&[("GIT_EXTERNAL_DIFF", "false")], &["diff", "--no-index", "d1/f", "d2/f"]),
        (String::new(), "fatal: external diff died, stopping at d1/f\n".into(), 128)
    );
}

#[test]
fn an_untrusted_program_makes_every_pair_a_change_under_ignore_options() {
    let f = Fixture::new("ws");
    assert_eq!(f.run(&[], &["diff", "--no-index", "-w", "--raw", "ws1", "ws2"]), (String::new(), String::new(), 0));
    assert_eq!(
        f.run(&[], &["-c", "diff.external=./show.sh", "diff", "--no-index", "-w", "--raw", "ws1", "ws2"]),
        (":100644 100644 587be6b 9f009d6 M\tws1\n".into(), String::new(), 1)
    );
}
