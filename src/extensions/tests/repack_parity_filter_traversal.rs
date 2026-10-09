//! `repack --filter=<spec>` hands the spec to `pack-objects --all --reflog --indexed-objects`, and
//! what lands in the new pack is decided by that traversal:
//!
//! * `blob:limit=<n>` keeps a blob only when it is *smaller* than `n`;
//! * `tree:<depth>` omits what lies at `depth` or deeper, but an object the user named - the
//!   index's blobs and cache-tree nodes - is never put to the filter, so `tree:0` still packs
//!   every index blob and the cache-tree nodes that no earlier named tree listed as a child
//!   (the nodes at even depth, when each is the child of the one before).
#![cfg(unix)]

#[path = "support/stock_git.rs"]
mod stock_git;
#[path = "support/twin_repo.rs"]
mod twin_repo;

use twin_repo::Side;

/// Every object id in every pack of `side`, one sorted list per pack, the lists sorted.
fn packs(side: &Side) -> Vec<Vec<String>> {
    let dir = side.repo().join(".git/objects/pack");
    let mut all = Vec::new();
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_none_or(|e| e != "idx") {
            continue;
        }
        let idx = std::fs::read(&path).unwrap();
        assert_eq!(&idx[..8], b"\xfftOc\0\0\0\x02", "idx v2");
        let fanout = &idx[8..8 + 256 * 4];
        let n = u32::from_be_bytes(fanout[255 * 4..].try_into().unwrap()) as usize;
        let names = &idx[8 + 256 * 4..8 + 256 * 4 + n * 20];
        let mut ids: Vec<String> =
            names.chunks(20).map(|id| id.iter().map(|b| format!("{b:02x}")).collect()).collect();
        ids.sort();
        all.push(ids);
    }
    all.sort();
    all
}

/// Nested directories, history that adds and removes some, and a dirty index on top.
fn build(side: &Side) {
    side.write("d1/d2/d3/f.txt", "1\n");
    side.write("d1/d2/g.txt", "22\n");
    side.write("d1/e/h", "333\n");
    side.write("d1/top", "4444\n");
    side.write("a2/b2/c2/x", "x\n");
    for step in [
        &["add", "d1", "a2"][..],
        &["commit", "-q", "-m", "nested"],
    ] {
        assert_eq!(side.git(step).code, 0, "{step:?}");
    }
    side.write("d1/d2/d3/i.txt", "5\n");
    side.write("a", "1\n2\n3\nmore\n");
    side.git(&["add", "-A", "d1", "a"]);
    side.git(&["commit", "-q", "-m", "grow"]);
    side.git(&["rm", "-rq", "d1/e"]);
    side.git(&["commit", "-q", "-m", "shrink"]);
    // Index-only content: a new nested tree and a file nothing committed.
    side.write("n1/n2/n3/n4/deep", "deep\n");
    side.write("n1/shallow", "s\n");
    side.git(&["add", "n1"]);
    side.git(&["write-tree"]);
    side.write("a", "dirty\n");
}

#[test]
fn the_filter_decides_what_the_new_pack_holds() {
    let Some(stock) = stock_git::stock_git() else { return };
    for spec in [
        "tree:0",
        "tree:1",
        "tree:2",
        "tree:3",
        "tree:5",
        "blob:none",
        "blob:limit=0",
        "blob:limit=2",
        "blob:limit=3",
        "blob:limit=5",
        "blob:limit=1k",
    ] {
        let (s, z) = twin_repo::pair(&format!("repack-filter-{}", spec.replace([':', '='], "-")), stock);
        build(&s);
        build(&z);
        let flag = format!("--filter={spec}");
        let want = s.git(&["repack", &flag]);
        assert_eq!(want.code, 0, "{spec}: {want:?}");
        assert_eq!(z.git(&["repack", &flag]), want, "{spec}");
        assert_eq!(packs(&z), packs(&s), "{spec}: pack contents");
    }
}

