//! Developer tasks. `cargo xtask perf <command>` is the one entry point of
//! the performance framework (issue #110); `docs/performance.md` is the
//! guide.
//!
//! ```text
//! cargo xtask perf list                      the suites
//! cargo xtask perf bench [--quick] [--suite S]... [--filter RE] [--out FILE]
//! cargo xtask perf compare <base> [<head>]   result files or git revisions
//! cargo xtask perf check [--metrics FILE]    gate on perf/baseline.json
//! cargo xtask perf bless                     rewrite perf/baseline.json
//! cargo xtask perf flamegraph <target> [-- args]
//! cargo xtask perf stat <target> [-- args]
//! ```
//!
//! The xtask never links the game; it drives `cargo`, criterion, the
//! `perf-metrics` binary and Linux `perf`.

mod bench;
mod check;
mod compare;
mod profile;
mod util;

use std::process::ExitCode;

const USAGE: &str = "\
usage: cargo xtask perf <command> [options]

commands:
  list                         list the benchmark suites
  bench [options]              run suites, write <target>/perf/<label>.json
      --quick                  criterion --quick, smallest cases only (PERF_QUICK=1)
      --suite <name>           only this suite (repeatable)
      --filter <regex>         criterion filter (benchmark id regex)
      --label <name>           result name (default: the date and git revision)
      --out <file>             also write the result here
      --no-metrics             skip the deterministic metrics
  compare <base> [<head>]      compare two result files or git revisions
                               (head defaults to the latest result for a
                               file base, to the working tree for a revision)
      --quick, --suite, --filter   as for bench, when running revisions
      --noise <pct>            changes within this are noise (default 2)
  check [options]              compare metrics with perf/baseline.json, fail
                               beyond the thresholds
      --metrics <file>         use these metrics instead of measuring
      --baseline <file>        another baseline file
  bless [--instructions]       measure and rewrite perf/baseline.json
                               (--instructions: also gate perf-stat counts)
  flamegraph <target> [-- args]  perf record -g, SVG under <target>/perf/
  stat <target> [-- args]        perf stat hardware counters
      targets: example:<name> (game example), bench:<suite>[/<filter>],
               bin:perf-metrics
      --call-graph <fp|dwarf|lbr>   (flamegraph; default fp)
      --freq <hz>                   (flamegraph; default 997)
";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.first().map(String::as_str) {
        Some("perf") => perf(&args[1..]),
        _ => Err(format!("{USAGE}\n(only `perf` tasks exist)")),
    };
    match result {
        Ok(code) => code,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::from(1)
        }
    }
}

fn perf(args: &[String]) -> Result<ExitCode, String> {
    let rest = args.get(1..).unwrap_or_default();
    match args.first().map(String::as_str) {
        Some("list") => {
            for s in bench::SUITES {
                println!("{:<8} {:<10} {}", s.name, s.package, s.about);
            }
            Ok(ExitCode::SUCCESS)
        }
        Some("bench") => bench::cmd(rest),
        Some("compare") => compare::cmd(rest),
        Some("check") => check::check(rest),
        Some("bless") => check::bless(rest),
        Some("flamegraph") => profile::flamegraph(rest),
        Some("stat") => profile::stat(rest),
        Some("help" | "--help" | "-h") | None => {
            print!("{USAGE}");
            Ok(ExitCode::SUCCESS)
        }
        Some(other) => Err(format!("unknown command {other:?}\n\n{USAGE}")),
    }
}
