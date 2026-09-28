//! `range-diff --ext-diff` hands each matched pair to the external program.
//!
//! `add_diff_options()` binds the whole `git diff` table to range-diff's
//! `diffopt` (builtin/range-diff.c:83), so `--ext-diff` raises
//! `flags.allow_external` (diff.c:6260) and `run_diff_cmd()` sends the
//! `a`/`b` filepair `patch_diff()` queues (range-diff.c:477-499) to
//! `external_diff()`'s program — `diff.external`, which range-diff reads
//! through `git_diff_ui_config()` — or to `a`'s `diff.<driver>.command`
//! (diff.c:4953-4972). Neither side carries an object id, so
//! `prepare_temp_file()` (diff.c:4698-4750) passes the worktree's `a` and `b`
//! when they exist (a symlink as a temporary file holding its target, mode
//! 120000) and the `/dev/null . .` triple when they do not. `diff_flush_patch()`'s
//! `diff_unmodified_pair()` compares the two paths, which always differ, so an
//! `=` pair runs the program too, and `o->diff_path_counter` keeps counting
//! across pairs. zvcs refused the flag (`unsupported flag "--ext-diff"`).
//!
//! Expectations measured from stock git 2.55.0 under the same environment.

use std::path::PathBuf;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

struct Fixture {
    root: PathBuf,
    work: PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

impl Fixture {
    /// `t1` and `t2` both fork `main`: an identical `change f` and a `change g`
    /// that differs in one line, so the page has an `=` and a `!` pair. The
    /// driver at `../ext.sh` prints its scalar arguments, the argument count and
    /// the path counters, then the two files it was handed.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir()
            .join(format!("zvcs-range-diff-ext-diff-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        let lines = |edit: &[(usize, &str)]| -> String {
            (1..=20)
                .map(|n| {
                    let word = edit.iter().find(|(at, _)| *at == n).map(|(_, w)| w.to_string());
                    word.unwrap_or_else(|| n.to_string()) + "\n"
                })
                .collect()
        };
        f.ok(&["init", "-q", "-b", "main", "."]);
        std::fs::write(f.work.join("f"), lines(&[])).unwrap();
        std::fs::write(f.work.join("g"), lines(&[])).unwrap();
        f.ok(&["add", "f", "g"]);
        f.ok(&["commit", "-q", "-m", "init"]);
        for (branch, three) in [("t1", "three"), ("t2", "THREE")] {
            f.ok(&["checkout", "-q", "-b", branch, "main"]);
            std::fs::write(f.work.join("f"), lines(&[(5, "five"), (9, "nine")])).unwrap();
            f.ok(&["commit", "-q", "-am", "change f"]);
            std::fs::write(f.work.join("g"), lines(&[(3, three)])).unwrap();
            f.ok(&["commit", "-q", "-am", "change g"]);
        }
        f.ok(&["checkout", "-q", "main"]);
        let script = f.root.join("ext.sh");
        std::fs::write(
            &script,
            "#!/bin/sh\necho \"$1|$3|$4|$6|$7|$8|$#|$GIT_DIFF_PATH_COUNTER/$GIT_DIFF_PATH_TOTAL\"\ncat \"$2\" \"$5\"\n",
        )
        .unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        f
    }

    fn run(&self, args: &[&str]) -> (String, String, i32) {
        let out = Command::new(BIN)
            .args(args)
            .current_dir(&self.work)
            .env("HOME", &self.root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env_remove("GIT_EXTERNAL_DIFF")
            .env("GIT_AUTHOR_NAME", "A U Thor")
            .env("GIT_AUTHOR_EMAIL", "a@e.x")
            .env("GIT_COMMITTER_NAME", "C O Mitter")
            .env("GIT_COMMITTER_EMAIL", "c@e.x")
            .env("GIT_AUTHOR_DATE", "1112911993 -0700")
            .env("GIT_COMMITTER_DATE", "1112911993 -0700")
            .env("LC_ALL", "C")
            .output()
            .unwrap();
        (
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
            out.status.code().expect("no signal"),
        )
    }

    fn ok(&self, args: &[&str]) -> String {
        let (out, err, code) = self.run(args);
        assert_eq!(code, 0, "{args:?}: {err}");
        out
    }
}

const RANGES: [&str; 2] = ["main..t1", "main..t2"];

fn range_diff<'a>(pre: &[&'a str], opts: &[&'a str]) -> Vec<&'a str> {
    let mut v = pre.to_vec();
    v.push("range-diff");
    v.extend_from_slice(opts);
    v.extend_from_slice(&RANGES);
    v
}

#[test]
fn diff_external_runs_for_every_matched_pair() {
    let f = Fixture::new("external");
    let out = f.ok(&range_diff(&["-c", "diff.external=../ext.sh"], &["--ext-diff"]));
    assert_eq!(
        out,
        "1:  3354083 = 1:  3354083 change f\n\
         a|.|.|.|.|b|8|1/1\n\
         2:  d09b753 ! 2:  98b7c20 change g\n\
         a|.|.|.|.|b|8|2/1\n"
    );
}

#[test]
fn worktree_a_and_b_are_what_the_program_reads() {
    let f = Fixture::new("worktree");
    std::fs::write(f.work.join("a"), "left\n").unwrap();
    std::os::unix::fs::symlink("target", f.work.join("b")).unwrap();
    let out = f.ok(&range_diff(&["-c", "diff.external=../ext.sh"], &["--ext-diff"]));
    let null = "0000000000000000000000000000000000000000";
    assert_eq!(
        out,
        format!(
            "1:  3354083 = 1:  3354083 change f\n\
             a|{null}|100644|{null}|120000|b|8|1/1\n\
             left\n\
             target2:  d09b753 ! 2:  98b7c20 change g\n\
             a|{null}|100644|{null}|120000|b|8|2/1\n\
             left\n\
             target"
        )
    );
}

#[test]
fn a_driver_command_for_path_a_beats_diff_external() {
    let f = Fixture::new("driver");
    std::fs::write(f.work.join(".gitattributes"), "a diff=drv\n").unwrap();
    let out = f.ok(&range_diff(
        &["-c", "diff.external=false", "-c", "diff.drv.command=../ext.sh"],
        &["--ext-diff"],
    ));
    assert_eq!(
        out,
        "1:  3354083 = 1:  3354083 change f\n\
         a|.|.|.|.|b|8|1/1\n\
         2:  d09b753 ! 2:  98b7c20 change g\n\
         a|.|.|.|.|b|8|2/1\n"
    );
}

#[test]
fn allow_external_is_off_by_default_and_last_flag_wins() {
    let f = Fixture::new("toggle");
    let internal = f.ok(&range_diff(&["-c", "diff.external=../ext.sh"], &[]));
    assert!(internal.contains("    -+three\n    ++THREE\n"), "{internal}");
    let toggled = f.ok(&range_diff(&["-c", "diff.external=../ext.sh"], &["--ext-diff", "--no-ext-diff"]));
    assert_eq!(toggled, internal);
    // The stat is `builtin_diffstat()`'s own pass and never consults the program.
    let stat = f.ok(&range_diff(&["-c", "diff.external=../ext.sh"], &["--ext-diff", "--stat"]));
    assert_eq!(
        stat,
        "1:  3354083 = 1:  3354083 change f\n\
         2:  d09b753 ! 2:  98b7c20 change g\n     \
         a => b | 2 +-\n     \
         1 file changed, 1 insertion(+), 1 deletion(-)\n"
    );
}

#[test]
fn a_failing_program_stops_after_the_header_it_follows() {
    let f = Fixture::new("died");
    let (out, err, code) = f.run(&range_diff(&["-c", "diff.external=false"], &["--ext-diff"]));
    assert_eq!(
        (out.as_str(), err.as_str(), code),
        (
            "1:  3354083 = 1:  3354083 change f\n",
            "fatal: external diff died, stopping at a\n",
            128
        )
    );
}
