//! `git bundle create` runs `pack-objects --stdout --thin --delta-base-offset` as a child
//! (`write_pack_data()`, bundle.c), and that child reads its configuration before it looks at a
//! revision: `prepare_repo_settings()`, then `git_pack_config()`, which ends in
//! `git_default_config()`. A value one of them refuses is the child's `fatal:` (with any
//! `error:` lines in front of it) and the parent adds `error: pack-objects died` and ends at 1 —
//! after the header it streams as it goes has reached a `-` destination, and with a file's lock
//! rolled back so no bundle appears.
//!
//! zvcs packs in-process and never read those values, so it wrote the bundle and exited 0.

#[path = "support/stock_git.rs"]
mod stock_git;
#[path = "support/twin.rs"]
mod twin;
use twin::Twin;

const REFUSED: &[&str] = &[
    "color.advice=input",
    "push.default=no",
    "core.abbrev=bogus",
    "pack.packSizeLimit=bogus",
    "pack.window=bogus",
    "pack.depth=bogus",
    "pack.threads=bogus",
    "pack.allowPackReuse=bogus",
    "advice.statusHints=bogus",
];

#[test]
fn a_refused_value_ends_bundle_create_with_the_childs_death() {
    let Some(t) = Twin::new("bundle-child-config") else { return };
    for key in REFUSED {
        t.same(&["-c", key, "bundle", "create", "out.bundle", "--all"]);
        let (stock, zvcs) = t.run_in("work", &["ls-files", "--others"]);
        assert_eq!(zvcs, stock, "{key}: left is zvcs, right is stock");
    }
}

#[test]
fn the_header_reaches_standard_output_first() {
    let Some(t) = Twin::new("bundle-child-config-stdout") else { return };
    t.same(&["-c", "color.advice=input", "bundle", "create", "-", "--all"]);
}

#[test]
fn the_settings_block_is_the_childs_first_read() {
    let Some(t) = Twin::new("bundle-child-settings") else { return };
    t.same(&["-c", "feature.manyFiles=bogus", "-c", "color.advice=input", "bundle", "create", "out.bundle", "--all"]);
}

#[test]
fn arguments_the_parent_refuses_never_start_the_child() {
    let Some(t) = Twin::new("bundle-child-parent-first") else { return };
    t.same(&["-c", "color.advice=input", "bundle", "create", "out.bundle", "nosuch"]);
    t.same(&["-c", "color.advice=input", "bundle", "create", "out.bundle", "main..main"]);
}

#[test]
fn a_clean_configuration_still_bundles() {
    let Some(t) = Twin::new("bundle-child-clean") else { return };
    t.same(&["bundle", "create", "out.bundle", "--all"]);
    t.same(&["bundle", "verify", "out.bundle"]);
    t.same(&["-c", "pack.window=3", "-c", "pack.threads=1", "bundle", "create", "again.bundle", "--all"]);
}
