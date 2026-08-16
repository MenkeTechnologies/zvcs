//! `Parents::First` against the `make_repo_for_topo.sh` history, which has the two merges and the
//! side branch needed to tell "the walk follows one parent" apart from "the commit has one parent".
//!
//! ```text
//! c12 --- merge --- merge --- c9 --- c8 --- c7 --- c6 --- c5 --- c4 --- c3 --- c2 --- c1 --- c0
//!            \         \                                  /
//!             b1c2 -- c11 -- c10 ------------ b1c1 ------/
//! ```
use super::*;
use crate::util::{fixture, odb_at};
use gix_traverse::commit::{Info, simple::CommitTimeOrder};

fn topo_repo() -> crate::Result<gix_odb::Handle> {
    let dir = fixture("make_repo_for_topo.sh")?;
    odb_at(dir.join(".git").join("objects"))
}

/// Walk and keep the whole [`Info`], with and without the commit-graph, asserting the two agree.
fn infos(
    tips: impl IntoIterator<Item = ObjectId> + Clone,
    odb: &gix_odb::Handle,
    sorting: Sorting,
    parents: Parents,
) -> crate::Result<Vec<Info>> {
    let without_graph: Vec<_> = Simple::new(tips.clone(), odb)
        .sorting(sorting)?
        .parents(parents)
        .commit_graph(None)
        .collect::<Result<Vec<_>, _>>()?;
    let with_graph: Vec<_> = Simple::new(tips, odb)
        .sorting(sorting)?
        .parents(parents)
        .commit_graph(commit_graph(odb.store_ref()))
        .collect::<Result<Vec<_>, _>>()?;

    // Only the ids and parents are compared: `commit_time` is `None` for `BreadthFirst` by design.
    let ids_and_parents = |v: &[Info]| -> Vec<(ObjectId, Vec<ObjectId>)> {
        v.iter().map(|i| (i.id, i.parent_ids.to_vec())).collect()
    };
    assert_eq!(
        ids_and_parents(&without_graph),
        ids_and_parents(&with_graph),
        "results must be consistent with and without commit-graph"
    );
    Ok(with_graph)
}

/// `git rev-list --parents --first-parent <c12>` prints both parents of each merge it walks
/// through: `first_parent_only` only makes `add_parents_to_list()` stop queueing after the first
/// parent, `commit->parents` is left alone. Reporting a truncated list here made every merge look
/// like an ordinary commit downstream, which silently defeats `--min-parents`/`--max-parents`.
#[test]
fn merge_parents_are_reported_in_full() -> crate::Result {
    let odb = topo_repo()?;
    let c12 = hex_to_id("62ed296d9986f50477e9f7b7e81cd0258939a43d");
    let merge_c12 = hex_to_id("722bf6b8c3d9e3a11fa5100a02ed9b140e1d209c");
    let merge_c9 = hex_to_id("d09384f312b03e4a1413160739805ff25e8fe99d");

    // Baseline: `git rev-list --parents --first-parent 62ed296d`, truncated to the merges.
    let expected_merge_parents = [
        (
            merge_c12,
            vec![
                merge_c9,
                hex_to_id("3be0c4c793c634c8fd95054345d4935d10a0879a"), // b1c2, not walked
            ],
        ),
        (
            merge_c9,
            vec![
                hex_to_id("eeab3243aad67bc838fc4425f759453bf0b47785"), // c9
                hex_to_id("22fbc169eeca3c9678fc7028aa80fad5ef49019f"), // b1c1, not walked
            ],
        ),
    ];

    for sorting in all_sortings() {
        let infos = infos([c12], &odb, sorting, Parents::First)?;

        let merges: Vec<_> = infos
            .iter()
            .filter(|i| i.parent_ids.len() > 1)
            .map(|i| (i.id, i.parent_ids.to_vec()))
            .collect();
        assert_eq!(
            merges, expected_merge_parents,
            "both merges keep their second parent, sorting = {sorting:?}"
        );

        // The walk itself still follows only the first parent, so the side branch is absent.
        let walked: Vec<_> = infos.iter().map(|i| i.id).collect();
        assert_eq!(walked.len(), 13, "the first-parent chain has 13 commits, sorting = {sorting:?}");
        for (_, parents) in &expected_merge_parents {
            assert!(
                !walked.contains(&parents[1]),
                "the second parent is reported but never walked, sorting = {sorting:?}"
            );
        }
    }
    Ok(())
}

/// git's `--first-parent` picks which parent the walk follows; it never picks the queue. The
/// parent it does queue still goes through `commit_list_insert_by_date()`, so a commit-date sort
/// stays a commit-date sort. `Simple::parents()` used to flatten the date queue into the FIFO one,
/// which turned every date-sorted first-parent walk into [`Sorting::BreadthFirst`] — visible as
/// soon as a second tip makes the two orders disagree.
#[test]
fn commit_date_sorting_survives_first_parent() -> crate::Result {
    let odb = topo_repo()?;
    let c12 = hex_to_id("62ed296d9986f50477e9f7b7e81cd0258939a43d");
    let b1c1 = hex_to_id("22fbc169eeca3c9678fc7028aa80fad5ef49019f");

    // `git rev-list --first-parent 62ed296d 22fbc169`, which is commit-date order: b1c1 sits
    // between c8 and c9 in time, so it comes out in the middle of the chain rather than second.
    let expected_by_date = [
        "62ed296d9986f50477e9f7b7e81cd0258939a43d", // c12
        "722bf6b8c3d9e3a11fa5100a02ed9b140e1d209c", // merge
        "d09384f312b03e4a1413160739805ff25e8fe99d", // merge
        "eeab3243aad67bc838fc4425f759453bf0b47785", // c9
        "22fbc169eeca3c9678fc7028aa80fad5ef49019f", // b1c1
        "693c775700cf90bd158ee6e7f14dd1b7bd83a4ce", // c8
        "33eb18340e4eaae3e3dcf80222b02f161cd3f966", // c7
        "1a27cb1a26c9faed9f0d1975326fe51123ab01ed", // c6
        "f1cce1b5c7efcdfa106e95caa6c45a2cae48a481", // c5
        "945d8a360915631ad545e0cf04630d86d3d4eaa1", // c4
        "a863c02247a6c5ba32dff5224459f52aa7f77f7b", // c3
        "2f291881edfb0597493a52d26ea09dd7340ce507", // c2
        "9c46b8765703273feb10a2ebd810e70b8e2ca44a", // c1
        "fb3e21cf45b04b617011d2b30973f3e5ce60d0cd", // c0
    ]
    .map(hex_to_id);

    let by_date = traverse_both(
        [c12, b1c1],
        &odb,
        Sorting::ByCommitTime(CommitTimeOrder::NewestFirst),
        Parents::First,
        [],
    )?;
    assert_eq!(by_date, expected_by_date);

    // The mode this used to silently fall back to, kept here so the fallback can't come back
    // unnoticed: `BreadthFirst` alternates between the two tips instead.
    let breadth_first = traverse_both([c12, b1c1], &odb, Sorting::BreadthFirst, Parents::First, [])?;
    assert_eq!(
        breadth_first[1], b1c1,
        "breadth-first drains one tip per step, so the older tip is second"
    );
    assert_ne!(
        breadth_first, by_date,
        "the two sortings must stay distinguishable under `Parents::First`"
    );
    Ok(())
}
