//! `cargo x recomp`: static recompiler, x86 engine DLL (as Ghidra P-code) -> portable C.
//!
//! Input: the P-code export written by tools/ghidra_scripts/ExportPcode.java (`cargo x
//! ghidra-export`). Output: C sources (one C function per guest function entry, plus the
//! address -> function dispatch table), compiled together with native/rt/.
//!
//! Model (docs/NATIVE_PORT.md):
//! * Guest memory is a flat 32-bit space at host pointer M (rt.h); guest addresses stay 32-bit.
//! * General-purpose registers live in cpu.r[8]; each generated function copies the ones it uses
//!   into locals and spills them around calls/returns, so the C compiler can keep them in host
//!   registers. Flags (except DF) are function-local; MSVC code never keeps flags live across a
//!   call. x87, DF and other rare registers live in the cpu.rf register file.
//! * A guest CALL pushes its return address like the original and calls the C function; RET pops
//!   it and returns. Indirect calls and jumps go through rt_call() (hash table lookup, or a host
//!   import/JNI stub).
//! * A function's body is everything reachable from its entry without crossing a call, so code
//!   shared between functions (MSVC tail merging) is duplicated, and jumps into another known
//!   entry become tail calls.

use crate::util::R;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fmt::Write as _;
use std::path::Path;

const GPR: [&str; 8] = ["eax", "ecx", "edx", "ebx", "esp", "ebp", "esi", "edi"];
const DF_OFF: u64 = 0x20a;

/// C type of an integer of `n` bytes (unsigned / signed); the error names the size, as the
/// generated `rt_unimpl` calls report it.
fn ct(n: u32) -> Result<&'static str, String> {
    Ok(match n {
        1 => "uint8_t",
        2 => "uint16_t",
        4 => "uint32_t",
        8 => "uint64_t",
        10 => "double",
        _ => return Err(n.to_string()),
    })
}

fn st(n: u32) -> Result<&'static str, String> {
    Ok(match n {
        1 => "int8_t",
        2 => "int16_t",
        4 => "int32_t",
        8 => "int64_t",
        _ => return Err(n.to_string()),
    })
}

/// A P-code varnode: space ("c" constant, "u" unique, "m" memory, "r" register, "o" user-defined
/// op name, or another space name), offset, size.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Vn {
    sp: &'static str,
    off: u64,
    size: u32,
    name: &'static str,
}

impl Vn {
    fn c(off: u64, size: u32) -> Vn {
        Vn { sp: "c", off, size, name: "" }
    }
    fn r(off: u64, size: u32) -> Vn {
        Vn { sp: "r", off, size, name: "" }
    }
}

struct Op {
    opc: &'static str,
    out: Option<Vn>,
    ins: Vec<Vn>,
}

struct Instr {
    addr: u64,
    length: u64,
    text: String,
    ops: Vec<Op>,
    jumps: Vec<u64>,
}

impl Instr {
    fn mnem(&self) -> &str {
        self.text.split(' ').next().unwrap_or("")
    }
    fn next(&self) -> u64 {
        self.addr + self.length
    }
}

#[derive(Default)]
struct Interner(HashMap<String, &'static str>);

impl Interner {
    fn get(&mut self, s: &str) -> &'static str {
        if let Some(v) = self.0.get(s) {
            return v;
        }
        let v: &'static str = Box::leak(s.to_string().into_boxed_str());
        self.0.insert(s.to_string(), v);
        v
    }
}

fn hex(s: &str) -> R<u64> {
    Ok(u64::from_str_radix(s, 16).map_err(|e| format!("bad hex {s:?}: {e}"))?)
}

fn parse_vn(s: &str, names: &mut Interner) -> R<Option<Vn>> {
    if s == "-" {
        return Ok(None);
    }
    if let Some(n) = s.strip_prefix("o:") {
        return Ok(Some(Vn { sp: "o", off: 0, size: 0, name: names.get(n) }));
    }
    let mut it = s.split(':');
    let (sp, off, size) = (it.next().unwrap_or(""), it.next().unwrap_or(""), it.next().unwrap_or(""));
    Ok(Some(Vn { sp: names.get(sp), off: hex(off)?, size: size.parse()?, name: "" }))
}

fn load(path: &Path) -> R<(BTreeMap<u64, String>, HashMap<u64, Instr>)> {
    let data = std::fs::read(path)?;
    let text = String::from_utf8_lossy(&data);
    let mut names = Interner::default();
    let mut funcs = BTreeMap::new();
    let mut instrs: HashMap<u64, Instr> = HashMap::new();
    let mut cur: Option<u64> = None;
    for line in text.lines() {
        let line = line.trim_end_matches('\r');
        match line.as_bytes().first() {
            Some(b'P') => {
                let parts: Vec<&str> = line.split_whitespace().collect();
                let out = parse_vn(parts[2], &mut names)?;
                let mut ins = Vec::with_capacity(parts.len() - 3);
                for p in &parts[3..] {
                    ins.push(parse_vn(p, &mut names)?.ok_or("input varnode '-'")?);
                }
                let op = Op { opc: names.get(parts[1]), out, ins };
                instrs.get_mut(&cur.ok_or("P before I")?).unwrap().ops.push(op);
            }
            Some(b'I') => {
                let mut it = line.splitn(5, ' ');
                let (_, a, n, _hex) = (it.next(), it.next().unwrap_or(""), it.next().unwrap_or(""), it.next());
                let ins = Instr { addr: hex(a)?, length: n.parse()?, text: it.next().unwrap_or("").to_string(), ops: Vec::new(), jumps: Vec::new() };
                cur = Some(ins.addr);
                instrs.insert(ins.addr, ins);
            }
            Some(b'J') => {
                let j = hex(line.split_whitespace().nth(1).unwrap_or(""))?;
                instrs.get_mut(&cur.ok_or("J before I")?).unwrap().jumps.push(j);
            }
            Some(b'F') => {
                let mut it = line.splitn(3, ' ');
                let (_, a, name) = (it.next(), it.next().unwrap_or(""), it.next().unwrap_or(""));
                funcs.insert(hex(a)?, name.to_string());
            }
            _ => {}
        }
    }
    for ins in instrs.values_mut() {
        if matches!(ins.mnem(), "BSR" | "BSF") {
            fix_bitscan(ins);
        }
    }
    apply_patches(&mut instrs)?;
    Ok((funcs, instrs))
}

// Layout patches for the 640x360 native build (the engine is laid out for 240x320). The tutorial /
// story dialogue screen (FUN_100b8360 builds it, FUN_100b8570 draws it) places its speech box
// (centre x 0x78 = 120), its text (x 0x4b) and the portrait (x = slide offset) for a 240-wide
// screen, so on 640 they hug the left edge. Shifting x by (640 - 240) / 2 centres them.
const DIALOG_DX: u64 = 200;
const POPUP_DX: u64 = (640 - 420) / 2;
const VIEW_COLS_EXTRA: u64 = 18;

enum Patch {
    /// replace a constant input
    Const(u64, u64),
    /// replace a register input of a COPY by a constant
    SrcReg(u64, u64),
    /// add a constant to a register after the instruction
    AddReg(u64, u64),
}

const PATCHES: &[(u64, Patch)] = &[
    (0x100b85fa, Patch::Const(0x78, 0x78 + DIALOG_DX)), // ADD EBP,0x78: speech box centre x
    (0x100b875e, Patch::Const(0x4b, 0x4b + DIALOG_DX)), // ADD EBP,0x4b: text x
    (0x100b8808, Patch::AddReg(8, DIALOG_DX)),          // MOV EDX,[ESP+0x10]: portrait x (EDX += dx)
    // Fight cut-in popup (same sites as src/java/rg/DllPatcher.java): the 420x378 frame is drawn
    // at (0,0) and the 240x320 fight surface at (90,29) inside it. Centre both horizontally.
    (0x10028ac5, Patch::Const(0x5a, 0x5a + POPUP_DX)), // MOV [ESP+0x10],0x5a: fight surface x
    (0x10028af3, Patch::SrcReg(0xc, POPUP_DX)),        // MOV [ESP+8],EBX (=0): frame x
    // Tile views (FUN_100867c0: the block-placement ghost / highlight layers) draw only
    // [start, this+0x64) viewport columns, 13+1 for a 240 px screen. The board is centred on 640,
    // so its columns start at viewport column 4 and the last 2-3 were never drawn. Widen it.
    (0x10086822, Patch::AddReg(8, VIEW_COLS_EXTRA)), // MOV EDX,[ECX+0x18]: viewport columns (EDX += n)
];

// End-of-match win/lose screens (scene draw FUN_100dafa0) draw their layers straight onto the
// screen with 240-wide coordinates. While that function runs, rt_xoff is DX and the engine's
// sprite/text draw (FUN_10052de0: surface, &pos, &rect, frame) adds it to pos.x (and restores it).
const XOFF_FUNCS: &[(u64, u64)] = &[(0x100dafa0, DIALOG_DX)];
const XOFF_DRAW: u64 = 0x10052de0;

/// Function entries that call rt_hook() (native/rt/rt.c): the host can intercept them (game
/// server mode: native/rt/server.c). Inert unless a handler is registered.
const DEFAULT_HOOKS: &[u64] = &[
    0x1008fa80, 0x1008f500, // main menu flow: event handler, constructor
    0x100d5350,             // block placement tool activated (rt: hook_tile_tool)
];

fn apply_patches(instrs: &mut HashMap<u64, Instr>) -> R<()> {
    for (addr, p) in PATCHES {
        let ins = instrs.get_mut(addr).ok_or_else(|| format!("patch site {addr:x} not found"))?;
        match *p {
            Patch::Const(from, to) => {
                let mut n = 0;
                for op in &mut ins.ops {
                    for v in &mut op.ins {
                        if v.sp == "c" && v.off == from {
                            *v = Vn::c(to, v.size);
                            n += 1;
                        }
                    }
                }
                if n == 0 {
                    return Err(format!("patch site {addr:x}: constant {from:#x} not found").into());
                }
            }
            Patch::SrcReg(reg, to) => {
                let mut n = 0;
                for op in &mut ins.ops {
                    let copy = op.opc == "COPY";
                    for v in &mut op.ins {
                        if copy && v.sp == "r" && v.off == reg {
                            *v = Vn::c(to, v.size);
                            n += 1;
                        }
                    }
                }
                if n == 0 {
                    return Err(format!("patch site {addr:x}: register {reg:#x} not found").into());
                }
            }
            Patch::AddReg(reg, add) => ins.ops.push(Op { opc: "INT_ADD", out: Some(Vn::r(reg, 4)), ins: vec![Vn::r(reg, 4), Vn::c(add, 4)] }),
        }
    }
    Ok(())
}

/// Signed value of a constant varnode (relative P-code branch offsets).
fn rel_const(t: &Vn) -> i64 {
    let bits = 8 * t.size.min(8);
    let v = if bits >= 64 { t.off } else { t.off & ((1u64 << bits) - 1) };
    if bits >= 64 {
        v as i64
    } else if v >> (bits - 1) != 0 {
        v as i64 - (1i64 << bits)
    } else {
        v as i64
    }
}

/// Ghidra's BSR/BSF model writes 31/0 to the destination for a zero source; hardware leaves it
/// unchanged (compilers rely on it: MOV EAX,-1 / BSR EAX,ECX). Make the zero-source branch skip
/// the final destination write.
fn fix_bitscan(ins: &mut Instr) {
    let Some(last) = ins.ops.len().checked_sub(1) else { return };
    if last < 1 || ins.ops[last].opc != "COPY" {
        return;
    }
    for (i, op) in ins.ops.iter_mut().enumerate() {
        if op.opc == "CBRANCH" && op.ins[0].sp == "c" && i as i64 + rel_const(&op.ins[0]) == last as i64 {
            op.ins[0] = Vn::c(op.ins[0].off + 1, op.ins[0].size);
            return;
        }
    }
}

struct Recompiler {
    instrs: HashMap<u64, Instr>,
    entries: HashSet<u64>,
    jump_targets: HashSet<u64>,
    unsupported: BTreeMap<String, u64>,
    trace: HashSet<u64>,
    hooks: HashSet<u64>,
}

fn fname(a: u64) -> String {
    format!("f_{a:08x}")
}

const SPILL: &str = "@SPILL@";
const RELOAD: &str = "@RELOAD@";

type G<T> = Result<T, String>;

/// Generates the C body of one function.
struct FuncGen<'a> {
    rc: &'a Recompiler,
    entry: u64,
    body: Vec<u64>,
    body_set: HashSet<u64>,
    temps: BTreeMap<String, &'static str>,
    flags: BTreeSet<String>,
    gprs: [bool; 8],
    targets: HashSet<u64>,
}

impl<'a> FuncGen<'a> {
    fn new(rc: &'a Recompiler, entry: u64) -> Self {
        FuncGen { rc, entry, body: Vec::new(), body_set: HashSet::new(), temps: BTreeMap::new(), flags: BTreeSet::new(), gprs: [false; 8], targets: HashSet::new() }
    }

    fn is_other_entry(&self, a: u64) -> bool {
        self.rc.entries.contains(&a) && a != self.entry
    }

    // ------------------------------------------------------------------ CFG
    fn discover(&mut self) {
        let rc = self.rc;
        let mut seen = HashSet::new();
        let mut work = vec![self.entry];
        while let Some(a) = work.pop() {
            if seen.contains(&a) {
                continue;
            }
            let Some(ins) = rc.instrs.get(&a) else { continue };
            seen.insert(a);
            let mut falls = true;
            for op in &ins.ops {
                match op.opc {
                    "BRANCH" | "CBRANCH" => {
                        let t = op.ins[0];
                        if t.sp == "m" {
                            if self.is_other_entry(t.off) {
                                // tail call
                            } else if rc.instrs.contains_key(&t.off) {
                                work.push(t.off);
                                self.targets.insert(t.off);
                            }
                            if op.opc == "BRANCH" {
                                falls = false;
                            }
                        }
                    }
                    "BRANCHIND" => {
                        for &j in &ins.jumps {
                            if rc.instrs.contains_key(&j) && !self.is_other_entry(j) {
                                work.push(j);
                                self.targets.insert(j);
                            }
                        }
                        falls = false;
                    }
                    "RETURN" => falls = false,
                    _ => {}
                }
            }
            // A relative BRANCH inside the instruction (rep prefixes, CPUID) still falls through.
            if falls {
                work.push(ins.next());
            }
        }
        self.body = seen.into_iter().collect();
        self.body.sort_unstable();
        self.targets.insert(self.entry);
    }

    // ------------------------------------------------------------------ varnodes
    fn temp(&mut self, off: u64, size: u32) -> G<String> {
        let ty = ct(size).map_err(|_| format!("temp size {size}"))?;
        let name = format!("u{off:x}_{size}");
        self.temps.insert(name.clone(), ty);
        Ok(name)
    }

    fn gpr(&mut self, off: u64) -> &'static str {
        let i = (off >> 2) as usize;
        self.gprs[i] = true;
        GPR[i]
    }

    fn is_flag(v: &Vn) -> bool {
        (0x200..0x220).contains(&v.off) && v.size == 1 && v.off != DF_OFF
    }

    fn rd(&mut self, v: &Vn) -> G<String> {
        let (off, size) = (v.off, v.size);
        match v.sp {
            "c" => {
                if size == 10 {
                    return Err("float const".into());
                }
                let val = if size >= 8 { off } else { off & ((1u64 << (8 * size)) - 1) };
                Ok(match size {
                    8 => format!("0x{val:x}ull"),
                    4 => format!("0x{val:x}u"),
                    _ => format!("(({})0x{val:x}u)", ct(size)?),
                })
            }
            "u" => self.temp(off, size),
            "m" => ld(size, &format!("0x{off:x}u")),
            "r" if off < 0x20 => {
                let g = self.gpr(off);
                match (off & 3, size) {
                    (0, 4) => Ok(g.to_string()),
                    (0, 2) => Ok(format!("((uint16_t){g})")),
                    (0, 1) => Ok(format!("((uint8_t){g})")),
                    (1, 1) => Ok(format!("((uint8_t)({g} >> 8))")),
                    _ => Err(format!("gpr sub {off:x}:{size}")),
                }
            }
            "r" if Self::is_flag(v) => {
                let n = format!("f{off:x}");
                self.flags.insert(n.clone());
                Ok(n)
            }
            "r" => {
                if size == 10 {
                    return Ok(format!("RFD(0x{off:x})"));
                }
                ct(size).map_err(|_| format!("register size {size}"))?;
                Ok(format!("RF{}(0x{off:x})", size * 8))
            }
            sp => Err(format!("space {sp}")),
        }
    }

    fn wr(&mut self, v: &Vn, expr: &str) -> G<String> {
        let (off, size) = (v.off, v.size);
        match v.sp {
            "u" => Ok(format!("{} = {expr};", self.temp(off, size)?)),
            "m" => store(size, &format!("0x{off:x}u"), expr),
            "r" if off < 0x20 => {
                let g = self.gpr(off);
                match (off & 3, size) {
                    (0, 4) => Ok(format!("{g} = {expr};")),
                    (0, 2) => Ok(format!("{g} = ({g} & 0xffff0000u) | (uint16_t)({expr});")),
                    (0, 1) => Ok(format!("{g} = ({g} & 0xffffff00u) | (uint8_t)({expr});")),
                    (1, 1) => Ok(format!("{g} = ({g} & 0xffff00ffu) | ((uint32_t)(uint8_t)({expr}) << 8);")),
                    _ => Err(format!("gpr sub write {off:x}:{size}")),
                }
            }
            "r" if Self::is_flag(v) => {
                let n = format!("f{off:x}");
                self.flags.insert(n.clone());
                Ok(format!("{n} = {expr};"))
            }
            "r" => {
                if size == 10 {
                    return Ok(format!("RFD(0x{off:x}) = {expr};"));
                }
                ct(size).map_err(|_| format!("register write size {size}"))?;
                Ok(format!("RF{}(0x{off:x}) = {expr};", size * 8))
            }
            sp => Err(format!("write space {sp}")),
        }
    }

    // floats: size-4/8 varnodes hold IEEE bit patterns; size-10 values are host doubles.
    fn fval(&mut self, v: &Vn) -> G<String> {
        let e = self.rd(v)?;
        match v.size {
            4 => Ok(format!("((double)F32({e}))")),
            8 => Ok(format!("F64({e})")),
            10 => Ok(e),
            s => Err(format!("float size {s}")),
        }
    }

    // ------------------------------------------------------------------ code
    fn spill(&self) -> String {
        (0..8).filter(|&i| self.gprs[i]).map(|i| format!("cpu.r[{i}] = {};", GPR[i])).collect::<Vec<_>>().join(" ")
    }

    fn reload(&self) -> String {
        (0..8).filter(|&i| self.gprs[i]).map(|i| format!("{} = cpu.r[{i}];", GPR[i])).collect::<Vec<_>>().join(" ")
    }

    fn call_direct(&self, target: u64) -> String {
        if self.rc.entries.contains(&target) {
            format!("{SPILL} {}(); {RELOAD}", fname(target))
        } else {
            format!("{SPILL} rt_call(0x{target:x}u); {RELOAD}")
        }
    }

    fn gen_op(&mut self, ins: &Instr, idx: usize, op: &Op) -> G<String> {
        let l = format!("L_{:x}", ins.addr);
        let a = &op.ins;
        let out = op.out.as_ref();
        let o = || out.ok_or_else(|| "missing output".to_string());
        match op.opc {
            "COPY" => {
                let x = self.rd(&a[0])?;
                return self.wr(o()?, &x);
            }
            "LOAD" => {
                let x = self.rd(&a[1])?;
                let e = ld(o()?.size, &x)?;
                return self.wr(o()?, &e);
            }
            "STORE" => {
                let (p, x) = (self.rd(&a[1])?, self.rd(&a[2])?);
                return store(a[2].size, &p, &x);
            }
            _ => {}
        }
        let n = out.map(|v| v.size).unwrap_or(0);
        let w = if n == 8 { "uint64_t" } else { "uint32_t" };
        let e = match op.opc {
            "INT_ADD" | "INT_SUB" | "INT_MULT" | "INT_AND" | "INT_OR" | "INT_XOR" => {
                let sym = match op.opc {
                    "INT_ADD" => "+",
                    "INT_SUB" => "-",
                    "INT_MULT" => "*",
                    "INT_AND" => "&",
                    "INT_OR" => "|",
                    _ => "^",
                };
                let (x, y) = (self.rd(&a[0])?, self.rd(&a[1])?);
                if n >= 4 {
                    format!("{x} {sym} {y}")
                } else if op.opc == "INT_MULT" {
                    format!("({})((uint32_t){x} * {y})", ct(n)?)
                } else {
                    format!("({})({x} {sym} {y})", ct(n)?)
                }
            }
            "INT_2COMP" => {
                let t = ct(n)?;
                format!("({t})(0u - ({w}){})", self.rd(&a[0])?)
            }
            "INT_NEGATE" => {
                let t = ct(n)?;
                format!("({t})(~({w}){})", self.rd(&a[0])?)
            }
            "INT_LEFT" | "INT_RIGHT" | "INT_SRIGHT" => {
                let bits = 8 * n;
                let x = self.rd(&a[0])?;
                let s = format!("(uint64_t){}", self.rd(&a[1])?);
                match op.opc {
                    "INT_LEFT" => format!("({s} >= {bits} ? 0 : ({})(({w}){x} << {s}))", ct(n)?),
                    "INT_RIGHT" => format!("({s} >= {bits} ? 0 : ({})({x} >> {s}))", ct(n)?),
                    _ => format!("({})(({}){x} >> ({s} >= {bits} ? {} : {s}))", ct(n)?, st(n)?, bits as i64 - 1),
                }
            }
            "INT_EQUAL" | "INT_NOTEQUAL" | "INT_LESS" | "INT_LESSEQUAL" => {
                let sym = match op.opc {
                    "INT_EQUAL" => "==",
                    "INT_NOTEQUAL" => "!=",
                    "INT_LESS" => "<",
                    _ => "<=",
                };
                let x = self.rd(&a[0])?;
                format!("(uint8_t)({x} {sym} {})", self.rd(&a[1])?)
            }
            "INT_SLESS" | "INT_SLESSEQUAL" => {
                let m = a[0].size;
                let sym = if op.opc == "INT_SLESS" { "<" } else { "<=" };
                let t = st(m)?;
                let x = self.rd(&a[0])?;
                format!("(uint8_t)(({t}){x} {sym} ({t}){})", self.rd(&a[1])?)
            }
            "INT_ZEXT" => {
                let t = ct(n)?;
                format!("({t}){}", self.rd(&a[0])?)
            }
            "INT_SEXT" => {
                let (t, s1, s2) = (ct(n)?, st(n)?, st(a[0].size)?);
                format!("({t})({s1})({s2}){}", self.rd(&a[0])?)
            }
            "INT_CARRY" | "INT_SCARRY" | "INT_SBORROW" => {
                let f = match op.opc {
                    "INT_CARRY" => "CARRY",
                    "INT_SCARRY" => "SCARRY",
                    _ => "SBORROW",
                };
                let x = self.rd(&a[0])?;
                format!("{f}{}({x}, {})", a[0].size * 8, self.rd(&a[1])?)
            }
            "INT_DIV" | "INT_REM" => {
                let sym = if op.opc == "INT_DIV" { "/" } else { "%" };
                let t = ct(n)?;
                let x = self.rd(&a[0])?;
                format!("({t})({x} {sym} RT_NZ({}))", self.rd(&a[1])?)
            }
            "INT_SDIV" | "INT_SREM" => {
                let sym = if op.opc == "INT_SDIV" { "/" } else { "%" };
                let t = ct(n)?;
                let s = st(a[0].size)?;
                let x = self.rd(&a[0])?;
                format!("({t})(({s}){x} {sym} ({s})RT_NZ({}))", self.rd(&a[1])?)
            }
            "BOOL_NEGATE" => format!("(uint8_t)(!{})", self.rd(&a[0])?),
            "BOOL_AND" | "BOOL_OR" | "BOOL_XOR" => {
                let sym = match op.opc {
                    "BOOL_AND" => "&",
                    "BOOL_OR" => "|",
                    _ => "^",
                };
                let x = self.rd(&a[0])?;
                format!("(uint8_t)({x} {sym} {})", self.rd(&a[1])?)
            }
            "POPCOUNT" => {
                let t = ct(n)?;
                format!("({t})__builtin_popcountll((uint64_t){})", self.rd(&a[0])?)
            }
            "LZCOUNT" => {
                let t = ct(n)?;
                format!("({t})LZCNT{}({})", a[0].size * 8, self.rd(&a[0])?)
            }
            "PIECE" => {
                let t = ct(n)?;
                let x = self.rd(&a[0])?;
                format!("({t})((({t}){x} << {}) | ({t}){})", 8 * a[1].size, self.rd(&a[1])?)
            }
            "SUBPIECE" => {
                if matches!(a[0].size, 10 | 16) {
                    return Err("subpiece of float/vector".into());
                }
                let sh = 8 * a[1].off;
                let src = self.rd(&a[0])?;
                if sh != 0 {
                    format!("({})({src} >> {sh})", ct(n)?)
                } else {
                    format!("({}){src}", ct(n)?)
                }
            }
            opc if opc.starts_with("FLOAT") || matches!(opc, "INT2FLOAT" | "TRUNC" | "ROUND" | "FLOOR" | "CEIL") => {
                return self.gen_float(op.opc, o()?, a);
            }
            "BRANCH" => {
                let t = a[0];
                return Ok(if t.sp == "c" { format!("goto {l}_{};", idx as i64 + rel_const(&t)) } else { self.jump(t.off) });
            }
            "CBRANCH" => {
                let t = a[0];
                let c = self.rd(&a[1])?;
                return Ok(if t.sp == "c" {
                    format!("if ({c}) goto {l}_{};", idx as i64 + rel_const(&t))
                } else {
                    format!("if ({c}) {{ {} }}", self.jump(t.off))
                });
            }
            "BRANCHIND" => {
                let tgt = self.rd(&a[0])?;
                let local: BTreeSet<u64> = ins.jumps.iter().copied().filter(|j| self.body_set.contains(j)).collect();
                return Ok(if local.is_empty() {
                    format!("{SPILL} rt_call({tgt}); return;")
                } else {
                    let cases: Vec<String> = local.iter().map(|j| format!("case 0x{j:x}u: goto L_{j:x};")).collect();
                    format!("switch ({tgt}) {{ {} default: {SPILL} rt_call({tgt}); return; }}", cases.join(" "))
                });
            }
            "CALL" => {
                let t = a[0].off;
                return Ok(if t == ins.next() { "/* call next: get-EIP idiom */".into() } else { self.call_direct(t) });
            }
            "CALLIND" => return Ok(format!("{SPILL} rt_call({}); {RELOAD}", self.rd(&a[0])?)),
            "RETURN" => {
                return Ok(if self.rc.trace.contains(&self.entry) {
                    self.gprs[0] = true;
                    format!("rt_trace_ret(0x{:x}u, eax); {SPILL} return;", self.entry)
                } else {
                    format!("{SPILL} return;")
                });
            }
            "CALLOTHER" => {
                let name = a[0].name;
                return match name {
                    "LOCK" | "UNLOCK" => Ok(String::new()),
                    "fsin" | "fcos" | "fsqrt" => {
                        let f = &name[1..];
                        let x = self.fval(&a[1])?;
                        let e = fout(o()?.size, &format!("{f}({x})"))?;
                        self.wr(o()?, &e)
                    }
                    _ => Err(format!("callother {name}")),
                };
            }
            opc => return Err(opc.to_string()),
        };
        self.wr(o()?, &e)
    }

    fn gen_float(&mut self, opc: &str, out: &Vn, a: &[Vn]) -> G<String> {
        let n = out.size;
        let bin = match opc {
            "FLOAT_ADD" => Some("+"),
            "FLOAT_SUB" => Some("-"),
            "FLOAT_MULT" => Some("*"),
            "FLOAT_DIV" => Some("/"),
            _ => None,
        };
        let cmp = match opc {
            "FLOAT_EQUAL" => Some("=="),
            "FLOAT_NOTEQUAL" => Some("!="),
            "FLOAT_LESS" => Some("<"),
            "FLOAT_LESSEQUAL" => Some("<="),
            _ => None,
        };
        let e = if let Some(sym) = bin {
            let x = self.fval(&a[0])?;
            fout(n, &format!("{x} {sym} {}", self.fval(&a[1])?))?
        } else if let Some(sym) = cmp {
            let x = self.fval(&a[0])?;
            format!("(uint8_t)({x} {sym} {})", self.fval(&a[1])?)
        } else {
            match opc {
                "FLOAT_NEG" => fout(n, &format!("-{}", self.fval(&a[0])?))?,
                "FLOAT_ABS" => fout(n, &format!("fabs({})", self.fval(&a[0])?))?,
                "FLOAT_SQRT" => fout(n, &format!("sqrt({})", self.fval(&a[0])?))?,
                "FLOAT_NAN" => format!("(uint8_t)isnan({})", self.fval(&a[0])?),
                "INT2FLOAT" => {
                    let s = st(a[0].size)?;
                    fout(n, &format!("(double)({s}){}", self.rd(&a[0])?))?
                }
                "FLOAT2FLOAT" => {
                    let x = self.fval(&a[0])?;
                    fout(n, &x)?
                }
                "TRUNC" => {
                    let t = ct(n)?;
                    format!("({t})F2I{}({})", n * 8, self.fval(&a[0])?)
                }
                "ROUND" | "FLOOR" | "CEIL" => {
                    let f = match opc {
                        "ROUND" => "rint",
                        "FLOOR" => "floor",
                        _ => "ceil",
                    };
                    fout(n, &format!("{f}({})", self.fval(&a[0])?))?
                }
                _ => return Err(opc.to_string()),
            }
        };
        self.wr(out, &e)
    }

    /// Address control reaches after this instruction, or None if it never falls through.
    fn fallthrough(ins: &Instr) -> Option<u64> {
        if let Some(last) = ins.ops.last() {
            if matches!(last.opc, "RETURN" | "BRANCHIND") || (last.opc == "BRANCH" && last.ins[0].sp == "m") {
                return None;
            }
        }
        Some(ins.next())
    }

    fn jump(&self, target: u64) -> String {
        if self.body_set.contains(&target) && !self.is_other_entry(target) {
            return format!("goto L_{target:x};");
        }
        if self.rc.entries.contains(&target) {
            return format!("{SPILL} {}(); return;", fname(target));
        }
        if self.rc.instrs.contains_key(&target) {
            return format!("{SPILL} rt_call(0x{target:x}u); return;");
        }
        format!("rt_bad_branch(0x{:x}u, 0x{target:x}u);", self.entry)
    }

    /// Instructions emitted as a single runtime call instead of their P-code.
    fn special(&mut self, ins: &Instr) -> Option<String> {
        let t = &ins.text;
        let width = |c: u8| match c {
            b'B' => 1,
            b'W' => 2,
            _ => 4,
        };
        if t.starts_with("MOVS") && t.contains(".REP ") {
            for i in [1, 6, 7] {
                self.gprs[i] = true;
            }
            return Some(format!("RT_REP_MOVS({});", width(t.as_bytes()[4])));
        }
        if t.starts_with("STOS") && t.contains(".REP ") {
            for i in [0, 1, 7] {
                self.gprs[i] = true;
            }
            return Some(format!("RT_REP_STOS({});", width(t.as_bytes()[4])));
        }
        if ins.mnem() == "CPUID" {
            for i in [0, 1, 2, 3] {
                self.gprs[i] = true;
            }
            return Some("RT_CPUID();".into());
        }
        None
    }

    fn generate(&mut self, unsupported: &mut BTreeMap<String, u64>) -> String {
        let rc = self.rc;
        self.discover();
        self.body_set = self.body.iter().copied().collect();
        // Pre-pass: fallthroughs that are not to the next emitted instruction need labels.
        for k in 0..self.body.len() {
            if let Some(nxt) = Self::fallthrough(&rc.instrs[&self.body[k]]) {
                if k + 1 >= self.body.len() || self.body[k + 1] != nxt {
                    self.targets.insert(nxt);
                }
            }
        }
        let mut out: Vec<String> = Vec::new();
        let mut prev_next: Option<u64> = None;
        let body = self.body.clone();
        for &a in &body {
            let ins = &rc.instrs[&a];
            if let Some(p) = prev_next.filter(|&p| p != a) {
                if self.body_set.contains(&p) {
                    out.push(format!("    goto L_{p:x};"));
                } else if rc.instrs.contains_key(&p) || rc.entries.contains(&p) {
                    out.push(format!("    {}", self.jump(p)));
                } else {
                    out.push(format!("    rt_bad_branch(0x{:x}u, 0x{p:x}u);", self.entry));
                }
            }
            let text = ins.text.replace("*/", "* /");
            out.push(if self.targets.contains(&a) { format!("L_{a:x}: /* {text} */") } else { format!("    /* {a:x} {text} */") });
            if let Some(sp) = self.special(ins) {
                out.push(format!("    {sp}"));
            } else {
                let mut need = HashSet::new();
                for (i, op) in ins.ops.iter().enumerate() {
                    if matches!(op.opc, "BRANCH" | "CBRANCH") && op.ins[0].sp == "c" {
                        need.insert(i as i64 + rel_const(&op.ins[0]));
                    }
                }
                let mut failed = None;
                for (i, op) in ins.ops.iter().enumerate() {
                    match self.gen_op(ins, i, op) {
                        Ok(code) => out.push(if need.contains(&(i as i64)) { format!("L_{a:x}_{i}: {code}") } else { format!("    {code}") }),
                        Err(e) => {
                            failed = Some(e);
                            break;
                        }
                    }
                }
                match failed {
                    None => {
                        if need.contains(&(ins.ops.len() as i64)) {
                            out.push(format!("L_{a:x}_{}: ;", ins.ops.len()));
                        }
                    }
                    Some(e) => {
                        *unsupported.entry(e.clone()).or_default() += 1;
                        out.push(format!("    rt_unimpl(0x{a:x}u, \"{e}\");"));
                    }
                }
            }
            prev_next = Self::fallthrough(ins);
        }
        if let Some(p) = prev_next {
            if rc.instrs.contains_key(&p) || rc.entries.contains(&p) {
                out.push(format!("    {}", self.jump(p)));
            } else {
                out.push(format!("    rt_bad_branch(0x{:x}u, 0x{p:x}u);", self.entry));
            }
        }
        let (spill, reload) = (self.spill(), self.reload());
        let e = self.entry;
        let mut head = vec![format!("void {}(void) {{", fname(e))];
        for i in (0..8).filter(|&i| self.gprs[i]) {
            head.push(format!("    uint32_t {} = cpu.r[{i}];", GPR[i]));
        }
        for f in &self.flags {
            head.push(format!("    uint8_t {f} = 0;"));
        }
        for (name, ty) in &self.temps {
            head.push(format!("    {ty} {name};"));
        }
        head.push(format!("    RT_ENTER(0x{e:x}u);"));
        if rc.trace.contains(&e) {
            head.push(format!("    rt_trace(0x{e:x}u);"));
        }
        if rc.hooks.contains(&e) {
            head.push(format!("    rt_hook(0x{e:x}u);"));
        }
        let xoff = XOFF_FUNCS.iter().find(|(a, _)| *a == e).map(|(_, dx)| *dx);
        if let Some(dx) = xoff {
            head.push(format!("    rt_xoff += {dx};"));
        }
        if e == XOFF_DRAW {
            // text-box widgets (return address in 0x100a2xxx) already centre themselves on the real screen width
            head.push("    uint32_t xo_p = LD32(cpu.r[ESP] + 8), xo = (LD32(cpu.r[ESP]) >> 12) == 0x100a2u ? 0 : (uint32_t)rt_xoff;".into());
            head.push("    if (xo) ST32(xo_p, LD32(xo_p) + xo);".into());
        }
        if self.body.first().is_some_and(|&b| b != e) {
            head.push(format!("    goto L_{e:x};"));
        }
        head.extend(out);
        head.push("}".into());
        head.push(String::new());
        let mut text = head.join("\n").replace(" return;", " RT_LEAVE(); return;");
        if let Some(dx) = xoff {
            text = text.replace(" RT_LEAVE(); return;", &format!(" RT_LEAVE(); rt_xoff -= {dx}; return;"));
        }
        if e == XOFF_DRAW {
            text = text.replace(" RT_LEAVE(); return;", " RT_LEAVE(); if (xo) ST32(xo_p, LD32(xo_p) - xo); return;");
        }
        text.replace(SPILL, &spill).replace(RELOAD, &reload)
    }
}

fn ld(size: u32, addr: &str) -> G<String> {
    if size == 10 {
        return Ok(format!("LDX({addr})"));
    }
    ct(size).map_err(|_| format!("load size {size}"))?;
    Ok(format!("LD{}({addr})", size * 8))
}

fn store(size: u32, addr: &str, expr: &str) -> G<String> {
    if size == 10 {
        return Ok(format!("STX({addr}, {expr});"));
    }
    ct(size).map_err(|_| format!("store size {size}"))?;
    Ok(format!("ST{}({addr}, {expr});", size * 8))
}

fn fout(size: u32, d: &str) -> G<String> {
    match size {
        4 => Ok(format!("B32((float)({d}))")),
        8 => Ok(format!("B64({d})")),
        10 => Ok(format!("({d})")),
        s => Err(format!("float out size {s}")),
    }
}

/// The sections of a PE image: (name, virtual address, raw data).
type Sections = Vec<(String, u64, Vec<u8>)>;

fn read_pe(path: &Path) -> R<Sections> {
    let d = std::fs::read(path)?;
    let u16_at = |o: usize| -> R<u64> { Ok(u16::from_le_bytes(d.get(o..o + 2).ok_or("short PE")?.try_into()?) as u64) };
    let u32_at = |o: usize| -> R<u64> { Ok(u32::from_le_bytes(d.get(o..o + 4).ok_or("short PE")?.try_into()?) as u64) };
    let pe = u32_at(0x3c)? as usize;
    let nsec = u16_at(pe + 6)? as usize;
    let optsz = u16_at(pe + 20)? as usize;
    let base = u32_at(pe + 24 + 28)?;
    let mut secs = Vec::new();
    for i in 0..nsec {
        let o = pe + 24 + optsz + 40 * i;
        let name: String = d[o..o + 8].iter().take_while(|&&c| c != 0).map(|&c| c as char).collect();
        let (vsize, va, rsize, rptr) = (u32_at(o + 8)?, u32_at(o + 12)?, u32_at(o + 16)?, u32_at(o + 20)? as usize);
        let end = (rptr + vsize.min(rsize) as usize).min(d.len());
        secs.push((name, base + va, d[rptr.min(end)..end].to_vec()));
    }
    Ok(secs)
}

/// The table address of an indirect jump `JMP dword ptr [Exx*0x4 + 0xTABLE]`.
fn jump_table_operand(text: &str) -> Option<u64> {
    let mut rest = text;
    while let Some(i) = rest.find("[E") {
        let s = &rest[i + 2..];
        let b = s.as_bytes();
        if b.len() > 2 && b[0].is_ascii_uppercase() && b[1].is_ascii_uppercase() {
            if let Some(t) = s[2..].strip_prefix("*0x4 + 0x") {
                let hex_len = t.bytes().take_while(|c| matches!(c, b'0'..=b'9' | b'a'..=b'f')).count();
                if hex_len > 0 && t[hex_len..].starts_with(']') {
                    return u64::from_str_radix(&t[..hex_len], 16).ok();
                }
            }
        }
        rest = &rest[i + 2..];
    }
    None
}

impl Recompiler {
    fn new(funcs: &BTreeMap<u64, String>, instrs: HashMap<u64, Instr>) -> Self {
        let mut entries: HashSet<u64> = funcs.keys().copied().filter(|a| instrs.contains_key(a)).collect();
        // direct call targets Ghidra did not turn into functions
        for ins in instrs.values() {
            for op in &ins.ops {
                if op.opc == "CALL" && op.ins[0].sp == "m" && instrs.contains_key(&op.ins[0].off) && op.ins[0].off != ins.next() {
                    entries.insert(op.ins[0].off);
                }
            }
        }
        Recompiler { instrs, entries, jump_targets: HashSet::new(), unsupported: BTreeMap::new(), trace: HashSet::new(), hooks: HashSet::new() }
    }

    /// Indirect jumps Ghidra left unresolved: read `JMP [reg*4 + table]` tables from the image.
    fn resolve_jump_tables(&mut self, secs: &Sections) {
        let rd32 = |va: u64| -> Option<u64> {
            secs.iter().find(|(_, sva, raw)| *sva <= va && va + 3 < sva + raw.len() as u64).map(|(_, sva, raw)| {
                let o = (va - sva) as usize;
                u32::from_le_bytes(raw[o..o + 4].try_into().unwrap()) as u64
            })
        };
        let known: HashSet<u64> = self.instrs.keys().copied().collect();
        let mut fixed = 0;
        for ins in self.instrs.values_mut() {
            if !ins.ops.iter().any(|op| op.opc == "BRANCHIND") {
                continue;
            }
            let Some(table) = jump_table_operand(&ins.text) else { continue };
            for k in 0..512 {
                match rd32(table + 4 * k) {
                    Some(v) if known.contains(&v) && v.abs_diff(ins.addr) <= 0x10000 => {
                        if !ins.jumps.contains(&v) {
                            ins.jumps.push(v);
                        }
                    }
                    _ => break,
                }
            }
            fixed += !ins.jumps.is_empty() as usize;
        }
        println!("jump tables resolved from the image: {fixed}");
    }

    /// Code addresses stored in data (vtables, callbacks) become entries too.
    fn add_data_pointers(&mut self, secs: &Sections) -> usize {
        let mut added = 0;
        for (name, _, raw) in secs {
            if name == ".text" || name == ".reloc" {
                continue;
            }
            for c in raw.chunks_exact(4) {
                let v = u32::from_le_bytes(c.try_into().unwrap()) as u64;
                if self.instrs.contains_key(&v) && !self.entries.contains(&v) && !self.jump_targets.contains(&v) {
                    self.entries.insert(v);
                    added += 1;
                }
            }
        }
        added
    }

    /// Code addresses used as immediates (PUSH offset f / MOV [x], offset f: callbacks,
    /// constructors stored at run time) become entries. Branch and call targets are not
    /// immediates in P-code (they are 'm' varnodes), so this does not pick up plain labels.
    fn add_code_immediates(&mut self) -> usize {
        let mut add = HashSet::new();
        for ins in self.instrs.values() {
            // only data moves: CALL pushes its return address as a constant, and x87 instructions
            // record their own address (FPU instruction pointer)
            if !matches!(ins.mnem(), "PUSH" | "MOV") {
                continue;
            }
            for op in &ins.ops {
                for v in &op.ins {
                    if v.sp == "c" && v.size == 4 && self.instrs.contains_key(&v.off) && !self.entries.contains(&v.off) && !self.jump_targets.contains(&v.off) {
                        add.insert(v.off);
                    }
                }
            }
        }
        self.entries.extend(&add);
        add.len()
    }
}

/// Keeps the timestamp of unchanged outputs so builds only recompile what changed.
fn write_if_changed(path: &Path, text: &str) -> R<()> {
    if std::fs::read(path).ok().as_deref() == Some(text.as_bytes()) {
        return Ok(());
    }
    std::fs::write(path, text)?;
    Ok(())
}

pub struct Options {
    pub files: usize,
    pub dll: Option<std::path::PathBuf>,
    pub trace: Vec<u64>,
    pub hooks: Vec<u64>,
}

fn addr_list(s: &str) -> R<Vec<u64>> {
    s.split(',').filter(|x| !x.is_empty()).map(|x| hex(x.trim_start_matches("0x"))).collect()
}

impl Options {
    /// `--files N --dll PATH --trace a,b --hook a,b`
    pub fn parse(args: &[String]) -> R<Options> {
        let mut o = Options { files: 48, dll: None, trace: Vec::new(), hooks: Vec::new() };
        let mut it = args.iter();
        while let Some(a) = it.next() {
            let mut val = || it.next().cloned().ok_or_else(|| format!("{a} needs a value"));
            match a.as_str() {
                "--files" => o.files = val()?.parse()?,
                "--dll" => o.dll = Some(val()?.into()),
                "--trace" => o.trace = addr_list(&val()?)?,
                "--hook" => o.hooks = addr_list(&val()?)?,
                _ => return Err(format!("unknown recomp option {a}").into()),
            }
        }
        Ok(o)
    }
}

pub fn recompile(pcode: &Path, outdir: &Path, opts: &Options) -> R<()> {
    let (funcs, instrs) = load(pcode)?;
    let ninstrs = instrs.len();
    let mut rc = Recompiler::new(&funcs, instrs);
    let secs = opts.dll.as_deref().map(read_pe).transpose()?;
    if let Some(s) = &secs {
        rc.resolve_jump_tables(s);
    }
    rc.jump_targets = rc.instrs.values().flat_map(|i| i.jumps.iter().copied()).collect();
    rc.trace = opts.trace.iter().copied().collect();
    rc.hooks = DEFAULT_HOOKS.iter().chain(&opts.hooks).copied().collect();
    if let Some(s) = &secs {
        println!("data-pointer entries added: {}", rc.add_data_pointers(s));
    }
    println!("code-immediate entries added: {}", rc.add_code_immediates());
    let mut entries: Vec<u64> = rc.entries.iter().copied().collect();
    entries.sort_unstable();
    println!("{ninstrs} instructions, {} Ghidra functions, {} entries", funcs.len(), entries.len());
    std::fs::create_dir_all(outdir)?;
    let nfiles = opts.files.max(1);
    let mut total = 0;
    let mut unsupported = BTreeMap::new();
    for k in 0..nfiles {
        let mut parts = vec![
            "/* generated by rgtool (cargo x recomp) - do not edit */".to_string(),
            "#include \"rt.h\"".into(),
            "#include \"rg_gen.h\"".into(),
            String::new(),
        ];
        for &e in entries.iter().skip(k).step_by(nfiles) {
            let mut g = FuncGen::new(&rc, e);
            parts.push(g.generate(&mut unsupported));
            total += g.body.len();
        }
        write_if_changed(&outdir.join(format!("rg_gen_{k:02}.c")), &parts.join("\n"))?;
    }
    let mut h = String::from("/* generated - do not edit */\n#pragma once\n");
    let mut t = String::from("/* generated - do not edit */\n#include \"rt.h\"\n#include \"rg_gen.h\"\nconst rt_func_entry rt_func_table[] = {\n");
    for &e in &entries {
        let _ = writeln!(h, "void {}(void);", fname(e));
        let _ = writeln!(t, "    {{0x{e:x}u, {}}},", fname(e));
    }
    let _ = write!(t, "}};\nconst unsigned rt_func_count = {};\n", entries.len());
    write_if_changed(&outdir.join("rg_gen.h"), &h)?;
    write_if_changed(&outdir.join("rg_gen_table.c"), &t)?;
    println!("emitted {total} instructions ({:.2}x duplication) into {nfiles} files", total as f64 / ninstrs.max(1) as f64);
    rc.unsupported = unsupported;
    if !rc.unsupported.is_empty() {
        println!("unsupported: {:?}", rc.unsupported);
    }
    Ok(())
}

/// `cargo x recomp PCODE OUTDIR [options]`
pub fn cmd(args: &[String]) -> R<()> {
    if args.len() < 2 {
        return Err("usage: cargo x recomp PCODE.txt OUTDIR [--files N] [--dll DLL] [--trace a,b] [--hook a,b]".into());
    }
    recompile(Path::new(&args[0]), Path::new(&args[1]), &Options::parse(&args[2..])?)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vn(sp: &'static str, off: u64, size: u32) -> Vn {
        Vn { sp, off, size, name: "" }
    }

    #[test]
    fn relative_branch_offsets_are_signed() {
        assert_eq!(rel_const(&vn("c", 0xffff_ffff, 4)), -1);
        assert_eq!(rel_const(&vn("c", 3, 4)), 3);
        assert_eq!(rel_const(&vn("c", 0xffff_ffff_ffff_fffe, 8)), -2);
    }

    /// BSR/BSF with a zero source must leave the destination unchanged (the guest relies on it).
    #[test]
    fn bitscan_zero_source_skips_the_final_write() {
        let op = |opc, ins: Vec<Vn>| Op { opc, out: None, ins };
        let mut ins = Instr {
            addr: 0x1000,
            length: 3,
            text: "BSR EAX,ECX".into(),
            ops: vec![op("INT_EQUAL", vec![]), op("CBRANCH", vec![vn("c", 2, 4), vn("u", 0, 1)]), op("COPY", vec![]), op("COPY", vec![])],
            jumps: vec![],
        };
        fix_bitscan(&mut ins);
        assert_eq!(ins.ops[1].ins[0].off, 3, "the branch now skips the last op");
    }

    #[test]
    fn jump_table_operands() {
        assert_eq!(jump_table_operand("JMP dword ptr [EAX*0x4 + 0x1000a2b0]"), Some(0x1000a2b0));
        assert_eq!(jump_table_operand("JMP dword ptr [EAX + 0x10]"), None);
        assert_eq!(jump_table_operand("JMP EAX"), None);
    }

    /// Sizes the C types cannot hold turn into an `rt_unimpl` with the size as the reason
    /// (instead of aborting the whole recompilation).
    #[test]
    fn odd_sizes_are_reported_not_fatal() {
        assert_eq!(ct(16), Err("16".into()));
        assert_eq!(ld(16, "x"), Err("load size 16".into()));
        assert_eq!(ld(10, "x"), Ok("LDX(x)".into()));
    }
}
