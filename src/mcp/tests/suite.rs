//! Tests of the native suite runner (`crate::mcp::suite`): Harte-shaped
//! suites on a small machine built here and on the gallery's 16-bit CPUs
//! (gates and words), a nestest-style log of the gallery CPU, and the
//! trap and status protocols of `klaus` and `blargg` on purpose-built
//! circuits.

use std::path::{Path, PathBuf};

use serde_json::{Map, Value, json};

use super::{edit, empty, scratch};
use crate::mcp::batch::Place;
use crate::mcp::rig::Rig;
use crate::mcp::run::{self, RunState};
use crate::mcp::suite::{self, Body, Config, Report, RunOptions};
use crate::sim::demos::catalog;
use crate::sim::{GateKind, WorkspaceDocument};

// ── Helpers ──────────────────────────────────────────────────────────────

/// A small seeded generator (xorshift64*).
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

fn write_json(path: &Path, value: &Value) {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).unwrap();
    }
    std::fs::write(path, serde_json::to_string(value).unwrap()).unwrap();
}

fn options(jobs: usize) -> RunOptions {
    RunOptions {
        jobs,
        ..RunOptions::default()
    }
}

fn run_suite(ws: &WorkspaceDocument, dir: &Path, config: Value, options: &RunOptions) -> Report {
    let config = Config::parse(&config, dir).unwrap_or_else(|e| panic!("{e}"));
    suite::run(ws, &config, options, ("workspace", "config")).unwrap_or_else(|e| panic!("{e}"))
}

fn run_error(ws: &WorkspaceDocument, dir: &Path, config: Value) -> String {
    let config = match Config::parse(&config, dir) {
        Ok(c) => c,
        Err(e) => return e,
    };
    match suite::run(ws, &config, &options(1), ("workspace", "config")) {
        Ok(r) => panic!("the suite should not run: {}", r.summary()),
        Err(e) => e,
    }
}

/// The report without what depends on time or on the number of jobs.
fn stable(report: &Report) -> Value {
    let mut v = report.json();
    for k in ["seconds", "cases_per_second", "ticks_per_second", "jobs"] {
        v.as_object_mut().unwrap().remove(k);
    }
    v
}

fn harte_files(report: &Report) -> &[suite::FileResult] {
    match &report.body {
        Body::Harte { files, .. } => files,
        Body::Program(_) => panic!("not a harte report"),
    }
}

fn program_result(report: &Report) -> &suite::ProgramResult {
    match &report.body {
        Body::Program(p) => p,
        Body::Harte { .. } => panic!("not a program report"),
    }
}

/// One Harte case as JSON.
fn case(
    name: &str,
    init: &[(&str, u64)],
    ram: &[(u64, u64)],
    fin: &[(&str, u64)],
    fin_ram: &[(u64, u64)],
    cycles: &[(u64, u64, &str)],
) -> Value {
    let state = |regs: &[(&str, u64)], ram: &[(u64, u64)]| {
        let mut m: Map<String, Value> = regs
            .iter()
            .map(|(n, v)| (n.to_string(), json!(v)))
            .collect();
        m.insert(
            "ram".into(),
            json!(ram.iter().map(|(a, v)| json!([a, v])).collect::<Vec<_>>()),
        );
        Value::Object(m)
    };
    json!({
        "name": name,
        "initial": state(init, ram),
        "final": state(fin, fin_ram),
        "cycles": cycles.iter().map(|(a, d, rw)| json!([a, d, rw])).collect::<Vec<_>>(),
    })
}

// ── A small machine: one instruction per tick, one bus cycle each ────────

/// At `pc`, opcode 1 adds the byte at the operand's address to `a`
/// (carry into `c`), opcode 2 stores `a` there, anything else halts; `pc`
/// moves on by 2. The operand's address and the byte read or written are
/// the bus (`is_st` is 1 on a write).
fn accumulator() -> WorkspaceDocument {
    let (ws, report) = edit(
        &empty(),
        json!({"ops": [
            {"add": [
                {"name": "pc", "type": "Register", "w": 8},
                {"name": "a", "type": "Register", "w": 8},
                {"name": "c", "type": "Register"},
                {"name": "mem", "type": "Ram2", "addr_bits": 8, "w": 8, "reads": 3},
                {"name": "one", "type": "Const", "w": 8, "value": 1},
                {"name": "two", "type": "Const", "w": 8, "value": 2},
                {"name": "next", "type": "Add", "w": 8},
                {"name": "oper", "type": "Add", "w": 8},
                {"name": "is_add", "type": "Eq", "w": 8},
                {"name": "is_st", "type": "Eq", "w": 8},
                {"name": "go", "type": "Or"},
                {"name": "sum", "type": "Add", "w": 8},
                {"name": "A", "type": "Output", "w": 8},
                {"name": "PC", "type": "Output", "w": 8}
            ]},
            {"wire": [
                ["pc", "mem.raddr0", "oper.a", "next.a", "PC"],
                ["one", "oper.b", "is_add.b"],
                ["two", "next.b", "is_st.b"],
                ["oper.sum", "mem.raddr1"],
                ["mem.rdata1", "mem.raddr2", "mem.waddr"],
                ["mem.rdata0", "is_add.a", "is_st.a"],
                ["is_add", "go.a", "a.en", "c.en"], ["is_st", "go.b", "mem.we"],
                ["go", "pc.en"], ["next.sum", "pc.d"],
                ["a", "sum.a", "mem.wdata", "A"], ["mem.rdata2", "sum.b"],
                ["sum.sum", "a.d"], ["sum.cout", "c.d"]
            ]}
        ]}),
    );
    assert!(report.build.is_ok(), "{report}");
    ws
}

fn accumulator_config() -> Value {
    json!({
        "kind": "harte",
        "cases": "cases",
        "chunk": 40,
        "signals": {"operand": "mem.rdata1", "read": "mem.rdata2", "wrote": "a", "store": "is_st"},
        "registers": {"pc": "pc", "a": "a", "c": "c"},
        "memory": "mem",
        "bus": {"addr": "operand", "read_data": "read", "write_data": "wrote", "rw": "store",
                "read_value": 0},
        "timing": {"ticks_per_cycle": 1, "sample": 0}
    })
}

/// A random add (`op` 1) or store (`op` 2), as the machine runs it.
fn accumulator_case(rng: &mut Rng, op: u64, i: usize) -> Value {
    let pc = rng.below(0x40) * 2;
    let operand = 0x80 + rng.below(0x80);
    let (a, c, m) = (rng.below(256), rng.below(2), rng.below(256));
    let ram = [(pc, op), (pc + 1, operand), (operand, m)];
    if op == 1 {
        let s = a + m;
        case(
            &format!("add {i}"),
            &[("pc", pc), ("a", a), ("c", c)],
            &ram,
            &[("pc", pc + 2), ("a", s & 0xFF), ("c", s >> 8)],
            &ram,
            &[(operand, m, "read")],
        )
    } else {
        let mut fin = ram;
        fin[2].1 = a;
        case(
            &format!("store {i}"),
            &[("pc", pc), ("a", a), ("c", c)],
            &ram,
            &[("pc", pc + 2), ("a", a), ("c", c)],
            &fin,
            &[(operand, a, "write")],
        )
    }
}

fn accumulator_suite(dir: &Path, per_file: usize) {
    let mut rng = Rng(0x5EED);
    for (file, op) in [("01.json", 1), ("02.json", 2)] {
        let cases: Vec<Value> = (0..per_file)
            .map(|i| accumulator_case(&mut rng, op, i))
            .collect();
        write_json(&dir.join("cases").join(file), &json!(cases));
    }
}

#[test]
fn a_harte_suite_passes_with_the_same_report_on_one_and_four_jobs() {
    let ws = accumulator();
    let dir = scratch("suite-pass");
    accumulator_suite(&dir, 300);
    let one = run_suite(&ws, &dir, accumulator_config(), &options(1));
    assert!(one.ok(), "{}", one.markdown());
    assert_eq!(one.cases(), 600);
    let four = run_suite(&ws, &dir, accumulator_config(), &options(4));
    assert_eq!(four.jobs, 4);
    assert_eq!(stable(&one), stable(&four));
    let md = one.markdown();
    assert!(md.contains("| 01.json | 300 | 300 |"), "{md}");
    assert!(md.contains("compiled with the default options"), "{md}");
    let summary = one.summary();
    assert!(
        summary.starts_with("harte PASSED: 600 of 600 cases passed in 2 file(s)"),
        "{summary}"
    );
    assert!(summary.lines().count() <= 4, "{summary}");

    let unoptimized = run_suite(
        &ws,
        &dir,
        accumulator_config(),
        &RunOptions {
            unoptimized: true,
            ..options(2)
        },
    );
    assert!(unoptimized.ok(), "{}", unoptimized.markdown());
    assert!(
        unoptimized.summary().contains("--unoptimized"),
        "{}",
        unoptimized.summary()
    );
    assert_eq!(
        unoptimized.compile.used(),
        unoptimized.compile.plain_instructions
    );
}

#[test]
fn a_wrong_register_ram_byte_or_bus_cycle_is_reported_exactly() {
    let ws = accumulator();
    let dir = scratch("suite-fail");
    let good = |name: &str, fin_a: u64, fin_ram: u64, read: u64| {
        // pc 0x10: add [0x80] (0x22) to a = 0x30: a = 0x52.
        let ram = [(0x10, 1), (0x11, 0x80), (0x80, 0x22)];
        case(
            name,
            &[("pc", 0x10), ("a", 0x30), ("c", 1)],
            &ram,
            &[("pc", 0x12), ("a", fin_a), ("c", 0)],
            &[(0x10, 1), (0x11, 0x80), (0x80, fin_ram)],
            &[(0x80, read, "read")],
        )
    };
    write_json(
        &dir.join("cases/regs.json"),
        &json!([
            good("ok", 0x52, 0x22, 0x22),
            good("bad a", 0x53, 0x22, 0x22)
        ]),
    );
    write_json(
        &dir.join("cases/ram.json"),
        &json!([good("bad ram", 0x52, 0x21, 0x22)]),
    );
    write_json(
        &dir.join("cases/bus.json"),
        &json!([
            good("bad bus", 0x52, 0x22, 0x23),
            good("ok", 0x52, 0x22, 0x22)
        ]),
    );
    let report = run_suite(&ws, &dir, accumulator_config(), &options(2));
    assert!(!report.ok());
    let files = harte_files(&report);
    let by = |f: &str| files.iter().find(|r| r.file == f).unwrap();
    let only = |f: &str| {
        let r = by(f);
        assert_eq!(r.failed, 1, "{f}");
        r.failures[0].diffs.clone()
    };
    assert_eq!(only("regs.json"), vec!["a: expected 0x53, got 0x52"]);
    assert_eq!(by("regs.json").failures[0].index, 1);
    assert_eq!(
        only("ram.json"),
        vec!["ram[0x0080]: expected 0x21, got 0x22"]
    );
    assert_eq!(
        only("bus.json"),
        vec!["cycle 1: expected 0x0080 0x23 read, got 0x0080 0x22 read"]
    );
    let md = report.markdown();
    assert!(
        md.contains("| bus.json | 1 | 2 | case 0 `bad bus`: cycle 1"),
        "{md}"
    );
    assert!(md.contains("## regs.json: case 1 `bad a`"), "{md}");
    assert!(
        md.contains("| a | 0x30 | 0x53 | 0x52 **differs** |"),
        "{md}"
    );
    assert!(
        md.contains("| 0x0080 | 0x22 | 0x21 | 0x22 **differs** |"),
        "{md}"
    );
    assert!(
        md.contains("| 1 | 0x0080 0x23 read | 0x0080 0x22 read **differs** |"),
        "{md}"
    );
    let json = report.json();
    assert_eq!(json["failed"], 3);
    assert_eq!(
        json["files"][0]["first_failure"]["first_differing_cycle"], 1,
        "{json}"
    );
    let summary = report.summary();
    assert!(
        summary.starts_with("harte FAILED: 2 of 5 cases passed"),
        "{summary}"
    );

    // The script for the first failing case (bus.json, in name order)
    // fails the same way through the `run` tool's engine.
    let Body::Harte {
        repro: Some(script),
        ..
    } = &report.body
    else {
        panic!("no repro script");
    };
    let mut state = RunState::new(Rig::build(&ws, Place::Root).unwrap());
    let out = run::run(&mut state, script.as_object().unwrap()).unwrap();
    assert!(out.failed, "{}", out.text);
    assert!(
        out.text.contains("read: expected 0x23, got 0x22"),
        "{}",
        out.text
    );
    // With the expectation fixed it passes.
    let fixed: Value =
        serde_json::from_str(&script.to_string().replace("\"read\":35", "\"read\":34")).unwrap();
    let mut state = RunState::new(Rig::build(&ws, Place::Root).unwrap());
    let out = run::run(&mut state, fixed.as_object().unwrap()).unwrap();
    assert!(!out.failed, "{}", out.text);

    let out_dir = dir.join("out");
    let written = report.write(&out_dir).unwrap();
    assert_eq!(written.len(), 3);
    assert!(out_dir.join("repro.json").is_file());

    // The script of an unoptimized run asks for the same compilation.
    assert!(script.get("off").is_none(), "{script}");
    let unoptimized = RunOptions {
        unoptimized: true,
        ..options(2)
    };
    let report = run_suite(&ws, &dir, accumulator_config(), &unoptimized);
    let Body::Harte {
        repro: Some(script),
        ..
    } = &report.body
    else {
        panic!("no repro script");
    };
    assert_eq!(
        script["off"],
        json!(crate::mcp::compiled::PASSES),
        "{script}"
    );
}

#[test]
fn memory_a_case_touched_is_cleared_before_the_next() {
    let ws = accumulator();
    let dir = scratch("suite-clean");
    // The first case stores 0x55 at 0x90; the second adds the byte at
    // 0x90, which its initial state does not list, so it reads 0.
    let store = case(
        "store",
        &[("pc", 0), ("a", 0x55), ("c", 0)],
        &[(0, 2), (1, 0x90)],
        &[("pc", 2), ("a", 0x55), ("c", 0)],
        &[(0, 2), (1, 0x90), (0x90, 0x55)],
        &[(0x90, 0x55, "write")],
    );
    let add = case(
        "add",
        &[("pc", 0x20), ("a", 0x10), ("c", 0)],
        &[(0x20, 1), (0x21, 0x90)],
        &[("pc", 0x22), ("a", 0x10), ("c", 0)],
        &[(0x20, 1), (0x21, 0x90), (0x90, 0)],
        &[(0x90, 0, "read")],
    );
    // The program bytes of the first case are gone too: at 0 a halt.
    let halt = case(
        "halt",
        &[("pc", 0), ("a", 1), ("c", 1)],
        &[],
        &[("pc", 0), ("a", 1), ("c", 1)],
        &[(0, 0), (1, 0)],
        &[],
    );
    write_json(&dir.join("cases/clean.json"), &json!([store, add, halt]));
    let report = run_suite(&ws, &dir, accumulator_config(), &options(1));
    assert!(report.ok(), "{}", report.markdown());
}

#[test]
fn limit_only_and_stop_after_cut_the_work() {
    let ws = accumulator();
    let dir = scratch("suite-limit");
    accumulator_suite(&dir, 100);
    let report = run_suite(
        &ws,
        &dir,
        accumulator_config(),
        &RunOptions {
            limit: Some(7),
            ..options(2)
        },
    );
    assert!(report.ok());
    assert_eq!(report.cases(), 14);

    let report = run_suite(
        &ws,
        &dir,
        accumulator_config(),
        &RunOptions {
            only: vec!["0?".into()],
            limit: Some(5),
            ..options(1)
        },
    );
    assert_eq!(harte_files(&report).len(), 2);
    let report = run_suite(
        &ws,
        &dir,
        accumulator_config(),
        &RunOptions {
            only: vec!["02".into()],
            ..options(1)
        },
    );
    let files = harte_files(&report);
    assert_eq!((files.len(), files[0].file.as_str()), (1, "02.json"));
    assert_eq!(report.cases(), 100);
    let e = run_error_with(&ws, &dir, accumulator_config(), vec!["9*".into()]);
    assert!(e.contains("no case file"), "{e}");

    // With reads and writes swapped every case fails; stop after 3.
    let mut config = accumulator_config();
    config["bus"]["read_value"] = json!(1);
    for jobs in [1, 4] {
        let report = run_suite(
            &ws,
            &dir,
            config.clone(),
            &RunOptions {
                stop_after: Some(3),
                ..options(jobs)
            },
        );
        let files = harte_files(&report);
        let failed: usize = files.iter().map(|f| f.failed).sum();
        assert_eq!(failed, 3, "{jobs} jobs");
        assert!(!report.ok());
        assert!(
            report.summary().contains("(stopped early)"),
            "{}",
            report.summary()
        );
        // The cut is at the third failure in case order, whatever the
        // jobs.
        let last = files.iter().rev().find(|f| f.failed > 0).unwrap();
        assert_eq!(
            last.failures.last().unwrap().index + 1,
            last.run,
            "{jobs} jobs"
        );
    }
}

fn run_error_with(ws: &WorkspaceDocument, dir: &Path, config: Value, only: Vec<String>) -> String {
    let config = Config::parse(&config, dir).unwrap();
    match suite::run(ws, &config, &RunOptions { only, ..options(1) }, ("w", "c")) {
        Ok(r) => panic!("should fail: {}", r.summary()),
        Err(e) => e,
    }
}

#[test]
fn configs_that_do_not_fit_are_refused_with_the_reason() {
    let ws = accumulator();
    let dir = scratch("suite-config");
    accumulator_suite(&dir, 2);
    let mut bad = accumulator_config();
    bad["colour"] = json!(1);
    assert!(run_error(&ws, &dir, bad).contains("unknown field \"colour\""));
    let mut bad = accumulator_config();
    bad["registers"]["a"] = json!("nothing");
    let e = run_error(&ws, &dir, bad);
    assert!(e.contains("register \"a\""), "{e}");
    let mut bad = accumulator_config();
    bad["registers"] = json!({"pc": "pc", "a": "a"});
    let e = run_error(&ws, &dir, bad);
    assert!(
        e.contains("\"c\", which is not a register of the config"),
        "{e}"
    );
    let mut ignoring = accumulator_config();
    ignoring["registers"] = json!({"pc": "pc", "a": "a"});
    ignoring["ignore"] = json!(["c"]);
    assert!(run_suite(&ws, &dir, ignoring, &options(1)).ok());
    let mut bad = accumulator_config();
    bad["memory"] = json!({"part": "mem", "mask": "0x1FF"});
    let e = run_error(&ws, &dir, bad);
    assert!(e.contains("reaches past the 256 words"), "{e}");
    // A comment field is allowed.
    let mut commented = accumulator_config();
    commented["_comment"] = json!("the accumulator machine");
    assert!(Config::parse(&commented, &dir).is_ok());
}

// ── The gallery's 16-bit CPUs ────────────────────────────────────────────

fn gallery(id: &str) -> WorkspaceDocument {
    catalog()
        .expect("demos load")
        .get(id)
        .expect("gallery item")
        .workspace
        .as_ref()
        .clone()
}

fn program_of(ws: &WorkspaceDocument) -> Vec<u64> {
    ws.root
        .nodes
        .iter()
        .find_map(|n| match (&n.kind, n.label.as_deref()) {
            (GateKind::Rom { contents, .. }, Some("program")) => Some(contents.clone()),
            _ => None,
        })
        .expect("a program ROM")
}

/// The cpu16 instruction set (see the gallery manifest), one instruction
/// at a time: the reference the suites are made from.
#[derive(Clone)]
struct Cpu16 {
    pc: u64,
    r: [u64; 8],
    z: u64,
    n: u64,
    c: u64,
    sp: u64,
    stack: [u64; 16],
    ram: [u64; 256],
}

fn alu(a: u64, b: u64, f: u64) -> (u64, u64) {
    let sub = u64::from(f == 1);
    let t = a + if sub == 1 { b ^ 0xFFFF } else { b } + sub;
    let (y, carry) = match f {
        0 | 1 => (t, t >> 16 & 1),
        2 => (a & b, 0),
        3 => (a | b, 0),
        4 => (a ^ b, 0),
        5 => (!a, 0),
        6 => (a << 1, a >> 15 & 1),
        _ => (a >> 1, a & 1),
    };
    (y & 0xFFFF, carry)
}

impl Cpu16 {
    fn new() -> Cpu16 {
        Cpu16 {
            pc: 0,
            r: [0; 8],
            z: 0,
            n: 0,
            c: 0,
            sp: 0,
            stack: [0; 16],
            ram: [0; 256],
        }
    }

    /// Run one instruction; the data RAM address it reads or writes.
    fn step(&mut self, program: &[u64]) -> Option<u64> {
        let ins = program.get((self.pc & 0xFF) as usize).copied().unwrap_or(0);
        self.pc = (self.pc + 1) & 0xFFFF;
        let (op, rd, rs, rt, f) = (
            ins >> 12,
            (ins >> 9 & 7) as usize,
            (ins >> 6 & 7) as usize,
            (ins >> 3 & 7) as usize,
            ins & 7,
        );
        let imm8 = ins & 0xFF;
        let simm6 = if ins & 32 != 0 {
            (ins & 63) | 0xFFC0
        } else {
            ins & 63
        };
        let flags = |s: &mut Cpu16, y: u64, carry: u64| {
            s.z = u64::from(y == 0);
            s.n = y >> 15;
            s.c = carry;
        };
        let addr = (self.r[rs] + simm6) & 0xFF;
        match op {
            1 => {
                let (y, carry) = alu(self.r[rs], self.r[rt], f);
                flags(self, y, carry);
                self.r[rd] = y;
            }
            2 => {
                let (y, carry) = alu(self.r[rs], self.r[rt], 1);
                flags(self, y, carry);
            }
            3 => self.r[rd] = imm8,
            4 => {
                let (y, carry) = alu(self.r[rs], simm6, 0);
                flags(self, y, carry);
                self.r[rd] = y;
            }
            5 => {
                self.r[rd] = self.ram[addr as usize];
                return Some(addr);
            }
            6 => {
                self.ram[addr as usize] = self.r[rd];
                return Some(addr);
            }
            8 => self.pc = imm8,
            9 => {
                let flag = [self.z, self.c, self.n, 1][rd >> 1];
                if flag ^ (rd as u64 & 1) == 1 {
                    self.pc = imm8;
                }
            }
            10 => {
                self.sp = self.sp.wrapping_sub(1) & 0xFFFF;
                self.stack[(self.sp & 15) as usize] = self.pc;
                self.pc = imm8;
            }
            11 => {
                self.pc = self.stack[(self.sp & 15) as usize];
                self.sp = (self.sp + 1) & 0xFFFF;
            }
            _ => {}
        }
        None
    }

    fn registers(&self) -> Vec<(String, u64)> {
        let mut out = vec![("pc".to_string(), self.pc)];
        for (i, r) in self.r.iter().enumerate() {
            out.push((format!("r{i}"), *r));
        }
        out.extend([
            ("z".to_string(), self.z),
            ("n".to_string(), self.n),
            ("c".to_string(), self.c),
            ("sp".to_string(), self.sp),
        ]);
        for (i, w) in self.stack.iter().enumerate() {
            out.push((format!("s{i}"), *w));
        }
        out
    }
}

/// Clock periods an instruction takes on the gate CPU: fetch, decode,
/// execute, and a fourth (write) when it writes a register, the RAM or
/// the stack.
fn gate_cycles(op: u64) -> usize {
    if matches!(op, 1 | 3 | 4 | 5 | 6 | 10) {
        4
    } else {
        3
    }
}

/// A random program: words `16 op .. 16 op + 15` hold opcode `op`.
fn random_program(rng: &mut Rng) -> Vec<u64> {
    (0..256u64)
        .map(|i| (i / 16) << 12 | rng.below(0x1000))
        .collect()
}

/// A Harte-shaped suite for cpu16: per opcode a file of cases, each one
/// instruction from a random state. Every case lists the RAM words it
/// reads or writes and two more random words. The same cases go to
/// `cases/` for the gate CPU, with its 3 or 4 cycles, and `cases_rtl/` for
/// the word CPU, with its one. The data RAM access is the last cycle (as
/// the word CPU's bus shows it); other cycles are placeholders.
fn cpu16_suite(dir: &Path, per_op: usize, seed: u64) -> Vec<u64> {
    let mut rng = Rng(seed);
    let program = random_program(&mut rng);
    let text: Vec<String> = program.iter().map(|w| format!("{w:04X}")).collect();
    std::fs::create_dir_all(dir).unwrap();
    std::fs::write(dir.join("program.hex"), text.join(" ")).unwrap();
    for op in 0..16u64 {
        let mut cases = Vec::new();
        let mut cases_rtl = Vec::new();
        for i in 0..per_op {
            let mut s = Cpu16::new();
            s.pc = op * 16 + rng.below(16);
            for r in &mut s.r {
                *r = rng.below(0x10000);
            }
            (s.z, s.n, s.c) = (rng.below(2), rng.below(2), rng.below(2));
            s.sp = rng.below(0x10000);
            for w in &mut s.stack {
                *w = rng.below(0x10000);
            }
            // The words of RAM the case lists: the one it would touch,
            // and two others.
            let mut addrs = vec![rng.below(256), rng.below(256)];
            let mut probe = s.clone();
            let touched = probe.step(&program);
            addrs.extend(touched);
            addrs.sort_unstable();
            addrs.dedup();
            for &a in &addrs {
                s.ram[a as usize] = rng.below(0x10000);
            }
            let init = s.clone();
            let touched = s.step(&program);
            let ram = |c: &Cpu16| -> Vec<Value> {
                addrs
                    .iter()
                    .map(|&a| json!([a, c.ram[a as usize]]))
                    .collect()
            };
            let regs = |c: &Cpu16| -> Map<String, Value> {
                c.registers()
                    .into_iter()
                    .map(|(n, v)| (n, json!(v)))
                    .collect()
            };
            let mut initial = regs(&init);
            initial.insert("ram".into(), json!(ram(&init)));
            let mut fin = regs(&s);
            fin.insert("ram".into(), json!(ram(&s)));
            let ins = program[init.pc as usize];
            for (n, list) in [(gate_cycles(op), &mut cases), (1, &mut cases_rtl)] {
                let mut cycles: Vec<Value> = (0..n).map(|_| json!([0, 0, "read"])).collect();
                if let Some(a) = touched {
                    cycles[n - 1] = if op == 5 {
                        json!([a, init.ram[a as usize], "read"])
                    } else {
                        json!([a, s.ram[a as usize], "write"])
                    };
                }
                list.push(json!({
                    "name": format!("{ins:04x} #{i}"),
                    "initial": initial,
                    "final": fin,
                    "cycles": cycles,
                }));
            }
        }
        write_json(&dir.join(format!("cases/op_{op:02x}.json")), &json!(cases));
        write_json(
            &dir.join(format!("cases_rtl/op_{op:02x}.json")),
            &json!(cases_rtl),
        );
    }
    program
}

fn cpu16_registers(gates: bool) -> Map<String, Value> {
    let mut m = Map::new();
    if gates {
        m.insert("pc".into(), json!("program_counter/pc"));
    } else {
        m.insert("pc".into(), json!("pc"));
    }
    for i in 0..8 {
        m.insert(format!("r{i}"), json!(format!("registers_r0_r7/r{i}")));
    }
    if gates {
        for (n, f) in [("z", "Z"), ("n", "N"), ("c", "C")] {
            m.insert(n.into(), json!(format!("control/flags_branch/{f}_flag")));
        }
        m.insert("sp".into(), json!("stack_pointer/sp"));
        // The low bank of the stack shares its label "words 0-7" with a
        // Not gate, so it is named by its short id.
        for i in 0..16 {
            let bank = if i < 8 { "#3ca596" } else { "words_8_15" };
            m.insert(
                format!("s{i}"),
                json!(format!("stack_16_words_of_gates/{bank}/word_{}", i % 8)),
            );
        }
    } else {
        for (bit, n) in ["z", "n", "c"].iter().enumerate() {
            m.insert(
                (*n).into(),
                json!(format!("flags_branch/flags:{bit}..{}", bit + 1)),
            );
        }
        m.insert("sp".into(), json!("sp"));
        for i in 0..16 {
            m.insert(format!("s{i}"), json!(format!("stack@0x{i:X}")));
        }
    }
    m
}

/// The Harte config of cpu16 (`gates`) or cpu16_rtl.
fn cpu16_config(gates: bool) -> Value {
    let mut c = json!({
        "kind": "harte",
        "cases": "cases",
        "registers": cpu16_registers(gates),
        "memory": "data_RAM",
        "start": [{"load": {"part": "program", "file": "program.hex"}}],
    });
    if gates {
        c["fixed"] = json!({"control/state/decode": 0, "control/state/execute": 0,
                            "control/state/write": 0, "control/state/halt": 0});
        c["timing"] = json!({"ticks_per_cycle": 2});
    } else {
        c["cases"] = json!("cases_rtl");
        c["fixed"] = json!({"halted": 0});
        c["timing"] = json!({"ticks_per_cycle": 1});
    }
    c
}

#[test]
fn a_cpu16_suite_passes_on_the_gate_and_the_word_cpu() {
    let dir = scratch("suite-cpu16");
    cpu16_suite(&dir, 120, 0xC0FFEE);
    for (id, gates) in [("cpu16", true), ("cpu16_rtl", false)] {
        let report = run_suite(&gallery(id), &dir, cpu16_config(gates), &options(4));
        assert!(report.ok(), "{id}: {}", report.markdown());
        assert_eq!(report.cases(), 16 * 120, "{id}");
    }
    // The word CPU, with its bus: the RAM read of LD and the write of ST
    // are its one cycle.
    let mut config = cpu16_config(false);
    config["signals"] = json!({"addr": "address", "rdata": "data_RAM",
                               "wdata": "registers_r0_r7.QB", "store": "control.Store"});
    config["bus"] = json!({"addr": "addr", "read_data": "rdata", "write_data": "wdata",
                           "rw": "store", "read_value": 0});
    config["timing"] = json!({"ticks_per_cycle": 1, "sample": 0});
    let only = RunOptions {
        only: vec!["op_05".into(), "op_06".into()],
        ..options(2)
    };
    let report = run_suite(&gallery("cpu16_rtl"), &dir, config, &only);
    assert!(report.ok(), "{}", report.markdown());
    assert_eq!(report.cases(), 240);
}

#[test]
fn a_cpu16_bug_shows_in_the_report_of_the_gate_cpu() {
    let dir = scratch("suite-cpu16-bug");
    cpu16_suite(&dir, 20, 7);
    // Swap two registers in the mapping: every case that writes either
    // one differs.
    let mut config = cpu16_config(true);
    config["registers"]["r1"] = json!("registers_r0_r7/r2");
    config["registers"]["r2"] = json!("registers_r0_r7/r1");
    let report = run_suite(&gallery("cpu16"), &dir, config, &options(2));
    assert!(!report.ok());
    let md = report.markdown();
    assert!(md.contains("| r1 |"), "{md}");
    assert!(md.contains("**differs**"), "{md}");
    // One tick per cycle is half the clock period: instructions do not
    // finish.
    let mut config = cpu16_config(true);
    config["timing"] = json!({"ticks_per_cycle": 1});
    let report = run_suite(&gallery("cpu16"), &dir, config, &options(2));
    let files = harte_files(&report);
    assert!(
        files.iter().filter(|f| f.failed > 0).count() >= 10,
        "{}",
        report.markdown()
    );
    // Without the state of the sequencer written, a halt stays: the cases
    // after one fail.
    let mut config = cpu16_config(true);
    config["fixed"] = json!({});
    let report = run_suite(
        &gallery("cpu16"),
        &dir,
        config,
        &RunOptions {
            only: vec!["op_0f".into()],
            ..options(1)
        },
    );
    let files = harte_files(&report);
    assert_eq!(files[0].failed, 19, "{}", report.markdown());
}

// ── nestest: the gallery CPU against a log ───────────────────────────────

/// A log of the cpu16 program as the interpreter runs it: per
/// instruction, its address and word, then `R0:` .. `R7:` and `CYC:`
/// (the cycle of the gate CPU it starts in, in decimal), as nestest.log
/// writes 6502 state.
fn cpu16_log(program: &[u64], gates: bool, steps: usize) -> String {
    let mut s = Cpu16::new();
    let mut cycle = 0;
    let mut out = String::new();
    for _ in 0..steps {
        let ins = program[(s.pc & 0xFF) as usize];
        let regs: Vec<String> = (0..8).map(|i| format!("R{i}:{:04X}", s.r[i])).collect();
        out.push_str(&format!(
            "{:04X}  {ins:04X}  OP{:X}  {}  CYC:{cycle}\n",
            s.pc,
            ins >> 12,
            regs.join(" ")
        ));
        if ins >> 12 == 15 {
            break;
        }
        cycle += if gates { gate_cycles(ins >> 12) } else { 1 };
        s.step(program);
    }
    out
}

fn nestest_config(gates: bool) -> Value {
    let mut registers = Map::new();
    registers.insert(
        "pc".into(),
        json!(if gates { "program_counter/pc" } else { "pc" }),
    );
    for i in 0..8 {
        registers.insert(format!("r{i}"), json!(format!("registers_r0_r7/r{i}")));
    }
    let mut c = json!({
        "kind": "nestest",
        "log": "cpu16.log",
        "registers": registers,
        "record": ["pc", "r0", "r1", "r2", "r3", "r4", "r5", "r6", "r7"],
        "compare": {"columns": ["pc"], "radix": 16, "context": 3},
        "cycle_column": {"name": "cyc", "start": 0},
        "max_cycles": 2_000_000,
    });
    if gates {
        c["signals"] = json!({"fetch": "control.Fetch"});
        c["boundary"] = json!("fetch");
        c["timing"] = json!({"ticks_per_cycle": 2, "sample": 1});
    } else {
        // One instruction per tick: its state is there before the tick.
        c["timing"] = json!({"ticks_per_cycle": 1, "sample": 0});
    }
    c
}

#[test]
fn nestest_compares_the_gallery_cpu_with_a_log() {
    let dir = scratch("suite-nestest");
    for (id, gates) in [("cpu16_rtl", false), ("cpu16", true)] {
        let ws = gallery(id);
        let program = program_of(&ws);
        let log = cpu16_log(&program, gates, 4000);
        std::fs::write(dir.join("cpu16.log"), &log).unwrap();
        let report = run_suite(&ws, &dir, nestest_config(gates), &options(1));
        let p = program_result(&report);
        assert!(p.passed, "{id}: {}\n{}", p.verdict, p.details);
        assert!(p.verdict.contains("all 4000 rows"), "{}", p.verdict);

        // A wrong register in row 1234 is found there, with the rows
        // before it.
        let mut lines: Vec<String> = log.lines().map(str::to_string).collect();
        let at = lines[1233].find("R3:").unwrap() + 3;
        let r3 = u64::from_str_radix(&lines[1233][at..at + 4], 16).unwrap();
        lines[1233].replace_range(at..at + 4, &format!("{:04X}", (r3 + 1) & 0xFFFF));
        std::fs::write(dir.join("cpu16.log"), lines.join("\n")).unwrap();
        let report = run_suite(&ws, &dir, nestest_config(gates), &options(1));
        let p = program_result(&report);
        assert!(!p.passed);
        assert!(
            p.verdict.starts_with("row 1234 of 4000 rows differs (r3)"),
            "{}",
            p.verdict
        );
        assert!(
            p.details.contains("first difference at row 1234"),
            "{}",
            p.details
        );
        assert!(p.details.contains("  row 1231 (tick"), "{}", p.details);
        assert!(
            report.markdown().contains("## Result"),
            "{}",
            report.markdown()
        );
    }
}

// ── klaus and blargg on purpose-built circuits ───────────────────────────

/// `pc` follows a table in RAM: each tick `pc = mem[pc]`. A word that
/// holds its own address is a trap.
fn follower() -> WorkspaceDocument {
    let (ws, report) = edit(
        &empty(),
        json!({"ops": [
            {"add": [
                {"name": "pc", "type": "Register", "w": 8},
                {"name": "mem", "type": "Ram", "addr_bits": 8, "w": 8},
                {"name": "on", "type": "Const", "value": 1}
            ]},
            {"wire": [["pc", "mem.addr"], ["mem", "pc.d"], ["on", "pc.en"]]}
        ]}),
    );
    assert!(report.build.is_ok(), "{report}");
    ws
}

#[test]
fn klaus_passes_on_a_trap_at_the_success_address_only() {
    let ws = follower();
    let dir = scratch("suite-klaus");
    // 0x10 -> 0x11 -> 0x12 -> 0x12 ...
    std::fs::write(dir.join("prog.bin"), [0x11u8, 0x12, 0x12]).unwrap();
    let config = |success: u64| {
        json!({
            "kind": "klaus",
            "registers": {"pc": "pc"},
            "start": [{"load": {"part": "mem", "file": "prog.bin", "at": "0x10"}},
                      {"poke": {"pc": "0x10"}}],
            "success": success,
            "max_cycles": 1000,
        })
    };
    let report = run_suite(&ws, &dir, config(0x12), &options(1));
    let p = program_result(&report);
    assert!(p.passed, "{}", p.verdict);
    assert_eq!(p.verdict, "trapped at the success address 0x0012");
    let report = run_suite(&ws, &dir, config(0x3469), &options(1));
    let p = program_result(&report);
    assert!(!p.passed);
    assert_eq!(
        p.verdict,
        "trapped at 0x0012, not at the success address 0x3469"
    );
    assert!(
        report.summary().starts_with("klaus FAILED"),
        "{}",
        report.summary()
    );
    // No trap: a loop of two.
    std::fs::write(dir.join("prog.bin"), [0x11u8, 0x10]).unwrap();
    let report = run_suite(&ws, &dir, config(0x12), &options(1));
    let p = program_result(&report);
    assert_eq!(p.verdict, "no trap in 1000 cycles (max_cycles)");
}

/// A writer that plays a script from a ROM: at tick `t` it writes the
/// byte of ROM word `t` (bits 0-7) at address bits 8-20 of an 8 KiB RAM
/// when bit 24 is set, as a test ROM writes its status at `$6000`.
fn scripted_writer() -> WorkspaceDocument {
    let (ws, report) = edit(
        &empty(),
        json!({"ops": [
            {"add": [
                {"name": "t", "type": "Register", "w": 8},
                {"name": "inc", "type": "Add", "w": 8},
                {"name": "one", "type": "Const", "w": 8, "value": 1},
                {"name": "on", "type": "Const", "value": 1},
                {"name": "script", "type": "Rom", "addr_bits": 8, "w": 32},
                {"name": "data", "type": "Split", "w": 32, "lo": 0, "len": 8},
                {"name": "where", "type": "Split", "w": 32, "lo": 8, "len": 13},
                {"name": "we", "type": "Split", "w": 32, "lo": 24, "len": 1},
                {"name": "sram", "type": "Ram", "addr_bits": 13, "w": 8}
            ]},
            {"wire": [
                ["t", "inc.a", "script"], ["one", "inc.b"], ["inc.sum", "t.d"], ["on", "t.en"],
                ["script", "data", "where", "we"],
                ["data", "sram.wdata"], ["where", "sram.addr"], ["we", "sram.we"]
            ]}
        ]}),
    );
    assert!(report.build.is_ok(), "{report}");
    ws
}

fn writes(list: &[(u64, u64)]) -> Vec<u64> {
    list.iter().map(|(a, v)| 1 << 24 | a << 8 | v).collect()
}

#[test]
fn blargg_reads_the_status_and_text_at_6000() {
    let ws = scripted_writer();
    let dir = scratch("suite-blargg");
    let script = |status: u64, text: &str| {
        let mut list = vec![(0, 0x80), (1, 0xDE), (2, 0xB0), (3, 0x61)];
        for (i, b) in text.bytes().enumerate() {
            list.push((4 + i as u64, u64::from(b)));
        }
        list.push((4 + text.len() as u64, 0));
        // Some idle ticks while "running", then the result.
        let mut words = writes(&list);
        words.extend([0; 5]);
        words.extend(writes(&[(0, status)]));
        words
    };
    let config = |words: Vec<u64>| {
        json!({
            "kind": "blargg",
            "memory": {"regions": [{"from": "0x6000", "to": "0x7FFF", "part": "sram"}]},
            "start": [{"load": {"part": "script", "data": words}}],
            "max_cycles": 200,
        })
    };
    let report = run_suite(
        &ws,
        &dir,
        config(script(0, "All tests passed\n")),
        &options(1),
    );
    let p = program_result(&report);
    assert!(p.passed, "{} {}", p.verdict, p.details);
    assert_eq!(p.verdict, "status 0x00 at 0x6000 (passed)");
    assert_eq!(p.details, "All tests passed\n");

    let report = run_suite(&ws, &dir, config(script(3, "Failed #3\n")), &options(1));
    let p = program_result(&report);
    assert!(!p.passed);
    assert_eq!(p.verdict, "status 0x03 at 0x6000");
    assert_eq!(p.details, "Failed #3\n");
    assert!(report.markdown().contains("Failed #3"));

    // Status without the signature is not a result.
    let report = run_suite(&ws, &dir, config(writes(&[(0, 0x80), (0, 0)])), &options(1));
    let p = program_result(&report);
    assert!(
        p.verdict.starts_with("no status protocol in 200 cycles"),
        "{}",
        p.verdict
    );
}

/// The example configs for the 6502 are configs the runner reads.
#[test]
fn the_example_configs_for_the_6502_parse() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("nes/tests");
    let mut kinds = Vec::new();
    for name in ["harte", "nestest", "klaus", "blargg"] {
        let config = Config::read(&dir.join(format!("{name}.example.json")))
            .unwrap_or_else(|e| panic!("{e}"));
        kinds.push(config.kind.name());
    }
    assert_eq!(kinds, ["harte", "nestest", "klaus", "blargg"]);
}

// ── The command line's suite on disk ─────────────────────────────────────

/// Writes the cpu16 suite, both configs and both workspaces to the folder
/// in `SUITE_OUT`, for a run of `examples/nes_suite.rs`:
/// `SUITE_OUT=dir cargo test ... write_cpu16_suite -- --ignored`.
#[test]
#[ignore]
fn write_cpu16_suite() {
    let Some(out) = std::env::var_os("SUITE_OUT").map(PathBuf::from) else {
        return;
    };
    let per_op = std::env::var("SUITE_CASES")
        .ok()
        .and_then(|n| n.parse().ok())
        .unwrap_or(1000);
    cpu16_suite(&out, per_op, 0xC0FFEE);
    for (id, gates) in [("cpu16", true), ("cpu16_rtl", false)] {
        crate::editor::workspace::save_workspace_at(&out.join(id), id, &gallery(id)).unwrap();
        let mut config = cpu16_config(gates);
        config["_comment"] = json!(format!("Harte-shaped suite for the gallery's {id}"));
        write_json(&out.join(format!("{id}.harte.json")), &config);
    }
    let ws = gallery("cpu16");
    std::fs::write(
        out.join("cpu16.log"),
        cpu16_log(&program_of(&ws), true, 100_000),
    )
    .unwrap();
    write_json(&out.join("cpu16.nestest.json"), &nestest_config(true));
}
