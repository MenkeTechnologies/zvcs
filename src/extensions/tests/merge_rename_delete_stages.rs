//! A rename/delete conflict carries its merge base into the index.
//!
//! `process_renames()` copies the source path's stages onto the destination
//! (merge-ort.c:3192-3195), so the new name reaches `process_entry()` with
//! `filemask == 5` — ancestor at stage 1, the renaming side at stage 3. Without that
//! stage-1 entry the destination looked like a plain addition: `git status` called it
//! `UA` instead of `DU`, and `git merge-tree` printed one conflict line where git prints
//! two, because the follow-up `modify/delete` notice is decided by comparing stage 1
//! against the surviving side (merge-ort.c:4396-4410) and stays silent when a rename
//! carried its content over untouched.
//!
//! Expectations are stock git 2.55.0's on the same fixtures.

use std::path::PathBuf;
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

const BASE: &str = "l1\nl2\nl3\nl4\nl5\nl6\nl7\nl8\n";
const MODIFIED: &str = "l1\nl2\nl3\nl4\nl5\nl6\nl7\nCHANGED\n";

struct Fixture {
    repo: PathBuf,
    home: PathBuf,
}

impl Fixture {
    fn run(&self, args: &[&str]) -> Output {
        Command::new(BIN)
            .args(args)
            .current_dir(&self.repo)
            .env("HOME", &self.home)
            .env("ZVCS_HOME", &self.home)
            .env("GIT_CONFIG_GLOBAL", self.home.join("gitconfig"))
            .env("GIT_CONFIG_SYSTEM", self.home.join("gitsystem"))
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .stdin(std::process::Stdio::null())
            .output()
            .expect("run binary")
    }

    fn ok(&self, args: &[&str]) -> String {
        let out = self.run(args);
        assert!(
            out.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).into_owned()
    }
}

/// `main` deletes `f.txt`; `side` renames it to `g.txt`, modifying the content only when
/// `modify` is set. Returns the fixture and the base blob id of `f.txt`, which is what
/// stage 1 of `g.txt` must hold.
fn fixture(tag: &str, modify: bool) -> (Fixture, String) {
    let root = std::env::temp_dir().join(format!("zvcs-rename-delete-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let home = root.join("home");
    let repo = root.join("repo");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&repo).unwrap();
    let fx = Fixture { repo, home };
    fx.ok(&["init", "-q", "-b", "main", "."]);
    fx.ok(&["config", "user.email", "t@e.co"]);
    fx.ok(&["config", "user.name", "t"]);

    std::fs::write(fx.repo.join("f.txt"), BASE).unwrap();
    fx.ok(&["add", "f.txt"]);
    fx.ok(&["commit", "-q", "-m", "base"]);
    let base_blob = fx.ok(&["rev-parse", "HEAD:f.txt"]).trim().to_string();

    fx.ok(&["checkout", "-q", "-b", "side"]);
    fx.ok(&["mv", "f.txt", "g.txt"]);
    if modify {
        std::fs::write(fx.repo.join("g.txt"), MODIFIED).unwrap();
        fx.ok(&["add", "g.txt"]);
    }
    fx.ok(&["commit", "-q", "-m", "rename"]);

    fx.ok(&["checkout", "-q", "main"]);
    fx.ok(&["rm", "-q", "f.txt"]);
    fx.ok(&["commit", "-q", "-m", "delete"]);
    (fx, base_blob)
}

/// `<mode> <oid> <stage>\t<path>` lines of `git ls-files -u`, as `(stage, oid, path)`.
fn unmerged(listing: &str) -> Vec<(String, String, String)> {
    listing
        .lines()
        .map(|line| {
            let (meta, path) = line.split_once('\t').expect("meta TAB path");
            let mut fields = meta.split_whitespace();
            let _mode = fields.next().expect("mode");
            let oid = fields.next().expect("oid").to_string();
            let stage = fields.next().expect("stage").to_string();
            (stage, oid, path.to_string())
        })
        .collect()
}

#[test]
fn merge_records_the_base_at_stage_one_of_the_new_name() {
    let (fx, base_blob) = fixture("merge", true);
    let out = fx.run(&["merge", "side"]);
    assert!(!out.status.success(), "a rename/delete must not merge cleanly");

    let stages = unmerged(&fx.ok(&["ls-files", "-u"]));
    assert_eq!(
        stages,
        vec![
            ("1".into(), base_blob, "g.txt".into()),
            ("3".into(), fx.ok(&["rev-parse", "side:g.txt"]).trim().to_string(), "g.txt".into()),
        ]
    );
    // With stage 1 present the path is a delete/unmerged, not an add/unmerged.
    assert_eq!(fx.ok(&["status", "--porcelain"]), "DU g.txt\n");
}

#[test]
fn merge_tree_reports_rename_delete_and_the_modify_delete_that_follows() {
    let (fx, base_blob) = fixture("mt-mod", true);
    let out = fx.run(&["merge-tree", "--write-tree", "main", "side"]);
    assert!(!out.status.success(), "a rename/delete must not merge cleanly");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let (head, messages) = stdout.split_once("\n\n").expect("blank line between stages and messages");

    let mut lines = head.lines();
    let tree = lines.next().expect("merged tree id");
    assert_eq!(tree.len(), 40, "expected a full tree id, got {tree:?}");
    assert_eq!(
        unmerged(&lines.collect::<Vec<_>>().join("\n")),
        vec![
            ("1".into(), base_blob, "g.txt".into()),
            ("3".into(), fx.ok(&["rev-parse", "side:g.txt"]).trim().to_string(), "g.txt".into()),
        ]
    );

    assert_eq!(
        messages,
        "CONFLICT (rename/delete): f.txt renamed to g.txt in side, but deleted in main.\n\
         CONFLICT (modify/delete): g.txt deleted in main and modified in side.  \
         Version side of g.txt left in tree.\n"
    );
}

#[test]
fn an_unmodified_rename_gets_no_modify_delete_notice() {
    let (fx, base_blob) = fixture("mt-pure", false);
    let out = fx.run(&["merge-tree", "--write-tree", "main", "side"]);
    assert!(!out.status.success(), "a rename/delete must not merge cleanly");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let (head, messages) = stdout.split_once("\n\n").expect("blank line between stages and messages");

    // Both stages hold the same blob, which is what makes git stay silent about the
    // second conflict — but stage 1 still has to be recorded.
    assert_eq!(
        unmerged(&head.lines().skip(1).collect::<Vec<_>>().join("\n")),
        vec![
            ("1".into(), base_blob.clone(), "g.txt".into()),
            ("3".into(), base_blob, "g.txt".into()),
        ]
    );
    assert_eq!(
        messages,
        "CONFLICT (rename/delete): f.txt renamed to g.txt in side, but deleted in main.\n"
    );
}
