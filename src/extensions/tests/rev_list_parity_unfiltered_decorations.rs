//! `git rev-list` reads decorations with no filter at all.
//!
//! `--simplify-by-decoration` keeps a commit when `get_name_decoration()` names
//! it (revision.c:789-794), and `--bisect-all` prints the same names. That
//! function loads the refs with `load_ref_decorations(NULL, …)`
//! (log-tree.c:94-98), and with a NULL filter `add_ref_decoration()` never calls
//! `ref_filter_match()` (log-tree.c:159-160): every ref decorates, including
//! `refs/bisect/*`, `refs/notes/*` and refs outside any namespace, and
//! `log.excludeDecoration` — read only by `git log`'s
//! `set_default_decoration_filter()` — excludes nothing. zvcs built `git log`'s
//! default filter for `--simplify-by-decoration`, dropping commits named only by
//! such refs, and let `log.excludeDecoration` strip `--bisect-all` names.
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
    /// Six commits `c1`..`c6` one second apart on `main`, with `refs/bisect/bad`
    /// on `c5`, `refs/notes/pin` on `c3` and `refs/top` on `c2`.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir()
            .join(format!("zvcs-rev-list-unfiltered-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.run(&["init", "-q", "-b", "main", "."], None);
        for i in 1..=6 {
            std::fs::write(f.work.join("f"), format!("{i}\n")).unwrap();
            f.run(&["add", "f"], None);
            f.run(&["commit", "-q", "-m", &format!("c{i}")], Some(&format!("170000000{i} +0000")));
        }
        f.run(&["update-ref", "refs/bisect/bad", "HEAD~1"], None);
        f.run(&["update-ref", "refs/notes/pin", "HEAD~3"], None);
        f.run(&["update-ref", "refs/top", "HEAD~4"], None);
        f
    }

    fn run(&self, args: &[&str], date: Option<&str>) -> (String, String, i32) {
        let date = date.unwrap_or("1700000000 +0000");
        let out = Command::new(BIN)
            .args(args)
            .current_dir(&self.work)
            .env("HOME", &self.root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_NAME", "A U Thor")
            .env("GIT_AUTHOR_EMAIL", "author@example.com")
            .env("GIT_COMMITTER_NAME", "C O Mitter")
            .env("GIT_COMMITTER_EMAIL", "committer@example.com")
            .env("GIT_AUTHOR_DATE", date)
            .env("GIT_COMMITTER_DATE", date)
            .env("LC_ALL", "C")
            .env("TZ", "UTC")
            .output()
            .unwrap();
        (
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
            out.status.code().expect("no signal"),
        )
    }
}

const C6: &str = "2c3104436399d232898bd00b7ca00f8e3fa9b924";
const C5: &str = "63313977cd9de0e4473ce67b8ffe35ae0aa0a52a";
const C4: &str = "b460f88002d70dfce9c2b4d13bcee29cae4949d8";
const C3: &str = "54a100d0a70c8fd7dfbcce02bdf0e19c3a7eb185";
const C2: &str = "e61b077e92de367b7536173518a9033c9702398b";
const C1: &str = "394f1eed138ac2b24cda882a3d016ad84b578dc4";

#[test]
fn simplify_by_decoration_keeps_bisect_notes_and_unnamespaced_refs() {
    let f = Fixture::new("simplify");
    assert_eq!(f.run(&["rev-parse", "HEAD"], None).0, format!("{C6}\n"));
    let want = format!("{C6}\n{C5}\n{C3}\n{C2}\n{C1}\n");
    assert_eq!(f.run(&["rev-list", "--simplify-by-decoration", "HEAD"], None), (want.clone(), String::new(), 0));
    // `log.excludeDecoration` is `git log`'s alone.
    assert_eq!(
        f.run(&["-c", "log.excludeDecoration=refs/bisect/", "rev-list", "--simplify-by-decoration", "HEAD"], None),
        (want, String::new(), 0)
    );
    // `git log` keeps its default namespaces: only the tip and the root remain.
    assert_eq!(
        f.run(&["log", "--simplify-by-decoration", "--format=%s", "HEAD"], None),
        ("c6\nc1\n".to_string(), String::new(), 0)
    );
}

#[test]
fn bisect_all_names_ignore_log_exclude_decoration() {
    let f = Fixture::new("bisect");
    assert_eq!(
        f.run(&["-c", "log.excludeDecoration=refs/top", "rev-list", "--bisect-all", "HEAD~2", "--not", "HEAD~5"], None),
        (
            format!("{C3} (refs/notes/pin, dist=1)\n{C2} (refs/top, dist=1)\n{C4} (dist=0)\n"),
            String::new(),
            0
        )
    );
}
