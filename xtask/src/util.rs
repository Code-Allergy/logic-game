//! Paths, processes and small helpers shared by the commands.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// The checkout this xtask was built from.
pub fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask sits in the repository")
        .to_path_buf()
}

/// Cargo's target directory: `$CARGO_TARGET_DIR`, or `<repo>/target`.
pub fn target_dir() -> PathBuf {
    match std::env::var_os("CARGO_TARGET_DIR") {
        Some(dir) => {
            let dir = PathBuf::from(dir);
            if dir.is_absolute() {
                dir
            } else {
                repo_root().join(dir)
            }
        }
        None => repo_root().join("target"),
    }
}

/// Where results, worktrees and profiles go: `<target>/perf`.
pub fn perf_dir() -> PathBuf {
    target_dir().join("perf")
}

/// `cargo`, as the one running us (or from PATH), building into the
/// shared target directory from `dir`.
pub fn cargo(dir: &Path) -> Command {
    let mut c = Command::new(std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into()));
    c.current_dir(dir).env("CARGO_TARGET_DIR", target_dir());
    c
}

/// Run `cmd`, inheriting stdio; an error unless it succeeds.
pub fn run(cmd: &mut Command) -> Result<(), String> {
    let status = cmd
        .status()
        .map_err(|e| format!("cannot run {cmd:?}: {e}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("{cmd:?} failed ({status})"))
    }
}

/// Run `cmd` and return its stdout (stderr passes through).
pub fn output(cmd: &mut Command) -> Result<String, String> {
    let out = cmd
        .stderr(Stdio::inherit())
        .output()
        .map_err(|e| format!("cannot run {cmd:?}: {e}"))?;
    if !out.status.success() {
        return Err(format!("{cmd:?} failed ({})", out.status));
    }
    String::from_utf8(out.stdout).map_err(|e| format!("{cmd:?}: {e}"))
}

/// `git <args>` in `dir`, trimmed stdout.
pub fn git(dir: &Path, args: &[&str]) -> Result<String, String> {
    output(Command::new("git").current_dir(dir).args(args)).map(|s| s.trim().to_string())
}

/// The commit checked out in `dir`, and whether the tree has changes.
pub fn git_state(dir: &Path) -> (Option<String>, bool) {
    let rev = git(dir, &["rev-parse", "HEAD"]).ok();
    let dirty = git(dir, &["status", "--porcelain", "--untracked-files=no"])
        .map(|s| !s.is_empty())
        .unwrap_or(false);
    (rev, dirty)
}

/// Seconds since the epoch.
pub fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// `perf` on PATH (or `$PERF`), if it runs.
pub fn find_perf() -> Option<PathBuf> {
    let cand = std::env::var_os("PERF").map_or_else(|| PathBuf::from("perf"), PathBuf::from);
    let ok = Command::new(&cand)
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success());
    ok.then_some(cand)
}

/// Why `perf` is missing and how to get it.
pub const PERF_HINT: &str = "\
Linux `perf` was not found on PATH (set $PERF to use another binary).
Get it from the repository's nix dev shell:
    nix develop                                   # on a normal checkout
    nix shell --inputs-from . nixpkgs#perf        # just perf
  (in a git worktree, use --inputs-from path:$PWD)
or from your distribution: `linux-tools-common linux-tools-$(uname -r)`
(Debian/Ubuntu), `perf` (Fedora, Arch). See docs/performance.md.";

/// Read and parse a JSON file.
pub fn read_json(path: &Path) -> Result<serde_json::Value, String> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))
}

/// Write `value` as pretty JSON, creating parent directories.
pub fn write_json(path: &Path, value: &serde_json::Value) -> Result<(), String> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    let text = serde_json::to_string_pretty(value).expect("serializable") + "\n";
    std::fs::write(path, text).map_err(|e| format!("cannot write {}: {e}", path.display()))
}

/// A duration in nanoseconds, with a unit that keeps it readable.
pub fn fmt_ns(ns: f64) -> String {
    if ns < 1e3 {
        format!("{ns:.2} ns")
    } else if ns < 1e6 {
        format!("{:.2} µs", ns / 1e3)
    } else if ns < 1e9 {
        format!("{:.2} ms", ns / 1e6)
    } else {
        format!("{:.2} s", ns / 1e9)
    }
}

/// Minimal option parsing: flags and `--name value` options, the rest
/// positional; everything after `--` is passed through.
#[derive(Debug, Default)]
pub struct Args {
    pub flags: Vec<String>,
    pub opts: Vec<(String, String)>,
    pub positional: Vec<String>,
    pub passthrough: Vec<String>,
}

impl Args {
    /// `takes_value` names the options that take a value.
    pub fn parse(args: &[String], takes_value: &[&str], flags: &[&str]) -> Result<Self, String> {
        let mut out = Args::default();
        let mut it = args.iter();
        while let Some(a) = it.next() {
            if a == "--" {
                out.passthrough = it.cloned().collect();
                break;
            }
            if let Some(name) = a.strip_prefix("--") {
                if takes_value.contains(&name) {
                    let v = it.next().ok_or(format!("--{name} needs a value"))?;
                    out.opts.push((name.to_string(), v.clone()));
                } else if flags.contains(&name) {
                    out.flags.push(name.to_string());
                } else {
                    return Err(format!("unknown option --{name}"));
                }
            } else {
                out.positional.push(a.clone());
            }
        }
        Ok(out)
    }

    pub fn flag(&self, name: &str) -> bool {
        self.flags.iter().any(|f| f == name)
    }

    pub fn opt(&self, name: &str) -> Option<&str> {
        self.opts
            .iter()
            .rev()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v.as_str())
    }

    pub fn all(&self, name: &str) -> Vec<String> {
        self.opts
            .iter()
            .filter(|(n, _)| n == name)
            .map(|(_, v)| v.clone())
            .collect()
    }
}
