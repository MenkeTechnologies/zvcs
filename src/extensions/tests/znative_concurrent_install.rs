//! Two `git znative` installs at the same moment.
//!
//! The plugin index and its two derived tables are rewritten whole: load every
//! installed plugin, add one, save. Two callers that load before either saves
//! cannot both survive, and neither call fails —
//! `pkg::store::tests::two_indexes_loaded_at_once_lose_one_of_the_plugins`
//! demonstrates exactly that, deterministically, at the layer where it happens.
//! `pkg::commands` is what keeps real installs away from it, by holding the
//! registry lock across the load and the save.
//!
//! These two cases pin the observable contract rather than the race: two
//! distinct plugins installed simultaneously are both in the index and both in
//! `verbs.tsv`, and several plugins claiming one verb leave exactly one
//! installed with the rest refused rather than silently dropped.
//!
//! Being straight about what they do not prove: an install spends most of its
//! time staging and copying, so two of them rarely overlap inside the load-save
//! window, and removing the lock does not fail them. The unit test above is the
//! evidence for the window; these are the evidence for the outcome.
//!
//! Both build the example plugins, so they skip where there is no cargo — the
//! one environment the plugin tests cannot run in.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize().unwrap()
}

fn git(dir: &Path, home: &Path, args: &[&str]) -> Output {
    Command::new(BIN)
        .args(args)
        .current_dir(dir)
        .env("HOME", home)
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("ZVCS_HOME", home.join("zvcs"))
        .output()
        .unwrap()
}

fn ok(out: &Output, what: &str) -> String {
    assert!(out.status.success(), "{what} failed: {}{}",
        String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// Start an install without waiting for it, so the two overlap.
fn spawn_install(dir: &Path, home: &Path, from: &Path) -> Child {
    Command::new(BIN)
        .args(["znative", "install"])
        .arg(from)
        .current_dir(dir)
        .env("HOME", home)
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("ZVCS_HOME", home.join("zvcs"))
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("spawn install")
}

fn fixture(tag: &str) -> (PathBuf, PathBuf, PathBuf) {
    let root = std::env::temp_dir().join(format!("zvcs-zninst-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let root = root.canonicalize().unwrap();
    let home = root.join("home");
    std::fs::create_dir_all(home.join("zvcs")).unwrap();
    let repo = root.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    ok(&git(&repo, &home, &["init", "-q", "-b", "main"]), "init");
    ok(&git(&repo, &home, &["-c", "user.email=t@e.x", "-c", "user.name=t",
        "commit", "--allow-empty", "-q", "-m", "first"]), "commit");
    (root, home, repo)
}

/// Build one example plugin and stage it the way a published plugin ships.
/// `None` when there is no cargo.
fn stage(root: &Path, work: &Path, example: &str, as_name: Option<&str>) -> Option<PathBuf> {
    let manifest = repo_root().join("examples").join(example).join("Cargo.toml");
    let target = work.join(format!("build-{example}"));
    let out = Command::new("cargo")
        .args(["build", "--release", "--manifest-path"])
        .arg(&manifest)
        .arg("--target-dir")
        .arg(&target)
        .output()
        .ok()?;
    assert!(out.status.success(), "building {example} failed: {}", String::from_utf8_lossy(&out.stderr));
    let rel = target.join("release");
    let lib = std::fs::read_dir(&rel)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .find(|n| n.starts_with(std::env::consts::DLL_PREFIX) && n.ends_with(std::env::consts::DLL_SUFFIX))
        .expect("no cdylib produced");

    let dir = work.join(format!("staged-{}", as_name.unwrap_or(example)));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::copy(rel.join(&lib), dir.join(&lib)).unwrap();
    // The manifest names the plugin; a copy installed under another name is how
    // the duplicate-verb case gets several plugins claiming one verb.
    let name = as_name.unwrap_or(example);
    std::fs::write(
        dir.join("znative.toml"),
        format!("[plugin]\nname = \"{name}\"\nversion = \"0.1.0\"\n"),
    )
    .unwrap();
    let _ = root;
    Some(dir)
}

#[test]
fn two_plugins_installed_at_once_are_both_recorded() {
    let (root, home, repo) = fixture("both");
    let Some(hello) = stage(&root, &root, "plugin-hello", None) else {
        eprintln!("skipping: no cargo to build the example plugin with");
        return;
    };
    let wip = stage(&root, &root, "plugin-wip", None).expect("cargo was available a moment ago");

    // Distinct plugins with distinct verbs: nothing here should refuse anything,
    // so both must be in the index afterwards.
    let a = spawn_install(&repo, &home, &hello);
    let b = spawn_install(&repo, &home, &wip);
    for (name, k) in [("hello", a), ("wip", b)] {
        let out = k.wait_with_output().unwrap();
        assert!(out.status.success(), "installing {name} failed: {}", String::from_utf8_lossy(&out.stderr));
    }

    // `stage` names each plugin after its example directory.
    let listing = ok(&git(&repo, &home, &["znative", "list"]), "znative list");
    assert!(listing.contains("plugin-hello"), "the hello plugin was lost:\n{listing}");
    assert!(listing.contains("plugin-wip"), "the wip plugin was lost:\n{listing}");

    // The derived table dispatch reads must carry both plugins' verbs.
    let verbs = std::fs::read_to_string(home.join("zvcs/pkg/verbs.tsv")).unwrap_or_default();
    let owners: std::collections::HashSet<&str> =
        verbs.lines().filter_map(|l| l.split('\t').nth(1)).collect();
    assert!(
        owners.contains("plugin-hello") && owners.contains("plugin-wip"),
        "verbs.tsv lost a plugin:\n{verbs}"
    );

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn only_one_of_several_plugins_claiming_a_verb_is_installed() {
    let (root, home, repo) = fixture("conflict");
    let Some(first) = stage(&root, &root, "plugin-hello", Some("p1")) else {
        eprintln!("skipping: no cargo to build the example plugin with");
        return;
    };
    // The same cdylib under different plugin names: every copy registers the
    // same verb, so exactly one may be installed however many race.
    let others: Vec<PathBuf> = (2..=5)
        .map(|i| {
            let d = root.join(format!("staged-p{i}"));
            std::fs::create_dir_all(&d).unwrap();
            for e in std::fs::read_dir(&first).unwrap().flatten() {
                std::fs::copy(e.path(), d.join(e.file_name())).unwrap();
            }
            std::fs::write(d.join("znative.toml"), format!("[plugin]\nname = \"p{i}\"\nversion = \"0.1.0\"\n")).unwrap();
            d
        })
        .collect();

    let mut kids = vec![spawn_install(&repo, &home, &first)];
    for d in &others {
        kids.push(spawn_install(&repo, &home, d));
    }
    let outs: Vec<Output> = kids.into_iter().map(|k| k.wait_with_output().unwrap()).collect();

    // The index is the authority: one plugin, one owner per verb.
    let listing = ok(&git(&repo, &home, &["znative", "list"]), "znative list");
    let installed = listing.lines().filter(|l| l.starts_with('p')).count();
    assert_eq!(installed, 1, "the verb-conflict check let {installed} plugins claim one verb:\n{listing}");

    let refused = outs.iter().filter(|o| !o.status.success()).count();
    assert_eq!(refused, outs.len() - 1, "an install that was not recorded still reported success");

    let verbs = std::fs::read_to_string(home.join("zvcs/pkg/verbs.tsv")).unwrap_or_default();
    let rows = verbs.lines().filter(|l| !l.trim().is_empty()).count();
    assert_eq!(rows, 1, "verbs.tsv should name exactly one owner:\n{verbs}");

    let _ = std::fs::remove_dir_all(&root);
}
