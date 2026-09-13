//! The ported `struct option` arrays, one module per git source file that
//! declares them (`builtin/<name>.c`, or the library file for `apply.c`,
//! `diff.c` and `pack-refs.c`), and the registry that names which builtins
//! answer.

use super::Builtin;

mod add;
mod am;
mod apply;
mod archive;
mod backfill;
mod bisect;
mod blame;
mod branch;
mod bugreport;
mod bundle;
mod cat_file;
mod check_attr;
mod check_ignore;
mod check_mailmap;
mod checkout;
mod checkout__worker;
mod checkout_index;
mod clean;
mod clone;
mod column;
mod commit;
mod commit_graph;
mod commit_tree;
mod config;
mod count_objects;
mod credential_cache;
mod credential_cache__daemon;
mod credential_store;
mod describe;
mod diagnose;
mod diff;
mod difftool;
mod fast_export;
mod fetch;
mod fmt_merge_msg;
mod for_each_ref;
mod for_each_repo;
mod fsck;
mod fsmonitor__daemon;
mod gc;
mod grep;
mod hash_object;
mod help;
mod history;
mod hook;
mod init_db;
mod interpret_trailers;
mod last_modified;
mod log;
mod ls_files;
mod ls_remote;
mod ls_tree;
mod mailinfo;
mod merge;
mod merge_base;
mod merge_file;
mod merge_tree;
mod mktag;
mod mktree;
mod multi_pack_index;
mod mv;
mod name_rev;
mod notes;
mod pack_objects;
mod pack_refs;
mod prune;
mod prune_packed;
mod pull;
mod push;
mod range_diff;
mod read_tree;
mod rebase;
mod receive_pack;
mod reflog;
mod refs;
mod remote;
mod repack;
mod replace;
mod replay;
mod repo;
mod rerere;
mod reset;
mod revert;
mod rm;
mod send_pack;
mod shortlog;
mod show_branch;
mod show_index;
mod show_ref;
mod sparse_checkout;
mod stash;
mod stripspace;
mod submodule__helper;
mod symbolic_ref;
mod tag;
mod update_index;
mod update_ref;
mod update_server_info;
mod upload_pack;
mod url_parse;
mod verify_commit;
mod verify_pack;
mod verify_tag;
mod version;
mod worktree;
mod write_tree;

/// `commands[]` (git.c:529-685) order, restricted to the builtins whose table
/// is ported. `run_setup` is the entry's `RUN_SETUP` bit; the trailing comment
/// names the `parse_options()` call that sees the lone argument, which receives
/// the `parse_options_concat()` of `options` in order.
#[rustfmt::skip]
pub(super) const BUILTINS: &[Builtin] = &[
    Builtin { name: "add", run_setup: true, options: &[add::BUILTIN_ADD_OPTIONS] }, // builtin/add.c:400
    Builtin { name: "am", run_setup: true, options: &[am::AM_OPTIONS] }, // builtin/am.c:2458
    // `cmd_annotate()` runs `cmd_blame()` as `annotate -c <args>` (builtin/annotate.c:20-30),
    // so blame's own parse sees two arguments; the unknown one goes through
    // `parse_revision_opt()` to `diff_opt_parse()` (revision.c:2721, diff.c:6328-6335),
    // whose `parse_options()` over an empty array plus `add_diff_options()` sees it alone.
    Builtin { name: "annotate", run_setup: true, options: &[diff::PARSEOPTS] },
    Builtin { name: "apply", run_setup: false, options: &[apply::BUILTIN_APPLY_OPTIONS] }, // apply.c:5285
    Builtin { name: "archive", run_setup: false, options: &[archive::ARCHIVE_OPTIONS] }, // builtin/archive.c:97
    Builtin { name: "backfill", run_setup: true, options: &[backfill::BACKFILL_OPTIONS] }, // builtin/backfill.c:167
    Builtin { name: "bisect", run_setup: true, options: &[bisect::BISECT_OPTIONS] }, // builtin/bisect.c:1465
    Builtin { name: "blame", run_setup: true, options: &[blame::BLAME_OPTIONS] }, // builtin/blame.c:1008-1011
    Builtin { name: "branch", run_setup: true, options: &[branch::BRANCH_OPTIONS] }, // builtin/branch.c:810
    Builtin { name: "bugreport", run_setup: false, options: &[bugreport::BUGREPORT_OPTIONS] }, // builtin/bugreport.c:122
    Builtin { name: "bundle", run_setup: false, options: &[bundle::BUNDLE_OPTIONS] }, // builtin/bundle.c:249
    Builtin { name: "cat-file", run_setup: true, options: &[cat_file::CAT_FILE_OPTIONS] }, // builtin/cat-file.c:1154
    Builtin { name: "check-attr", run_setup: true, options: &[check_attr::CHECK_ATTR_OPTIONS] }, // builtin/check-attr.c:124
    Builtin { name: "check-ignore", run_setup: true, options: &[check_ignore::CHECK_IGNORE_OPTIONS] }, // builtin/check-ignore.c:165
    Builtin { name: "check-mailmap", run_setup: true, options: &[check_mailmap::CHECK_MAILMAP_OPTIONS] }, // builtin/check-mailmap.c:61
    Builtin { name: "checkout", run_setup: true, options: &[checkout::CHECKOUT_OPTIONS, checkout::ADD_COMMON_OPTIONS, checkout::ADD_COMMON_SWITCH_BRANCH_OPTIONS, checkout::ADD_CHECKOUT_PATH_OPTIONS] }, // builtin/checkout.c:1892
    Builtin { name: "checkout--worker", run_setup: true, options: &[checkout__worker::CHECKOUT_WORKER_OPTIONS] }, // builtin/checkout--worker.c:137
    Builtin { name: "checkout-index", run_setup: true, options: &[checkout_index::BUILTIN_CHECKOUT_INDEX_OPTIONS] }, // builtin/checkout-index.c:266
    Builtin { name: "cherry", run_setup: true, options: &[log::CHERRY_OPTIONS] }, // builtin/log.c:2755
    Builtin { name: "cherry-pick", run_setup: true, options: &[revert::BASE_OPTIONS, revert::CP_EXTRA_PICK] }, // builtin/revert.c:172
    Builtin { name: "clean", run_setup: true, options: &[clean::CLEAN_OPTIONS] }, // builtin/clean.c:953
    Builtin { name: "clone", run_setup: false, options: &[clone::BUILTIN_CLONE_OPTIONS] }, // builtin/clone.c:1014
    Builtin { name: "column", run_setup: false, options: &[column::COLUMN_OPTIONS] }, // builtin/column.c:51
    Builtin { name: "commit", run_setup: true, options: &[commit::BUILTIN_COMMIT_OPTIONS] }, // builtin/commit.c:1316
    Builtin { name: "commit-graph", run_setup: true, options: &[commit_graph::BUILTIN_COMMIT_GRAPH_OPTIONS, commit_graph::COMMON_OPTS] }, // builtin/commit-graph.c:352-359
    Builtin { name: "commit-tree", run_setup: true, options: &[commit_tree::COMMIT_TREE_OPTIONS] }, // builtin/commit-tree.c:134
    Builtin { name: "config", run_setup: false, options: &[config::SUBCOMMAND_OPTS] }, // builtin/config.c:1649
    Builtin { name: "count-objects", run_setup: true, options: &[count_objects::COUNT_OBJECTS_OPTIONS] }, // builtin/count-objects.c:112
    Builtin { name: "credential-cache", run_setup: false, options: &[credential_cache::CREDENTIAL_CACHE_OPTIONS] }, // builtin/credential-cache.c:161
    Builtin { name: "credential-cache--daemon", run_setup: false, options: &[credential_cache__daemon::CREDENTIAL_CACHE__DAEMON_OPTIONS] }, // builtin/credential-cache--daemon.c:312
    Builtin { name: "credential-store", run_setup: false, options: &[credential_store::CREDENTIAL_STORE_OPTIONS] }, // builtin/credential-store.c:190
    Builtin { name: "describe", run_setup: true, options: &[describe::DESCRIBE_OPTIONS] }, // builtin/describe.c:689
    Builtin { name: "diagnose", run_setup: false, options: &[diagnose::DIAGNOSE_OPTIONS] }, // builtin/diagnose.c:40
    Builtin { name: "difftool", run_setup: false, options: &[difftool::BUILTIN_DIFFTOOL_OPTIONS] }, // builtin/difftool.c:759
    Builtin { name: "fast-export", run_setup: true, options: &[fast_export::FAST_EXPORT_OPTIONS] }, // builtin/fast-export.c:1380
    Builtin { name: "fetch", run_setup: true, options: &[fetch::BUILTIN_FETCH_OPTIONS] }, // builtin/fetch.c:2613
    Builtin { name: "fmt-merge-msg", run_setup: true, options: &[fmt_merge_msg::FMT_MERGE_MSG_OPTIONS] }, // builtin/fmt-merge-msg.c:57
    Builtin { name: "for-each-ref", run_setup: true, options: &[for_each_ref::FOR_EACH_REF_OPTIONS] }, // builtin/for-each-ref.c:62
    Builtin { name: "for-each-repo", run_setup: false, options: &[for_each_repo::FOR_EACH_REPO_OPTIONS] }, // builtin/for-each-repo.c:52
    Builtin { name: "format-patch", run_setup: true, options: &[log::BUILTIN_FORMAT_PATCH_OPTIONS] }, // builtin/log.c:2133
    Builtin { name: "format-rev", run_setup: true, options: &[name_rev::FORMAT_REV_OPTIONS] }, // builtin/name-rev.c:845
    Builtin { name: "fsck", run_setup: true, options: &[fsck::FSCK_OPTS] }, // builtin/fsck.c:1027
    Builtin { name: "fsck-objects", run_setup: true, options: &[fsck::FSCK_OPTS] }, // builtin/fsck.c:1027
    Builtin { name: "fsmonitor--daemon", run_setup: true, options: &[fsmonitor__daemon::FSMONITOR__DAEMON_OPTIONS] }, // builtin/fsmonitor--daemon.c:1589
    Builtin { name: "gc", run_setup: true, options: &[gc::BUILTIN_GC_OPTIONS] }, // builtin/gc.c:907
    Builtin { name: "grep", run_setup: false, options: &[grep::GREP_OPTIONS] }, // builtin/grep.c:1194
    Builtin { name: "hash-object", run_setup: false, options: &[hash_object::HASH_OBJECT_OPTIONS] }, // builtin/hash-object.c:99
    Builtin { name: "help", run_setup: false, options: &[help::BUILTIN_HELP_OPTIONS] }, // builtin/help.c:672
    Builtin { name: "history", run_setup: true, options: &[history::HISTORY_OPTIONS] }, // builtin/history.c:997
    Builtin { name: "hook", run_setup: false, options: &[hook::BUILTIN_HOOK_OPTIONS] }, // builtin/hook.c:204
    Builtin { name: "init", run_setup: false, options: &[init_db::INIT_DB_OPTIONS] }, // builtin/init-db.c:117
    Builtin { name: "init-db", run_setup: false, options: &[init_db::INIT_DB_OPTIONS] }, // builtin/init-db.c:117
    Builtin { name: "interpret-trailers", run_setup: false, options: &[interpret_trailers::INTERPRET_TRAILERS_OPTIONS] }, // builtin/interpret-trailers.c:171
    Builtin { name: "last-modified", run_setup: true, options: &[last_modified::LAST_MODIFIED_OPTIONS] }, // builtin/last-modified.c:542
    Builtin { name: "log", run_setup: true, options: &[log::BUILTIN_LOG_OPTIONS] }, // builtin/log.c:309
    Builtin { name: "ls-files", run_setup: true, options: &[ls_files::BUILTIN_LS_FILES_OPTIONS] }, // builtin/ls-files.c:684
    Builtin { name: "ls-remote", run_setup: false, options: &[ls_remote::LS_REMOTE_OPTIONS] }, // builtin/ls-remote.c:98
    Builtin { name: "ls-tree", run_setup: true, options: &[ls_tree::LS_TREE_OPTIONS] }, // builtin/ls-tree.c:383
    Builtin { name: "mailinfo", run_setup: false, options: &[mailinfo::MAILINFO_OPTIONS] }, // builtin/mailinfo.c:89
    Builtin { name: "maintenance", run_setup: true, options: &[gc::BUILTIN_MAINTENANCE_OPTIONS] }, // builtin/gc.c:3533
    Builtin { name: "merge", run_setup: true, options: &[merge::BUILTIN_MERGE_OPTIONS] }, // builtin/merge.c:1409
    Builtin { name: "merge-base", run_setup: true, options: &[merge_base::MERGE_BASE_OPTIONS] }, // builtin/merge-base.c:174
    Builtin { name: "merge-file", run_setup: false, options: &[merge_file::MERGE_FILE_OPTIONS] }, // builtin/merge-file.c:103
    Builtin { name: "merge-tree", run_setup: true, options: &[merge_tree::MT_OPTIONS] }, // builtin/merge-tree.c:597
    Builtin { name: "mktag", run_setup: true, options: &[mktag::BUILTIN_MKTAG_OPTIONS] }, // builtin/mktag.c:90
    Builtin { name: "mktree", run_setup: true, options: &[mktree::MKTREE_OPTIONS] }, // builtin/mktree.c:174
    Builtin { name: "multi-pack-index", run_setup: true, options: &[multi_pack_index::BUILTIN_MULTI_PACK_INDEX_OPTIONS, multi_pack_index::COMMON_OPTS] }, // builtin/multi-pack-index.c:419-430
    Builtin { name: "mv", run_setup: true, options: &[mv::BUILTIN_MV_OPTIONS] }, // builtin/mv.c:245
    Builtin { name: "name-rev", run_setup: true, options: &[name_rev::NAME_REV_OPTIONS] }, // builtin/name-rev.c:678
    Builtin { name: "notes", run_setup: true, options: &[notes::NOTES_OPTIONS] }, // builtin/notes.c:1151
    Builtin { name: "pack-objects", run_setup: true, options: &[pack_objects::PACK_OBJECTS_OPTIONS] }, // builtin/pack-objects.c:5178
    Builtin { name: "pack-refs", run_setup: true, options: &[pack_refs::OPTS] }, // pack-refs.c:38
    Builtin { name: "pickaxe", run_setup: true, options: &[blame::BLAME_OPTIONS] }, // builtin/blame.c:1008-1011
    Builtin { name: "prune", run_setup: true, options: &[prune::PRUNE_OPTIONS] }, // builtin/prune.c:176
    Builtin { name: "prune-packed", run_setup: true, options: &[prune_packed::PRUNE_PACKED_OPTIONS] }, // builtin/prune-packed.c:25
    Builtin { name: "pull", run_setup: true, options: &[pull::PULL_OPTIONS] }, // builtin/pull.c:1024
    Builtin { name: "push", run_setup: true, options: &[push::PUSH_OPTIONS] }, // builtin/push.c:722
    Builtin { name: "range-diff", run_setup: true, options: &[range_diff::RANGE_DIFF_OPTIONS, diff::PARSEOPTS] }, // builtin/range-diff.c:83-84
    Builtin { name: "read-tree", run_setup: true, options: &[read_tree::READ_TREE_OPTIONS] }, // builtin/read-tree.c:174
    Builtin { name: "rebase", run_setup: true, options: &[rebase::BUILTIN_REBASE_OPTIONS] }, // builtin/rebase.c:1295
    Builtin { name: "receive-pack", run_setup: false, options: &[receive_pack::RECEIVE_PACK_OPTIONS] }, // builtin/receive-pack.c:2639
    Builtin { name: "reflog", run_setup: true, options: &[reflog::REFLOG_OPTIONS] }, // builtin/reflog.c:484
    Builtin { name: "refs", run_setup: true, options: &[refs::REFS_OPTIONS] }, // builtin/refs.c:202
    Builtin { name: "remote", run_setup: true, options: &[remote::REMOTE_OPTIONS] }, // builtin/remote.c:1955
    Builtin { name: "repack", run_setup: true, options: &[repack::BUILTIN_REPACK_OPTIONS] }, // builtin/repack.c:248
    Builtin { name: "replace", run_setup: true, options: &[replace::REPLACE_OPTIONS] }, // builtin/replace.c:578
    Builtin { name: "replay", run_setup: true, options: &[replay::REPLAY_OPTIONS] }, // builtin/replay.c:117
    Builtin { name: "repo", run_setup: true, options: &[repo::REPO_OPTIONS] }, // builtin/repo.c:943
    Builtin { name: "rerere", run_setup: true, options: &[rerere::RERERE_OPTIONS] }, // builtin/rerere.c:67
    Builtin { name: "reset", run_setup: true, options: &[reset::RESET_OPTIONS] }, // builtin/reset.c:387
    Builtin { name: "restore", run_setup: true, options: &[checkout::RESTORE_OPTIONS, checkout::ADD_COMMON_OPTIONS, checkout::ADD_CHECKOUT_PATH_OPTIONS] }, // builtin/checkout.c:1892
    Builtin { name: "revert", run_setup: true, options: &[revert::BASE_OPTIONS, revert::CP_EXTRA_REVERT] }, // builtin/revert.c:172
    Builtin { name: "rm", run_setup: true, options: &[rm::BUILTIN_RM_OPTIONS] }, // builtin/rm.c:277
    Builtin { name: "send-pack", run_setup: true, options: &[send_pack::SEND_PACK_OPTIONS] }, // builtin/send-pack.c:216
    Builtin { name: "shortlog", run_setup: false, options: &[shortlog::SHORTLOG_OPTIONS] }, // builtin/shortlog.c:427-431
    Builtin { name: "show", run_setup: true, options: &[log::BUILTIN_LOG_OPTIONS] }, // builtin/log.c:309
    Builtin { name: "show-branch", run_setup: true, options: &[show_branch::BUILTIN_SHOW_BRANCH_OPTIONS] }, // builtin/show-branch.c:727
    Builtin { name: "show-index", run_setup: false, options: &[show_index::SHOW_INDEX_OPTIONS] }, // builtin/show-index.c:34
    Builtin { name: "show-ref", run_setup: true, options: &[show_ref::SHOW_REF_OPTIONS] }, // builtin/show-ref.c:337
    Builtin { name: "sparse-checkout", run_setup: true, options: &[sparse_checkout::BUILTIN_SPARSE_CHECKOUT_OPTIONS] }, // builtin/sparse-checkout.c:1200
    Builtin { name: "stage", run_setup: true, options: &[add::BUILTIN_ADD_OPTIONS] }, // builtin/add.c:400
    Builtin { name: "stash", run_setup: true, options: &[stash::STASH_OPTIONS] }, // builtin/stash.c:2483
    Builtin { name: "status", run_setup: true, options: &[commit::BUILTIN_STATUS_OPTIONS] }, // builtin/commit.c:1609
    Builtin { name: "stripspace", run_setup: false, options: &[stripspace::STRIPSPACE_OPTIONS] }, // builtin/stripspace.c:52
    Builtin { name: "submodule--helper", run_setup: true, options: &[submodule__helper::SUBMODULE__HELPER_OPTIONS] }, // builtin/submodule--helper.c:3833
    Builtin { name: "switch", run_setup: true, options: &[checkout::SWITCH_OPTIONS, checkout::ADD_COMMON_OPTIONS, checkout::ADD_COMMON_SWITCH_BRANCH_OPTIONS] }, // builtin/checkout.c:1892
    Builtin { name: "symbolic-ref", run_setup: true, options: &[symbolic_ref::SYMBOLIC_REF_OPTIONS] }, // builtin/symbolic-ref.c:64
    Builtin { name: "tag", run_setup: true, options: &[tag::TAG_OPTIONS] }, // builtin/tag.c:557
    Builtin { name: "update-index", run_setup: true, options: &[update_index::UPDATE_INDEX_OPTIONS] }, // builtin/update-index.c:1120-1130
    Builtin { name: "update-ref", run_setup: true, options: &[update_ref::UPDATE_REF_OPTIONS] }, // builtin/update-ref.c:835
    Builtin { name: "update-server-info", run_setup: true, options: &[update_server_info::UPDATE_SERVER_INFO_OPTIONS] }, // builtin/update-server-info.c:26
    Builtin { name: "upload-pack", run_setup: false, options: &[upload_pack::UPLOAD_PACK_OPTIONS] }, // builtin/upload-pack.c:51
    Builtin { name: "url-parse", run_setup: false, options: &[url_parse::BUILTIN_URL_PARSE_OPTIONS] }, // builtin/url-parse.c:110
    Builtin { name: "verify-commit", run_setup: true, options: &[verify_commit::VERIFY_COMMIT_OPTIONS] }, // builtin/verify-commit.c:69
    Builtin { name: "verify-pack", run_setup: false, options: &[verify_pack::VERIFY_PACK_OPTIONS] }, // builtin/verify-pack.c:86
    Builtin { name: "verify-tag", run_setup: true, options: &[verify_tag::VERIFY_TAG_OPTIONS] }, // builtin/verify-tag.c:40
    Builtin { name: "version", run_setup: false, options: &[version::VERSION_OPTIONS] }, // help.c:847
    Builtin { name: "whatchanged", run_setup: true, options: &[log::BUILTIN_LOG_OPTIONS] }, // builtin/log.c:309
    Builtin { name: "worktree", run_setup: true, options: &[worktree::WORKTREE_OPTIONS] }, // builtin/worktree.c:1487
    Builtin { name: "write-tree", run_setup: true, options: &[write_tree::WRITE_TREE_OPTIONS] }, // builtin/write-tree.c:48
];
