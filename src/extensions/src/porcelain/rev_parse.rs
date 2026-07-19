use anyhow::Result;
use std::process::ExitCode;

/// `git rev-parse` — currently resolves `HEAD` only.
///
/// Supports the two forms the meta workflow leans on:
///   * `git rev-parse HEAD`              → full object id of HEAD
///   * `git rev-parse --abbrev-ref HEAD` → short symbolic ref (branch), or `HEAD` if detached
pub fn rev_parse(args: &[String]) -> Result<ExitCode> {
    let repo = gix::discover(".")?;

    let abbrev = args.iter().any(|a| a == "--abbrev-ref");
    // Positional rev is the last non-flag argument; default to HEAD.
    let spec = args
        .iter()
        .rev()
        .find(|a| !a.starts_with('-'))
        .map(String::as_str)
        .unwrap_or("HEAD");

    if spec != "HEAD" {
        anyhow::bail!("only HEAD is ported so far, got {spec:?}");
    }

    if abbrev {
        match repo.head_name()? {
            Some(name) => println!("{}", name.shorten()),
            None => println!("HEAD"), // detached HEAD
        }
    } else {
        println!("{}", repo.head_id()?);
    }
    Ok(ExitCode::SUCCESS)
}
