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
//!                                    #   and 40 generated sequences per entry point
//!   zvcs-parity --fuzz 40 --seed 7   # reproduce a specific fuzz run
//!   zvcs-parity --fuzz 40 --fuzz-sequences 0   # argv sweep only, no sequences
//!   zvcs-parity --fuzz 40 --list-cases         # what that run would execute
//!   zvcs-parity --only status,log    # restrict to some subcommands
//!   zvcs-parity --verbose            # print every failure in detail
//!   zvcs-parity --bin path/to/git    # explicit binary under test
//!   zvcs-parity --html docs/port_report.html   # regenerate the HTML report
//!   zvcs-parity --alt-git /usr/bin/git         # name the second oracle
//!   zvcs-parity --no-alt-git                   # one oracle, as it always was
//!   zvcs-parity --alt-git-every-case           # ask the second oracle about
//!                                              #   passing cases too
//!   zvcs-parity --concurrency        # also run the concurrent-writer corpus
//!
//! A machine with two real gits gets the second oracle without being asked: a
//! difference against the newest git is otherwise reported identically whether
//! the port is wrong or whether git changed between the two releases, and an
//! opt-in flag is one nobody remembers on the run that would have needed it. It
//! costs nothing on a case that matched — see `runner`'s header for the gate, the
//! classification and what it does to the denominator.

mod concurrent;
mod corpus;
mod env;
mod fixture;
mod nested;
mod fuzz;
mod grammars_generated;
mod report;
mod runner;
mod stock;

use anyhow::{Context, Result};
use runner::{run_case, Job};
use std::process::ExitCode;

struct Args {
    fuzz_per_cmd: usize,
    /// `--fuzz-sequences <n>`: generated multi-step workflows per entry point.
    ///
    /// `None` means "follow `--fuzz`", which is the one-knob default. It is a
    /// knob of its own because the two generated corpora have very different
    /// unit prices — a case is one invocation and one state probe per side, a
    /// sequence is five or six of each — so a caller who wants a deep argv sweep
    /// would otherwise be made to buy a proportionally larger sequence bill to
    /// get it, and a caller chasing one stateful defect would be made to buy the
    /// argv sweep. `0` turns the family off.
    fuzz_sequences: Option<usize>,
    seed: u64,
    only: Vec<String>,
    verbose: bool,
    bin: Option<String>,
    keep: bool,
    shrink: bool,
    /// `--html <path>`: also write the HTML port report to this path from the
    /// run's real coverage + parity numbers (regenerates `docs/port_report.html`).
    html: Option<String>,
    /// `--list-cases`: print the id of every case the run *would* execute, then
    /// exit without running anything.
    ///
    /// A case id is this harness's reproduction recipe — it carries the shape,
    /// the argv, the configuration and the scope each setting came from, the
    /// working directory, the environment and the stdin digest — and until this
    /// flag existed the only way to see one was to make it fail. That is a poor
    /// trade for the question it answers most often, which is "what does seed N
    /// actually generate", and it is the question a reader asks before trusting
    /// that a sampled dimension fires at all. Costs nothing: generation is pure,
    /// so no fixture is built and no child process is spawned.
    list_cases: bool,
    /// `--alt-git <path>` / `--no-alt-git`: which second git to measure against,
    /// or none.
    ///
    /// Defaults to [`stock::AltChoice::Auto`] — the newest *other* real git the
    /// machine has — rather than to off, because a dimension that has to be
    /// switched on is one nobody switches on for the run that needed it, and this
    /// one exists precisely to answer a question a reader does not know they have
    /// until a case fails. `--no-alt-git` and `ZVCS_STOCK_GIT_ALT=none` are the
    /// escape hatches, and a machine with one git never had a choice to make.
    alt_git: stock::AltChoice,
    /// `--alt-git-every-case`: lift the failure gate on the second oracle.
    ///
    /// The gated default asks the second oracle only about cases that already
    /// failed, which is where the port-defect-or-version-difference question is
    /// asked and costs nothing on the ~99% that pass. It has one blind spot, and
    /// this flag is what buys it: a case where the port matches the newest git
    /// and the older git would have said something else never fails, so it is
    /// never adjudicated — yet it is exactly the case whose *expected value* is
    /// version-dependent, and therefore exactly the case where a curated
    /// expectation might be pinned to the wrong git. Off by default because it
    /// costs one extra invocation on every case in the corpus — and rather more
    /// than that on a sequence, where the second oracle has to replay the prefix
    /// to reach step *k* (see `runner::alt_sequence_side`), so a seven-step
    /// workflow costs 1+2+…+7 = 28 invocations instead of 7. Gated, only the step
    /// that diverged pays, and only once.
    alt_git_every_case: bool,
    /// `--concurrency`: also run the concurrent-writer corpus.
    ///
    /// Off by default, and the only dimension that is. Every other case is one
    /// invocation against a pristine copy; a concurrency case releases up to
    /// eight processes at once and then waits out a settle window, so it is
    /// priced in seconds rather than milliseconds and it is the one dimension
    /// that can saturate the machine it runs on. It is also the only dimension
    /// whose result is a *distribution* — a read-modify-write race reproduces on
    /// some runs and not others — so a green concurrency run is weaker evidence
    /// than a green case elsewhere, and folding it into the headline parity
    /// number would make that number noisier without making it more true.
    ///
    /// It is reported and gated separately for the same reason: a defect it finds
    /// is a defect (the port lost a write it said it had done), but a run that
    /// found none has not proved the race absent.
    concurrency: bool,
}

fn parse_args() -> Result<Args> {
    let mut a = Args {
        fuzz_per_cmd: 0,
        fuzz_sequences: None,
        // Fixed default so an unseeded run is still reproducible; override to explore.
        seed: 0x5A5A_C0DE,
        only: Vec::new(),
        verbose: false,
        bin: None,
        keep: false,
        shrink: false,
        html: None,
        list_cases: false,
        alt_git: stock::AltChoice::Auto,
        alt_git_every_case: false,
        concurrency: false,
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
            "--fuzz-sequences" => {
                a.fuzz_sequences =
                    Some(next(i)?.parse().context("--fuzz-sequences needs a number")?);
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
            "--list-cases" => {
                a.list_cases = true;
                i += 1;
            }
            "--alt-git" => {
                a.alt_git = stock::AltChoice::Named(next(i)?.into());
                i += 2;
            }
            "--no-alt-git" => {
                a.alt_git = stock::AltChoice::Off;
                i += 1;
            }
            "--alt-git-every-case" => {
                a.alt_git_every_case = true;
                i += 1;
            }
            "--concurrency" => {
                a.concurrency = true;
                i += 1;
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

/// Print what a run would execute, without executing it.
///
/// Deliberately the *same* assembly the real run does — the curated corpus, then
/// the generated cases, then the sequences, then `--only` — rather than a second
/// path that lists the corpus it thinks exists. A listing that could disagree
/// with the run is worse than no listing, because it would be believed.
///
/// Sequences print one line per step, under the same id `report` prints for a
/// failure at that step, so a step id copied out of a listing and a step id
/// copied out of a failure are the same string.
fn list_cases(args: &Args) -> Result<ExitCode> {
    let fuzz_sequences = args.fuzz_sequences.unwrap_or(args.fuzz_per_cmd);
    let mut jobs: Vec<Job> = corpus::cases().into_iter().map(Job::Single).collect();
    jobs.extend(corpus::sequences().into_iter().map(Job::Sequence));
    if args.fuzz_per_cmd > 0 {
        jobs.extend(fuzz::generate(args.seed, args.fuzz_per_cmd).into_iter().map(Job::Single));
    }
    if fuzz_sequences > 0 {
        jobs.extend(fuzz::generate_sequences(args.seed, fuzz_sequences).into_iter().map(Job::Sequence));
    }
    if !args.only.is_empty() {
        jobs.retain(|j| args.only.iter().any(|o| o == j.cmd()));
    }
    for job in &jobs {
        match job {
            Job::Single(c) => println!("{}", c.id()),
            Job::Sequence(s) => {
                for i in 0..s.len() {
                    println!("{}", s.step_id(i));
                }
            }
        }
    }
    Ok(ExitCode::SUCCESS)
}

fn real_main() -> Result<ExitCode> {
    let args = parse_args()?;
    if args.list_cases {
        return list_cases(&args);
    }
    let zvcs_bin = runner::locate_zvcs_bin(args.bin.as_deref())?;

    // Both second-oracle knobs are fixed here, before a fixture exists or a
    // worker starts, and never touched again. The resolution behind them is
    // memoized and read from every worker thread, so a knob that could still
    // move once cases were running would mean two cases in one report had been
    // measured against different oracles — a report whose own premise varies
    // down its length is worse than one that never asked.
    stock::set_alt_choice(args.alt_git.clone());
    runner::set_alt_every_case(args.alt_git_every_case);

    // Everything lands under one root so a run leaves nothing behind.
    let root = std::env::temp_dir().join(format!("zvcs-parity-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root)?;

    eprintln!("binary   : {}", zvcs_bin.display());
    eprintln!("workdir  : {}", root.display());
    eprintln!("building fixtures…");
    let templates = fixture::Templates::build_all(&root)?;

    // Single invocations and multi-step sequences share one list: the pool
    // schedules by index, and two lists would mean two passes over the same
    // machinery. `Job::cmd` is what `--only` filters on either way.
    let mut cases: Vec<Job> = corpus::cases().into_iter().map(Job::Single).collect();
    cases.extend(corpus::sequences().into_iter().map(Job::Sequence));
    // `--fuzz-sequences` defaults to `--fuzz`, so one knob deepens both generated
    // corpora; see `Args::fuzz_sequences` for why it is separable at all.
    let fuzz_sequences = args.fuzz_sequences.unwrap_or(args.fuzz_per_cmd);
    if args.fuzz_per_cmd > 0 {
        eprintln!("fuzzing  : {} cases/cmd, seed {}", args.fuzz_per_cmd, args.seed);
        cases.extend(fuzz::generate(args.seed, args.fuzz_per_cmd).into_iter().map(Job::Single));
    }
    let mut generated_sequences = 0;
    if fuzz_sequences > 0 {
        let generated = fuzz::generate_sequences(args.seed, fuzz_sequences);
        generated_sequences = generated.len();
        eprintln!(
            "workflows: {fuzz_sequences} sequences/entry-point, seed {} ({generated_sequences} generated)",
            args.seed
        );
        cases.extend(generated.into_iter().map(Job::Sequence));
    }
    if !args.only.is_empty() {
        cases.retain(|c| args.only.iter().any(|o| o == c.cmd()));
    }
    // Sequences are counted apart from the invocations they cost, so the price of
    // the multi-step corpus is stated rather than left to be inferred from a
    // wall clock: one sequence of seven steps is one case in the parity
    // denominator and seven child processes per side.
    //
    // The generated share is printed beside the total because the two are priced
    // and produced differently — curated sequences are a fixed cost every run
    // pays, generated ones scale with a knob — and a single number would leave a
    // reader unable to tell which one just got more expensive. It is counted
    // before `--only` is applied and the total after, so a filtered run shows the
    // filtered price.
    let sequences = cases.iter().filter(|j| matches!(j, Job::Sequence(_))).count();
    let invocations: usize = cases.iter().map(Job::invocations).sum();
    eprintln!(
        "cases    : {} ({sequences} sequences, {generated_sequences} of them generated before \
         --only; {invocations} invocations per side)",
        cases.len()
    );

    let workdir = root.join("run");
    std::fs::create_dir_all(&workdir)?;

    // Cases are independent, so they run across a worker pool. Each worker owns
    // its own workdir subtree (run/w<k>), so the fixed `stock`/`zvcs`/
    // `stock-repeat`/`zvcs-repeat` child dirs `run_case` uses never collide
    // between threads.
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
                    match cases[i].run(zvcs_bin, templates, &wdir) {
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

    // The second oracle's identity travels into the tally rather than being
    // looked up while printing, so a run that resolved one and never needed it
    // still says so. "Configured and asked about nothing" and "not present at
    // all" are different facts, and only the second may print nothing.
    let alt_oracle = stock::alt_git().map(|(p, v)| (p.to_path_buf(), v));
    let rep = report::tally(outcomes, alt_oracle, args.alt_git_every_case);
    rep.print((have.len(), stock.len()), &missing, args.verbose);

    // `--html <path>`: regenerate the HTML port report from THIS run's real
    // numbers — dispatch coverage, per-command parity, and the per-command option
    // support matrix, all measured now; nothing hand-typed.
    if let Some(path) = &args.html {
        eprintln!("probing option support ({} commands)…", have.len());
        let opt_root = root.join("optprobe");
        std::fs::create_dir_all(&opt_root)?;
        let opts = report::option_matrix(&zvcs_bin, &templates.home, &have, &templates, &opt_root);

        // Config-variable support: scan the source the `git` binary is built from
        // (the extensions crate + vendored gitoxide) for every `git help --config`
        // key. Paths are relative to this crate at compile time.
        let cfg_keys = report::git_config_keys();
        eprintln!("scanning config support ({} variables)…", cfg_keys.len());
        let base = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let src_roots = vec![base.join("../extensions/src"), base.join("../ported")];
        let cfg = report::config_support(&cfg_keys, &src_roots);

        report::emit_html(
            std::path::Path::new(path),
            &rep,
            &stock,
            &have,
            &missing,
            &opts,
            &cfg,
        )?;
        eprintln!("wrote {path}");
    }

    // The concurrent-writer corpus runs after everything else, sequentially, and
    // is tallied on its own. Sequentially because each case already spawns up to
    // eight children at once: multiplying that by the worker pool is how this
    // harness previously exhausted the machine's fork capacity and took every
    // shell on the box down with it. Its cases are independent of the pool's, so
    // nothing is lost but wall clock.
    let mut concurrency_defects = 0usize;
    if args.concurrency {
        let mut cases = concurrent::cases();
        if !args.only.is_empty() {
            cases.retain(|c| args.only.iter().any(|o| o == c.cmd));
        }
        let conc_dir = root.join("concurrent");
        std::fs::create_dir_all(&conc_dir)?;
        println!("\nconcurrent writers ({} cases, sequential)", cases.len());
        println!(
            "  invariant: a writer that exits 0 has done its work — asserted against \
             stock git, which\n  satisfies it by failing its losers honestly. An \
             invariant stock breaks too is not scored."
        );
        for case in &cases {
            let outcome = concurrent::run_concurrent_case(case, &zvcs_bin, &templates, &conc_dir);
            match &outcome.verdict {
                concurrent::Verdict::Honest => {
                    if args.verbose {
                        if let Some(z) = &outcome.zvcs {
                            println!(
                                "  ok   {} — {} exited 0, {} of those landed, {} queued",
                                outcome.id, z.exited_ok, z.exited_ok_landed, z.queued
                            );
                        }
                    }
                }
                concurrent::Verdict::Skipped(why) => {
                    println!("  skip {} — {why}", outcome.id);
                }
                // Printed unconditionally, not behind --verbose: a case that
                // measures nothing is a defect in this corpus, and the only way it
                // gets fixed is by being visible on the run that produced it.
                concurrent::Verdict::Vacuous(why) => {
                    println!("  ??   {} — measured nothing: {why}", outcome.id);
                }
                concurrent::Verdict::ControlAlsoFails => {
                    println!(
                        "  ==   {} — stock git breaks the same invariant, so the port may too",
                        outcome.id
                    );
                }
                concurrent::Verdict::Defect => {
                    concurrency_defects += 1;
                    println!("  FAIL {}", outcome.id);
                    if let Some(z) = &outcome.zvcs {
                        for line in z.failures() {
                            println!("       zvcs : {line}");
                        }
                        println!(
                            "       zvcs : {} exited 0, {} of those landed, {} queued",
                            z.exited_ok, z.exited_ok_landed, z.queued
                        );
                    }
                    if let Some(s) = &outcome.stock {
                        println!(
                            "       stock: {} exited 0, {} of those landed — invariant held",
                            s.exited_ok, s.exited_ok_landed
                        );
                    }
                }
            }
        }
        if cases.is_empty() {
            // "No defect found" over an empty corpus is the harness lying by
            // omission: --only filters the concurrent cases as well, so a run
            // restricted to commands with none would otherwise report a clean
            // concurrency result having measured nothing at all.
            println!("  no concurrent cases selected by --only — nothing was measured");
        } else if concurrency_defects == 0 {
            println!(
                "  no defect found in {} case(s). A race reproduces on some runs and not others, \
                 so this is\n  evidence, not proof — re-run to accumulate it.",
                cases.len()
            );
        }
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
            // Neither does a case nothing could measure, or one zvcs does not
            // reproduce: shrinking searches for the smallest case that still
            // fails, and a predicate that answers from a coin flip walks to
            // whichever argv flaked next and prints it as the culprit.
            if !f.verdict.is_measured_failure() {
                continue;
            }
            // A sequence step cannot be shrunk by this shrinker. Its failure is a
            // function of the steps that ran before it, and `fuzz::shrink` re-runs
            // one `Case` against a pristine copy — which for a step means running
            // `cherry-pick --continue` with no cherry-pick in progress, a
            // predicate that answers "still fails" for reasons that have nothing
            // to do with the token it just dropped. It would print a confident
            // minimal case that never reproduced anything, which is exactly the
            // failure mode `is_measured_failure` exists to prevent above.
            if f.step.is_some() {
                continue;
            }
            let minimal = fuzz::shrink(&f.case, &mut |c| {
                run_case(c, &zvcs_bin, &templates, &shrink_dir)
                    // `is_measured_failure`, not `!is_match`: a re-run that timed
                    // out its own oracle, or that zvcs answered differently this
                    // time, is not evidence the dropped argument mattered.
                    .map(|o| o.verdict.is_measured_failure())
                    .unwrap_or(false)
            });
            // Compared on `size()`, not on argv length: the shrinker also drops
            // config keys, global options, environment variables, the working
            // directory and the stdin payload, and a run that peeled four config
            // keys off a one-flag case reduced nothing by the old measure.
            if minimal.size() < f.case.size() {
                // Both ids in full. The minimal case's argv alone would not say
                // which environment or working directory survived, and those are
                // now as likely to be the responsible fact as a flag is.
                println!("  {} → {}", f.case.id(), minimal.id());
            }
        }
    }

    if args.keep {
        eprintln!("\nkept workdir: {}", root.display());
    } else {
        let _ = std::fs::remove_dir_all(&root);
    }

    // Non-zero when anything failed, so CI can gate on it. A concurrency defect
    // counts: the port reporting success for a write it did not perform is not a
    // softer failure than a stdout diff, it is a harder one — the caller cannot
    // detect it and cannot retry it.
    Ok(if rep.overall.matched == rep.overall.total() && concurrency_defects == 0 {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    })
}
