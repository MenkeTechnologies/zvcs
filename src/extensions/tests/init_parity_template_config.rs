//! What a template directory's `config` does to `git init`.
//!
//! `copy_templates()` (setup.c:2376-2428) reads the template's `config` as a
//! repository format first: a version this build cannot read is
//! `warning: not copying templates from '<dir>': <reason>` and nothing is
//! copied. Otherwise `copy_templates_1()` copies that `config` into the still
//! empty git directory, and every key `create_default_files()` sets afterwards
//! is written into it — replacing a key the template spells in place, appending
//! the rest to the section. zvcs copied the rest of the template but kept its own
//! `config`, dropping every template key, and copied a template of any version.
//! Expectations measured against stock git 2.56.0.

use std::path::Path;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn git(dir: &Path, args: &[&str]) -> (String, String, i32) {
    let out = Command::new(BIN)
        .args(args)
        .current_dir(dir)
        .env("HOME", dir)
        .env("ZVCS_HOME", dir)
        .env("GIT_CEILING_DIRECTORIES", dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("LC_ALL", "C")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_TEMPLATE_DIR")
        .output()
        .expect("run the binary under test");
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
        out.status.code().expect("no signal"),
    )
}

#[test]
fn a_template_config_is_the_base_the_init_keys_are_written_into() {
    let root = std::env::temp_dir().join(format!("zvcs-init-template-config-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("tpl")).unwrap();
    std::fs::write(root.join("tpl/config"), "[core]\n\tfoo = bar\n\trepositoryformatversion = 0\n[user]\n\tname = T\n").unwrap();

    let (_, err, code) = git(&root, &["init", "-q", "-b", "main", "--template=tpl", "r"]);
    assert_eq!((err.as_str(), code), ("", 0));
    let config = std::fs::read_to_string(root.join("r/.git/config")).unwrap();
    assert!(
        config.starts_with("[core]\n\tfoo = bar\n\trepositoryformatversion = 0\n\tfilemode = "),
        "{config}"
    );
    assert!(config.contains("\tlogallrefupdates = true\n") && config.ends_with("\n[user]\n\tname = T\n"), "{config}");
    assert_eq!(git(&root.join("r"), &["config", "user.name"]).0, "T\n");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_template_of_an_unreadable_vintage_is_not_copied() {
    let root = std::env::temp_dir().join(format!("zvcs-init-template-vintage-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("tpl")).unwrap();
    std::fs::write(root.join("tpl/config"), "[core]\n\trepositoryformatversion = 9\n").unwrap();
    std::fs::write(root.join("tpl/description"), "d\n").unwrap();
    let real = root.canonicalize().unwrap();

    let (_, err, code) = git(&root, &["init", "-q", "-b", "main", "--template=tpl", "r"]);
    assert_eq!(code, 0);
    assert_eq!(
        err,
        format!(
            "warning: not copying templates from '{}/tpl': Expected git repo version <= 1, found 9\n",
            real.display()
        )
    );
    assert!(!root.join("r/.git/description").exists());
    let config = std::fs::read_to_string(root.join("r/.git/config")).unwrap();
    assert!(config.starts_with("[core]\n\trepositoryformatversion = 0\n"), "{config}");
    let _ = std::fs::remove_dir_all(&root);
}
