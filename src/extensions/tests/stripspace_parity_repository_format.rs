//! `stripspace -s` and `-c` call `setup_git_directory_gently()` after their option parse
//! (builtin/stripspace.c:56), and that setup reads the repository's configuration for its format:
//! a line the parser rejects, an `extensions.<key>` value `check_repo_format()` rejects and a
//! `core.repositoryformatversion` that is no number are all fatal at 128 in those two modes. The
//! default mode never sets anything up and ignores the same files. Stock git is the oracle
//! (`support/stock_git.rs`).
#![cfg(unix)]

#[path = "support/stock_git.rs"]
mod stock_git;
#[path = "support/config_twins.rs"]
mod config_twins;

use config_twins::Twins;

/// Append `text` verbatim to the repository config of both sides.
fn append_config(t: &Twins, text: &str) {
    for side in ["stock", "zvcs"] {
        let path = t.root.join(side).join(".git/config");
        let mut body = std::fs::read_to_string(&path).unwrap();
        body.push_str(text);
        std::fs::write(path, body).unwrap();
    }
}

fn run_both(t: &Twins, args: &[&str]) -> config_twins::Outcome {
    let home = t.root.join("home");
    let stock = config_twins::run(t.stock_bin, &t.root.join("stock"), &home, args);
    let zvcs = config_twins::run(config_twins::BIN, &t.root.join("zvcs"), &home, args);
    assert_eq!(stock, zvcs, "`git {args:?}`: stock (left) vs zvcs (right)");
    stock
}

#[test]
fn comment_modes_refuse_a_repository_whose_format_cannot_be_read() {
    for (tag, text) in [
        ("ext", "[extensions]\n\trefStorage = input\n"),
        ("line", "[core]\n\tbogus ===\n[x\n"),
        ("version", "[core]\n\trepositoryFormatVersion = bogus\n"),
    ] {
        let Some(t) = Twins::new(&format!("stripspace-format-{tag}")) else { return };
        append_config(&t, text);
        for args in [&["stripspace", "-s"][..], &["stripspace", "-c"][..], &["stripspace", "--strip-comments"][..]] {
            let stock = run_both(&t, args);
            assert_eq!(stock.code, 128, "{tag} {args:?}: {stock:?}");
        }
    }
}

#[test]
fn the_default_mode_ignores_the_same_files() {
    for (tag, text) in [
        ("ext", "[extensions]\n\trefStorage = input\n"),
        ("version", "[core]\n\trepositoryFormatVersion = bogus\n"),
    ] {
        let Some(t) = Twins::new(&format!("stripspace-format-default-{tag}")) else { return };
        append_config(&t, text);
        let stock = run_both(&t, &["stripspace"]);
        assert_eq!(stock.code, 0, "{tag}: {stock:?}");
    }
}

#[test]
fn a_usage_error_precedes_the_format_read() {
    let Some(t) = Twins::new("stripspace-format-usage") else { return };
    append_config(&t, "[extensions]\n\trefStorage = input\n");
    let stock = run_both(&t, &["stripspace", "-s", "extra"]);
    assert_eq!(stock.code, 129, "{stock:?}");
}
