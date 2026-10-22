// Copyright 2026 the kurbo-se Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Golden-output geometric regression suite for kurbo-se.
//!
//! Every case of the corpus is stroked twice: with a pinned reference
//! revision, `kurbo-se-ref`, and with the working tree, `kurbo-se`. The
//! candidate's output must paint the same region as the reference's,
//! within the case's tolerance. README.md describes the contract.

mod compare;
mod corpus;
mod glyphs;
mod perturb;

use std::cell::RefCell;
use std::fmt::Write as _;
use std::io::Write as _;
use std::panic::AssertUnwindSafe;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use kurbo::BezPath;

use crate::compare::{Contract, Outcome, Verdict, compare};
use crate::corpus::{Case, Corpus};

const USAGE: &str = "\
usage: kurbo-se-regress [options]

  --filter SUBSTR    run only cases whose name contains SUBSTR (repeatable;
                     a case matching any filter runs)
  --threads N        worker threads (default: 4, at most the cores there are;
                     each worker can hold a few hundred MB on the largest cases)
  --report FILE.tsv  write one row per case
  --delta-scale X    allowed boundary distance δ = X · tol (default 1)
  --list             print the selected case names and exit
  --top N            list the N worst / slowest cases (default 10)
  --geometric-all    run the geometric check on identical outputs too
                     (a comparator self-check; identical outputs must pass)
  --perturb KIND     compare each reference output against a synthetic edit
                     of itself instead of the candidate's output, to
                     validate the comparator over the corpus. KIND is one of
                     reorder, reverse, split, degenerate, jitter, reflatten
                     (must pass) or translate, scale (must fail)
  --trace            print each case's name before comparing it, to find
                     a case that hangs, crashes or uses too much memory
  --dump DIR         write an SVG overlay of the reference and candidate
                     outlines for every failing case into DIR: NAME.svg,
                     and NAME.zoom.svg around where they differ most

environment:
  KURBO_SE_REGRESS_FONT  font for the glyph cases (default: the sandbox's
                         Inter, ../../sandbox/wasm/fonts/Inter-Regular.ttf)";

/// Command-line options.
struct Options {
    filters: Vec<String>,
    threads: usize,
    report: Option<PathBuf>,
    delta_scale: f64,
    list: bool,
    top: usize,
    geometric_all: bool,
    perturb: Option<String>,
    dump: Option<PathBuf>,
    trace: bool,
}

fn parse_args() -> Result<Options, String> {
    let mut opts = Options {
        filters: Vec::new(),
        threads: std::thread::available_parallelism().map_or(1, |n| n.get().min(4)),
        report: None,
        delta_scale: 1.0,
        list: false,
        top: 10,
        geometric_all: false,
        perturb: None,
        dump: None,
        trace: false,
    };
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        let mut value = |name: &str| args.next().ok_or_else(|| format!("{name} needs a value"));
        match arg.as_str() {
            "--filter" => opts.filters.push(value("--filter")?),
            "--threads" => {
                opts.threads = value("--threads")?
                    .parse()
                    .ok()
                    .filter(|&n| n > 0)
                    .ok_or("--threads needs a positive integer")?;
            }
            "--report" => opts.report = Some(PathBuf::from(value("--report")?)),
            "--delta-scale" => {
                opts.delta_scale = value("--delta-scale")?
                    .parse()
                    .ok()
                    .filter(|&x: &f64| x.is_finite() && x > 0.0)
                    .ok_or("--delta-scale needs a positive number")?;
            }
            "--list" => opts.list = true,
            "--top" => {
                opts.top = value("--top")?
                    .parse()
                    .map_err(|_| "--top needs a non-negative integer")?;
            }
            "--geometric-all" => opts.geometric_all = true,
            "--perturb" => {
                let kind = value("--perturb")?;
                if perturb::named(&kind).is_none() {
                    return Err(format!(
                        "unknown perturbation {kind:?}; one of {}",
                        perturb::NAMES.join(", ")
                    ));
                }
                opts.perturb = Some(kind);
            }
            "--dump" => opts.dump = Some(PathBuf::from(value("--dump")?)),
            "--trace" => opts.trace = true,
            "-h" | "--help" => return Err(String::new()),
            other => return Err(format!("unknown argument {other:?}")),
        }
    }
    Ok(opts)
}

thread_local! {
    /// The message of this thread's last panic, recorded by the panic hook.
    static LAST_PANIC: RefCell<Option<String>> = const { RefCell::new(None) };
}

/// Run `f`, turning a panic into its message.
fn guarded<T>(f: impl FnOnce() -> T) -> Result<T, String> {
    LAST_PANIC.with(|m| m.borrow_mut().take());
    std::panic::catch_unwind(AssertUnwindSafe(f)).map_err(|_| {
        LAST_PANIC
            .with(|m| m.borrow_mut().take())
            .unwrap_or_else(|| "panic without message".to_string())
            .replace(['\n', '\t'], " ")
    })
}

/// Per-thread stroke contexts, reused across cases the way an animation
/// loop reuses one. State that leaks from one call into the next then
/// shows up as a difference.
struct Contexts {
    reference: kurbo_se_ref::AlignedStrokeCtx,
    candidate: kurbo_se::AlignedStrokeCtx,
}

/// Everything recorded about one case.
struct CaseResult {
    outcome: Outcome,
    elements_ref: Option<usize>,
    elements_cand: Option<usize>,
    time_ref: Duration,
    time_cand: Duration,
    time_compare: Duration,
}

fn run_case(index: usize, case: &Case, shape: &BezPath, ctx: &mut Contexts, opts: &Options) -> CaseResult {
    let tol = case.tol;
    let (style_ref, style_cand) = (case.style.reference(), case.style.candidate());
    let stroke_ref = |ctx: &mut Contexts| {
        guarded(|| {
            let start = Instant::now();
            let out = kurbo_se_ref::stroke_aligned_with(shape, &style_ref, tol, &mut ctx.reference);
            let elapsed = start.elapsed();
            (out.clone(), elapsed)
        })
        .inspect_err(|_| ctx.reference = kurbo_se_ref::AlignedStrokeCtx::new())
    };
    let stroke_cand = |ctx: &mut Contexts| {
        guarded(|| {
            let start = Instant::now();
            let out = kurbo_se::stroke_aligned_with(shape, &style_cand, tol, &mut ctx.candidate);
            let elapsed = start.elapsed();
            (out.clone(), elapsed)
        })
        .inspect_err(|_| ctx.candidate = kurbo_se::AlignedStrokeCtx::new())
    };
    // Alternate the order so neither version always runs with warm caches.
    let (reference, candidate) = if index % 2 == 0 {
        let r = stroke_ref(ctx);
        (r, stroke_cand(ctx))
    } else {
        let c = stroke_cand(ctx);
        (stroke_ref(ctx), c)
    };

    let mut result = CaseResult {
        outcome: Outcome::failed(""),
        elements_ref: reference.as_ref().ok().map(|(p, _)| p.elements().len()),
        elements_cand: candidate.as_ref().ok().map(|(p, _)| p.elements().len()),
        time_ref: reference.as_ref().map_or(Duration::ZERO, |r| r.1),
        time_cand: candidate.as_ref().map_or(Duration::ZERO, |c| c.1),
        time_compare: Duration::ZERO,
    };
    let contract = Contract {
        tol,
        delta_scale: opts.delta_scale,
    };
    result.outcome = match (reference, candidate) {
        (_, Err(msg)) => Outcome::failed(format!("candidate panicked: {msg}")),
        (Err(msg), Ok(_)) => Outcome::failed(format!("reference panicked: {msg}")),
        (Ok((a, _)), Ok((b, _))) => {
            let b = match opts.perturb.as_deref().and_then(perturb::named) {
                Some((edit, _)) => edit(&a, tol),
                None => b,
            };
            let start = Instant::now();
            if opts.trace {
                eprintln!("comparing {}", case.name);
            }
            let outcome = guarded(|| compare(&a, &b, contract, opts.geometric_all))
                .unwrap_or_else(|msg| Outcome::failed(format!("comparator panicked: {msg}")));
            result.time_compare = start.elapsed();
            if let (Verdict::Fail, Some(dir)) = (outcome.verdict, &opts.dump) {
                if let Err(e) = dump_svg(dir, &case.name, &a, &b, outcome.at, contract.delta()) {
                    eprintln!("warning: cannot dump {}: {e}", case.name);
                }
            }
            outcome
        }
    };
    result
}

/// Write `NAME.svg` with both outlines whole. When the comparison located
/// the difference, also write `NAME.zoom.svg`, a window `80 δ` wide around
/// it. The reference fills blue and the candidate red, both translucent
/// under the nonzero rule, so where they agree it reads purple.
fn dump_svg(
    dir: &std::path::Path,
    name: &str,
    reference: &BezPath,
    candidate: &BezPath,
    at: Option<kurbo::Point>,
    delta: f64,
) -> std::io::Result<()> {
    use kurbo::{Rect, Shape};
    std::fs::create_dir_all(dir)?;
    let file = name.replace(['/', '\\', ':'], "_");
    let render = |view: Rect| {
        let pad = 0.02 * view.width().max(view.height());
        let v = view.inflate(pad, pad);
        let mut svg = format!(
            "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"{} {} {} {}\" width=\"900\" height=\"{:.0}\">\n\
             <rect x=\"{}\" y=\"{}\" width=\"{}\" height=\"{}\" fill=\"white\"/>\n",
            v.x0,
            v.y0,
            v.width(),
            v.height(),
            900.0 * v.height() / v.width().max(1e-300),
            v.x0,
            v.y0,
            v.width(),
            v.height(),
        );
        for (path, color) in [(reference, "0,90,255"), (candidate, "230,30,30")] {
            let _ = writeln!(
                svg,
                "<path d=\"{}\" fill=\"rgba({color},0.45)\" fill-rule=\"nonzero\" \
                 stroke=\"rgb({color})\" stroke-width=\"0.6\" vector-effect=\"non-scaling-stroke\"/>",
                path.to_svg()
            );
        }
        if let Some(p) = at {
            let _ = writeln!(
                svg,
                "<circle cx=\"{}\" cy=\"{}\" r=\"{}\" fill=\"none\" stroke=\"black\" \
                 stroke-width=\"1\" vector-effect=\"non-scaling-stroke\"/>",
                p.x, p.y, delta
            );
        }
        svg + "</svg>\n"
    };
    let whole = reference.bounding_box().union(candidate.bounding_box());
    std::fs::write(dir.join(format!("{file}.svg")), render(whole))?;
    if let Some(p) = at {
        let window = Rect::from_center_size(p, (80.0 * delta, 80.0 * delta));
        std::fs::write(dir.join(format!("{file}.zoom.svg")), render(window))?;
    }
    Ok(())
}

fn main() -> ExitCode {
    let opts = match parse_args() {
        Ok(o) => o,
        Err(msg) => {
            if !msg.is_empty() {
                eprintln!("error: {msg}\n");
            }
            eprintln!("{USAGE}");
            return ExitCode::from(2);
        }
    };
    std::panic::set_hook(Box::new(|info| {
        let msg = info.to_string();
        LAST_PANIC.with(|m| *m.borrow_mut() = Some(msg));
    }));

    let wall = Instant::now();
    let corpus = Corpus::build();
    for w in &corpus.warnings {
        eprintln!("warning: {w}");
    }
    let selected: Vec<usize> = (0..corpus.cases.len())
        .filter(|&i| {
            opts.filters.is_empty() || opts.filters.iter().any(|f| corpus.cases[i].name.contains(f.as_str()))
        })
        .collect();
    if opts.list {
        let mut out = std::io::stdout().lock();
        for &i in &selected {
            let _ = writeln!(out, "{}", corpus.cases[i].name);
        }
        return ExitCode::SUCCESS;
    }
    if selected.is_empty() {
        eprintln!("no cases selected");
        return ExitCode::from(2);
    }

    let results = run_all(&corpus, &selected, &opts);
    let elapsed = wall.elapsed();

    if let Some(path) = &opts.report {
        if let Err(e) = std::fs::write(path, report_tsv(&corpus, &selected, &results)) {
            eprintln!("error: cannot write {}: {e}", path.display());
            return ExitCode::from(2);
        }
    }
    let ok = print_summary(&corpus, &selected, &results, &opts, elapsed);
    for w in &corpus.warnings {
        println!("warning: {w}");
    }
    if ok { ExitCode::SUCCESS } else { ExitCode::from(1) }
}

/// Run the selected cases on `opts.threads` workers. A free worker takes
/// the next case instead of owning a fixed chunk, so long cases do not
/// hold up the rest. The results come back in the order of `selected`.
fn run_all(corpus: &Corpus, selected: &[usize], opts: &Options) -> Vec<CaseResult> {
    let next = AtomicUsize::new(0);
    let threads = opts.threads.min(selected.len());
    let mut done: Vec<(usize, CaseResult)> = std::thread::scope(|scope| {
        let workers: Vec<_> = (0..threads)
            .map(|_| {
                scope.spawn(|| {
                    let mut ctx = Contexts {
                        reference: kurbo_se_ref::AlignedStrokeCtx::new(),
                        candidate: kurbo_se::AlignedStrokeCtx::new(),
                    };
                    let mut out = Vec::new();
                    loop {
                        let k = next.fetch_add(1, Ordering::Relaxed);
                        let Some(&i) = selected.get(k) else {
                            break;
                        };
                        let case = &corpus.cases[i];
                        let shape = &corpus.shapes[case.shape].path;
                        out.push((k, run_case(i, case, shape, &mut ctx, opts)));
                    }
                    out
                })
            })
            .collect();
        workers
            .into_iter()
            .flat_map(|w| w.join().expect("worker panicked outside a guarded call"))
            .collect()
    });
    done.sort_by_key(|(k, _)| *k);
    done.into_iter().map(|(_, r)| r).collect()
}

/// A number for the report, `-` when not computed.
fn cell(v: f64) -> String {
    if v.is_nan() { "-".to_string() } else { format!("{v:.6}") }
}

fn report_tsv(corpus: &Corpus, selected: &[usize], results: &[CaseResult]) -> String {
    let mut s = String::from(
        "name\tverdict\th_over_tol\tsymdiff\tarea_ref\tarea_cand\telements_ref\telements_cand\tref_us\tcand_us\tcompare_us\treason\n",
    );
    let count = |n: Option<usize>| n.map_or("-".to_string(), |n| n.to_string());
    for (&i, r) in selected.iter().zip(results) {
        let case = &corpus.cases[i];
        let o = &r.outcome;
        let _ = writeln!(
            s,
            "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{:.1}\t{:.1}\t{:.1}\t{}",
            case.name,
            o.verdict.label(),
            cell(o.hausdorff / case.tol),
            cell(o.symdiff),
            cell(o.area_ref),
            cell(o.area_cand),
            count(r.elements_ref),
            count(r.elements_cand),
            r.time_ref.as_secs_f64() * 1e6,
            r.time_cand.as_secs_f64() * 1e6,
            r.time_compare.as_secs_f64() * 1e6,
            o.reason.replace(['\t', '\n'], " "),
        );
    }
    s
}

/// Print the summary and return whether the run passed.
fn print_summary(
    corpus: &Corpus,
    selected: &[usize],
    results: &[CaseResult],
    opts: &Options,
    elapsed: Duration,
) -> bool {
    let cases = || selected.iter().map(|&i| &corpus.cases[i]).zip(results);
    let count = |v: Verdict| results.iter().filter(|r| r.outcome.verdict == v).count();
    let mut by_tol: Vec<(String, usize)> = Vec::new();
    for (case, _) in cases() {
        let t = corpus::num(case.tol);
        match by_tol.iter_mut().find(|(k, _)| *k == t) {
            Some((_, n)) => *n += 1,
            None => by_tol.push((t, 1)),
        }
    }
    let tols: Vec<String> = by_tol.iter().map(|(t, n)| format!("{n} at tol {t}")).collect();
    println!(
        "kurbo-se regression: {} cases ({}), {} threads, δ = {} × tol",
        selected.len(),
        tols.join(", "),
        opts.threads,
        opts.delta_scale
    );
    // Written by `setup-ref.sh` next to the reference tree it extracts.
    let ref_rev = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../target/regress-ref/REGRESS_REF"
    ));
    println!(
        "reference revision: {}",
        ref_rev.as_deref().map_or("unknown (not set up by setup-ref.sh)", str::trim)
    );
    if let Some(kind) = &opts.perturb {
        println!("comparator validation: candidate = reference edited by `{kind}`");
    }
    if opts.geometric_all {
        println!("geometric check forced on identical outputs");
    }
    println!("  IDENTICAL   {}", count(Verdict::Identical));
    println!("  EQUIVALENT  {}", count(Verdict::Equivalent));
    println!("  FAIL        {}", count(Verdict::Fail));

    let mut equivalent: Vec<_> = cases()
        .filter(|(_, r)| r.outcome.verdict == Verdict::Equivalent && !r.outcome.hausdorff.is_nan())
        .collect();
    equivalent.sort_by(|a, b| {
        (b.1.outcome.hausdorff / b.0.tol).total_cmp(&(a.1.outcome.hausdorff / a.0.tol))
    });
    if !equivalent.is_empty() && opts.top > 0 {
        println!("worst H/tol among EQUIVALENT:");
        for (case, r) in equivalent.iter().take(opts.top) {
            println!(
                "  {:7.4}  {}  (symdiff {:.3e})",
                r.outcome.hausdorff / case.tol,
                case.name,
                r.outcome.symdiff
            );
        }
    }

    let failures: Vec<_> = cases().filter(|(_, r)| r.outcome.verdict == Verdict::Fail).collect();
    if !failures.is_empty() {
        println!("FAIL:");
        for (case, r) in &failures {
            println!("  {}: {}", case.name, r.outcome.reason);
        }
    }

    let total_ref: Duration = results.iter().map(|r| r.time_ref).sum();
    let total_cand: Duration = results.iter().map(|r| r.time_cand).sum();
    let total_compare: Duration = results.iter().map(|r| r.time_compare).sum();
    println!(
        "stroke time (informative; use the benchmarks to measure): reference {:.3} s, candidate {:.3} s, candidate/reference {:.3}",
        total_ref.as_secs_f64(),
        total_cand.as_secs_f64(),
        total_cand.as_secs_f64() / total_ref.as_secs_f64().max(1e-12)
    );
    println!("comparison time {:.3} s", total_compare.as_secs_f64());
    if opts.top > 0 {
        let mut slow: Vec<_> = cases().collect();
        slow.sort_by(|a, b| (b.1.time_ref + b.1.time_cand).cmp(&(a.1.time_ref + a.1.time_cand)));
        println!("slowest cases (reference / candidate stroke):");
        for (case, r) in slow.iter().take(opts.top) {
            println!(
                "  {:9.3} ms / {:9.3} ms  {}",
                r.time_ref.as_secs_f64() * 1e3,
                r.time_cand.as_secs_f64() * 1e3,
                case.name
            );
        }
    }
    println!("wall time {:.2} s", elapsed.as_secs_f64());

    let mut ok = failures.is_empty();
    if let Some(kind) = &opts.perturb {
        let must_pass = perturb::named(kind).is_some_and(|(_, pass)| pass);
        // An edit of an empty outline is still empty, so it cannot fail.
        let contradicted: Vec<_> = cases()
            .filter(|(_, r)| {
                let failed = r.outcome.verdict == Verdict::Fail;
                let empty = r.outcome.area_ref == 0.0 || r.elements_ref == Some(0);
                if must_pass { failed } else { !failed && !empty }
            })
            .collect();
        println!(
            "`{kind}` must {}: {} of {} cases contradict it",
            if must_pass { "pass" } else { "fail" },
            contradicted.len(),
            results.len()
        );
        if !must_pass {
            for (case, r) in contradicted.iter().take(opts.top.max(10)) {
                println!(
                    "  {}  H/tol {:.3}, symdiff {:.3e}, area {:.3e}",
                    case.name,
                    r.outcome.hausdorff / case.tol,
                    r.outcome.symdiff,
                    r.outcome.area_ref
                );
            }
        }
        ok = contradicted.is_empty();
    }
    println!("{}", if ok { "PASS" } else { "FAILED" });
    ok
}
