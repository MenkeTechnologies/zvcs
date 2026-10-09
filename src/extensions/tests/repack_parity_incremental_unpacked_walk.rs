//! An incremental `repack` (no `-a`) runs `pack-objects --unpacked`, whose walk ignores a commit
//! some pack already holds: the commit's tree is never added to the pending list, so a tree left
//! loose by an earlier `--filter` run stays loose unless the index (or a commit not yet packed)
//! names it.
#![cfg(unix)]

#[path = "support/stock_git.rs"]
mod stock_git;
#[path = "support/twin_repo.rs"]
mod twin_repo;

use twin_repo::Side;

fn packs(side: &Side) -> Vec<Vec<String>> {
    let mut all = Vec::new();
    for entry in std::fs::read_dir(side.repo().join(".git/objects/pack")).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_none_or(|e| e != "idx") {
            continue;
        }
        let idx = std::fs::read(&path).unwrap();
        let n = u32::from_be_bytes(idx[8 + 255 * 4..8 + 256 * 4].try_into().unwrap()) as usize;
        let mut ids: Vec<String> = idx[8 + 256 * 4..8 + 256 * 4 + n * 20]
            .chunks(20)
            .map(|id| id.iter().map(|b| format!("{b:02x}")).collect())
            .collect();
        ids.sort();
        all.push(ids);
    }
    all.sort();
    all
}

#[test]
fn an_incremental_repack_does_not_reach_the_trees_of_packed_commits() {
    let Some(stock) = stock_git::stock_git() else { return };
    let (s, z) = twin_repo::pair("repack-incremental-unpacked", stock);
    for args in [
        &["repack", "--no-keep-unreachable", "--filter=tree:0"][..],
        &["repack", "--no-quiet"],
        &["repack"],
    ] {
        let want = s.git(args);
        assert_eq!(want.code, 0, "{args:?}: {want:?}");
        assert_eq!(z.git(args), want, "{args:?}");
        assert_eq!(packs(&z), packs(&s), "{args:?}");
    }
    // A commit that is not packed yet brings its tree along.
    for side in [&s, &z] {
        side.write("late", "late\n");
        side.git(&["add", "late"]);
        let committed = side.git(&["commit", "-q", "-m", "late"]);
        assert_eq!(committed.code, 0, "{committed:?}");
    }
    assert_eq!(z.git(&["repack"]), s.git(&["repack"]));
    assert_eq!(packs(&z), packs(&s));
}
