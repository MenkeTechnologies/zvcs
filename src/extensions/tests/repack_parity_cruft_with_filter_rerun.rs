//! With `--cruft` and `--filter` the packs are written in the order main, cruft, filtered, and
//! the filtered pack is `pack-objects --stdin-packs` over every older pack with the new ones
//! excluded. So an object the filter left out of the main pack - carried by the previous run's
//! cruft pack - is excluded again once this run's cruft pack holds it, and the filtered pack is
//! the empty one, run after run, instead of a second copy of the cruft pack.
#![cfg(unix)]

#[path = "support/stock_git.rs"]
mod stock_git;
#[path = "support/twin_repo.rs"]
mod twin_repo;

use twin_repo::Side;

/// Per pack: whether it is a cruft pack (has a `.mtimes`) and the object ids its index lists;
/// sorted, so two repositories compare equal when they hold the same packs whatever the entry
/// order inside a pack.
fn packs(side: &Side) -> Vec<(bool, Vec<String>)> {
    let dir = side.repo().join(".git/objects/pack");
    let mut all = Vec::new();
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_none_or(|e| e != "idx") {
            continue;
        }
        let idx = std::fs::read(&path).unwrap();
        assert_eq!(&idx[..8], b"\xfftOc\0\0\0\x02", "idx v2");
        let n = u32::from_be_bytes(idx[8 + 255 * 4..8 + 256 * 4].try_into().unwrap()) as usize;
        let names = &idx[8 + 256 * 4..8 + 256 * 4 + n * 20];
        let mut ids: Vec<String> =
            names.chunks(20).map(|id| id.iter().map(|b| format!("{b:02x}")).collect()).collect();
        ids.sort();
        all.push((path.with_extension("mtimes").exists(), ids));
    }
    all.sort();
    all
}

#[test]
fn rerunning_a_cruft_repack_with_a_filter_keeps_the_same_three_packs() {
    let Some(stock) = stock_git::stock_git() else { return };
    let (s, z) = twin_repo::pair("repack-cruft-filter-rerun", stock);
    for run in 1..=3 {
        let args = ["repack", "-a", "-d", "-l", "-q", "--cruft", "--filter=tree:0"];
        let want = s.git(&args);
        assert_eq!(want.code, 0, "run {run}: {want:?}");
        assert_eq!(z.git(&args), want, "run {run}");
        assert_eq!(packs(&z), packs(&s), "run {run}");
    }
    // `gc` runs the same repack with the filter from its configuration, after specs that leave
    // everything in the main pack.
    for spec in ["blob:none", "tree:1", "tree:0", "tree:0"] {
        let config = format!("gc.repackFilter={spec}");
        let args = ["-c", config.as_str(), "gc", "-q"];
        let want = s.git(&args);
        assert_eq!(want.code, 0, "{spec}: {want:?}");
        assert_eq!(z.git(&args), want, "{spec}");
        assert_eq!(packs(&z), packs(&s), "{spec}");
    }
    // Three packs in the end: the main one, the cruft one and the empty filtered one.
    let shape: Vec<(bool, usize)> = packs(&s).iter().map(|(cruft, ids)| (*cruft, ids.len())).collect();
    assert_eq!(shape.len(), 3, "{shape:?}");
    assert!(shape.contains(&(false, 0)) && shape.iter().any(|(cruft, _)| *cruft), "{shape:?}");
}
