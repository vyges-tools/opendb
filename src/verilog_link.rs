// SPDX-License-Identifier: Apache-2.0
//! A HIERARCHICAL structural Verilog netlist linked into one flat database, as the reference's
//! `read_verilog` + `link_design` (without `-hier`) builds it.
//!
//! The reference reads the whole file into a hierarchical network first (its Verilog reader and
//! linker), then walks that network twice to fill the database (`Verilog2db::makeDbNetlist`):
//!
//! 1. `makeDbModule` / `makeChildInsts` — every leaf instance, DEPTH FIRST, each level's children
//!    in BYTE ORDER of their local names (the network keeps an instance's children in an ordered
//!    map keyed by name; module and leaf children interleave in that one order);
//! 2. `makeDbNets` — the nets, again top first and depth first: at each level its nets in byte
//!    order of their local names (also an ordered map), skipping a child net that reaches a port
//!    of its instance (it belongs to the parent's net) and a net whose pins were already taken by
//!    an earlier one. A net is named by the FIRST of its aliases met in that order, its pins sorted
//!    by instance path (level by level) then pin name.
//!
//! This module reproduces the result WITHOUT holding the hierarchy: the file is indexed (byte range
//! and ports of each module), then walked once depth first, parsing each module's text when an
//! instance of it is entered and dropping it on exit. Leaves go straight to the [`Sink`]; nets are
//! one union-find over every local net met on the way, named and connected after the walk from the
//! list of naming candidates the walk recorded in reference order.
//!
//! The rules, each with its test below: R1 instance order, R2 path names and escaping, R3 net
//! order, R4 child port nets skipped, R5 aliases and the right-hand side of an `assign` surviving,
//! R6 pin order, R7 constants, R8 unconnected top ports, R9 unreferenced wires, R10 terminal
//! order, R11 supply nets, R12 macro bus pins member by member, R13 `dont_touch`, R14 bus port
//! order properties.
//!
//! The module view the reference builds even when flattening (`makeDbModule`, phase B0), each with
//! its test: a uniquely named module and a module instance per hierarchical instance, in the same
//! depth-first order (`b0_modules_uniquely_named_in_creation_order`, `b0_unique_name_counter`,
//! `b0_top_name_taken_and_empty_src`); each leaf owned by its module and each module's lists put
//! back in creation order after its children (`b0_leaves_owned_by_their_module`); the `src`,
//! `src_file_id`, `src_file_line` and `implements_operator` properties
//! (`b0_src_and_operator_properties`, `b0_line_info_regex`); an unused `implements_operator`
//! module, a child block in the reference, refused (`b0_unused_operator_module_refused`).
#![deny(clippy::unwrap_used)]
#![cfg_attr(test, allow(clippy::unwrap_used))]

use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::os::unix::fs::FileExt;
use std::rc::Rc;

// =================================================================================================
// What the walk produces, and where it goes
// =================================================================================================

/// A net's signal type as the reference sets it (`network_->isPower` / `isGround` of the naming
/// net; ground is applied last and wins).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SigType {
    Signal,
    Power,
    Ground,
}

impl SigType {
    pub fn as_str(self) -> &'static str {
        match self {
            SigType::Signal => "SIGNAL",
            SigType::Power => "POWER",
            SigType::Ground => "GROUND",
        }
    }
}

/// A block terminal's direction (`Verilog2db::staToDb(PortDirection*)`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IoType {
    Input,
    Output,
    Inout,
}

impl IoType {
    pub fn as_str(self) -> &'static str {
        match self {
            IoType::Input => "INPUT",
            IoType::Output => "OUTPUT",
            IoType::Inout => "INOUT",
        }
    }
}

/// Where the linked design goes: the database, or a recorder in tests. Calls arrive in the
/// reference's creation order — instances, then each net with its terminals and pins.
pub trait Sink {
    /// The block, named after the top module (`Verilog2db::makeBlock`), before anything else.
    fn block(&mut self, top: &str) -> Result<(), String>;
    /// The pin names of a library master, in its own order; `None` when there is no such master.
    fn master_pins(&mut self, master: &str) -> Option<Vec<String>>;
    /// `dbModule::create(block, name)` — `name` already made unique by the link
    /// ([`unique_module_name`]).
    fn module(&mut self, name: &str) -> Result<(), String>;
    /// `dbModInst::create(parent, module, name)`; `parent` is `""` for the top module.
    fn mod_inst(&mut self, parent: &str, module: &str, name: &str) -> Result<(), String>;
    /// `dbInst::create(block, master, name, false, module)`; `module` is `""` for the top module.
    fn inst(&mut self, master: &str, name: &str, module: &str) -> Result<(), String>;
    /// The end of `makeChildInsts` for `module` (`""` = top): its child and instance lists put
    /// back in creation order when odb holds them reversed.
    fn module_done(&mut self, module: &str) -> Result<(), String>;
    /// `dbStringProperty::create(owner, key, value)`.
    fn string_property(&mut self, owner: Owner<'_>, key: &str, value: &str) -> Result<(), String>;
    /// `dbIntProperty::create(owner, key, value)`.
    fn int_property(&mut self, owner: Owner<'_>, key: &str, value: i32) -> Result<(), String>;
    /// `dbModBTerm::create(module, name)`, then `setIoType(io)` (`-hier` only).
    fn mod_bterm(&mut self, module: &str, name: &str, io: IoType) -> Result<(), String>;
    /// `dbBusPort::create(module, root, from, to)`, then `root->setBusPort`.
    fn bus_port(&mut self, module: &str, root: &str, from: i64, to: i64) -> Result<(), String>;
    /// `setMembers(member)` (first bit) or `setLast(member)` (last bit) on `root`'s bus port.
    fn bus_port_member(&mut self, module: &str, root: &str, member: &str, last: bool) -> Result<(), String>;
    /// `module->getModBTerms().reverse()`.
    fn mod_bterms_done(&mut self, module: &str) -> Result<(), String>;
    /// `dbModITerm::create(modinst, name, <its master's port name>)`; the module instance is
    /// `modinst` of `parent` (`""` = top).
    fn mod_iterm(&mut self, parent: &str, modinst: &str, name: &str) -> Result<(), String>;
    /// `module->getModNet(name)`, else `dbModNet::create(module, name)` (`constructModNet`).
    fn mod_net(&mut self, module: &str, name: &str) -> Result<(), String>;
    /// `<terminal>->connect(modnet)` for module net `net` of `module`.
    fn mod_net_connect(&mut self, module: &str, net: &str, term: Term<'_>) -> Result<(), String>;
    /// `dbNet::create`, then `setSigType` when not a signal net.
    fn net(&mut self, name: &str, sig: SigType) -> Result<(), String>;
    /// `dbBTerm::create(net, name)`, then `setIoType`.
    fn bterm(&mut self, net: &str, name: &str, io: IoType) -> Result<(), String>;
    /// `dbITerm::connect(net)` of `inst/pin`.
    fn connect(&mut self, inst: &str, pin: &str, net: &str) -> Result<(), String>;
    /// `dbInst::setDoNotTouch(true)`, applied after every net exists.
    fn dont_touch(&mut self, inst: &str) -> Result<(), String>;
    /// `dbBoolProperty::create(block, "bus_msb_first <port> <top>", msb_first)`.
    fn bus_order(&mut self, key: &str, msb_first: bool) -> Result<(), String>;
}

/// The object a property is created on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Owner<'a> {
    Block,
    /// A leaf instance by its flat name.
    Inst(&'a str),
    /// A module by its (unique) name.
    Module(&'a str),
    /// A module instance: its parent module (`""` = top) and its local name.
    ModInst(&'a str, &'a str),
}

/// A terminal joined to a module net.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Term<'a> {
    /// A leaf instance's pin: (flat instance name, pin).
    ITerm(&'a str, &'a str),
    /// A top-level port.
    BTerm(&'a str),
    /// A pin of a module instance of the net's module: (its local name, pin).
    ModITerm(&'a str, &'a str),
    /// A port of the net's module.
    ModBTerm(&'a str),
}

/// What a link did — counts, and the reference's warnings it would have printed.
#[derive(Debug, Default, Clone)]
#[must_use]
pub struct LinkReport {
    pub top: String,
    pub modules_in_file: usize,
    pub module_instances: usize,
    pub insts: usize,
    pub nets: usize,
    pub bterms: usize,
    pub iterms: usize,
    /// Modules created in the database (one per module instance, uniquely named).
    pub modules: usize,
    /// Module nets created (`-hier`), and terminal connections made to them.
    pub mod_nets: usize,
    pub mod_net_connects: usize,
    /// Module bodies parsed in the build walk (a body is re-parsed per instance unless cached).
    pub bodies_parsed: usize,
    /// Module bodies taken from the cache instead.
    pub bodies_cached: usize,
    /// `(code, message)` per warning, in the order met.
    pub warnings: Vec<(u32, String)>,
}

/// Options for [`link_file`].
#[derive(Debug, Clone)]
pub struct LinkOptions {
    /// The top module; `None` takes the one module no other instantiates.
    pub top: Option<String>,
    /// Bytes of parsed module bodies kept for reuse by later instances of the same module
    /// (0 re-parses every instance).
    pub cache_bytes: usize,
    /// Leave out the `src` / `src_file_id` / `src_file_line` instance properties (the
    /// reference's `link_design -omit_filename_prop`).
    pub omit_filename_prop: bool,
    /// Keep the hierarchy (the reference's `link_design -hier`): module ports, module-instance pins
    /// and module nets as well. The database's hierarchy flag is not set (phase B3).
    pub hierarchy: bool,
}

impl Default for LinkOptions {
    fn default() -> Self {
        LinkOptions { top: None, cache_bytes: 256 << 20, omit_filename_prop: false, hierarchy: false }
    }
}

// =================================================================================================
// Names — `verilogToSta` (VerilogNamespace)
// =================================================================================================

/// A Verilog identifier as the reference's network names it. An escaped identifier (`\a/b[1] `)
/// loses its backslash and terminating space, and inside it the bus brackets, the path divider
/// and the escape itself are escaped with `\` (R2): `\esc/name ` is `esc\/name`, `\x[3] ` is
/// `x\[3\]`. A plain identifier is unchanged.
fn verilog_to_sta(v: &[u8]) -> String {
    if v.first() != Some(&b'\\') {
        return String::from_utf8_lossy(v).into_owned();
    }
    let mut len = v.len();
    if len > 1 && v[len - 1].is_ascii_whitespace() {
        len -= 1;
    }
    let mut out = Vec::with_capacity(len + 4);
    for &c in &v[1..len] {
        if matches!(c, b'[' | b']' | b'/' | b'\\') {
            out.push(b'\\');
        }
        out.push(c);
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// `verilogBusBitName`: `name[index]`.
fn bit_name(name: &str, index: i64) -> String {
    format!("{name}[{index}]")
}

/// The bits from `from` to `to`, stepping towards `to` (`VerilogBusNetNameIterator`).
fn range_bits(name: &str, from: i64, to: i64) -> Vec<String> {
    if to > from {
        (from..=to).map(|i| bit_name(name, i)).collect()
    } else {
        (to..=from).rev().map(|i| bit_name(name, i)).collect()
    }
}

// =================================================================================================
// Lexer — the reference's scanner (VerilogLex.ll), over one module's bytes
// =================================================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kw {
    Assign,
    Endmodule,
    Inout,
    Input,
    Module,
    Output,
    Parameter,
    Defparam,
    Reg,
    Signed,
    Specify,
    Endspecify,
    Specparam,
    Supply0,
    Supply1,
    Tri,
    Wand,
    Wire,
    Wor,
}

fn keyword(t: &[u8]) -> Option<Kw> {
    Some(match t {
        b"assign" => Kw::Assign,
        b"endmodule" => Kw::Endmodule,
        b"inout" => Kw::Inout,
        b"input" => Kw::Input,
        b"module" => Kw::Module,
        b"output" => Kw::Output,
        b"parameter" => Kw::Parameter,
        b"defparam" => Kw::Defparam,
        b"reg" => Kw::Reg,
        b"signed" => Kw::Signed,
        b"specify" => Kw::Specify,
        b"endspecify" => Kw::Endspecify,
        b"specparam" => Kw::Specparam,
        b"supply0" => Kw::Supply0,
        b"supply1" => Kw::Supply1,
        b"tri" => Kw::Tri,
        b"wand" => Kw::Wand,
        b"wire" => Kw::Wire,
        b"wor" => Kw::Wor,
        _ => return None,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum T {
    Id,
    Const,
    Int,
    Str,
    Punct(u8),
    AttrOpen,
    AttrClose,
    Kw(Kw),
    Eof,
}

#[derive(Debug, Clone, Copy)]
struct Tok {
    t: T,
    s: usize,
    e: usize,
    line: u32,
}

struct Lexer<'a> {
    b: &'a [u8],
    p: usize,
    line: u32,
    /// Only blanks seen since the last newline: a backtick there starts a macro line, skipped.
    at_line_start: bool,
}

fn is_id_start(c: u8) -> bool {
    c.is_ascii_alphabetic() || c == b'_'
}
fn is_id_char(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'_' || c == b'$'
}

impl<'a> Lexer<'a> {
    fn new(b: &'a [u8], line: u32) -> Self {
        Lexer { b, p: 0, line, at_line_start: true }
    }

    fn peek(&self, k: usize) -> u8 {
        self.b.get(self.p + k).copied().unwrap_or(0)
    }

    /// One identifier token (`ID_ALPHA_TOKEN` or `ID_ESCAPED_TOKEN`) from `p`; false if none.
    fn id_part(&mut self) -> bool {
        let c = self.peek(0);
        if c == b'\\' {
            // `\\[^ \t\r\n]+[\r\n\t ]` — the terminating blank is part of the token.
            let mut q = self.p + 1;
            while q < self.b.len() && !matches!(self.b[q], b' ' | b'\t' | b'\r' | b'\n') {
                q += 1;
            }
            if q == self.p + 1 || q >= self.b.len() {
                return false;
            }
            if self.b[q] == b'\n' {
                self.line += 1;
            }
            self.p = q + 1;
            true
        } else if is_id_start(c) {
            self.p += 1;
            while is_id_char(self.peek(0)) {
                self.p += 1;
            }
            true
        } else {
            false
        }
    }

    fn next(&mut self) -> Result<Tok, String> {
        loop {
            let c = self.peek(0);
            if self.p >= self.b.len() {
                return Ok(Tok { t: T::Eof, s: self.p, e: self.p, line: self.line });
            }
            match c {
                b'\n' => {
                    self.line += 1;
                    self.p += 1;
                    self.at_line_start = true;
                }
                b' ' | b'\t' | b'\r' => self.p += 1,
                b'`' if self.at_line_start => {
                    while self.p < self.b.len() && self.b[self.p] != b'\n' {
                        self.p += 1;
                    }
                }
                b'/' if self.peek(1) == b'/' => {
                    while self.p < self.b.len() && self.b[self.p] != b'\n' {
                        self.p += 1;
                    }
                }
                b'/' if self.peek(1) == b'*' => {
                    let start = self.line;
                    self.p += 2;
                    loop {
                        if self.p + 1 >= self.b.len() {
                            return Err(format!("line {start}: unterminated comment"));
                        }
                        if self.b[self.p] == b'*' && self.b[self.p + 1] == b'/' {
                            self.p += 2;
                            break;
                        }
                        if self.b[self.p] == b'\n' {
                            self.line += 1;
                        }
                        self.p += 1;
                    }
                }
                _ => break,
            }
        }
        self.at_line_start = false;
        let (s, line) = (self.p, self.line);
        let c = self.peek(0);
        let tok = |t, e| Ok(Tok { t, s, e, line });
        // Identifiers, plain or escaped, with `.`-joined continuations as one token.
        if c == b'\\' || is_id_start(c) {
            if !self.id_part() {
                return Err(format!("line {line}: bad escaped identifier"));
            }
            while self.peek(0) == b'.' && (self.peek(1) == b'\\' || is_id_start(self.peek(1))) {
                let save = self.p;
                self.p += 1;
                if !self.id_part() {
                    self.p = save;
                    break;
                }
            }
            if c != b'\\' {
                if let Some(k) = keyword(&self.b[s..self.p]) {
                    return tok(T::Kw(k), self.p);
                }
            }
            return tok(T::Id, self.p);
        }
        // Numbers: `{SIGN}?{UNSIGNED_NUMBER}?'[sS]?<base><digits>` (a constant) or `{SIGN}?[0-9]+`.
        if c.is_ascii_digit() || c == b'\'' || ((c == b'+' || c == b'-') && (self.peek(1).is_ascii_digit() || self.peek(1) == b'\'')) {
            let mut q = self.p;
            if c == b'+' || c == b'-' {
                q += 1;
            }
            let digits_start = q;
            if q < self.b.len() && self.b[q].is_ascii_digit() {
                while q < self.b.len() && (self.b[q].is_ascii_digit() || self.b[q] == b'_') {
                    q += 1;
                }
            }
            if q < self.b.len() && self.b[q] == b'\'' {
                let mut r = q + 1;
                if r < self.b.len() && (self.b[r] == b's' || self.b[r] == b'S') {
                    r += 1;
                }
                let base = self.b.get(r).copied().unwrap_or(0);
                let ok: fn(u8) -> bool = match base {
                    b'b' | b'B' => |d| matches!(d, b'0' | b'1' | b'_' | b'x' | b'z'),
                    b'o' | b'O' => |d| matches!(d, b'0'..=b'7' | b'_' | b'x' | b'z'),
                    b'd' | b'D' => |d| d.is_ascii_digit() || d == b'_',
                    b'h' | b'H' => |d| d.is_ascii_hexdigit() || matches!(d, b'_' | b'x' | b'z'),
                    _ => |_| false,
                };
                let mut e = r + 1;
                while e < self.b.len() && ok(self.b[e]) {
                    e += 1;
                }
                if e > r + 1 {
                    self.p = e;
                    return tok(T::Const, e);
                }
            }
            // `{SIGN}?[0-9]+` — no underscores in a plain integer.
            let mut e = digits_start;
            while e < self.b.len() && self.b[e].is_ascii_digit() {
                e += 1;
            }
            if e > digits_start {
                self.p = e;
                return tok(T::Int, e);
            }
            if c == b'\'' {
                return Err(format!("line {line}: syntax error at '"));
            }
        }
        if c == b'(' && self.peek(1) == b'*' {
            self.p += 2;
            return tok(T::AttrOpen, self.p);
        }
        if c == b'*' && self.peek(1) == b')' {
            self.p += 2;
            return tok(T::AttrClose, self.p);
        }
        if c == b'"' {
            let mut q = self.p + 1;
            while q < self.b.len() && self.b[q] != b'"' {
                if self.b[q] == b'\n' {
                    return Err(format!("line {line}: unterminated quoted string"));
                }
                q += 1;
            }
            if q >= self.b.len() {
                return Err(format!("line {line}: unterminated quoted string"));
            }
            self.p = q + 1;
            return Ok(Tok { t: T::Str, s: s + 1, e: q, line });
        }
        if b":.{}[],*;=-+|()".contains(&c) {
            self.p += 1;
            return tok(T::Punct(c), self.p);
        }
        // ⛔ The reference's scanner has no rule for `#` (so `#(...)` parameter values become a
        // syntax error further on — `bind_param_sv` in the corpus) and silently echoes any other
        // stray character. Both are refused here, with the character.
        Err(format!("line {line}: unexpected character '{}'", c as char))
    }
}

// =================================================================================================
// Parse tree of ONE module — the reference's VerilogModule / VerilogStmt / VerilogNet
// =================================================================================================

/// A net expression (`VerilogNet`).
#[derive(Debug, Clone)]
enum Expr {
    Scalar(String),
    Bit(String, i64),
    Part(String, i64, i64),
    /// Bits, least significant first (`VerilogConstantValue`).
    Const(Vec<bool>),
    Concat(Vec<Expr>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Dir {
    Input,
    Output,
    Inout,
    Tristate,
    Ground,
    Power,
    Internal,
    Unknown,
}

impl Dir {
    /// `Verilog2db::staToDb(PortDirection*)`.
    fn io(self) -> IoType {
        match self {
            Dir::Input | Dir::Unknown => IoType::Input,
            Dir::Output | Dir::Tristate => IoType::Output,
            Dir::Inout | Dir::Ground | Dir::Power | Dir::Internal => IoType::Inout,
        }
    }
    fn is_port_dir(self) -> bool {
        matches!(self, Dir::Input | Dir::Output | Dir::Inout)
    }
}

#[derive(Debug, Clone)]
struct DclArg {
    name: String,
    assign: Option<(Expr, Expr)>,
}

#[derive(Debug, Clone)]
struct Dcl {
    dir: Dir,
    range: Option<(i64, i64)>,
    args: Vec<DclArg>,
    line: u32,
}

#[derive(Debug, Clone)]
enum Pins {
    None,
    Named(Vec<(String, Option<Expr>)>),
    Ordered(Vec<Expr>),
}

#[derive(Debug, Clone)]
struct Inst {
    cell: String,
    name: String,
    pins: Pins,
    dont_touch: Option<String>,
    /// The `src` attribute (the last one wins, as `setProperty` overwrites).
    src: Option<String>,
    line: u32,
}

#[derive(Debug, Clone)]
enum Stmt {
    Inst(Inst),
    Assign(Expr, Expr, u32),
    Dcl(usize),
}

/// A header port-list entry.
#[derive(Debug, Clone)]
enum HeaderPort {
    Named(String),
    /// A port expression with no identifier (`{a, b}`): no port (warning 2721).
    Unnamed,
}

#[derive(Debug, Clone)]
struct Module {
    name: String,
    header: Vec<HeaderPort>,
    dcls: Vec<Dcl>,
    /// `VerilogModule::dcl_map_`: name -> index into `dcls`.
    dcl_map: HashMap<String, usize>,
    stmts: Vec<Stmt>,
    /// A `wand` / `wor` declaration (the reference fails the whole read, error 171).
    unsupported: Option<(String, u32)>,
    warnings: Vec<(u32, String)>,
    /// The module's own `implements_operator` attribute (on the cell, not an instance; the last
    /// one wins).
    implements_operator: Option<String>,
}

/// A cell port after `makeCellPorts`: a scalar or a bus with its declared range.
#[derive(Debug, Clone)]
struct Port {
    name: String,
    dir: Dir,
    bus: Option<(i64, i64)>,
}

impl Port {
    /// The bit ports, in member order (`from` towards `to`).
    fn bits(&self) -> Vec<String> {
        match self.bus {
            Some((f, t)) => range_bits(&self.name, f, t),
            None => vec![self.name.clone()],
        }
    }
}

struct Parser<'a> {
    lx: Lexer<'a>,
    la: Vec<Tok>,
}

impl<'a> Parser<'a> {
    fn new(b: &'a [u8], line: u32) -> Self {
        Parser { lx: Lexer::new(b, line), la: Vec::new() }
    }
    fn peek(&mut self, k: usize) -> Result<Tok, String> {
        while self.la.len() <= k {
            let t = self.lx.next()?;
            self.la.push(t);
        }
        Ok(self.la[k])
    }
    fn bump(&mut self) -> Result<Tok, String> {
        let t = self.peek(0)?;
        self.la.remove(0);
        Ok(t)
    }
    fn text(&self, t: Tok) -> &'a [u8] {
        &self.lx.b[t.s..t.e]
    }
    fn err<X>(&self, t: Tok, what: &str) -> Result<X, String> {
        let near = String::from_utf8_lossy(&self.lx.b[t.s..t.e.min(t.s + 40)]).into_owned();
        Err(format!("line {}: syntax error, {what} (near '{near}')", t.line))
    }
    fn expect(&mut self, c: u8) -> Result<Tok, String> {
        let t = self.bump()?;
        if t.t != T::Punct(c) {
            return self.err(t, &format!("expected '{}'", c as char));
        }
        Ok(t)
    }
    fn is(&mut self, c: u8) -> Result<bool, String> {
        Ok(self.peek(0)?.t == T::Punct(c))
    }
    fn id(&mut self) -> Result<String, String> {
        let t = self.bump()?;
        if t.t != T::Id {
            return self.err(t, "expected an identifier");
        }
        Ok(verilog_to_sta(self.text(t)))
    }
    fn int(&mut self) -> Result<i64, String> {
        let t = self.bump()?;
        if t.t != T::Int {
            return self.err(t, "expected an integer");
        }
        // `atol` of the token — a plain integer with an optional sign.
        let s = String::from_utf8_lossy(self.text(t)).into_owned();
        s.parse::<i64>().or_else(|_| self.err(t, "integer out of range"))
    }

    /// `attr_instance_seq`: the attributes, as (key, value); a key alone is "1".
    fn attrs(&mut self) -> Result<Vec<(String, String)>, String> {
        let mut out = Vec::new();
        while self.peek(0)?.t == T::AttrOpen {
            self.bump()?;
            loop {
                let key = self.id()?;
                let mut value = "1".to_string();
                if self.is(b'=')? {
                    self.bump()?;
                    let v = self.bump()?;
                    value = match v.t {
                        T::Const | T::Str => String::from_utf8_lossy(self.text(v)).into_owned(),
                        // `std::to_string($1)` of the scanned int.
                        T::Int => String::from_utf8_lossy(self.text(v)).trim_start_matches('+').parse::<i64>().map(|n| n.to_string()).unwrap_or_default(),
                        _ => return self.err(v, "bad attribute value"),
                    };
                }
                out.push((key, value));
                let t = self.bump()?;
                match t.t {
                    T::Punct(b',') => continue,
                    T::AttrClose => break,
                    _ => return self.err(t, "expected ',' or '*)' in an attribute"),
                }
            }
        }
        Ok(out)
    }

    /// `net_expr`.
    fn expr(&mut self) -> Result<Expr, String> {
        let t = self.peek(0)?;
        match t.t {
            T::Punct(b'{') => {
                self.bump()?;
                let mut v = vec![self.expr()?];
                while self.is(b',')? {
                    self.bump()?;
                    v.push(self.expr()?);
                }
                self.expect(b'}')?;
                Ok(Expr::Concat(v))
            }
            T::Const => {
                self.bump()?;
                Ok(Expr::Const(constant_bits(self.text(t))?))
            }
            T::Id => {
                let name = self.id()?;
                if self.is(b'[')? {
                    self.bump()?;
                    let a = self.int()?;
                    if self.is(b':')? {
                        self.bump()?;
                        let b = self.int()?;
                        self.expect(b']')?;
                        return Ok(Expr::Part(name, a, b));
                    }
                    self.expect(b']')?;
                    return Ok(Expr::Bit(name, a));
                }
                Ok(Expr::Scalar(name))
            }
            _ => self.err(t, "expected a net expression"),
        }
    }

    fn skip_to_semicolon(&mut self) -> Result<(), String> {
        loop {
            let t = self.bump()?;
            match t.t {
                T::Punct(b';') => return Ok(()),
                T::Eof => return self.err(t, "missing ';'"),
                _ => {}
            }
        }
    }

    /// One `dcl_arg`: a name, or `name = net_expr`.
    fn dcl_arg(&mut self) -> Result<DclArg, String> {
        let t = self.peek(0)?;
        if t.t == T::Punct(b'{') {
            // `net_assignment` with a concatenation on the left has no name to declare.
            return self.err(t, "a declaration assigning to a concatenation is not supported");
        }
        let name = self.id()?;
        if self.is(b'=')? {
            self.bump()?;
            let rhs = self.expr()?;
            return Ok(DclArg { name: name.clone(), assign: Some((Expr::Scalar(name), rhs)) });
        }
        Ok(DclArg { name, assign: None })
    }

    fn range_opt(&mut self) -> Result<Option<(i64, i64)>, String> {
        if !self.is(b'[')? {
            return Ok(None);
        }
        self.bump()?;
        let a = self.int()?;
        self.expect(b':')?;
        let b = self.int()?;
        self.expect(b']')?;
        Ok(Some((a, b)))
    }

    /// The whole module, from `module` to `endmodule`.
    fn module(&mut self) -> Result<Module, String> {
        let attrs = self.attrs()?;
        let implements_operator = attrs.iter().rev().find(|(k, _)| k == "implements_operator").map(|(_, v)| v.clone());
        let t = self.bump()?;
        if t.t != T::Kw(Kw::Module) {
            return self.err(t, "expected 'module'");
        }
        let name = self.id()?;
        let mut m = Module {
            name,
            header: Vec::new(),
            dcls: Vec::new(),
            dcl_map: HashMap::new(),
            stmts: Vec::new(),
            unsupported: None,
            warnings: Vec::new(),
            implements_operator,
        };
        let mut ansi: Vec<usize> = Vec::new();
        if self.is(b'(')? {
            self.bump()?;
            if self.is(b')')? {
                self.bump()?;
            } else {
                let k = self.peek(0)?;
                let first_after_attrs = if k.t == T::AttrOpen { T::Kw(Kw::Input) } else { k.t };
                if matches!(first_after_attrs, T::Kw(Kw::Input) | T::Kw(Kw::Output) | T::Kw(Kw::Inout)) {
                    self.port_dcls(&mut m, &mut ansi)?;
                } else {
                    self.port_list(&mut m)?;
                }
                self.expect(b')')?;
            }
        }
        self.expect(b';')?;
        self.stmts(&mut m)?;
        // ANSI port declarations are appended to the statements AFTER the body (`makeModule`).
        for d in ansi {
            for a in &m.dcls[d].args {
                m.header.push(HeaderPort::Named(a.name.clone()));
            }
            m.stmts.push(Stmt::Dcl(d));
        }
        build_dcl_map(&mut m);
        Ok(m)
    }

    fn port_dcls(&mut self, m: &mut Module, ansi: &mut Vec<usize>) -> Result<(), String> {
        loop {
            let k = self.peek(0)?;
            let starts_dcl = k.t == T::AttrOpen || matches!(k.t, T::Kw(Kw::Input) | T::Kw(Kw::Output) | T::Kw(Kw::Inout));
            if starts_dcl {
                let _ = self.attrs()?;
                let t = self.bump()?;
                let dir = match t.t {
                    T::Kw(Kw::Input) => Dir::Input,
                    T::Kw(Kw::Output) => Dir::Output,
                    T::Kw(Kw::Inout) => Dir::Inout,
                    _ => return self.err(t, "expected a port direction"),
                };
                let n = self.peek(0)?;
                match (dir, n.t) {
                    (_, T::Kw(Kw::Wire)) => {
                        self.bump()?;
                    }
                    (Dir::Output | Dir::Inout, T::Kw(Kw::Reg)) => {
                        self.bump()?;
                    }
                    _ => {}
                }
                if self.peek(0)?.t == T::Kw(Kw::Signed) {
                    self.bump()?;
                }
                let range = self.range_opt()?;
                let arg = self.dcl_arg()?;
                m.dcls.push(Dcl { dir, range, args: vec![arg], line: t.line });
                ansi.push(m.dcls.len() - 1);
            } else {
                // `port_dcls ',' dcl_arg` — another name for the previous declaration.
                let arg = self.dcl_arg()?;
                let Some(&last) = ansi.last() else { return self.err(k, "expected a port declaration") };
                m.dcls[last].args.push(arg);
            }
            if !self.is(b',')? {
                return Ok(());
            }
            self.bump()?;
        }
    }

    fn port_list(&mut self, m: &mut Module) -> Result<(), String> {
        loop {
            let t = self.peek(0)?;
            match t.t {
                T::Punct(b'.') => return self.err(t, "a named port expression in the module header (a bundle port) is not supported"),
                T::Punct(b'{') => {
                    let _ = self.expr()?;
                    m.header.push(HeaderPort::Unnamed);
                }
                _ => match self.expr()? {
                    Expr::Scalar(n) => m.header.push(HeaderPort::Named(n)),
                    Expr::Bit(n, i) => m.header.push(HeaderPort::Named(bit_name(&n, i))),
                    _ => return self.err(t, "a part select in the module header is not supported"),
                },
            }
            if !self.is(b',')? {
                return Ok(());
            }
            self.bump()?;
        }
    }

    fn stmts(&mut self, m: &mut Module) -> Result<(), String> {
        let mut inst_names: HashSet<String> = HashSet::new();
        loop {
            let attrs = self.attrs()?;
            let t = self.peek(0)?;
            match t.t {
                T::Kw(Kw::Endmodule) => {
                    self.bump()?;
                    return Ok(());
                }
                T::Kw(Kw::Parameter) | T::Kw(Kw::Defparam) => {
                    self.bump()?;
                    self.skip_to_semicolon()?;
                }
                T::Kw(Kw::Specify) => loop {
                    let x = self.bump()?;
                    match x.t {
                        T::Kw(Kw::Endspecify) => break,
                        T::Eof => return self.err(x, "missing endspecify"),
                        _ => {}
                    }
                },
                T::Kw(k @ (Kw::Input | Kw::Inout | Kw::Output | Kw::Supply0 | Kw::Supply1 | Kw::Tri | Kw::Wand | Kw::Wire | Kw::Wor)) => {
                    self.bump()?;
                    let dir = match k {
                        Kw::Input => Dir::Input,
                        Kw::Inout => Dir::Inout,
                        Kw::Output => Dir::Output,
                        Kw::Supply0 => Dir::Ground,
                        Kw::Supply1 => Dir::Power,
                        Kw::Tri => Dir::Tristate,
                        _ => Dir::Internal,
                    };
                    if self.peek(0)?.t == T::Kw(Kw::Signed) {
                        self.bump()?;
                    }
                    let range = self.range_opt()?;
                    let mut args = vec![self.dcl_arg()?];
                    while self.is(b',')? {
                        self.bump()?;
                        args.push(self.dcl_arg()?);
                    }
                    self.expect(b';')?;
                    if matches!(k, Kw::Wand | Kw::Wor) {
                        if m.unsupported.is_none() {
                            let kind = if k == Kw::Wand { "wand" } else { "wor" };
                            m.unsupported = Some((kind.to_string(), t.line));
                        }
                        continue;
                    }
                    // `makeDcl`: a SCALAR wire declaration keeps only its args with an assign
                    // ("they just eat memory"); a bus wire declaration is kept whole.
                    if dir == Dir::Internal && range.is_none() {
                        args.retain(|a| a.assign.is_some());
                        if args.is_empty() {
                            continue;
                        }
                    }
                    m.dcls.push(Dcl { dir, range, args, line: t.line });
                    m.stmts.push(Stmt::Dcl(m.dcls.len() - 1));
                }
                T::Kw(Kw::Assign) => {
                    self.bump()?;
                    loop {
                        let lt = self.peek(0)?;
                        let lhs = self.expr()?;
                        if matches!(lhs, Expr::Const(_)) {
                            return self.err(lt, "a constant on the left of an assign");
                        }
                        self.expect(b'=')?;
                        let rhs = self.expr()?;
                        m.stmts.push(Stmt::Assign(lhs, rhs, lt.line));
                        if !self.is(b',')? {
                            break;
                        }
                        self.bump()?;
                    }
                    self.expect(b';')?;
                }
                T::Id => {
                    let cell = self.id()?;
                    if self.peek(0)?.t != T::Id {
                        let n = self.peek(0)?;
                        return self.err(n, "expected an instance name");
                    }
                    let mut name = self.id()?;
                    self.expect(b'(')?;
                    let pins = self.inst_pins()?;
                    self.expect(b')')?;
                    self.expect(b';')?;
                    // `VerilogModule::checkInstanceName`: a repeated name gets `_1`, `_2`, ….
                    if inst_names.contains(&name) {
                        let mut i = 1;
                        let mut r = format!("{name}_{i}");
                        while inst_names.contains(&r) {
                            i += 1;
                            r = format!("{name}_{i}");
                        }
                        m.warnings.push((1396, format!("instance name {name} duplicated - renamed to {r}.")));
                        name = r;
                    }
                    inst_names.insert(name.clone());
                    let dont_touch = attrs.iter().rev().find(|(k, _)| k == "dont_touch").map(|(_, v)| v.clone());
                    let src = attrs.iter().rev().find(|(k, _)| k == "src").map(|(_, v)| v.clone());
                    m.stmts.push(Stmt::Inst(Inst { cell, name, pins, dont_touch, src, line: t.line }));
                }
                T::Eof => return self.err(t, "missing endmodule"),
                _ => return self.err(t, "unexpected token in a module body"),
            }
        }
    }

    fn inst_pins(&mut self) -> Result<Pins, String> {
        if self.is(b')')? {
            return Ok(Pins::None);
        }
        if self.is(b'.')? {
            let mut v = Vec::new();
            loop {
                self.expect(b'.')?;
                let port = self.id()?;
                if self.is(b'[')? {
                    let t = self.peek(0)?;
                    return self.err(t, "a bit or part select on an instance port is not supported");
                }
                self.expect(b'(')?;
                let net = if self.is(b')')? { None } else { Some(self.expr()?) };
                self.expect(b')')?;
                v.push((port, net));
                if !self.is(b',')? {
                    break;
                }
                self.bump()?;
            }
            return Ok(Pins::Named(v));
        }
        let mut v = vec![self.expr()?];
        while self.is(b',')? {
            self.bump()?;
            v.push(self.expr()?);
        }
        Ok(Pins::Ordered(v))
    }
}

/// `VerilogModule::parseDcl` over the declaration statements in order: which declaration a name
/// keeps when it is declared twice (a `wire` declaration modifies an earlier port declaration, a
/// `tri` one an output, a supply one any port).
fn build_dcl_map(m: &mut Module) {
    for st in &m.stmts {
        let Stmt::Dcl(d) = st else { continue };
        let dcl = &m.dcls[*d];
        for a in &dcl.args {
            if a.assign.is_some() {
                continue;
            }
            match m.dcl_map.get(&a.name) {
                Some(&e) => {
                    let ex = m.dcls[e].dir;
                    if ex == Dir::Internal {
                        m.dcl_map.insert(a.name.clone(), *d);
                    } else if dcl.dir == Dir::Tristate {
                        if ex == Dir::Output {
                            m.dcl_map.insert(a.name.clone(), *d);
                        }
                    } else if matches!(dcl.dir, Dir::Power | Dir::Ground) && ex.is_port_dir() {
                        m.dcl_map.insert(a.name.clone(), *d);
                    } else if dcl.dir != Dir::Internal {
                        m.warnings.push((1395, format!("signal {} previously declared on line {}.", a.name, m.dcls[e].line)));
                    }
                }
                None => {
                    m.dcl_map.insert(a.name.clone(), *d);
                }
            }
        }
    }
}

/// `VerilogNetConstant::parseConstant` — the bits of a sized constant, least significant first.
/// An unsized constant is one bit (warning 2724 in the reference); `x` and `z` digits read as 0; a
/// signed base (`'sd`) is not a base the reference knows, and reads as all zeros (warning 1861).
fn constant_bits(c: &[u8]) -> Result<Vec<bool>, String> {
    let q = c.iter().position(|&b| b == b'\'').ok_or_else(|| "constant without a base".to_string())?;
    let size_text = String::from_utf8_lossy(&c[..q]).into_owned();
    let size = if size_text.is_empty() {
        1
    } else {
        match size_text.parse::<i64>() {
            Ok(n) if n > 0 => n as usize,
            _ => 1,
        }
    };
    if size > 1 << 20 {
        return Err(format!("constant {} is wider than this reader takes", String::from_utf8_lossy(c)));
    }
    let mut bits = vec![false; size];
    let base_idx = q + 1;
    let (radix, per_digit) = match c.get(base_idx) {
        Some(b'b' | b'B') => (2, 1),
        Some(b'o' | b'O') => (8, 3),
        Some(b'h' | b'H') => (16, 4),
        Some(b'd' | b'D') => {
            let digits: String = c[base_idx + 1..].iter().filter(|&&d| d != b'_').map(|&d| d as char).collect();
            // `constant10Max` = u64::MAX; anything larger is warning 1397 and reads as zero.
            if let Ok(v) = digits.parse::<u64>() {
                for (bit, b) in bits.iter_mut().enumerate() {
                    *b = bit < 64 && (v >> bit) & 1 == 1;
                }
            }
            return Ok(bits);
        }
        _ => return Ok(bits),
    };
    // Scan from the least significant digit; past the digits, '0'.
    let mut bit = 0;
    let mut idx = c.len() - 1;
    while bit < size {
        let ch = if idx > base_idx {
            let ch = c[idx];
            idx -= 1;
            ch
        } else {
            b'0'
        };
        if ch != b'_' {
            let digit = (ch as char).to_digit(radix).unwrap_or(0);
            for b in 0..per_digit {
                if bit >= size {
                    break;
                }
                bits[bit] = digit & (1 << b) != 0;
                bit += 1;
            }
        }
    }
    Ok(bits)
}

impl Module {
    /// `VerilogModule::declaration`.
    fn dcl(&self, name: &str) -> Option<&Dcl> {
        self.dcl_map.get(name).map(|&d| &self.dcls[d])
    }

    /// `VerilogNet::size`.
    fn size(&self, e: &Expr) -> usize {
        match e {
            Expr::Scalar(n) => match self.dcl(n) {
                Some(Dcl { range: Some((f, t)), .. }) => ((t - f).unsigned_abs() + 1) as usize,
                _ => 1,
            },
            Expr::Bit(..) => 1,
            Expr::Part(_, f, t) => ((t - f).unsigned_abs() + 1) as usize,
            Expr::Const(b) => b.len(),
            Expr::Concat(v) => v.iter().map(|x| self.size(x)).sum(),
        }
    }

    /// `VerilogNet::nameIterator`: the net name of each bit, in order. A constant's bits, most
    /// significant first, name the instance's `one_` / `zero_` nets (R7).
    fn names(&self, e: &Expr, out: &mut Vec<String>) {
        match e {
            Expr::Scalar(n) => match self.dcl(n) {
                Some(Dcl { range: Some((f, t)), .. }) => out.extend(range_bits(n, *f, *t)),
                _ => out.push(n.clone()),
            },
            Expr::Bit(n, i) => out.push(bit_name(n, *i)),
            Expr::Part(n, f, t) => out.extend(range_bits(n, *f, *t)),
            Expr::Const(b) => out.extend(b.iter().rev().map(|&v| if v { ONE.to_string() } else { ZERO.to_string() })),
            Expr::Concat(v) => {
                for x in v {
                    self.names(x, out);
                }
            }
        }
    }

    /// `makeCellPorts`: the cell's ports in header order, a repeated name once (warning 165).
    fn ports(&self) -> (Vec<Port>, Vec<(u32, String)>) {
        let mut seen = HashSet::new();
        let mut ports = Vec::new();
        let mut warnings = Vec::new();
        for h in &self.header {
            match h {
                HeaderPort::Unnamed => warnings.push((2721, format!("module {} port expression without a port identifier is not supported.", self.name))),
                HeaderPort::Named(n) => {
                    if !seen.insert(n.clone()) {
                        warnings.push((165, format!("module {} repeated port name {n}.", self.name)));
                        continue;
                    }
                    match self.dcl(n) {
                        Some(d) => ports.push(Port { name: n.clone(), dir: d.dir, bus: d.range }),
                        None => {
                            warnings.push((166, format!("module {} missing declaration for port {n}.", self.name)));
                            ports.push(Port { name: n.clone(), dir: Dir::Unknown, bus: None });
                        }
                    }
                }
            }
        }
        (ports, warnings)
    }
}

const ZERO: &str = "zero_";
const ONE: &str = "one_";

// =================================================================================================
// Index pass — where each module's text is, without keeping it
// =================================================================================================

#[derive(Debug, Clone)]
struct ModuleEntry {
    name: String,
    start: u64,
    end: u64,
    line: u32,
}

/// Find each module's byte range (`module` .. `endmodule`) with a line scanner that knows only
/// comments, strings and escaped identifiers — enough not to be fooled by either keyword inside
/// them. A module defined twice keeps its LAST definition, as `VerilogReader::makeModule` replaces
/// the earlier cell.
fn index_modules(path: &str) -> Result<Vec<ModuleEntry>, String> {
    let f = File::open(path).map_err(|e| format!("{path}: {e}"))?;
    let mut r = BufReader::with_capacity(1 << 20, f);
    let mut line = Vec::new();
    let mut off: u64 = 0;
    let mut lineno: u32 = 0;
    let mut in_comment = false;
    let mut open: Option<(u64, u32)> = None;
    // The first `(*` of the attributes before a `module`: they belong to the module (its
    // `implements_operator`), so its range starts there.
    let mut attrs_at: Option<(u64, u32)> = None;
    let mut want_name = false;
    let mut out: Vec<ModuleEntry> = Vec::new();
    loop {
        line.clear();
        let n = r.read_until(b'\n', &mut line).map_err(|e| format!("{path}: {e}"))?;
        if n == 0 {
            break;
        }
        lineno += 1;
        let b = &line[..];
        let mut p = 0;
        if !in_comment {
            let first = b.iter().position(|&c| c != b' ' && c != b'\t');
            if first.is_some_and(|i| b[i] == b'`') {
                off += n as u64;
                continue;
            }
        }
        while p < b.len() {
            if in_comment {
                if b[p] == b'*' && b.get(p + 1) == Some(&b'/') {
                    in_comment = false;
                    p += 2;
                } else {
                    p += 1;
                }
                continue;
            }
            let c = b[p];
            if c == b'/' && b.get(p + 1) == Some(&b'/') {
                break;
            }
            if c == b'/' && b.get(p + 1) == Some(&b'*') {
                in_comment = true;
                p += 2;
                continue;
            }
            if c == b'(' && b.get(p + 1) == Some(&b'*') && open.is_none() && attrs_at.is_none() {
                attrs_at = Some((off + p as u64, lineno));
            }
            if c == b'"' {
                p += 1;
                while p < b.len() && b[p] != b'"' {
                    p += 1;
                }
                p += 1;
                continue;
            }
            if c == b'\\' {
                let s = p;
                while p < b.len() && !matches!(b[p], b' ' | b'\t' | b'\r' | b'\n') {
                    p += 1;
                }
                if want_name {
                    want_name = false;
                    if let Some((st, l)) = open {
                        out.push(ModuleEntry { name: verilog_to_sta(&b[s..(p + 1).min(b.len())]), start: st, end: 0, line: l });
                    }
                }
                continue;
            }
            if is_id_start(c) {
                let s = p;
                while p < b.len() && is_id_char(b[p]) {
                    p += 1;
                }
                let w = &b[s..p];
                if want_name {
                    want_name = false;
                    if let Some((st, l)) = open {
                        out.push(ModuleEntry { name: String::from_utf8_lossy(w).into_owned(), start: st, end: 0, line: l });
                    }
                } else if w == b"module" {
                    if open.is_some() {
                        return Err(format!("{path}: line {lineno}: 'module' inside a module"));
                    }
                    open = Some(attrs_at.take().unwrap_or((off + s as u64, lineno)));
                    want_name = true;
                } else if w == b"endmodule" {
                    let Some(_) = open.take() else {
                        return Err(format!("{path}: line {lineno}: 'endmodule' without 'module'"));
                    };
                    match out.last_mut() {
                        Some(m) if m.end == 0 => m.end = off + p as u64,
                        _ => return Err(format!("{path}: line {lineno}: module without a name")),
                    }
                }
                continue;
            }
            p += 1;
        }
        off += n as u64;
    }
    if in_comment {
        return Err(format!("{path}: unterminated comment"));
    }
    if open.is_some() {
        return Err(format!("{path}: missing endmodule"));
    }
    Ok(out)
}

// =================================================================================================
// The link — one depth-first walk, then the nets
// =================================================================================================

/// One local net of an instance (`ConcreteNet`), in that instance's frame.
struct LocalNet {
    name: String,
    /// Global union-find node (nets joined across the hierarchy).
    g: u32,
    /// Local merge chain (`mergedInto`): the survivor of an `assign` is its RIGHT-hand side.
    up: u32,
    /// The net reaches a port of its instance (`hasTerminals`); terms move to the survivor.
    term: bool,
    zero: bool,
    one: bool,
    /// `-hier` only: its terms (port bits of its instance, interned) in `ConcreteNet::terms_`
    /// list order — head insertion, and a merge moves the merged net's list over one by one.
    terms: Vec<u32>,
}

/// One instance's frame: its binding table (`VerilogBindingTbl`) and nets.
struct Frame {
    nets: Vec<LocalNet>,
    by_name: HashMap<String, u32>,
}

/// A child of the current instance, after its pins were bound in this frame.
struct Child {
    name: String,
    kind: ChildKind,
    dont_touch: bool,
    src: Option<String>,
}

enum ChildKind {
    /// A library cell: its master, and (pin, local net) per connected pin bit.
    Leaf { master: String, pins: Vec<(String, u32)> },
    /// A module of the file: its index, and (port bit, local net) per connected port bit.
    Module { module: usize, ports: Vec<(String, u32)> },
}

/// A leaf pin met on the walk: its net's global node, its instance (creation order) and its pin.
#[derive(Clone, Copy)]
struct PinRec {
    g: u32,
    leaf: u32,
    pin: u32,
}

/// A top-level port bit: its net's global node, name and direction.
struct TopPin {
    g: u32,
    name: String,
    io: IoType,
}

/// A naming candidate: a net `makeDbNets` would reach, in its visiting order.
struct Candidate {
    g: u32,
    name: String,
    sig: SigType,
}

struct Linker<'s, S: Sink> {
    sink: &'s mut S,
    file: File,
    modules: Vec<ModuleEntry>,
    module_by_name: HashMap<String, usize>,
    /// Ports per module (small; always kept).
    ports: HashMap<usize, Rc<Vec<Port>>>,
    /// Parsed bodies kept for later instances, within `cache_bytes` of source text.
    cache: HashMap<usize, Rc<Module>>,
    cache_left: usize,
    instance_count: Vec<u32>,
    masters: HashMap<String, Option<Rc<LeafCell>>>,
    /// Global union-find over every local net met.
    g: Vec<u32>,
    pins: Vec<PinRec>,
    pin_names: Vec<String>,
    pin_ids: HashMap<String, u32>,
    leaf_names: String,
    leaf_ends: Vec<usize>,
    top_pins: Vec<TopPin>,
    candidates: Vec<Candidate>,
    dont_touch: Vec<u32>,
    report: LinkReport,
    buf: Vec<u8>,
    /// Every module name in the database (the top's included), and the `_N` counters of
    /// [`unique_module_name`] (`_dbBlock::module_name_id_map_`).
    module_names: HashSet<String>,
    module_name_ids: HashMap<String, u32>,
    /// `implements_operator` per module of the file.
    implements_operator: Vec<Option<String>>,
    /// `Verilog2db::src_file_id_`: file name -> id, in order of first use.
    src_file_ids: HashMap<String, i32>,
    omit_filename_prop: bool,
    hierarchy: bool,
    /// `-hier`: interned names and the module-net calls, recorded on the walk.
    strs: Interner,
    ops: Vec<ModOp>,
}

/// Interned strings (module, net, instance, pin and port names of the module-net calls).
#[derive(Default)]
struct Interner {
    all: Vec<String>,
    ids: HashMap<String, u32>,
}

impl Interner {
    fn id(&mut self, s: &str) -> u32 {
        if let Some(&i) = self.ids.get(s) {
            return i;
        }
        let i = self.all.len() as u32;
        self.all.push(s.to_string());
        self.ids.insert(s.to_string(), i);
        i
    }
    fn get(&self, i: u32) -> &str {
        &self.all[i as usize]
    }
}

#[derive(Clone, Copy)]
enum OpKind {
    Net,
    ITerm,
    BTerm,
    ModITerm,
    ModBTerm,
}

/// One module-net call: find-or-create `net` in `module`, or join a terminal (`a`, `b`) to it.
#[derive(Clone, Copy)]
struct ModOp {
    kind: OpKind,
    module: u32,
    net: u32,
    a: u32,
    b: u32,
}

/// A pin on a local net, as `constructModNet` resolves it.
#[derive(Clone, Copy)]
enum PinRef {
    Leaf { inst: u32, pin: u32 },
    Child { local: u32, bit: u32 },
}

#[derive(Default)]
struct NetView {
    /// (path name, pin), sorted by path name.
    pins: Vec<(String, PinRef)>,
}

/// One level's nets for the module-net calls.
struct FrameView {
    nets: HashMap<u32, NetView>,
    /// (local name, module of the file) per module child.
    module_children: Vec<(String, usize)>,
}

/// The parent side of a module instance being entered (`-hier`).
struct Up<'a> {
    frame: &'a Frame,
    view: &'a FrameView,
    /// The parent's module (`""` = top).
    module: &'a str,
    /// The instance's local name.
    local: &'a str,
    /// (port bit, parent local net) per connected bit.
    ports: &'a [(String, u32)],
}

/// A library master's pins, grouped as the timing network groups them: `A[0]` .. `A[7]` are bus
/// `A`, its members most significant first (probe 3: `.addr_in(ram_addr)` puts `ram_addr[8]` on
/// `addr_in[8]`).
struct LeafCell {
    /// name -> member pins in member order (one for a scalar pin).
    ports: HashMap<String, Vec<String>>,
}

fn leaf_cell(pins: &[String]) -> LeafCell {
    let mut ports: HashMap<String, Vec<String>> = HashMap::new();
    let mut buses: HashMap<String, Vec<(i64, String)>> = HashMap::new();
    for p in pins {
        ports.insert(p.clone(), vec![p.clone()]);
        if let Some(open) = p.rfind('[') {
            if p.ends_with(']') && open > 0 && !p[..open].ends_with('\\') {
                if let Ok(i) = p[open + 1..p.len() - 1].parse::<i64>() {
                    buses.entry(p[..open].to_string()).or_default().push((i, p.clone()));
                }
            }
        }
    }
    for (base, mut bits) in buses {
        bits.sort_by_key(|b| std::cmp::Reverse(b.0));
        ports.entry(base).or_insert_with(|| bits.into_iter().map(|(_, n)| n).collect());
    }
    LeafCell { ports }
}

fn uf_find(g: &mut [u32], mut x: u32) -> u32 {
    while g[x as usize] != x {
        let p = g[x as usize];
        g[x as usize] = g[p as usize];
        x = p;
    }
    x
}

impl Frame {
    fn root(&self, mut x: u32) -> u32 {
        while self.nets[x as usize].up != x {
            x = self.nets[x as usize].up;
        }
        x
    }
}

impl<S: Sink> Linker<'_, S> {
    fn new_g(&mut self) -> u32 {
        let id = self.g.len() as u32;
        self.g.push(id);
        id
    }

    fn union(&mut self, a: u32, b: u32) {
        let (ra, rb) = (uf_find(&mut self.g, a), uf_find(&mut self.g, b));
        if ra != rb {
            self.g[ra as usize] = rb;
        }
    }

    /// `VerilogBindingTbl::ensureNetBinding`: the surviving net of `name`, made if unbound. A net
    /// made under the constant names is a constant net (R7).
    fn ensure(&mut self, f: &mut Frame, name: &str) -> u32 {
        if let Some(&id) = f.by_name.get(name) {
            return f.root(id);
        }
        let id = f.nets.len() as u32;
        let g = self.new_g();
        f.nets.push(LocalNet { name: name.to_string(), g, up: id, term: false, zero: name == ZERO, one: name == ONE, terms: Vec::new() });
        f.by_name.insert(name.to_string(), id);
        id
    }

    /// `ConcreteNetwork::mergeInto(lhs, rhs)`: the left net merges INTO the right one, which keeps
    /// its name and gains the left one's terms (R5).
    fn merge(&mut self, f: &mut Frame, lhs: u32, rhs: u32) {
        let (a, b) = (f.root(lhs), f.root(rhs));
        if a == b {
            return;
        }
        f.nets[a as usize].up = b;
        if f.nets[a as usize].term {
            f.nets[b as usize].term = true;
        }
        // `ConcreteNet::mergeInto`: each term of the merged net, in its list order, is added at
        // the HEAD of the survivor's list — the moved block arrives reversed, in front.
        if !f.nets[a as usize].terms.is_empty() {
            let moved = std::mem::take(&mut f.nets[a as usize].terms);
            let dst = &mut f.nets[b as usize].terms;
            let mut v: Vec<u32> = moved.into_iter().rev().collect();
            v.append(dst);
            *dst = v;
        }
        let (ga, gb) = (f.nets[a as usize].g, f.nets[b as usize].g);
        self.union(ga, gb);
    }

    fn warn(&mut self, code: u32, msg: String) {
        self.report.warnings.push((code, msg));
    }

    /// The parsed module `idx`: from the cache, or its text re-read and parsed.
    fn module(&mut self, idx: usize) -> Result<Rc<Module>, String> {
        if let Some(m) = self.cache.get(&idx) {
            self.report.bodies_cached += 1;
            return Ok(m.clone());
        }
        let e = &self.modules[idx];
        let len = (e.end - e.start) as usize;
        self.buf.resize(len, 0);
        self.file.read_exact_at(&mut self.buf, e.start).map_err(|x| format!("reading module {}: {x}", e.name))?;
        let m = Parser::new(&self.buf, e.line).module().map_err(|x| format!("module {}: {x}", e.name))?;
        self.report.bodies_parsed += 1;
        let m = Rc::new(m);
        if self.instance_count[idx] > 1 && len <= self.cache_left {
            self.cache_left -= len;
            self.cache.insert(idx, m.clone());
        }
        Ok(m)
    }

    fn module_ports(&mut self, idx: usize) -> Result<Rc<Vec<Port>>, String> {
        if let Some(p) = self.ports.get(&idx) {
            return Ok(p.clone());
        }
        let m = self.module(idx)?;
        let (ports, _) = m.ports();
        let p = Rc::new(ports);
        self.ports.insert(idx, p.clone());
        Ok(p)
    }

    fn leaf(&mut self, master: &str) -> Option<Rc<LeafCell>> {
        if let Some(c) = self.masters.get(master) {
            return c.clone();
        }
        let c = self.sink.master_pins(master).map(|p| Rc::new(leaf_cell(&p)));
        self.masters.insert(master.to_string(), c.clone());
        c
    }

    fn pin_id(&mut self, pin: &str) -> u32 {
        if let Some(&i) = self.pin_ids.get(pin) {
            return i;
        }
        let i = self.pin_names.len() as u32;
        self.pin_names.push(pin.to_string());
        self.pin_ids.insert(pin.to_string(), i);
        i
    }

    /// `makeNamedInstPins` / `makeOrderedInstPins` → `makeInstPin`, for one instance of the
    /// module `m`: each connected port bit and the local net it binds.
    fn bind_pins(&mut self, f: &mut Frame, m: &Module, inst: &Inst, ports: &[(String, Vec<String>)], path: &str) -> Vec<(String, u32)> {
        let mut out = Vec::new();
        let mut names = Vec::new();
        let mut bind = |this: &mut Self, f: &mut Frame, bits: &[String], expr: &Expr, out: &mut Vec<(String, u32)>| {
            names.clear();
            m.names(expr, &mut names);
            for (bit, net) in bits.iter().zip(names.iter()) {
                let id = this.ensure(f, net);
                out.push((bit.clone(), id));
            }
        };
        let where_ = if path.is_empty() { inst.name.clone() } else { format!("{path}/{}", inst.name) };
        match &inst.pins {
            Pins::None => {}
            Pins::Named(v) => {
                for (port, net) in v {
                    let Some((_, bits)) = ports.iter().find(|(n, _)| n == port) else {
                        self.warn(201, format!("instance {where_} port {port} not found."));
                        continue;
                    };
                    let Some(expr) = net else { continue };
                    let size = m.size(expr);
                    if size != bits.len() {
                        self.warn(200, format!("instance {where_} port {port} size {} does not match net size {size}.", bits.len()));
                        continue;
                    }
                    bind(self, f, bits, expr, &mut out);
                }
            }
            Pins::Ordered(v) => {
                for ((port, bits), expr) in ports.iter().zip(v.iter()) {
                    let size = m.size(expr);
                    if size != bits.len() {
                        self.warn(202, format!("instance {where_} port {port} size {} does not match net size {size}.", bits.len()));
                        continue;
                    }
                    bind(self, f, bits, expr, &mut out);
                }
            }
        }
        out
    }

    /// One instance of module `idx` at `path` (empty for the top): its nets, then its children in
    /// name order (R1), recursing into module children. `bound`: (port bit, parent net node) per
    /// port bit its parent connected.
    fn instance(&mut self, idx: usize, path: &str, bound: &[(String, u32)], module: &str, up: Option<Up<'_>>) -> Result<(), String> {
        let m = self.module(idx)?;
        let mut f = Frame { nets: Vec::new(), by_name: HashMap::new() };
        let top = path.is_empty();
        if top {
            // `linkNetwork`: a net per top port bit, its terminal the top pin (R8).
            let (ports, w) = m.ports();
            self.report.warnings.extend(w);
            let mut seen = HashSet::new();
            for p in &ports {
                for bit in p.bits() {
                    if seen.insert(bit.clone()) {
                        let id = self.ensure(&mut f, &bit);
                        f.nets[id as usize].term = true;
                        if self.hierarchy {
                            let t = self.strs.id(&bit);
                            f.nets[id as usize].terms.insert(0, t);
                        }
                        let g = f.nets[id as usize].g;
                        self.top_pins.push(TopPin { g, name: bit, io: p.dir.io() });
                    }
                }
            }
        } else {
            // `makeInstPin` (non-leaf): the child-side net of each connected port bit, with a term.
            for (bit, pg) in bound {
                let id = self.ensure(&mut f, bit);
                f.nets[id as usize].term = true;
                if self.hierarchy {
                    let t = self.strs.id(bit);
                    f.nets[id as usize].terms.insert(0, t);
                }
                let g = f.nets[id as usize].g;
                self.union(g, *pg);
            }
        }

        // `makeModuleInstBody`: the statements in order.
        let mut children: Vec<Child> = Vec::new();
        let mut l = Vec::new();
        let mut r = Vec::new();
        for st in &m.stmts {
            match st {
                Stmt::Inst(inst) => {
                    let child = self.child(&mut f, &m, inst, path)?;
                    children.push(child);
                }
                Stmt::Assign(lhs, rhs, line) => self.assign(&mut f, &m, lhs, rhs, *line, &mut l, &mut r),
                Stmt::Dcl(d) => {
                    let dcl = &m.dcls[*d];
                    for a in &dcl.args {
                        if let Some((lhs, rhs)) = &a.assign {
                            self.assign(&mut f, &m, lhs, rhs, dcl.line, &mut l, &mut r);
                        }
                        // A supply declaration makes its net a constant one (R11).
                        if matches!(dcl.dir, Dir::Ground | Dir::Power) {
                            let id = self.ensure(&mut f, &a.name);
                            if dcl.dir == Dir::Ground {
                                f.nets[id as usize].zero = true;
                            } else {
                                f.nets[id as usize].one = true;
                            }
                        }
                    }
                }
            }
        }

        // `makeDbNets` at this level: its surviving nets by name (R3), a child's net that reaches
        // a port of its instance left to the parent (R4).
        let mut roots: Vec<u32> = (0..f.nets.len() as u32).filter(|&i| f.nets[i as usize].up == i).collect();
        roots.sort_by(|&a, &b| f.nets[a as usize].name.as_bytes().cmp(f.nets[b as usize].name.as_bytes()));
        for id in roots {
            let n = &f.nets[id as usize];
            if !top && n.term {
                continue;
            }
            let sig = if n.zero {
                SigType::Ground
            } else if n.one {
                SigType::Power
            } else {
                SigType::Signal
            };
            let name = if top { n.name.clone() } else { format!("{path}/{}", n.name) };
            self.candidates.push(Candidate { g: n.g, name, sig });
        }

        // `makeChildInsts`: the children in byte order of name (R1), depth first.
        children.sort_by(|a, b| a.name.as_bytes().cmp(b.name.as_bytes()));
        // `-hier`: this level's nets as `constructModNet` sees them, then this instance's own
        // `makeModNetsForSubmodule` (its pairs come in creation order: pre-order, as here).
        let view = if self.hierarchy { Some(self.frame_view(&f, &children, path)) } else { None };
        if let (Some(up), Some(view)) = (&up, &view) {
            self.mod_nets_for_submodule(up, idx, module, &f, view)?;
        }
        for c in children {
            let cpath = if top { c.name.clone() } else { format!("{path}/{}", c.name) };
            match c.kind {
                ChildKind::Leaf { master, pins } => {
                    self.sink.inst(&master, &cpath, module)?;
                    // Yosys's `src` attribute (`storeLineInfo`), unless `-omit_filename_prop`.
                    if !self.omit_filename_prop {
                        if let Some(src) = &c.src {
                            self.store_line_info(src, &cpath)?;
                        }
                    }
                    let leaf = self.leaf_ends.len() as u32;
                    self.leaf_names.push_str(&cpath);
                    self.leaf_ends.push(self.leaf_names.len());
                    self.report.insts += 1;
                    if c.dont_touch {
                        self.dont_touch.push(leaf);
                    }
                    for (pin, id) in pins {
                        let g = f.nets[id as usize].g;
                        let pin = self.pin_id(&pin);
                        self.pins.push(PinRec { g, leaf, pin });
                    }
                }
                ChildKind::Module { module: child, ports: child_ports } => {
                    self.report.module_instances += 1;
                    // `makeDbModule` (always, `-hier` or not): a uniquely named module per
                    // instance, `original_name` when renamed, the module instance in its parent,
                    // and the cell's `implements_operator` on it.
                    let cell = self.modules[child].name.clone();
                    let name = unique_module_name(&cell, &c.name, &mut self.module_names, &mut self.module_name_ids);
                    self.sink.module(&name)?;
                    self.report.modules += 1;
                    if name != cell {
                        self.sink.string_property(Owner::Module(&name), "original_name", &cell)?;
                    }
                    self.sink.mod_inst(module, &name, &c.name)?;
                    if let Some(op) = self.implements_operator[child].clone() {
                        self.sink.string_property(Owner::ModInst(module, &c.name), "implements_operator", &op)?;
                    }
                    if self.hierarchy {
                        self.mod_bterms(child, &name)?;
                        self.mod_iterms(child, module, &c.name)?;
                    }
                    let bound: Vec<(String, u32)> = child_ports.iter().map(|(bit, id)| (bit.clone(), f.nets[*id as usize].g)).collect();
                    let up = view.as_ref().map(|v| Up { frame: &f, view: v, module, local: &c.name, ports: &child_ports });
                    self.instance(child, &cpath, &bound, &name, up)?;
                }
            }
        }
        self.sink.module_done(module)
    }

    /// `makeModBTerms` (`-hier`): per port of the cell in port order, a scalar is one module port;
    /// a bus is a root port, its `dbBusPort(from, to)`, then one port per bit from `from` towards
    /// `to` (the first set as the bus's members, the last as its last). Then the module's port
    /// list is REVERSED (odb inserts at the head, so this puts it back in creation order).
    fn mod_bterms(&mut self, cell: usize, module: &str) -> Result<(), String> {
        let ports = self.module_ports(cell)?;
        for p in ports.iter() {
            let io = p.dir.io();
            self.sink.mod_bterm(module, &p.name, io)?;
            if let Some((from, to)) = p.bus {
                self.sink.bus_port(module, &p.name, from, to)?;
                let bits = p.bits();
                for (i, b) in bits.iter().enumerate() {
                    self.sink.mod_bterm(module, b, io)?;
                    if i == 0 {
                        self.sink.bus_port_member(module, &p.name, b, false)?;
                    }
                    if i + 1 == bits.len() {
                        self.sink.bus_port_member(module, &p.name, b, true)?;
                    }
                }
            }
        }
        self.sink.mod_bterms_done(module)
    }

    /// `makeModITerms` (`-hier`): one module-instance pin per module port that is not a bus
    /// root, in the port list's (creation) order: scalars and bus bits.
    fn mod_iterms(&mut self, cell: usize, parent: &str, modinst: &str) -> Result<(), String> {
        let ports = self.module_ports(cell)?;
        for p in ports.iter() {
            for b in p.bits() {
                self.sink.mod_iterm(parent, modinst, &b)?;
            }
        }
        Ok(())
    }

    /// `storeLineInfo`: string property `src` on the instance; when the value reads as
    /// `<file>:<line>.<col>-<line>.<col>`, int properties `src_file_id` (the file's id, a block
    /// string property `src_file_<id>` made on its first use) and `src_file_line`.
    fn store_line_info(&mut self, attr: &str, inst: &str) -> Result<(), String> {
        if attr.is_empty() {
            return Ok(());
        }
        self.sink.string_property(Owner::Inst(inst), "src", attr)?;
        let Some((file, line)) = line_info(attr) else {
            return Ok(());
        };
        let line: i32 = line.parse().map_err(|_| format!("instance {inst}: src line number {line} out of range"))?;
        let id = match self.src_file_ids.get(file) {
            Some(&id) => id,
            None => {
                let id = self.src_file_ids.len() as i32;
                self.src_file_ids.insert(file.to_string(), id);
                self.sink.string_property(Owner::Block, &format!("src_file_{id}"), file)?;
                id
            }
        };
        self.sink.int_property(Owner::Inst(inst), "src_file_id", id)?;
        self.sink.int_property(Owner::Inst(inst), "src_file_line", line)
    }

    /// `mergeAssignNet`: bit by bit, the left net into the right (R5).
    #[allow(clippy::too_many_arguments)]
    fn assign(&mut self, f: &mut Frame, m: &Module, lhs: &Expr, rhs: &Expr, line: u32, l: &mut Vec<String>, r: &mut Vec<String>) {
        let (ls, rs) = (m.size(lhs), m.size(rhs));
        if ls != rs {
            self.warn(203, format!("module {} line {line}: assign left hand side size {ls} not equal right hand size {rs}.", m.name));
            return;
        }
        l.clear();
        r.clear();
        m.names(lhs, l);
        m.names(rhs, r);
        for (a, b) in l.iter().zip(r.iter()) {
            let x = self.ensure(f, a);
            let y = self.ensure(f, b);
            self.merge(f, x, y);
        }
    }

    /// One instance statement of the current frame, its pins bound here.
    fn child(&mut self, f: &mut Frame, m: &Module, inst: &Inst, path: &str) -> Result<Child, String> {
        let dont_touch = inst.dont_touch.as_deref().is_some_and(|v| dont_touch_value(v, &mut self.report, path, &inst.name));
        // A module of the file wins over a library cell of the same name (`findAnyCell` looks in
        // the Verilog library first).
        if let Some(&idx) = self.module_by_name.get(&inst.cell) {
            let ports = self.module_ports(idx)?;
            let bits: Vec<(String, Vec<String>)> = ports.iter().map(|p| (p.name.clone(), p.bits())).collect();
            let bound = self.bind_pins(f, m, inst, &bits, path);
            return Ok(Child { name: inst.name.clone(), kind: ChildKind::Module { module: idx, ports: bound }, dont_touch, src: None });
        }
        let Some(cell) = self.leaf(&inst.cell) else {
            let full = if path.is_empty() { inst.name.clone() } else { format!("{path}/{}", inst.name) };
            // ORD-2013: the reference makes a black box, then refuses it as a master.
            return Err(format!("line {}: instance {full} LEF master {} not found", inst.line, inst.cell));
        };
        if matches!(inst.pins, Pins::Ordered(_)) {
            return Err(format!("line {}: instance {} of {} connects by position; a library cell's pin order is not in the netlist", inst.line, inst.name, inst.cell));
        }
        let mut ports: Vec<(String, Vec<String>)> = Vec::new();
        if let Pins::Named(v) = &inst.pins {
            for (p, _) in v {
                if let Some(bits) = cell.ports.get(p) {
                    ports.push((p.clone(), bits.clone()));
                }
            }
        }
        let pins = self.bind_pins(f, m, inst, &ports, path);
        Ok(Child { name: inst.name.clone(), kind: ChildKind::Leaf { master: inst.cell.clone(), pins }, dont_touch, src: inst.src.clone() })
    }

    /// `makeDbNets` after the walk: each net at its first naming alias, its terminals then its
    /// pins in path order (R5, R6, R10).
    fn nets(&mut self) -> Result<(), String> {
        let n = self.g.len();
        for x in 0..n as u32 {
            uf_find(&mut self.g, x);
        }
        // Pin order (`PinPathNameLess`): instance path level by level, then pin name. A leaf's
        // creation index already IS its path order — the walk visits each level's children in
        // byte order and a leaf is never another leaf's ancestor — so the key is (index, pin).
        let mut rank = vec![0u32; self.pin_names.len()];
        let mut order: Vec<u32> = (0..self.pin_names.len() as u32).collect();
        order.sort_by(|&a, &b| self.pin_names[a as usize].as_bytes().cmp(self.pin_names[b as usize].as_bytes()));
        for (r, &i) in order.iter().enumerate() {
            rank[i as usize] = r as u32;
        }
        let g = &self.g;
        let mut pins = std::mem::take(&mut self.pins);
        for p in pins.iter_mut() {
            p.g = g[p.g as usize];
        }
        pins.sort_unstable_by_key(|p| (p.g, p.leaf, rank[p.pin as usize]));
        let mut start = vec![u32::MAX; n];
        for (i, p) in pins.iter().enumerate().rev() {
            start[p.g as usize] = i as u32;
        }
        let mut tops = std::mem::take(&mut self.top_pins);
        for t in tops.iter_mut() {
            t.g = g[t.g as usize];
        }
        tops.sort_by(|a, b| (a.g, a.name.as_bytes()).cmp(&(b.g, b.name.as_bytes())));
        let mut tstart = vec![u32::MAX; n];
        for (i, t) in tops.iter().enumerate().rev() {
            tstart[t.g as usize] = i as u32;
        }
        let mut made = vec![false; n];
        let candidates = std::mem::take(&mut self.candidates);
        for c in &candidates {
            let root = self.g[c.g as usize] as usize;
            if made[root] {
                continue;
            }
            made[root] = true;
            self.sink.net(&c.name, c.sig)?;
            self.report.nets += 1;
            if tstart[root] != u32::MAX {
                for t in tops[tstart[root] as usize..].iter().take_while(|t| t.g as usize == root) {
                    self.sink.bterm(&c.name, &t.name, t.io)?;
                    self.report.bterms += 1;
                }
            }
            if start[root] != u32::MAX {
                for p in pins[start[root] as usize..].iter().take_while(|p| p.g as usize == root) {
                    let leaf = p.leaf as usize;
                    let s = if leaf == 0 { 0 } else { self.leaf_ends[leaf - 1] };
                    let inst = &self.leaf_names[s..self.leaf_ends[leaf]];
                    self.sink.connect(inst, &self.pin_names[p.pin as usize], &c.name)?;
                    self.report.iterms += 1;
                }
            }
        }
        Ok(())
    }

    /// `setDoNotTouch` on the marked leaves, last (an instance that is dont_touch accepts no new
    /// connection).
    fn dont_touch(&mut self) -> Result<(), String> {
        for &leaf in &self.dont_touch {
            let leaf = leaf as usize;
            let s = if leaf == 0 { 0 } else { self.leaf_ends[leaf - 1] };
            self.sink.dont_touch(&self.leaf_names[s..self.leaf_ends[leaf]])?;
        }
        Ok(())
    }

    /// One level's nets as `constructModNet` reads them (`-hier`): per surviving net with a pin or
    /// a term, its pins keyed by path name (an ordered map: byte order) and its terms in list order.
    fn frame_view(&mut self, f: &Frame, children: &[Child], path: &str) -> FrameView {
        let mut nets: HashMap<u32, NetView> = HashMap::new();
        for c in children {
            let local = self.strs.id(&c.name);
            match &c.kind {
                ChildKind::Leaf { pins, .. } => {
                    let full = if path.is_empty() { c.name.clone() } else { format!("{path}/{}", c.name) };
                    let inst = self.strs.id(&full);
                    for (pin, id) in pins {
                        let key = format!("{}/{pin}", c.name);
                        let pin = self.strs.id(pin);
                        nets.entry(f.root(*id)).or_default().pins.push((key, PinRef::Leaf { inst, pin }));
                    }
                }
                ChildKind::Module { ports, .. } => {
                    for (bit, id) in ports {
                        let key = format!("{}/{bit}", c.name);
                        let bit = self.strs.id(bit);
                        nets.entry(f.root(*id)).or_default().pins.push((key, PinRef::Child { local, bit }));
                    }
                }
            }
        }
        for v in nets.values_mut() {
            v.pins.sort_by(|a, b| a.0.as_bytes().cmp(b.0.as_bytes()));
        }
        let module_children = children
            .iter()
            .filter_map(|c| match c.kind {
                ChildKind::Module { module, .. } => Some((c.name.clone(), module)),
                ChildKind::Leaf { .. } => None,
            })
            .collect();
        FrameView { nets, module_children }
    }

    /// `makeModNetsForSubmodule` for the module instance being entered (`-hier`): per pin of the
    /// instance with a net, in pin (port bit) order, the parent's module net of it, the pin joined
    /// to it, and at the top the ports on that net too; then per port of its module, in list
    /// order, the inside net of that name as a module net of the child, the port joined to it.
    fn mod_nets_for_submodule(&mut self, up: &Up<'_>, cell: usize, child_module: &str, f: &Frame, view: &FrameView) -> Result<(), String> {
        let parent = self.strs.id(up.module);
        let child = self.strs.id(child_module);
        let local = self.strs.id(up.local);
        let ports = self.module_ports(cell)?;
        // A bound bit's net in the parent (a bit bound twice keeps its last binding, as the
        // instance's pin array does).
        let mut bound: HashMap<&str, u32> = HashMap::new();
        for (bit, id) in up.ports {
            bound.insert(bit.as_str(), up.frame.root(*id));
        }
        // `staToDb(child_module, pin)` looks for a module instance OF THE CHILD named like this
        // instance (and its pin of this name) before it looks for the child's port: when one
        // exists, nothing is joined above.
        let shadow: HashSet<String> = match view.module_children.iter().find(|(n, _)| n == up.local) {
            Some(&(_, m)) => self.module_ports(m)?.iter().flat_map(|p| p.bits()).collect(),
            None => HashSet::new(),
        };
        for p in ports.iter() {
            for bit in p.bits() {
                let Some(&r) = bound.get(bit.as_str()) else { continue };
                let net = self.construct_mod_net(parent, up.frame, up.view, r);
                if !shadow.contains(&bit) {
                    let b = self.strs.id(&bit);
                    self.ops.push(ModOp { kind: OpKind::ModITerm, module: parent, net, a: local, b });
                }
                if up.module.is_empty() {
                    for &t in &up.frame.nets[r as usize].terms {
                        self.ops.push(ModOp { kind: OpKind::BTerm, module: parent, net, a: t, b: 0 });
                    }
                }
            }
        }
        for p in ports.iter() {
            let mut names = vec![p.name.clone()];
            if p.bus.is_some() {
                names.extend(p.bits());
            }
            for n in names {
                let Some(&id) = f.by_name.get(&n) else { continue };
                let r = f.root(id);
                let net = self.construct_mod_net(child, f, view, r);
                let a = self.strs.id(&n);
                self.ops.push(ModOp { kind: OpKind::ModBTerm, module: child, net, a, b: 0 });
            }
        }
        Ok(())
    }

    /// `constructModNet(net, module)`: the module net named as the net (made if absent), then its
    /// pins in path-name order — a leaf's iterm, a module instance's pin — then its terms in list
    /// order, each a port of the module (none at the top). Returns the module net's name.
    fn construct_mod_net(&mut self, module: u32, f: &Frame, view: &FrameView, r: u32) -> u32 {
        let net = self.strs.id(&f.nets[r as usize].name);
        self.ops.push(ModOp { kind: OpKind::Net, module, net, a: 0, b: 0 });
        if let Some(v) = view.nets.get(&r) {
            for (_, p) in &v.pins {
                let op = match *p {
                    PinRef::Leaf { inst, pin } => ModOp { kind: OpKind::ITerm, module, net, a: inst, b: pin },
                    PinRef::Child { local, bit } => ModOp { kind: OpKind::ModITerm, module, net, a: local, b: bit },
                };
                self.ops.push(op);
            }
        }
        if !self.strs.get(module).is_empty() {
            for &t in &f.nets[r as usize].terms {
                self.ops.push(ModOp { kind: OpKind::ModBTerm, module, net, a: t, b: 0 });
            }
        }
        net
    }

    /// The module-net calls recorded on the walk, made once every flat net exists.
    fn replay_mod_nets(&mut self) -> Result<(), String> {
        let ops = std::mem::take(&mut self.ops);
        let mut made: HashSet<(u32, u32)> = HashSet::new();
        for o in &ops {
            let (m, n) = (self.strs.get(o.module), self.strs.get(o.net));
            match o.kind {
                OpKind::Net => {
                    if made.insert((o.module, o.net)) {
                        self.report.mod_nets += 1;
                    }
                    self.sink.mod_net(m, n)?;
                    continue;
                }
                OpKind::ITerm => self.sink.mod_net_connect(m, n, Term::ITerm(self.strs.get(o.a), self.strs.get(o.b)))?,
                OpKind::BTerm => self.sink.mod_net_connect(m, n, Term::BTerm(self.strs.get(o.a)))?,
                OpKind::ModITerm => self.sink.mod_net_connect(m, n, Term::ModITerm(self.strs.get(o.a), self.strs.get(o.b)))?,
                OpKind::ModBTerm => self.sink.mod_net_connect(m, n, Term::ModBTerm(self.strs.get(o.a)))?,
            }
            self.report.mod_net_connects += 1;
        }
        Ok(())
    }
}

/// `dbModule::makeUniqueDbModule(cell, inst, block)`: the cell's own name when free; else
/// `<cell>_<inst>`, then `<cell>_<inst>_<N>` with a counter per base name (`module_name_id_map_`)
/// that starts at 1 and is bumped on every try, until a free name. `inst` is the instance's LOCAL
/// name. A name is taken by any module created before — a renamed one included — so a real
/// module named `sub_x` met after `sub` was renamed `sub_x` becomes `sub_x_x`.
fn unique_module_name(cell: &str, inst: &str, taken: &mut HashSet<String>, ids: &mut HashMap<String, u32>) -> String {
    if taken.insert(cell.to_string()) {
        return cell.to_string();
    }
    let base = format!("{cell}_{inst}");
    loop {
        let id = ids.entry(base.clone()).or_insert(0);
        let full = if *id > 0 { format!("{base}_{id}") } else { base.clone() };
        *id += 1;
        if taken.insert(full.clone()) {
            return full;
        }
    }
}

/// `Verilog2db::kLineInfoRe` = `^(.*):(\d+)\.\d+-\d+\.\d+$` (ECMAScript, `.` not a line
/// terminator): (file, line). The greedy `(.*)` ends at the LAST `:` — no earlier one can match,
/// as the tail admits no `:` — so `a.v:1.2-3.4|b.v:5.6-7.8` is file `a.v:1.2-3.4|b.v`, line 5.
fn line_info(attr: &str) -> Option<(&str, &str)> {
    let colon = attr.rfind(':')?;
    let (file, tail) = (&attr[..colon], &attr[colon + 1..]);
    if file.contains(['\n', '\r']) {
        return None;
    }
    let digits = |x: &str| !x.is_empty() && x.bytes().all(|c| c.is_ascii_digit());
    let (l1, rest) = tail.split_once('-')?;
    let (a, b) = l1.split_once('.')?;
    let (c, d) = rest.split_once('.')?;
    if digits(a) && digits(b) && digits(c) && digits(d) {
        Some((file, a))
    } else {
        None
    }
}

/// `dont_touch` attribute value: true/yes or a non-zero integer sets it, false/no or zero does
/// not; anything else is warning 2059 and ignored.
fn dont_touch_value(v: &str, report: &mut LinkReport, path: &str, inst: &str) -> bool {
    let l = v.to_ascii_lowercase();
    if l == "true" || l == "yes" {
        return true;
    }
    if l == "false" || l == "no" {
        return false;
    }
    match v.trim().parse::<i64>() {
        Ok(n) => n != 0,
        Err(_) => {
            let full = if path.is_empty() { inst.to_string() } else { format!("{path}/{inst}") };
            report.warnings.push((2059, format!("Ignoring unrecognized dont_touch value \"{v}\" on instance {full}.")));
            false
        }
    }
}

/// The names of the modules a file defines, in file order — from the index pass alone (no module
/// is parsed). A file with more than one is linked with [`link_file`].
pub fn module_names(path: &str) -> Result<Vec<String>, String> {
    Ok(index_modules(path)?.into_iter().map(|m| m.name).collect())
}

/// Link the netlist at `path` into `sink`, as `read_verilog` + `link_design <top>` does.
pub fn link_file<S: Sink>(path: &str, opts: &LinkOptions, sink: &mut S) -> Result<LinkReport, String> {
    let modules = index_modules(path)?;
    let file = File::open(path).map_err(|e| format!("{path}: {e}"))?;
    let mut module_by_name = HashMap::new();
    for (i, m) in modules.iter().enumerate() {
        module_by_name.insert(m.name.clone(), i);
    }
    // Instance counts per module (for the cache), and every module checked for syntax: the
    // reference reads the whole file before linking.
    let mut instance_count = vec![0u32; modules.len()];
    let mut buf = Vec::new();
    let mut used: HashSet<String> = HashSet::new();
    let mut implements_operator = Vec::with_capacity(modules.len());
    // A port direction other than unknown, per module (`dbVerilogNetwork::isBlackBox` is false).
    let mut not_black_box = Vec::with_capacity(modules.len());
    for e in &modules {
        buf.resize((e.end - e.start) as usize, 0);
        file.read_exact_at(&mut buf, e.start).map_err(|x| format!("{path}: {x}"))?;
        let m = Parser::new(&buf, e.line).module().map_err(|x| format!("{path}: module {}: {x}", e.name))?;
        if let Some((kind, line)) = &m.unsupported {
            return Err(format!("{path}: line {line}: unsupported net type '{kind}'"));
        }
        implements_operator.push(m.implements_operator.clone());
        not_black_box.push(m.ports().0.iter().any(|p| p.dir != Dir::Unknown));
        for st in &m.stmts {
            if let Stmt::Inst(i) = st {
                if let Some(&k) = module_by_name.get(&i.cell) {
                    instance_count[k] += 1;
                }
                used.insert(i.cell.clone());
            }
        }
    }
    let top = match &opts.top {
        Some(t) => t.clone(),
        None => {
            let mut tops: Vec<&String> = Vec::new();
            for m in &modules {
                if !used.contains(&m.name) && module_by_name.get(&m.name).is_some_and(|&i| modules[i].start == m.start) {
                    tops.push(&m.name);
                }
            }
            match tops.as_slice() {
                [one] => (*one).clone(),
                [] => return Err(format!("{path}: no top module (every module is instantiated by another)")),
                many => {
                    let list: Vec<&str> = many.iter().map(|s| s.as_str()).collect();
                    return Err(format!("{path}: several candidate top modules ({}); name one with --top", list.join(", ")));
                }
            }
        }
    };
    let Some(&top_idx) = module_by_name.get(&top) else {
        return Err(format!("{path}: {top} is not a verilog module."));
    };
    let mut lk = Linker {
        sink,
        file,
        module_by_name,
        ports: HashMap::new(),
        cache: HashMap::new(),
        cache_left: opts.cache_bytes,
        instance_count,
        masters: HashMap::new(),
        g: Vec::new(),
        pins: Vec::new(),
        pin_names: Vec::new(),
        pin_ids: HashMap::new(),
        leaf_names: String::new(),
        leaf_ends: Vec::new(),
        top_pins: Vec::new(),
        candidates: Vec::new(),
        dont_touch: Vec::new(),
        report: LinkReport { top: top.clone(), modules_in_file: modules.len(), ..Default::default() },
        buf: Vec::new(),
        modules,
        // The block's top module bears the top's name.
        module_names: HashSet::from([top.clone()]),
        module_name_ids: HashMap::new(),
        implements_operator,
        src_file_ids: HashMap::new(),
        omit_filename_prop: opts.omit_filename_prop,
        hierarchy: opts.hierarchy,
        strs: Interner::default(),
        ops: Vec::new(),
    };
    // Interned id 0 is the top module's name, `""`.
    lk.strs.id("");
    lk.sink.block(&top)?;
    // `recordBusPortsOrder`: a bool property per top bus port, true when declared msb first (R14).
    let topm = lk.module(top_idx)?;
    let (ports, _) = topm.ports();
    for p in &ports {
        if let Some((from, to)) = p.bus {
            lk.sink.bus_order(&format!("bus_msb_first {} {}", p.name, top), from > to)?;
        }
    }
    // Every module's own warnings (as the reference prints them at read time).
    lk.report.warnings.extend(topm.warnings.iter().cloned());
    drop(topm);
    lk.instance(top_idx, "", &[], "", None)?;
    lk.nets()?;
    // `makeModNetsForSubmodules` (`-hier`), after every flat net and before dont_touch.
    lk.replay_mod_nets()?;
    lk.dont_touch()?;
    // `processUnusedCells`: a module of the file with `implements_operator` that no module of
    // the database is named after (and not a black box) is linked into a CHILD BLOCK of its own.
    // Not built yet: refused rather than left out.
    for (i, m) in lk.modules.iter().enumerate() {
        let live = lk.module_by_name.get(&m.name) == Some(&i);
        if live && lk.implements_operator[i].is_some() && not_black_box[i] && !lk.module_names.contains(&m.name) {
            return Err(format!(
                "module {} (implements_operator) is not used under {top}; the reference links such a module into a child block, which is not supported yet",
                m.name
            ));
        }
    }
    Ok(lk.report)
}

// =================================================================================================
// The database as the sink
// =================================================================================================

/// Links straight into a database that already holds the technology and libraries.
pub struct DbSink<'a> {
    pub db: &'a mut crate::Db,
}

fn e(x: crate::Error) -> String {
    x.to_string()
}

impl Sink for DbSink<'_> {
    /// `makeBlock`: a chip if none, the block, the DEF units from the LEF and `[` `]` bus
    /// delimiters — the same calls as the flat import.
    fn block(&mut self, top: &str) -> Result<(), String> {
        self.db.create_chip(top, "", "DIE").map_err(e)?;
        self.db.create_chip_block(top, top).map_err(e)?;
        let units = self.db.tech_get_lef_units();
        if units > 0 {
            self.db.block_set_def_units(units).map_err(e)?;
        }
        self.db.set_bus_delimiters('[', ']').map_err(e)
    }
    fn master_pins(&mut self, master: &str) -> Option<Vec<String>> {
        if self.db.master_get_name(master).is_empty() {
            return None;
        }
        Some(self.db.master_get_m_terms(master))
    }
    fn module(&mut self, name: &str) -> Result<(), String> {
        self.db.module_create(name).map_err(|x| format!("module {name}: {x}"))
    }
    fn mod_inst(&mut self, parent: &str, module: &str, name: &str) -> Result<(), String> {
        self.db.mod_inst_create(parent, module, name).map_err(|x| format!("module instance {name} of {module}: {x}"))
    }
    fn inst(&mut self, master: &str, name: &str, module: &str) -> Result<(), String> {
        self.db.create_inst_in_module(master, name, module).map_err(|x| format!("instance {name} of {master}: {x}"))
    }
    fn module_done(&mut self, module: &str) -> Result<(), String> {
        self.db.module_unreverse_lists(module).map_err(e)
    }
    fn string_property(&mut self, owner: Owner<'_>, key: &str, value: &str) -> Result<(), String> {
        let r = match owner {
            Owner::Block => self.db.create_string_property("block", "", key, value),
            Owner::Inst(i) => self.db.create_string_property("inst", i, key, value),
            Owner::Module(m) => self.db.create_string_property("module", m, key, value),
            Owner::ModInst(parent, name) => self.db.create_mod_inst_string_property(parent, name, key, value),
        };
        r.map_err(e)
    }
    fn int_property(&mut self, owner: Owner<'_>, key: &str, value: i32) -> Result<(), String> {
        let r = match owner {
            Owner::Block => self.db.create_int_property("block", "", key, value),
            Owner::Inst(i) => self.db.create_int_property("inst", i, key, value),
            Owner::Module(m) => self.db.create_int_property("module", m, key, value),
            Owner::ModInst(..) => return Err(format!("int property {key} on a module instance: not used by the link")),
        };
        r.map_err(e)
    }
    fn mod_bterm(&mut self, module: &str, name: &str, io: IoType) -> Result<(), String> {
        self.db.mod_bterm_create(module, name, io.as_str()).map_err(e)
    }
    fn bus_port(&mut self, module: &str, root: &str, from: i64, to: i64) -> Result<(), String> {
        let r = |x: i64| i32::try_from(x).map_err(|_| format!("bus port {root} index {x} out of range"));
        self.db.bus_port_create(module, root, r(from)?, r(to)?).map_err(e)
    }
    fn bus_port_member(&mut self, module: &str, root: &str, member: &str, last: bool) -> Result<(), String> {
        self.db.bus_port_set_member(module, root, member, last).map_err(e)
    }
    fn mod_bterms_done(&mut self, module: &str) -> Result<(), String> {
        self.db.module_reverse_mod_bterms(module).map_err(e)
    }
    fn mod_iterm(&mut self, parent: &str, modinst: &str, name: &str) -> Result<(), String> {
        self.db.mod_iterm_create(parent, modinst, name).map_err(e)
    }
    fn mod_net(&mut self, module: &str, name: &str) -> Result<(), String> {
        self.db.mod_net_find_or_create(module, name).map(|_| ()).map_err(e)
    }
    fn mod_net_connect(&mut self, module: &str, net: &str, term: Term<'_>) -> Result<(), String> {
        let r = match term {
            Term::ITerm(i, p) => self.db.mod_net_connect(module, net, "iterm", i, p),
            Term::BTerm(b) => self.db.mod_net_connect(module, net, "bterm", b, ""),
            Term::ModITerm(i, p) => self.db.mod_net_connect(module, net, "moditerm", i, p),
            Term::ModBTerm(b) => self.db.mod_net_connect(module, net, "modbterm", b, ""),
        };
        r.map_err(|x| format!("module net {net} of {module:?}: {x}"))
    }
    fn net(&mut self, name: &str, sig: SigType) -> Result<(), String> {
        self.db.create_net(name).map_err(|x| format!("net {name}: {x}"))?;
        if sig == SigType::Signal {
            return Ok(());
        }
        #[cfg(feature = "gen-write")]
        {
            self.db.net_set_sig_type(name, sig.as_str()).map_err(e)
        }
        #[cfg(not(feature = "gen-write"))]
        {
            Err(format!("net {name} is a {} net; setting its type needs a build with the write surface (feature gen-write)", sig.as_str()))
        }
    }
    fn bterm(&mut self, net: &str, name: &str, io: IoType) -> Result<(), String> {
        self.db.create_bterm(net, name).map_err(|x| format!("port {name}: {x}"))?;
        self.db.bterm_set_io_type(name, io.as_str()).map_err(e)
    }
    fn connect(&mut self, inst: &str, pin: &str, net: &str) -> Result<(), String> {
        self.db.connect(inst, pin, net).map_err(|x| format!("{inst}/{pin} to {net}: {x}"))
    }
    fn dont_touch(&mut self, inst: &str) -> Result<(), String> {
        #[cfg(feature = "gen-write")]
        {
            self.db.inst_set_do_not_touch(inst, true).map_err(e)
        }
        #[cfg(not(feature = "gen-write"))]
        {
            Err(format!("instance {inst} is dont_touch; setting it needs a build with the write surface (feature gen-write)"))
        }
    }
    fn bus_order(&mut self, key: &str, msb_first: bool) -> Result<(), String> {
        self.db.block_set_bool_property(key, msb_first).map_err(e)
    }
}

// =================================================================================================
// Tests — each reference rule, stated, against a recording sink
// =================================================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    /// Records the calls in order, as the reference's dump prints the database: instances, then
    /// each net with its iterms and bterms LISTED AS odb LISTS THEM — head insertion, so in reverse
    /// of the order they were connected.
    #[derive(Default)]
    struct Rec {
        insts: Vec<String>,
        nets: Vec<(String, SigType, Vec<String>, Vec<String>)>,
        bterms: Vec<(String, IoType, String)>,
        dont_touch: Vec<String>,
        props: Vec<(String, bool)>,
        /// (instance, owning module) per instance.
        owner: Vec<(String, String)>,
        modules: Vec<String>,
        /// (parent, module, name) per module instance.
        mod_insts: Vec<(String, String, String)>,
        done: Vec<String>,
        /// `<owner> <key>=<value>` per property, in creation order.
        sprops: Vec<String>,
        /// Module ports, bus ports, module-instance pins, in call order.
        hier: Vec<String>,
        /// Module nets made and terminals joined to them, in call order.
        modnets: Vec<String>,
        made: HashSet<String>,
    }

    fn owner_str(o: Owner<'_>) -> String {
        match o {
            Owner::Block => "block".to_string(),
            Owner::Inst(i) => format!("inst:{i}"),
            Owner::Module(m) => format!("module:{m}"),
            Owner::ModInst(p, n) => format!("modinst:{p}:{n}"),
        }
    }

    fn lib_pins(m: &str) -> Option<Vec<String>> {
        let v: &[&str] = match m {
            "INV" => &["A", "Y"],
            "BUF" => &["A", "Y"],
            "NAND2" => &["A", "B", "Y"],
            "DFF" => &["CLK", "D", "QN"],
            "TIEHI" => &["H"],
            "RAM" => &["addr[0]", "addr[1]", "addr[2]", "clk", "q[0]", "q[1]"],
            _ => return None,
        };
        Some(v.iter().map(|s| s.to_string()).collect())
    }

    impl Sink for Rec {
        fn block(&mut self, _top: &str) -> Result<(), String> {
            Ok(())
        }
        fn master_pins(&mut self, master: &str) -> Option<Vec<String>> {
            lib_pins(master)
        }
        fn module(&mut self, name: &str) -> Result<(), String> {
            assert!(!self.modules.iter().any(|m| m == name), "module {name} created twice");
            self.modules.push(name.to_string());
            Ok(())
        }
        fn mod_inst(&mut self, parent: &str, module: &str, name: &str) -> Result<(), String> {
            self.mod_insts.push((parent.to_string(), module.to_string(), name.to_string()));
            Ok(())
        }
        fn inst(&mut self, master: &str, name: &str, module: &str) -> Result<(), String> {
            self.insts.push(format!("{name} {master}"));
            self.owner.push((name.to_string(), module.to_string()));
            Ok(())
        }
        fn module_done(&mut self, module: &str) -> Result<(), String> {
            self.done.push(module.to_string());
            Ok(())
        }
        fn string_property(&mut self, owner: Owner<'_>, key: &str, value: &str) -> Result<(), String> {
            self.sprops.push(format!("{} {key}={value}", owner_str(owner)));
            Ok(())
        }
        fn int_property(&mut self, owner: Owner<'_>, key: &str, value: i32) -> Result<(), String> {
            self.sprops.push(format!("{} {key}={value}", owner_str(owner)));
            Ok(())
        }
        fn mod_bterm(&mut self, module: &str, name: &str, io: IoType) -> Result<(), String> {
            self.hier.push(format!("{module} bterm {name} {}", io.as_str()));
            Ok(())
        }
        fn bus_port(&mut self, module: &str, root: &str, from: i64, to: i64) -> Result<(), String> {
            self.hier.push(format!("{module} bus {root} {from}:{to}"));
            Ok(())
        }
        fn bus_port_member(&mut self, module: &str, root: &str, member: &str, last: bool) -> Result<(), String> {
            self.hier.push(format!("{module} {} {root} {member}", if last { "last" } else { "members" }));
            Ok(())
        }
        fn mod_bterms_done(&mut self, module: &str) -> Result<(), String> {
            self.hier.push(format!("{module} reverse"));
            Ok(())
        }
        fn mod_iterm(&mut self, parent: &str, modinst: &str, name: &str) -> Result<(), String> {
            self.hier.push(format!("{parent}>{modinst} iterm {name}"));
            Ok(())
        }
        fn mod_net(&mut self, module: &str, name: &str) -> Result<(), String> {
            if self.made.insert(format!("{module}:{name}")) {
                self.modnets.push(format!("created {module}:{name}"));
            }
            Ok(())
        }
        fn mod_net_connect(&mut self, module: &str, net: &str, term: Term<'_>) -> Result<(), String> {
            let t = match term {
                Term::ITerm(i, p) => format!("iterm {i}/{p}"),
                Term::BTerm(b) => format!("bterm {b}"),
                Term::ModITerm(i, p) => format!("moditerm {i}/{p}"),
                Term::ModBTerm(b) => format!("modbterm {b}"),
            };
            self.modnets.push(format!("{module}:{net} {t}"));
            Ok(())
        }
        fn net(&mut self, name: &str, sig: SigType) -> Result<(), String> {
            self.nets.push((name.to_string(), sig, Vec::new(), Vec::new()));
            Ok(())
        }
        fn bterm(&mut self, net: &str, name: &str, io: IoType) -> Result<(), String> {
            let n = self.nets.iter_mut().find(|n| n.0 == net).expect("net made before its bterm");
            n.3.insert(0, name.to_string());
            self.bterms.push((name.to_string(), io, net.to_string()));
            Ok(())
        }
        fn connect(&mut self, inst: &str, pin: &str, net: &str) -> Result<(), String> {
            let n = self.nets.iter_mut().find(|n| n.0 == net).expect("net made before its pins");
            n.2.insert(0, format!("{inst}/{pin}"));
            Ok(())
        }
        fn dont_touch(&mut self, inst: &str) -> Result<(), String> {
            self.dont_touch.push(inst.to_string());
            Ok(())
        }
        fn bus_order(&mut self, key: &str, msb_first: bool) -> Result<(), String> {
            self.props.push((key.to_string(), msb_first));
            Ok(())
        }
    }

    impl Rec {
        fn net(&self, name: &str) -> &(String, SigType, Vec<String>, Vec<String>) {
            self.nets.iter().find(|n| n.0 == name).unwrap_or_else(|| panic!("no net {name}; nets: {:?}", self.net_names()))
        }
        fn net_names(&self) -> Vec<&str> {
            self.nets.iter().map(|n| n.0.as_str()).collect()
        }
        fn inst_names(&self) -> Vec<&str> {
            self.insts.iter().map(|i| i.split(' ').next().unwrap_or("")).collect()
        }
    }

    fn link(src: &str) -> Rec {
        link_opts(src, &LinkOptions::default())
    }

    fn link_opts(src: &str, opts: &LinkOptions) -> Rec {
        let mut rec = Rec::default();
        let path = tmp(src);
        let r = link_file(path.to_str().unwrap(), opts, &mut rec);
        let _ = std::fs::remove_file(&path);
        let _ = r.unwrap();
        rec
    }

    fn link_err(src: &str) -> String {
        let mut rec = Rec::default();
        let path = tmp(src);
        let r = link_file(path.to_str().unwrap(), &LinkOptions::default(), &mut rec);
        let _ = std::fs::remove_file(&path);
        r.expect_err("expected a refusal")
    }

    fn tmp(src: &str) -> std::path::PathBuf {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static N: AtomicUsize = AtomicUsize::new(0);
        let p = std::env::temp_dir().join(format!("vlink-{}-{}.v", std::process::id(), N.fetch_add(1, Ordering::Relaxed)));
        let mut f = File::create(&p).unwrap();
        f.write_all(src.as_bytes()).unwrap();
        p
    }

    /// Probe 1 of the design doc (cells renamed to the recording library).
    const PROBE1: &str = r"
module leafpair (a, b, y, z);
  input a, b; output y, z;
  wire n1;
  NAND2 u2 (.A(a), .B(b), .Y(n1));
  INV     u1 (.A(n1), .Y(y));
  BUF     u0 (.A(n1), .Y(z));
endmodule
module mid (clk, d, q, bus_in, bus_out, thru_in, thru_out);
  input clk; input [1:0] d; output q; input [3:0] bus_in; output [3:0] bus_out;
  input thru_in; output thru_out;
  wire w_y, w_z, \esc/name ;
  leafpair zz_second (.a(d[0]), .b(d[1]), .y(w_y), .z(\esc/name ));
  leafpair aa_first  (.a(bus_in[3]), .b(\esc/name ), .y(bus_out[0]), .z());
  DFF r0 (.CLK(clk), .D(w_y), .QN(q));
  assign bus_out[3:1] = bus_in[2:0];
  assign thru_out = thru_in;
endmodule
module top (clk, din, qout, bin, bout, t_in, t_out, k);
  input clk; input [1:0] din; output qout; input [3:0] bin; output [3:0] bout;
  input t_in; output t_out; output k;
  wire q1, q2, mid_thru;
  mid m_b (.clk(clk), .d(din), .q(q1), .bus_in(bin), .bus_out(bout), .thru_in(t_in), .thru_out(mid_thru));
  mid m_a (.clk(clk), .d({q1, din[0]}), .q(q2), .bus_in({bin[1:0], 2'b10}), .bus_out(), .thru_in(mid_thru), .thru_out(t_out));
  INV top_inv (.A(q2), .Y(qout));
  TIEHI tie (.H(k));
endmodule
";

    /// R1: instances depth first, each level's children in BYTE order of local name — `aa_first`
    /// before `zz_second`, `u0 u1 u2` though declared `u2 u1 u0`; module and leaf children
    /// interleave (`m_a/r0` between `m_a/aa_first/*` and `m_a/zz_second/*`).
    #[test]
    fn r1_instances_depth_first_in_byte_order() {
        let r = link(PROBE1);
        assert_eq!(
            r.inst_names(),
            [
                "m_a/aa_first/u0", "m_a/aa_first/u1", "m_a/aa_first/u2", "m_a/r0", "m_a/zz_second/u0", "m_a/zz_second/u1", "m_a/zz_second/u2",
                "m_b/aa_first/u0", "m_b/aa_first/u1", "m_b/aa_first/u2", "m_b/r0", "m_b/zz_second/u0", "m_b/zz_second/u1", "m_b/zz_second/u2",
                "tie", "top_inv"
            ]
        );
        // Byte order, not natural or case-folded order (probe 2): `D1 < b < c10 < c2`, `U3 < _x < u10 < u2`.
        let r = link("module t(a); input a; INV u2 (.A(a)); INV u10 (.A(a)); INV \\_x (.A(a)); INV U3 (.A(a)); INV c2 (.A(a)); INV c10 (.A(a)); INV b (.A(a)); INV D1 (.A(a)); endmodule");
        assert_eq!(r.inst_names(), ["D1", "U3", "_x", "b", "c10", "c2", "u10", "u2"]);
    }

    /// R2: a leaf's name is its path joined with `/`; inside an escaped identifier the divider and
    /// the bus brackets are escaped with `\` (`m_a/esc\/name`, `u_br\[3\]`).
    #[test]
    fn r2_path_names_escape_divider_and_brackets() {
        assert_eq!(verilog_to_sta(b"\\esc/name "), "esc\\/name");
        assert_eq!(verilog_to_sta(b"\\u_br[3] "), "u_br\\[3\\]");
        assert_eq!(verilog_to_sta(b"\\a\\b "), "a\\\\b");
        assert_eq!(verilog_to_sta(b"\\_x "), "_x");
        assert_eq!(verilog_to_sta(b"plain"), "plain");
        let r = link(PROBE1);
        assert!(r.net_names().contains(&"m_a/esc\\/name"));
        let r = link("module s(a); input a; BUF \\u_br[3]  (.A(a)); endmodule module t(a); input a; s s0 (.a(a)); endmodule");
        assert_eq!(r.inst_names(), ["s0/u_br\\[3\\]"]);
    }

    /// R3: nets per instance, top first then children in R1 order, each level's nets in byte order
    /// of local name (`c[0] c[10] c[1] c[2]`).
    #[test]
    fn r3_nets_top_first_then_children_by_name() {
        let r = link(PROBE1);
        assert_eq!(
            r.net_names(),
            [
                "bin[0]", "bin[1]", "bin[2]", "bin[3]", "bout[0]", "clk", "din[0]", "din[1]", "k", "mid_thru", "one_", "q1", "q2", "qout", "zero_",
                "m_a/bus_out[0]", "m_a/esc\\/name", "m_a/w_y", "m_a/aa_first/n1", "m_a/aa_first/z", "m_a/zz_second/n1",
                "m_b/esc\\/name", "m_b/w_y", "m_b/aa_first/n1", "m_b/aa_first/z", "m_b/zz_second/n1"
            ]
        );
        let r = link("module t(c); input [10:0] c; INV a (.A(c[2])); endmodule");
        assert_eq!(&r.net_names()[..4], ["c[0]", "c[10]", "c[1]", "c[2]"]);
    }

    /// R4: a child net that reaches a port of its instance is not made there — no `m_b/w_y`-style
    /// duplicate of a port net; the parent's net carries its pins.
    #[test]
    fn r4_child_port_nets_belong_to_the_parent() {
        let r = link(PROBE1);
        for n in r.net_names() {
            assert!(!n.ends_with("/a") && !n.ends_with("/b") && !n.ends_with("/y") && !n.ends_with("/clk"), "port net {n} made in a child");
        }
        assert_eq!(r.net("clk").2, ["m_b/r0/CLK", "m_a/r0/CLK"]);
    }

    /// R4 (probe 4): a port the parent leaves unconnected — omitted, or `.a()` — has no parent
    /// net, so the child makes one at its own level. (The R4 skip itself never decides output: a
    /// connected port's net is always claimed first by the parent, whose nets come before the
    /// child's in R3 order. It is kept because the reference skips there.)
    #[test]
    fn r4_unconnected_child_ports_get_a_child_net() {
        let r = link("module sub(a, b, y); input a, b; output y; wire w; INV u0 (.A(a), .Y(w)); \
             NAND2 u1 (.A(w), .B(b), .Y(y)); INV u2 (.A(y), .Y()); endmodule \
             module top(i, o); input i; output o; sub m_omit (.b(i), .y(o)); \
             sub m_empty (.a(), .b(i), .y()); sub m_pos (i); endmodule");
        assert_eq!(r.net_names(), ["i", "o", "m_empty/a", "m_empty/w", "m_empty/y", "m_omit/a",
                                   "m_omit/w", "m_pos/b", "m_pos/w", "m_pos/y"]);
        assert_eq!(r.net("m_empty/a").2, ["m_empty/u0/A"]);
        assert_eq!(r.net("o").2, ["m_omit/u2/A", "m_omit/u1/Y"]);
    }

    /// R5: aliases are one net named by the first alias in R3 order; an `assign` merges its LEFT
    /// net into its RIGHT one, so the left name is never iterated. `bout[1]` sits on `bin[0]`, the
    /// feedthrough `t_in`→`mid_thru`→`t_out` is `mid_thru`; `assign k = 1'b1` makes `one_`, not `k`.
    #[test]
    fn r5_aliases_name_by_first_alias_right_side_survives() {
        let r = link(PROBE1);
        assert_eq!(r.net("bin[0]").3, ["bout[1]", "bin[0]"]);
        assert_eq!(r.net("mid_thru").3, ["t_out", "t_in"]);
        let r = link("module t(k, a); output k; input a; assign k = 1'b1; INV u (.A(a), .Y(k)); endmodule");
        assert_eq!(r.net_names(), ["a", "one_"]);
        assert_eq!(r.net("one_").1, SigType::Power);
        // Same level: the survivor is the RIGHT side even when it sorts later.
        let r = link("module t(a); input a; wire x, y; assign x = y; INV u (.A(a), .Y(x)); endmodule");
        assert_eq!(r.net_names(), ["a", "y"]);
    }

    /// R5 (probe 3): a concatenation on the LEFT of an assign merges bit by bit, msb first.
    #[test]
    fn r5_concatenation_on_the_left_of_an_assign() {
        let r = link("module t(a, b, y, p); input [1:0] a; input b; output y, p; assign {y, p} = {b, a[1]}; endmodule");
        assert_eq!(r.net("b").3, ["y", "b"]);
        assert_eq!(r.net("a[1]").3, ["p", "a[1]"]);
    }

    /// R6: a net's pins are connected in path order — instance path level by level, then pin name —
    /// and odb lists them reversed (head insertion).
    #[test]
    fn r6_pins_in_path_order_listed_reversed() {
        let r = link(PROBE1);
        assert_eq!(r.net("m_a/aa_first/n1").2, ["m_a/aa_first/u2/Y", "m_a/aa_first/u1/A", "m_a/aa_first/u0/A"]);
        assert_eq!(r.net("q2").2, ["top_inv/A", "m_a/r0/QN"]);
        // Level by level, not by the joined string: `u/x` (child `x` of `u`) before `u!`, though
        // '!' sorts before '/'.
        let r = link("module s(a); input a; INV x (.A(a)); endmodule module t(a); input a; s u (.a(a)); INV \\u!  (.A(a)); endmodule");
        assert_eq!(r.net("a").2, ["u!/A", "u/x/A"]);
    }

    /// R7: a constant is a net `one_` / `zero_` of the instance that uses it, POWER / GROUND, made
    /// even with no pins (top `one_`, `zero_` of probe 1); a constant on a leaf pin in a child is
    /// `<path>/one_`.
    #[test]
    fn r7_constants_are_per_instance_supply_nets() {
        let r = link(PROBE1);
        assert_eq!((r.net("one_").1, r.net("one_").2.len()), (SigType::Power, 0));
        assert_eq!((r.net("zero_").1, r.net("zero_").2.len()), (SigType::Ground, 0));
        let r = link("module l(a); input a; INV U3 (.A(1'b1)); INV u2 (.A(1'b0)); endmodule module t(a); input a; l L (.a(a)); endmodule");
        assert_eq!(r.net("L/one_").1, SigType::Power);
        assert_eq!(r.net("L/one_").2, ["L/U3/A"]);
        assert_eq!(r.net("L/zero_").1, SigType::Ground);
    }

    /// R7: a constant inside a child that drives one of its ports is not a net of the child (the
    /// child's `zero_` reaches a port, R4); the parent's net carries it, under the parent's name and
    /// as a SIGNAL net — the sig type is the naming net's own (corpus `port_sign_extend`).
    #[test]
    fn r7_constant_driving_a_child_port_is_the_parents_signal_net() {
        let r = link("module g(o); output o; assign o = 1'b0; endmodule module t(a); output a; g x (.o(a)); endmodule");
        assert_eq!(r.net_names(), ["a"]);
        assert_eq!(r.net("a").1, SigType::Signal);
    }

    /// R8: a top port with no connection still gets its net and terminal.
    #[test]
    fn r8_unconnected_top_ports_get_a_net() {
        let r = link("module t(a, unused_in, unused_out); input a, unused_in; output unused_out; INV b (.A(a)); endmodule");
        assert_eq!(r.net("unused_in").3, ["unused_in"]);
        assert_eq!(r.net("unused_out").3, ["unused_out"]);
    }

    /// R9: a wire nothing references makes no net, and an unconnected pin (`.Y()`) makes none.
    #[test]
    fn r9_unreferenced_wires_make_no_net() {
        let r = link("module t(a); input a; wire dangling; INV u (.A(a), .Y()); endmodule");
        assert_eq!(r.net_names(), ["a"]);
    }

    /// R10: terminals are made on the first visit of their net, so terminal order follows net
    /// order (probe 1: `bin[0] bout[1] bin[1] bout[2] …`).
    #[test]
    fn r10_terminals_follow_net_order() {
        let r = link(PROBE1);
        let order: Vec<&str> = r.bterms.iter().map(|b| b.0.as_str()).collect();
        assert_eq!(order, ["bin[0]", "bout[1]", "bin[1]", "bout[2]", "bin[2]", "bout[3]", "bin[3]", "bout[0]", "clk", "din[0]", "din[1]", "k", "t_in", "t_out", "qout"]);
    }

    /// R11 (probe 3): `supply1` / `supply0` nets keep their declared name and are POWER / GROUND;
    /// `inout` ports are INOUT terminals.
    #[test]
    fn r11_supply_nets_and_inout() {
        let r = link("module s(io); inout io; supply1 vdd; supply0 vss; INV a (.A(vdd)); INV b (.A(vss)); BUF c (.A(io)); endmodule module t(io); inout io; s s0 (.io(io)); endmodule");
        assert_eq!(r.net("s0/vdd").1, SigType::Power);
        assert_eq!(r.net("s0/vss").1, SigType::Ground);
        assert_eq!(r.bterms[0].1, IoType::Inout);
    }

    /// R12 (probe 3): a library cell's bus pins connect member by member, most significant first, so
    /// `.addr(a)` puts `a[2]` on `addr[2]`; a constant's bits go msb first too.
    #[test]
    fn r12_macro_bus_pins_member_by_member() {
        let r = link("module t(a, clk); input [2:0] a; input clk; wire [1:0] q; RAM m (.addr(a), .clk(clk), .q(q)); endmodule");
        assert_eq!(r.net("a[2]").2, ["m/addr[2]"]);
        assert_eq!(r.net("a[0]").2, ["m/addr[0]"]);
        let r = link("module t(clk); input clk; RAM m (.addr(3'b101), .clk(clk)); endmodule");
        assert_eq!(r.net("one_").2, ["m/addr[2]", "m/addr[0]"]);
        // A port whose width the connection does not match is left unconnected (warning 200).
        let r = link("module t(a, clk); input [1:0] a; input clk; RAM m (.addr(a), .clk(clk)); endmodule");
        assert!(r.net("a[1]").2.is_empty());
    }

    /// R13 (probe 3): `dont_touch` on a leaf instance sets do-not-touch after the nets; "true",
    /// "yes" and non-zero integers set it.
    #[test]
    fn r13_dont_touch_attribute() {
        let r = link("module t(a); input a; (* dont_touch = \"true\", src = \"x.v:1\" *) INV u1 (.A(a)); (* dont_touch = 1 *) INV u2 (.A(a)); (* dont_touch = \"false\" *) INV u3 (.A(a)); endmodule");
        assert_eq!(r.dont_touch, ["u1", "u2"]);
    }

    /// R14: a bool property `bus_msb_first <port> <top>` per top bus port.
    #[test]
    fn r14_bus_port_order_properties() {
        let r = link("module t(a, b, c); input [3:0] a; input [0:1] b; input c; endmodule");
        assert_eq!(r.props, [("bus_msb_first a t".to_string(), true), ("bus_msb_first b t".to_string(), false)]);
    }

    /// Constants as the reference scans them: sized, msb padded with 0, `x`/`z` as 0, unsized one
    /// bit, a signed base all zeros.
    #[test]
    fn constants_read_as_the_reference_reads_them() {
        assert_eq!(constant_bits(b"2'b10").unwrap(), [false, true]);
        assert_eq!(constant_bits(b"4'h5").unwrap(), [true, false, true, false]);
        assert_eq!(constant_bits(b"3'b1").unwrap(), [true, false, false]);
        assert_eq!(constant_bits(b"'b0").unwrap(), [false]);
        assert_eq!(constant_bits(b"2'bx1").unwrap(), [true, false]);
        assert_eq!(constant_bits(b"32'sd1").unwrap(), vec![false; 32]);
        assert_eq!(constant_bits(b"4'd9").unwrap(), [true, false, false, true]);
        assert_eq!(constant_bits(b"3'o5").unwrap(), [true, false, true]);
    }

    /// Positional connections to a MODULE follow its header order (probe 3); to a library cell they
    /// are refused, the cell's pin order not being in the netlist.
    #[test]
    fn positional_connections() {
        let r = link("module p(x, y); input x; output y; INV u (.A(x), .Y(y)); endmodule module t(i, o); input i; output o; p p0 (i, o); endmodule");
        assert_eq!(r.net("i").2, ["p0/u/A"]);
        assert_eq!(r.net("o").2, ["p0/u/Y"]);
        assert!(link_err("module t(i); input i; INV u (i); endmodule").contains("position"));
    }

    /// Refusals: a missing master (ORD-2013), parameter values `#(` (the reference's syntax error),
    /// `wand` (error 171), an empty positional slot, and a syntax error anywhere in the file.
    #[test]
    fn refusals_are_refusals() {
        assert!(link_err("module t(a); input a; NOPE u (.A(a)); endmodule").contains("not found"));
        assert!(link_err("module t(a); input a; foo #(.doit(32'sd0)) f (.a(a)); endmodule").contains("'#'"));
        assert!(link_err("module t(a); input a; wand w; endmodule").contains("unsupported net type"));
        assert!(link_err("module t(a); input a; INV u (.A(a)) endmodule").contains("syntax error"));
        // An empty positional slot `(i, , )` is a syntax error in the reference (STA-0171).
        assert!(link_err("module s(a, b); input a, b; endmodule module t(i); input i; s m (i, ); endmodule")
            .contains("syntax error"));
    }

    /// A repeated instance name is renamed `_1` (`checkInstanceName`).
    #[test]
    fn duplicate_instance_names_are_renamed() {
        let r = link("module t(a); input a; INV u (.A(a)); INV u (.A(a)); endmodule");
        assert_eq!(r.inst_names(), ["u", "u_1"]);
    }

    /// The body cache changes nothing but the parse count.
    #[test]
    fn cache_changes_only_the_parse_count() {
        let a = link_opts(PROBE1, &LinkOptions { top: None, cache_bytes: 0, ..Default::default() });
        let b = link_opts(PROBE1, &LinkOptions { top: None, cache_bytes: 1 << 20, ..Default::default() });
        assert_eq!(a.insts, b.insts);
        assert_eq!(format!("{:?}", a.nets), format!("{:?}", b.nets));
    }

    /// Probe 5 (phase B0), cells renamed to the recording library.
    const PROBE5: &str = r#"
(* implements_operator = "add" *)
module sub (a, y);
  input a; output y;
  (* src = "rtl/sub.v:7.3-9.5" *)
  INV i0 (.A(a), .Y(y));
  (* src = "rtl/other.v:12.1-12.40" *)
  BUF b1 (.A(a), .Y());
  (* src = "not-a-location" *)
  BUF b0 (.A(a), .Y());
endmodule
module sub_x (a, y);
  input a; output y;
  (* src = "rtl/sub.v:30.2-31.9|rtl/b.v:4.1-4.2" *)
  INV k (.A(a), .Y(y));
endmodule
module wrap (a, y);
  input a; output y;
  wire w;
  sub x (.a(a), .y(w));
  BUF zb (.A(w), .Y(y));
  INV ab (.A(w), .Y());
endmodule
module top (a, y1, y2, y3, y4);
  input a; output y1, y2, y3, y4;
  wrap p2 (.a(a), .y(y2));
  wrap p1 (.a(a), .y(y1));
  sub_x x (.a(a), .y(y3));
  sub q (.a(a), .y(y4));
  (* src = "rtl/top.v:3.3-3.9" *)
  INV t (.A(a), .Y());
endmodule
"#;

    /// B0 `makeUniqueDbModule`, in `makeDbModule`'s depth-first R1 order: `p1` takes `wrap`,
    /// `p1/x` takes `sub`, `p2` is `wrap_p2`, `p2/x` is `sub_x` — taken BEFORE the real module
    /// `sub_x` is met, which then becomes `sub_x_x`. Renamed modules carry `original_name`.
    #[test]
    fn b0_modules_uniquely_named_in_creation_order() {
        let r = link(PROBE5);
        assert_eq!(r.modules, ["wrap", "sub", "wrap_p2", "sub_x", "sub_q", "sub_x_x"]);
        let orig: Vec<&String> = r.sprops.iter().filter(|p| p.contains("original_name")).collect();
        assert_eq!(
            orig,
            [
                "module:wrap_p2 original_name=wrap",
                "module:sub_x original_name=sub",
                "module:sub_q original_name=sub",
                "module:sub_x_x original_name=sub_x"
            ]
        );
        let mi: Vec<String> = r.mod_insts.iter().map(|(p, m, n)| format!("{p}>{n}:{m}")).collect();
        assert_eq!(mi, [">p1:wrap", "wrap>x:sub", ">p2:wrap_p2", "wrap_p2>x:sub_x", ">q:sub_q", ">x:sub_x_x"]);
    }

    /// `_N` counter of `makeUniqueDbModule`: per base name `<cell>_<inst>`, starting at 1 once the
    /// base itself is taken.
    #[test]
    fn b0_unique_name_counter() {
        let mut taken = HashSet::from(["top".to_string()]);
        let mut ids = HashMap::new();
        let got: Vec<String> = ["x", "x", "x", "y"].iter().map(|i| unique_module_name("sub", i, &mut taken, &mut ids)).collect();
        assert_eq!(got, ["sub", "sub_x", "sub_x_1", "sub_y"]);
        // A real module named like a counter result is skipped over, the counter still bumped.
        taken.insert("sub_y_1".to_string());
        assert_eq!(unique_module_name("sub", "y", &mut taken, &mut ids), "sub_y_2");
        // The top's own name is taken from the start.
        assert_eq!(unique_module_name("top", "u", &mut taken, &mut ids), "top_u");
    }

    /// B0 ownership: each leaf is created IN its module (`dbInst::create(..., module)`), the top
    /// module as `""`; each module's lists are finished after its children (post-order), the top
    /// last.
    #[test]
    fn b0_leaves_owned_by_their_module() {
        let r = link(PROBE5);
        let own: Vec<String> = r.owner.iter().map(|(i, m)| format!("{i}@{m}")).collect();
        assert_eq!(
            own,
            [
                "p1/ab@wrap", "p1/x/b0@sub", "p1/x/b1@sub", "p1/x/i0@sub", "p1/zb@wrap", "p2/ab@wrap_p2", "p2/x/b0@sub_x",
                "p2/x/b1@sub_x", "p2/x/i0@sub_x", "p2/zb@wrap_p2", "q/b0@sub_q", "q/b1@sub_q", "q/i0@sub_q", "t@", "x/k@sub_x_x"
            ]
        );
        assert_eq!(r.done, ["sub", "wrap", "sub_x", "wrap_p2", "sub_q", "sub_x_x", ""]);
    }

    /// B0 `storeLineInfo`: `src` always; `src_file_id` / `src_file_line` only when the value
    /// reads as a location, file ids in order of FIRST USE with a block `src_file_<id>`; the
    /// greedy file part keeps everything up to the last `:`. `implements_operator` (a MODULE
    /// attribute) lands on each of its module instances.
    #[test]
    fn b0_src_and_operator_properties() {
        let r = link(PROBE5);
        let p1: Vec<&String> = r.sprops.iter().filter(|p| p.starts_with("inst:p1/") || p.starts_with("block") || p.contains("implements")).collect();
        assert_eq!(
            &p1[..8],
            [
                "modinst:wrap:x implements_operator=add",
                "inst:p1/x/b0 src=not-a-location",
                "inst:p1/x/b1 src=rtl/other.v:12.1-12.40",
                "block src_file_0=rtl/other.v",
                "inst:p1/x/b1 src_file_id=0",
                "inst:p1/x/b1 src_file_line=12",
                "inst:p1/x/i0 src=rtl/sub.v:7.3-9.5",
                "block src_file_1=rtl/sub.v",
            ]
        );
        assert!(r.sprops.contains(&"block src_file_3=rtl/sub.v:30.2-31.9|rtl/b.v".to_string()));
        assert!(r.sprops.contains(&"inst:x/k src_file_line=4".to_string()));
        assert!(r.sprops.contains(&"modinst::q implements_operator=add".to_string()));
        assert_eq!(r.sprops.iter().filter(|p| p.contains("implements_operator")).count(), 3);
        // `-omit_filename_prop`: none of the three.
        let o = link_opts(PROBE5, &LinkOptions { omit_filename_prop: true, ..Default::default() });
        assert!(!o.sprops.iter().any(|p| p.contains("src")), "{:?}", o.sprops);
    }

    #[test]
    fn b0_line_info_regex() {
        assert_eq!(line_info("a.v:12.3-14.5"), Some(("a.v", "12")));
        assert_eq!(line_info("a.v:1.2-3.4|b.v:5.6-7.8"), Some(("a.v:1.2-3.4|b.v", "5")));
        assert_eq!(line_info(":1.2-3.4"), Some(("", "1")));
        for bad in ["a.v:12.3-14", "a.v:12-14.5", "a.v", "a.v:1.2-3.4x", "a.v:1.2.3-4.5", "a\nb.v:1.2-3.4", "a.v:1.2-3.4-5"] {
            assert_eq!(line_info(bad), None, "{bad:?}");
        }
    }

    /// `processUnusedCells`: an `implements_operator` module no database module is named after
    /// becomes a child block in the reference — refused here until built.
    #[test]
    fn b0_unused_operator_module_refused() {
        let src = format!("{PROBE5}\n(* implements_operator = \"mul\" *)\nmodule unused_op (a, y);\n  input a; output y;\n  INV u (.A(a), .Y(y));\nendmodule\n");
        let mut rec = Rec::default();
        let path = tmp(&src);
        let r = link_file(path.to_str().unwrap(), &LinkOptions { top: Some("top".into()), ..Default::default() }, &mut rec);
        let _ = std::fs::remove_file(&path);
        assert!(r.expect_err("refused").contains("unused_op"));
        // Unused WITHOUT the attribute: no child block, accepted.
        let src = format!("{PROBE5}\nmodule unused_plain (a, y);\n  input a; output y;\n  INV u (.A(a), .Y(y));\nendmodule\n");
        let _ = link_opts(&src, &LinkOptions { top: Some("top".into()), ..Default::default() });
    }

    /// The top's own module bears the top's name from the start: a child module whose
    /// `<cell>_<inst>` spells the top's name takes the `_1` after it. An EMPTY `src` makes no
    /// property (`storeLineInfo` returns on an empty attribute).
    #[test]
    fn b0_top_name_taken_and_empty_src() {
        let src = r#"
module sub (a, y);
  input a; output y;
  (* src = "" *)
  INV i (.A(a), .Y(y));
endmodule
module sub_q (a, y1, y2);
  input a; output y1, y2;
  sub p (.a(a), .y(y1));
  sub q (.a(a), .y(y2));
endmodule
"#;
        let r = link(src);
        assert_eq!(r.modules, ["sub", "sub_q_1"]);
        assert!(r.sprops.iter().all(|p| !p.contains("src")), "{:?}", r.sprops);
    }

    /// B1 `makeModBTerms` / `makeModITerms` (`-hier` only): per port in port order a scalar, or a
    /// bus root + `dbBusPort(from, to)` + its bits from `from` towards `to` (`d[0:2]` ascends,
    /// `q[3:2]` descends) with members/last on the first/last bit; the list reversed after the
    /// module's ports; then one module-instance pin per non-root port, in that order. Nothing of
    /// it without `-hier`.
    #[test]
    fn b1_module_ports_and_pins() {
        let src = r"
module leaf2 (a, b, d, y, q, io);
  input a; input [1:0] b; input [0:2] d; output y; output [3:2] q; inout io;
  INV i0 (.A(a), .Y(y));
endmodule
module top (a, y);
  input a; output y;
  leaf2 u (.a(a), .y(y));
endmodule
";
        let r = link_opts(src, &LinkOptions { hierarchy: true, ..Default::default() });
        assert_eq!(
            r.hier,
            [
                "leaf2 bterm a INPUT",
                "leaf2 bterm b INPUT",
                "leaf2 bus b 1:0",
                "leaf2 bterm b[1] INPUT",
                "leaf2 members b b[1]",
                "leaf2 bterm b[0] INPUT",
                "leaf2 last b b[0]",
                "leaf2 bterm d INPUT",
                "leaf2 bus d 0:2",
                "leaf2 bterm d[0] INPUT",
                "leaf2 members d d[0]",
                "leaf2 bterm d[1] INPUT",
                "leaf2 bterm d[2] INPUT",
                "leaf2 last d d[2]",
                "leaf2 bterm y OUTPUT",
                "leaf2 bterm q OUTPUT",
                "leaf2 bus q 3:2",
                "leaf2 bterm q[3] OUTPUT",
                "leaf2 members q q[3]",
                "leaf2 bterm q[2] OUTPUT",
                "leaf2 last q q[2]",
                "leaf2 bterm io INOUT",
                "leaf2 reverse",
                ">u iterm a",
                ">u iterm b[1]",
                ">u iterm b[0]",
                ">u iterm d[0]",
                ">u iterm d[1]",
                ">u iterm d[2]",
                ">u iterm y",
                ">u iterm q[3]",
                ">u iterm q[2]",
                ">u iterm io",
            ]
        );
        assert!(link(src).hier.is_empty());
    }

    /// Probe 6 (phases B1/B2), cells renamed to the recording library.
    const PROBE6: &str = r"
module leaf2 (a, b, d, y, q);
  input a; input [1:0] b; input [0:2] d; output y; output [3:2] q;
  INV i0 (.A(a), .Y(y));
  NAND2 n0 (.A(b[1]), .B(d[0]), .Y(q[3]));
  BUF f0 (.A(b[0]), .Y(q[2]));
endmodule
module mid (clk, din, dout, thru_i, thru_o, io);
  input clk; input [1:0] din; output [1:0] dout; input thru_i; output thru_o; inout io;
  wire w;
  leaf2 u (.a(clk), .b(din), .d({w, 1'b1, din[0]}), .y(w), .q(dout));
  assign thru_o = thru_i;
endmodule
module top (clk, din, dout, t_i, t_o, io, unused);
  input clk; input [1:0] din; output [1:0] dout; input t_i; output t_o; inout io; input unused;
  wire mt;
  mid m1 (.clk(clk), .din(din), .dout(dout), .thru_i(t_i), .thru_o(mt), .io(io));
  mid m0 (.clk(clk), .din(din), .dout(), .thru_i(mt), .thru_o(t_o), .io());
endmodule
";

    /// B2 `makeModNetsForSubmodules`, the reference's call sequence on probe 6 (its
    /// `dbReadVerilog 2` trace) for the first module instance `m0`: per pin of `m0` with a net,
    /// in port-bit order (unconnected `dout`, `io` skipped), `constructModNet` of the parent's net
    /// — made once, then its pins in path-name order (`m0/…` before `m1/…`), then its terms (none
    /// at the top) — then the pin's own module-instance pin joined, and at the top the ports on
    /// that local net. Then per port of `mid` in list order (bus roots find no net), the inside
    /// net: `thru_o = thru_i` merged, so both ports find `thru_i`, whose terms list `thru_o`
    /// first (moved to the head on the merge).
    #[test]
    fn b2_mod_nets_in_the_reference_call_order() {
        let r = link_opts(PROBE6, &LinkOptions { hierarchy: true, ..Default::default() });
        let want = [
            "created :clk", ":clk moditerm m0/clk", ":clk moditerm m1/clk", ":clk moditerm m0/clk", ":clk bterm clk",
            "created :din[1]", ":din[1] moditerm m0/din[1]", ":din[1] moditerm m1/din[1]", ":din[1] moditerm m0/din[1]", ":din[1] bterm din[1]",
            "created :din[0]", ":din[0] moditerm m0/din[0]", ":din[0] moditerm m1/din[0]", ":din[0] moditerm m0/din[0]", ":din[0] bterm din[0]",
            "created :mt", ":mt moditerm m0/thru_i", ":mt moditerm m1/thru_o", ":mt moditerm m0/thru_i",
            "created :t_o", ":t_o moditerm m0/thru_o", ":t_o moditerm m0/thru_o", ":t_o bterm t_o",
            "created mid:clk", "mid:clk moditerm u/a", "mid:clk modbterm clk", "mid:clk modbterm clk",
            "created mid:din[1]", "mid:din[1] moditerm u/b[1]", "mid:din[1] modbterm din[1]", "mid:din[1] modbterm din[1]",
            "created mid:din[0]", "mid:din[0] moditerm u/b[0]", "mid:din[0] moditerm u/d[2]", "mid:din[0] modbterm din[0]", "mid:din[0] modbterm din[0]",
            "created mid:dout[1]", "mid:dout[1] moditerm u/q[3]", "mid:dout[1] modbterm dout[1]",
            "created mid:dout[0]", "mid:dout[0] moditerm u/q[2]", "mid:dout[0] modbterm dout[0]",
            "created mid:thru_i", "mid:thru_i modbterm thru_o", "mid:thru_i modbterm thru_i", "mid:thru_i modbterm thru_i",
            "mid:thru_i modbterm thru_o", "mid:thru_i modbterm thru_i", "mid:thru_i modbterm thru_o",
            // m0/u: its pins against `mid`'s nets (reused), the constant `one_` and `w` made here.
            "mid:clk moditerm u/a", "mid:clk modbterm clk", "mid:clk moditerm u/a",
        ];
        assert_eq!(&r.modnets[..want.len()], want);
        // Then into leaf2 (module `leaf2` for m0/u): a leaf iterm on its port net, the port joined.
        let i = r.modnets.iter().position(|l| l == "created leaf2:a").expect("leaf2:a made");
        assert_eq!(&r.modnets[i..i + 4], ["created leaf2:a", "leaf2:a iterm m0/u/i0/A", "leaf2:a modbterm a", "leaf2:a modbterm a"]);
        assert!(r.modnets.contains(&"created mid:one_".to_string()));
        // No module nets without -hier.
        assert!(link(PROBE6).modnets.is_empty());
    }

    /// Probe 7 (`hier-import-probes/hier7.v`, cells renamed to this test's library).
    const PROBE7: &str = r"
module inner (a, y);
  input a; output y;
  INV i (.A(a), .Y(y));
endmodule
module mid (a, y);
  input a; output y;
  inner u (.a(a), .y(y));
endmodule
module ord (z, a, o);
  input z; input a; output o;
  NAND2 g (.A(z), .B(a), .Y(o));
endmodule
module top (n, o1, o2, o3);
  input n; output o1; output o2; output o3;
  mid u (.a(n), .y(o1));
  ord a1 (.z(n), .a(n), .o(o2));
  INV b0 (.A(n), .Y(o3));
endmodule
";

    /// B2, the reference's `dbReadVerilog 2` trace on probe 7. (1) `constructModNet` joins a net's
    /// pins in PATH-NAME byte order (`std::map`): `a1/a, a1/z, b0/A, u/a`, not `ord`'s port order
    /// (`z` before `a`) nor leaf-after-module. (2) For top's `u` (module `mid`, which holds its own
    /// `u`), sta's `staToDb(mid, pin)` finds `mid`'s module instance `u` and its pin first, so `u`'s
    /// pins are NOT joined to the parent's net: only the top port is. `mid`'s own `u` (module
    /// `inner`, no child named `u`) is joined as usual.
    #[test]
    fn b2_pins_in_path_name_order_and_a_shadowed_pin_is_not_joined() {
        let r = link_opts(PROBE7, &LinkOptions { hierarchy: true, ..Default::default() });
        let n = [":n moditerm a1/a", ":n moditerm a1/z", ":n iterm b0/A", ":n moditerm u/a"];
        let mut want: Vec<String> = vec!["created :n".into()];
        // a1, pin z then pin a (port order), each after the net's pins.
        want.extend(n.iter().map(|s| s.to_string()));
        want.extend([":n moditerm a1/z", ":n bterm n"].map(String::from));
        want.extend(n.iter().map(|s| s.to_string()));
        want.extend([":n moditerm a1/a", ":n bterm n"].map(String::from));
        want.extend(["created :o2", ":o2 moditerm a1/o", ":o2 moditerm a1/o", ":o2 bterm o2"].map(String::from));
        for (p, pin) in [("z", "A"), ("a", "B"), ("o", "Y")] {
            want.extend([format!("created ord:{p}"), format!("ord:{p} iterm a1/g/{pin}"), format!("ord:{p} modbterm {p}"), format!("ord:{p} modbterm {p}")]);
        }
        // u: shadowed, so no `moditerm u/a` / `u/y` after the net's pins.
        want.extend(n.iter().map(|s| s.to_string()));
        want.extend([":n bterm n", "created :o1", ":o1 moditerm u/y", ":o1 bterm o1"].map(String::from));
        for p in ["a", "y"] {
            want.extend([format!("created mid:{p}"), format!("mid:{p} moditerm u/{p}"), format!("mid:{p} modbterm {p}"), format!("mid:{p} modbterm {p}")]);
        }
        // mid's u (module inner): joined above.
        for p in ["a", "y"] {
            want.extend([format!("mid:{p} moditerm u/{p}"), format!("mid:{p} modbterm {p}"), format!("mid:{p} moditerm u/{p}")]);
        }
        for (p, pin) in [("a", "A"), ("y", "Y")] {
            want.extend([format!("created inner:{p}"), format!("inner:{p} iterm u/u/i/{pin}"), format!("inner:{p} modbterm {p}"), format!("inner:{p} modbterm {p}")]);
        }
        assert_eq!(r.modnets, want);
    }
}
