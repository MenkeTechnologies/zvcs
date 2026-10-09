//! `--textconv` and `--filters` fill `opt` in `cmd_cat_file()`, so a batch run beside either
//! never reaches the "batch modes take no arguments" check and its operands are ignored.
//! zvcs refused them with the usage block.
#![cfg(unix)]

#[path = "support/stock_git.rs"]
mod stock_git;
#[path = "support/twin_repo.rs"]
mod twin_repo;

use twin_repo::Side;

fn world(label: &str) -> Option<(Side, Side)> {
    let stock = stock_git::stock_git()?;
    Some(twin_repo::pair(label, stock))
}

fn feed(side: &Side, args: &[&str]) -> twin_repo::Out {
    let mut cmd = std::process::Command::new(&side.bin);
    cmd.args(args)
        .current_dir(side.repo())
        .env_clear()
        .env("HOME", &side.root)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let mut child = cmd.spawn().unwrap();
    std::io::Write::write_all(child.stdin.as_mut().unwrap(), b"HEAD\nmain:a\n").unwrap();
    let out = child.wait_with_output().unwrap();
    let root = side.root.to_string_lossy().into_owned();
    let text = |b: &[u8]| String::from_utf8_lossy(b).replace(&root, "<root>");
    twin_repo::Out { code: out.status.code().unwrap_or(-1), stdout: text(&out.stdout), stderr: text(&out.stderr) }
}

#[test]
fn a_transformed_batch_ignores_operands_and_a_plain_batch_refuses_them() {
    let Some((s, z)) = world("cat-file-batch-operands") else { return };
    for args in [
        &["cat-file", "--textconv", "--batch-check", "HEAD"][..],
        &["cat-file", "--batch-check", "--filters", "HEAD", "x"],
        &["cat-file", "--filters", "--batch", "HEAD"],
        &["cat-file", "--batch-check", "HEAD"],
        &["cat-file", "--batch", "HEAD"],
    ] {
        let want = feed(&s, args);
        assert_eq!(feed(&z, args), want, "{args:?}");
    }
    assert_eq!(feed(&s, &["cat-file", "--textconv", "--batch-check", "HEAD"]).code, 0);
    assert_eq!(feed(&s, &["cat-file", "--batch-check", "HEAD"]).code, 129);
}
