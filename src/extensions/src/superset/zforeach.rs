//! `zforeach [selectors] [--] <command>...` — run a command across all (or a
//! filtered subset of) indexed repos, in parallel.
//!
//! The general "do anything, everywhere" primitive: it fans a command out over
//! the [`Selector`]'s repos through a bounded worker pool (concurrency = cores),
//! collects each repo's output, prints it grouped and in order (like
//! `git submodule foreach`, but over the whole-machine index and parallel), and
//! records failures in the ledger. It is lane-aware by composition — if the
//! command is a zvcs `git` write, that command acquires its repo's fair lane
//! itself, so same-repo writes serialize while different repos run concurrently.

use anyhow::{bail, Result};
use std::process::{Command, ExitCode};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;

use crate::superset::select::Selector;

pub fn zforeach(args: &[String]) -> Result<ExitCode> {
    // `--` is the guard that lets the fanned-out command contain tokens that look
    // like selectors (`--repo`, `--session`, …). Split on the FIRST `--` before
    // selecting: only the left half is parsed for selectors, and the right half is
    // the command verbatim. (Feeding the whole arg list to Selector::parse would
    // consume the command's own `--session x` as a selector — silently mangling
    // the command AND narrowing the repo set.) Without `--`, selectors lead and the
    // remainder is the command, as before.
    let (sel, cmd): (Selector, Vec<String>) = match args.iter().position(|a| a == "--") {
        Some(p) => {
            let (sel, _left_over) = Selector::parse(&args[..p]);
            (sel, args[p + 1..].to_vec())
        }
        None => {
            let (sel, rest) = Selector::parse(args);
            (sel, rest)
        }
    };
    if cmd.is_empty() {
        bail!("usage: git zforeach [--repo <sub>|--dirty|--ahead|--behind|--claimed|--session <s>] [--] <command>...");
    }

    let repos = sel.select()?;
    if repos.is_empty() {
        println!("no repos matched");
        return Ok(ExitCode::SUCCESS);
    }

    let n = repos.len();
    let repos = Arc::new(repos);
    let cmd = Arc::new(cmd);
    let next = Arc::new(AtomicUsize::new(0));
    let results: Arc<Mutex<Vec<Option<(bool, String)>>>> = Arc::new(Mutex::new(vec![None; n]));

    let workers = thread::available_parallelism().map(|c| c.get().min(16)).unwrap_or(4);
    let handles: Vec<_> = (0..workers)
        .map(|_| {
            let repos = Arc::clone(&repos);
            let cmd = Arc::clone(&cmd);
            let next = Arc::clone(&next);
            let results = Arc::clone(&results);
            thread::spawn(move || loop {
                let i = next.fetch_add(1, Ordering::SeqCst);
                if i >= repos.len() {
                    break;
                }
                let (_git_dir, workdir) = &repos[i];
                let out = Command::new(&cmd[0]).args(&cmd[1..]).current_dir(workdir).output();
                let res = match out {
                    Ok(o) => {
                        let mut s = String::from_utf8_lossy(&o.stdout).into_owned();
                        s.push_str(&String::from_utf8_lossy(&o.stderr));
                        (o.status.success(), s)
                    }
                    Err(e) => (false, format!("failed to run `{}`: {e}\n", cmd[0])),
                };
                results.lock().unwrap()[i] = Some(res);
            })
        })
        .collect();
    for h in handles {
        let _ = h.join();
    }

    let results = Arc::try_unwrap(results)
        .map(|m| m.into_inner().unwrap())
        .unwrap_or_default();

    let mut ok = 0usize;
    let mut failed = 0usize;
    for (i, (git_dir, workdir)) in repos.iter().enumerate() {
        let Some((success, output)) = &results[i] else { continue };
        println!("== {} ==", workdir.display());
        let trimmed = output.trim_end();
        if !trimmed.is_empty() {
            println!("{trimmed}");
        }
        if *success {
            ok += 1;
        } else {
            failed += 1;
            let _ = crate::db::record_failure(git_dir, "foreach", &format!("{}: command failed", workdir.display()));
        }
    }
    eprintln!("zforeach: {ok} ok, {failed} failed ({n} repos)");
    Ok(if failed > 0 { ExitCode::FAILURE } else { ExitCode::SUCCESS })
}
