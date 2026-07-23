//! `zvcs-parity` — differential parity + fuzz harness for the zvcs `git` binary.
//!
//! Runs every case against both stock git and zvcs in identical throwaway repos
//! and compares stdout, exit code, and resulting repository state.
//!
//! This is measurement infrastructure. Its output is only worth reading if it is
//! never tuned to flatter the implementation, so two properties are structural:
//! the denominator comes from the installed git at runtime, and an unported
//! command scores as a failure rather than a skip.
//!
//! Usage:
//!   zvcs-parity                      # curated corpus
//!   zvcs-parity --fuzz 40            # corpus + 40 generated cases per command
//!   zvcs-parity --fuzz 40 --seed 7   # reproduce a specific fuzz run
//!   zvcs-parity --only status,log    # restrict to some subcommands
//!   zvcs-parity --verbose            # print every failure in detail
//!   zvcs-parity --bin path/to/git    # explicit binary under test
//!   zvcs-parity --html docs/port_report.html   # regenerate the HTML report

mod corpus;
mod env;
mod fixture;
mod fuzz;
mod grammars_generated;
mod report;
mod runner;

use anyhow::{Context, Result};
use runner::{run_case, Case};
use std::process::ExitCode;

struct Args {
    fuzz_per_cmd: usize,
    seed: u64,
    only: Vec<String>,
    verbose: bool,
    bin: Option<String>,
    keep: bool,
    shrink: bool,
    /// `--html <path>`: also write the HTML port report to this path from the
    /// run's real coverage + parity numbers (regenerates `docs/port_report.html`).
    html: Option<String>,
}

fn parse_args() -> Result<Args> {
    let mut a = Args {
        fuzz_per_cmd: 0,
        // Fixed default so an unseeded run is still reproducible; override to explore.
        seed: 0x5A5A_C0DE,
        only: Vec::new(),
        verbose: false,
        bin: None,
        keep: false,
        shrink: false,
        html: None,
    };
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let mut i = 0;
    while i < argv.len() {
        let next = |i: usize| -> Result<String> {
            argv.get(i + 1).cloned().with_context(|| format!("{} needs a value", argv[i]))
        };
        match argv[i].as_str() {
            "--fuzz" => {
                a.fuzz_per_cmd = next(i)?.parse().context("--fuzz needs a number")?;
                i += 2;
            }
            "--seed" => {
                a.seed = next(i)?.parse().context("--seed needs a number")?;
                i += 2;
            }
            "--only" => {
                a.only = next(i)?.split(',').map(|s| s.trim().to_string()).collect();
                i += 2;
            }
            "--bin" => {
                a.bin = Some(next(i)?);
                i += 2;
            }
            "--verbose" | "-v" => {
                a.verbose = true;
                i += 1;
            }
            "--keep" => {
                a.keep = true;
                i += 1;
            }
            "--shrink" => {
                a.shrink = true;
                i += 1;
            }
            "--html" => {
                a.html = Some(next(i)?);
                i += 2;
            }
            other => anyhow::bail!("unknown argument {other:?}"),
        }
    }
    Ok(a)
}

fn main() -> ExitCode {
    match real_main() {
        Ok(code) => code,
        Err(e) => {
            eprintln!("zvcs-parity: {e:#}");
            ExitCode::FAILURE
        }
    }
}

fn real_main() -> Result<ExitCode> {
    let args = parse_args()?;
    let zvcs_bin = runner::locate_zvcs_bin(args.bin.as_deref())?;

    // Everything lands under one root so a run leaves nothing behind.
    let root = std::env::temp_dir().join(format!("zvcs-parity-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root)?;

    eprintln!("binary   : {}", zvcs_bin.display());
    eprintln!("workdir  : {}", root.display());
    eprintln!("building fixtures…");
    let templates = fixture::Templates::build_all(&root)?;

    let mut cases: Vec<Case> = corpus::cases();
    if args.fuzz_per_cmd > 0 {
        eprintln!("fuzzing  : {} cases/cmd, seed {}", args.fuzz_per_cmd, args.seed);
        cases.extend(fuzz::generate(args.seed, args.fuzz_per_cmd));
    }
    if !args.only.is_empty() {
        cases.retain(|c| args.only.iter().any(|o| o == c.cmd));
    }
    eprintln!("cases    : {}", cases.len());

    let workdir = root.join("run");
    std::fs::create_dir_all(&workdir)?;

    // Cases are independent, so they run across a worker pool. Each worker owns
    // its own workdir subtree (run/w<k>), so the fixed `stock`/`zvcs`/
    // `stock-repeat` child dirs `run_case` uses never collide between threads.
    // Results are written back by original index, so the report is identical to
    // a sequential run regardless of scheduling — determinism is preserved.
    let n_workers = std::thread::available_parallelism()
        .map(|n| n.get().saturating_sub(1).max(1))
        .unwrap_or(1)
        .min(cases.len().max(1));
    eprintln!("workers  : {n_workers}");

    let next = std::sync::atomic::AtomicUsize::new(0);
    let done = std::sync::atomic::AtomicUsize::new(0);
    let total = cases.len();
    // One owning slot per case, filled by index so the result order is
    // independent of which worker ran which case.
    let slots: Vec<std::sync::Mutex<Option<runner::Outcome>>> =
        (0..total).map(|_| std::sync::Mutex::new(None)).collect();

    let first_err: std::sync::Mutex<Option<anyhow::Error>> = std::sync::Mutex::new(None);

    std::thread::scope(|scope| {
        for w in 0..n_workers {
            let (next, done, slots, cases, templates, zvcs_bin, workdir, first_err) = (
                &next, &done, &slots, &cases, &templates, &zvcs_bin, &workdir, &first_err,
            );
            let wdir = workdir.join(format!("w{w}"));
            scope.spawn(move || {
                let _ = std::fs::create_dir_all(&wdir);
                loop {
                    let i = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    if i >= total || first_err.lock().unwrap().is_some() {
                        break;
                    }
                    match run_case(&cases[i], zvcs_bin, templates, &wdir) {
                        Ok(o) => *slots[i].lock().unwrap() = Some(o),
                        Err(e) => {
                            *first_err.lock().unwrap() = Some(e);
                            break;
                        }
                    }
                    let d = done.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
                    if d % 200 == 0 {
                        eprintln!("  … {d}/{total}");
                    }
                }
            });
        }
    });

    if let Some(e) = first_err.into_inner().unwrap() {
        return Err(e);
    }
    let outcomes: Vec<runner::Outcome> = slots
        .into_iter()
        .map(|m| {
            m.into_inner()
                .unwrap()
                .expect("every case slot filled unless an error aborted the run")
        })
        .collect();

    // Coverage is probed in a throwaway repo so a stray mutating probe cannot
    // touch anything that matters.
    let probe_dir = root.join("probe");
    std::fs::create_dir_all(&probe_dir)?;
    templates.instantiate(fixture::Shape::Linear, &probe_dir)?;
    let stock = report::stock_subcommands()?;
    let have = report::dispatched(&zvcs_bin, &templates.home, &stock, &probe_dir);
    let missing: Vec<String> = stock.iter().filter(|c| !have.contains(c)).cloned().collect();

    let rep = report::tally(outcomes);
    rep.print((have.len(), stock.len()), &missing, args.verbose);

    // `--html <path>`: regenerate the HTML port report from THIS run's real
    // numbers — dispatch coverage and per-command parity, nothing hand-typed.
    if let Some(path) = &args.html {
        report::emit_html(std::path::Path::new(path), &rep, &stock, &have, &missing)?;
        eprintln!("wrote {path}");
    }

    // Minimizing is opt-in: it costs a re-run per dropped argument, but turns a
    // three-flag failure into the one flag actually responsible.
    if args.shrink && !rep.failures.is_empty() {
        eprintln!("\nshrinking {} failures…", rep.failures.len());
        let shrink_dir = root.join("shrink");
        std::fs::create_dir_all(&shrink_dir)?;
        for f in &rep.failures {
            // Unsupported cases shrink to nothing useful — the gap is the whole
            // subcommand, not any particular argument.
            if f.verdict == runner::Verdict::Unsupported {
                continue;
            }
            let minimal = fuzz::shrink(&f.case, &mut |c| {
                run_case(c, &zvcs_bin, &templates, &shrink_dir)
                    .map(|o| !o.verdict.is_match())
                    .unwrap_or(false)
            });
            if minimal.args.len() < f.case.args.len() {
                println!("  {} → git {}", f.case.id(), minimal.args.join(" "));
            }
        }
    }

    if args.keep {
        eprintln!("\nkept workdir: {}", root.display());
    } else {
        let _ = std::fs::remove_dir_all(&root);
    }

    // Non-zero when anything failed, so CI can gate on it.
    Ok(if rep.overall.matched == rep.overall.total() {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    })
}
