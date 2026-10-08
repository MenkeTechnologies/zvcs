//! `git add -N` / `git stage -N` against stock git when the pathspec matches only paths that
//! are already tracked.
//!
//! Every path `add_files_to_cache()` hands to `add_to_index()` invalidates its cache-tree node
//! in `add_index_entry_with_check()` *before* the `ADD_CACHE_NEW_ONLY` return that leaves a
//! tracked entry as it was, so a `-N` run over tracked, modified paths changes no entry and
//! still rewrites the index with those directories marked invalid.
#![cfg(unix)]

#[path = "support/stock_git.rs"]
mod stock_git;
#[path = "support/twin_repo.rs"]
mod twin_repo;

fn tree_extension(side: &twin_repo::Side) -> Option<Vec<u8>> {
    twin_repo::index_extension(&side.read(".git/index")?, b"TREE")
}

#[test]
fn tracked_paths_matched_under_intent_to_add_invalidate_their_directories() {
    let Some(stock) = stock_git::stock_git() else { return };
    let (s, z) = twin_repo::pair("add-n-tracked", stock);
    for side in [&s, &z] {
        side.write("docs/guide.md", "guide\n");
        side.write("docs/keep.txt", "keep\n");
        side.write("top.md", "top\n");
        side.git(&["add", "."]);
        side.git(&["commit", "-q", "-m", "docs"]);
        side.git(&["write-tree"]);
        side.write("docs/guide.md", "guide, edited\n");
        side.write("top.md", "top, edited\n");
    }
    for args in [
        &["add", "-N", "--", "*.md"][..],
        &["add", "--intent-to-add", "."],
    ] {
        let (want, got) = (s.git(args), z.git(args));
        assert_eq!(got, want, "{args:?}");
        let stock_tree = tree_extension(&s).expect("stock keeps a cache tree");
        assert_eq!(tree_extension(&z), Some(stock_tree), "{args:?}");
        for side in [&s, &z] {
            side.git(&["reset", "-q", "--hard"]);
            side.git(&["write-tree"]);
            side.write("docs/guide.md", "guide, edited\n");
            side.write("top.md", "top, edited\n");
        }
    }
}
