//! `rev-list -g` under `--bisect`, `--bisect-all` and `--bisect-vars`.
//!
//! Under `-g` a named commit is handed to `add_reflog_for_walk()` and never
//! pended ("do not add the commit itself", revision.c:305-316), so
//! `find_bisection()` (builtin/rev-list.c:940-955) searches an empty
//! `revs->commits` while `get_revision_1()` streams the reflog regardless
//! (revision.c:4364-4387). The listing is therefore the plain reflog walk,
//! `--bisect-all` decorates it with refs but no `dist=` (bisect.c:249-252 only
//! decorates the weighed commits), and `show_bisect_vars()` returns 1 on the
//! empty list before printing anything (builtin/rev-list.c:441-442). zvcs ran
//! the search over the reflog entries: `--bisect` printed one of them,
//! `--bisect-all` reordered them with distances, `--bisect-vars` printed
//! variables and exited 0.
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
    fn empty(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-rl-g-bisect-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.run(&["init", "-q", "-b", "main", "."]);
        f
    }

    fn commit(&self, file: &str, body: &str, msg: &str) {
        std::fs::write(self.work.join(file), body).unwrap();
        self.run(&["add", file]);
        self.run(&["commit", "-q", "-m", msg]);
    }

    fn rev(&self, spec: &str) -> String {
        self.run(&["rev-parse", spec]).0.trim_end().to_string()
    }

    fn run(&self, args: &[&str]) -> (String, String, i32) {
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
            .env("GIT_AUTHOR_DATE", "1700000000 +0000")
            .env("GIT_COMMITTER_DATE", "1700000000 +0000")
            .env("GIT_PAGER", "cat")
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

/// Reflog of `main`: one, two, three, reset back to two, four.
fn reflog_fixture(tag: &str) -> Fixture {
    let f = Fixture::empty(tag);
    f.commit("a", "1\n", "one");
    f.commit("a", "2\n", "two");
    f.commit("a", "3\n", "three");
    f.run(&["reset", "-q", "--hard", "HEAD~1"]);
    f.commit("b", "4\n", "four");
    f
}

#[test]
fn bisect_lists_the_whole_reflog() {
    let f = reflog_fixture("plain");
    let walk = f.run(&["rev-list", "-g", "main"]);
    assert_eq!(walk.0.lines().count(), 5);
    assert_eq!(f.run(&["rev-list", "-g", "--bisect", "main"]), walk);
}

#[test]
fn bisect_all_decorates_refs_without_distances() {
    let f = reflog_fixture("all");
    let (four, two, three, one) = (f.rev("main"), f.rev("main~1"), f.rev("HEAD@{2}"), f.rev("main~2"));
    let want = format!("{four} (HEAD -> main)\n{two}\n{three}\n{two}\n{one}\n");
    assert_eq!(f.run(&["rev-list", "-g", "--bisect-all", "main"]), (want, String::new(), 0));
}

#[test]
fn bisect_vars_finds_nothing_to_search() {
    let f = reflog_fixture("vars");
    for extra in [&[][..], &["--bisect-all"][..]] {
        let mut args = vec!["rev-list", "-g", "--bisect-vars"];
        args.extend_from_slice(extra);
        args.push("main");
        assert_eq!(f.run(&args), (String::new(), String::new(), 1), "{args:?}");
    }
}
