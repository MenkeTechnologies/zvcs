//! `stash export` and `stash import` read parents the way git parses commits.
//!
//! `lookup_commit_reference()` parses through `parse_object()`, which hands the
//! object to `parse_commit_buffer()` (commit.c:516-596): a graft-table entry —
//! `info/grafts` or `shallow` — replaces the parents the commit records. The
//! parents' own parses are `repo_parse_commit()` (commit.c:603-652), which asks
//! the commit-graph first, so `check_stash_topology()` sees the graph's parents
//! for them whenever `commit_graph_compatible()` allows the graph. zvcs parsed
//! every commit from its buffer alone, so a graft that broke a stash's shape was
//! exported anyway and the graft file's deprecation advice was never printed.
//!
//! Each case runs stock git and zvcs on identical fixtures and compares stdout,
//! stderr, the exit status and the refs afterwards.

use std::path::{Path, PathBuf};
use std::process::Command;

const ZVCS: &str = env!("CARGO_BIN_EXE_git");
const STOCK: &str = "/opt/homebrew/bin/git";

fn stock_available() -> bool {
    Path::new(STOCK).exists()
}

fn run(bin: &str, dir: &Path, home: &Path, args: &[&str]) -> (String, String, i32) {
    let out = Command::new(bin)
        .args(args)
        .current_dir(dir)
        .env("HOME", home)
        .env("ZVCS_HOME", home)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "A U Thor")
        .env("GIT_AUTHOR_EMAIL", "author@example.com")
        .env("GIT_COMMITTER_NAME", "C O Mitter")
        .env("GIT_COMMITTER_EMAIL", "committer@example.com")
        .env("GIT_AUTHOR_DATE", "1700000000 +0000")
        .env("GIT_COMMITTER_DATE", "1700000000 +0000")
        .env("LC_ALL", "C")
        .env("TZ", "UTC")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .output()
        .expect("run git");
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
        out.status.code().expect("no signal"),
    )
}

/// The commits of the fixture the cases rewrite.
struct Ids {
    /// `stash@{0}`, its first parent (`second`) and its index commit.
    w: String,
    b: String,
    i: String,
    /// `base`, the parent of `second`.
    base: String,
    /// `stash@{1}`'s index commit.
    i1: String,
    /// The tip of the exported chain and its first parent.
    ex: String,
    ex1: String,
}

/// How the fixture is changed before the command runs.
#[derive(Clone, Copy)]
enum Damage<'a> {
    /// `info/grafts` holds these lines.
    Grafts(&'a dyn Fn(&Ids) -> String),
    /// `shallow` names this commit.
    Shallow(&'a dyn Fn(&Ids) -> String),
    /// A commit-graph is written and then the first parent the graph records
    /// for `stash@{0}`'s index commit is pointed at `base`.
    GraphParent,
}

struct Fixture {
    root: PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

impl Fixture {
    /// Two identical repositories, `stock/` and `zvcs/`: `base`, `second`, two
    /// stash entries over `second`, and `refs/exported` holding their export.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-stash-export-grafts-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let f = Fixture { root };
        for side in ["stock", "zvcs"] {
            let work = f.root.join(side);
            std::fs::create_dir_all(&work).unwrap();
            f.git(&work, &["init", "-q", "-b", "main", "."]);
            std::fs::write(work.join("f"), "a\n").unwrap();
            f.git(&work, &["add", "f"]);
            f.git(&work, &["commit", "-q", "-m", "base"]);
            std::fs::write(work.join("f"), "b\n").unwrap();
            f.git(&work, &["commit", "-q", "-a", "-m", "second"]);
            for n in 1..=2 {
                std::fs::write(work.join("f"), format!("{n}\n")).unwrap();
                f.git(&work, &["stash", "-q"]);
            }
            f.git(&work, &["stash", "export", "--to-ref", "refs/exported"]);
        }
        f
    }

    fn git(&self, dir: &Path, args: &[&str]) -> String {
        let (out, err, code) = run(STOCK, dir, &self.root, args);
        assert_eq!(code, 0, "fixture step {args:?} failed: {err}");
        out
    }

    fn ids(&self) -> Ids {
        let dir = self.root.join("stock");
        let rev = |spec: &str| self.git(&dir, &["rev-parse", spec]).trim().to_string();
        Ids {
            w: rev("stash@{0}"),
            b: rev("stash@{0}^1"),
            i: rev("stash@{0}^2"),
            base: rev("main^"),
            i1: rev("stash@{1}^2"),
            ex: rev("refs/exported"),
            ex1: rev("refs/exported^1"),
        }
    }

    fn damage(&self, damage: Damage<'_>, ids: &Ids) {
        for side in ["stock", "zvcs"] {
            let git_dir = self.root.join(side).join(".git");
            match damage {
                Damage::Grafts(lines) => std::fs::write(git_dir.join("info").join("grafts"), lines(ids)).unwrap(),
                Damage::Shallow(id) => std::fs::write(git_dir.join("shallow"), format!("{}\n", id(ids))).unwrap(),
                Damage::GraphParent => {
                    self.git(&self.root.join(side), &["commit-graph", "write", "--reachable"]);
                    rewrite_graph_parent(&git_dir.join("objects/info/commit-graph"), &ids.i, &ids.base);
                }
            }
        }
    }

    /// Every ref and the stash reflog, read by stock git.
    fn post_state(&self, side: &str) -> String {
        let dir = self.root.join(side);
        let (refs, _, _) = run(STOCK, &dir, &self.root, &["for-each-ref"]);
        let (log, _, _) = run(STOCK, &dir, &self.root, &["reflog", "show", "--format=%H %gs", "refs/stash"]);
        format!("{refs}--\n{log}")
    }
}

/// Point the first parent the commit-graph records for `commit` at `parent`: the
/// `CDAT` row of a commit is its tree id, then the first and second parent
/// positions as big-endian `u32`s (gitformat-commit-graph).
fn rewrite_graph_parent(graph: &Path, commit: &str, parent: &str) {
    let mut data = std::fs::read(graph).unwrap();
    let be32 = |d: &[u8], at: usize| u32::from_be_bytes(d[at..at + 4].try_into().unwrap()) as usize;
    let chunks = data[6] as usize;
    let mut offsets = std::collections::HashMap::new();
    for c in 0..=chunks {
        let at = 8 + 12 * c;
        let id = data[at..at + 4].to_vec();
        let offset = u64::from_be_bytes(data[at + 4..at + 12].try_into().unwrap()) as usize;
        offsets.insert(id, offset);
    }
    let (oidf, oidl, cdat) = (offsets[&b"OIDF".to_vec()], offsets[&b"OIDL".to_vec()], offsets[&b"CDAT".to_vec()]);
    let count = be32(&data, oidf + 255 * 4);
    let hex = |pos: usize| data[oidl + 20 * pos..oidl + 20 * pos + 20].iter().map(|b| format!("{b:02x}")).collect::<String>();
    let position = |id: &str| (0..count).find(|&p| hex(p) == id).expect("commit in graph");
    let (c, p) = (position(commit), position(parent));
    data[cdat + 36 * c + 20..cdat + 36 * c + 24].copy_from_slice(&(p as u32).to_be_bytes());
    let mut perms = std::fs::metadata(graph).unwrap().permissions();
    #[allow(clippy::permissions_set_readonly_false)]
    perms.set_readonly(false);
    std::fs::set_permissions(graph, perms).unwrap();
    std::fs::write(graph, data).unwrap();
}

fn check(tag: &str, damage: Damage<'_>) {
    // The ids are read before the damage, which can make them unresolvable.
    let ids = Fixture::new(tag).ids();
    let invocations: Vec<Vec<String>> = vec![
        vec!["stash".into(), "export".into(), "--print".into()],
        vec!["stash".into(), "export".into(), "--print".into(), "stash@{0}".into()],
        vec!["stash".into(), "export".into(), "--print".into(), "stash@{1}".into()],
        vec!["stash".into(), "export".into(), "--print".into(), ids.b.clone()],
        vec!["stash".into(), "export".into(), "--to-ref".into(), "refs/again".into()],
        vec!["stash".into(), "import".into(), "refs/exported".into()],
        vec!["stash".into(), "import".into(), ids.ex1.clone()],
    ];
    for args in invocations {
        let fresh = Fixture::new(&format!("{tag}-run"));
        fresh.damage(damage, &ids);
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        let stock = run(STOCK, &fresh.root.join("stock"), &fresh.root, &args);
        let zvcs = run(ZVCS, &fresh.root.join("zvcs"), &fresh.root, &args);
        assert_eq!(zvcs, stock, "{tag}: git {args:?}: (stdout, stderr, status) differ");
        assert_eq!(fresh.post_state("zvcs"), fresh.post_state("stock"), "{tag}: git {args:?}: post-state differs");
    }
}

#[test]
fn a_graft_on_the_stash_commit_replaces_its_parents() {
    if !stock_available() {
        return;
    }
    check("one-parent", Damage::Grafts(&|ids| format!("{} {}\n", ids.w, ids.b)));
    check("extra-parent", Damage::Grafts(&|ids| format!("{} {} {} {}\n", ids.w, ids.b, ids.i, ids.base)));
    check("no-parent", Damage::Grafts(&|ids| format!("{}\n", ids.w)));
}

#[test]
fn a_graft_on_the_index_commit_breaks_the_topology_check() {
    if !stock_available() {
        return;
    }
    check("index-base", Damage::Grafts(&|ids| format!("{} {}\n", ids.i, ids.base)));
    check("index1-second", Damage::Grafts(&|ids| format!("{} {}\n", ids.i1, ids.b)));
}

#[test]
fn a_graft_can_make_an_ordinary_commit_stash_shaped() {
    if !stock_available() {
        return;
    }
    check("second-shaped", Damage::Grafts(&|ids| format!("{} {} {}\n", ids.b, ids.base, ids.i1)));
}

#[test]
fn import_walks_the_exported_chain_through_its_grafts() {
    if !stock_available() {
        return;
    }
    check("chain-one-parent", Damage::Grafts(&|ids| format!("{} {}\n", ids.ex, ids.ex1)));
    check("chain-root", Damage::Grafts(&|ids| format!("{}\n", ids.ex)));
}

#[test]
fn a_shallow_entry_drops_the_recorded_parents() {
    if !stock_available() {
        return;
    }
    check("shallow-index", Damage::Shallow(&|ids| ids.i.clone()));
    check("shallow-stash", Damage::Shallow(&|ids| ids.w.clone()));
}

#[test]
fn the_parents_of_a_stash_come_from_the_commit_graph() {
    if !stock_available() {
        return;
    }
    check("graph", Damage::GraphParent);
}
