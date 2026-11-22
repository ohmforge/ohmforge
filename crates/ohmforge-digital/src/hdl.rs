//! HDL-lite: data-defined combinational blocks and memories.
//!
//! Where [`GateKind`](crate::GateKind) cells are fixed primitives, this module
//! lets a block's behaviour be supplied as *data*: a [`TruthTable`], a boolean
//! [`Expr`]ession over named inputs, a read-only [`Rom`], or a writable [`Ram`].
//! Each is a pure, deterministic function of its (resolved) input levels — the
//! [`Sim`](crate::Sim) run loop wires them into the event kernel exactly like a
//! gate, scheduling output transitions a propagation `delay` later.
//!
//! # Bit convention
//! Everywhere a bus of nets encodes an integer, **input/output index 0 is the
//! least-significant bit**. A truth-table row index, a ROM/RAM address, and a
//! data word all pack the same way: `bit_i` has weight `1 << i`.
//!
//! # Four-valued inputs
//! A block cannot compute a definite output from an input it cannot read, so if
//! *any* addressed/selected input is unknown (`X`) or floating (`Z`), the block
//! drives every output `X`. Boolean expressions are the one exception: they use
//! the `X`-propagating [`Logic`] truth tables, so `0 AND X = 0` still pins an
//! output low. (docs/ROADMAP.md — M3 HDL-lite.)

use crate::Logic;

/// Pack resolved input levels into an integer index, LSB = `levels[0]`.
///
/// Returns `None` if any level is not a definite `0`/`1` (an `X` or `Z` input
/// means the addressed row/word is unknowable).
fn pack(levels: &[Logic]) -> Option<u64> {
    let mut v = 0u64;
    for (i, &l) in levels.iter().enumerate() {
        match l.sensed() {
            Logic::High => v |= 1 << i,
            Logic::Low => {}
            _ => return None,
        }
    }
    Some(v)
}

/// Unpack the low `width` bits of `word` into logic levels, LSB first.
fn unpack(word: u64, width: usize) -> Vec<Logic> {
    (0..width)
        .map(|i| {
            if (word >> i) & 1 == 1 {
                Logic::High
            } else {
                Logic::Low
            }
        })
        .collect()
}

/// A combinational block whose behaviour is a user-supplied **truth table**.
///
/// The table has `inputs` input bits and `outputs` output bits. Each defined row
/// maps a packed input index (LSB = input 0) to a packed output word. Rows left
/// undefined — and any evaluation with an unknown/floating input — drive every
/// output `X`.
///
/// ```
/// # use ohmforge_digital::hdl::TruthTable;
/// # use ohmforge_digital::Logic::{High, Low};
/// // A half-adder: inputs [a, b] → outputs [sum, carry].
/// let ha = TruthTable::from_fn(2, 2, |i| {
///     let (a, b) = (i & 1, (i >> 1) & 1);
///     let s = a ^ b;
///     let c = a & b;
///     (s | (c << 1)) as u64
/// });
/// assert_eq!(ha.eval(&[High, High]), vec![Low, High]); // 1+1 = sum 0, carry 1
/// ```
#[derive(Debug, Clone)]
pub struct TruthTable {
    inputs: usize,
    outputs: usize,
    /// `rows[idx]` is the packed output word for input index `idx`, or `None`
    /// where the row is unspecified (→ all-`X`).
    rows: Vec<Option<u64>>,
}

impl TruthTable {
    /// A fully specified table built from a closure mapping each input index
    /// (`0..2^inputs`, LSB = input 0) to its packed output word (LSB = output 0).
    ///
    /// `inputs` must be `0..=16` (a table caps at 2^16 rows) and `outputs`
    /// `1..=64`.
    pub fn from_fn(inputs: usize, outputs: usize, f: impl Fn(usize) -> u64) -> Self {
        assert!(inputs <= 16, "truth table is capped at 16 input bits");
        assert!((1..=64).contains(&outputs), "outputs must be 1..=64 bits");
        let mask = out_mask(outputs);
        let rows = (0..(1usize << inputs)).map(|i| Some(f(i) & mask)).collect();
        Self {
            inputs,
            outputs,
            rows,
        }
    }

    /// An all-unspecified table (every row drives `X`); fill rows with
    /// [`set_row`](Self::set_row). Useful for sparse / don't-care tables.
    pub fn sparse(inputs: usize, outputs: usize) -> Self {
        assert!(inputs <= 16, "truth table is capped at 16 input bits");
        assert!((1..=64).contains(&outputs), "outputs must be 1..=64 bits");
        Self {
            inputs,
            outputs,
            rows: vec![None; 1usize << inputs],
        }
    }

    /// Define (or redefine) one row: input index `index` → packed output `word`.
    pub fn set_row(&mut self, index: usize, word: u64) -> &mut Self {
        assert!(index < self.rows.len(), "row index out of range");
        self.rows[index] = Some(word & out_mask(self.outputs));
        self
    }

    /// Number of input bits.
    pub fn inputs(&self) -> usize {
        self.inputs
    }

    /// Number of output bits.
    pub fn outputs(&self) -> usize {
        self.outputs
    }

    /// Evaluate the table over resolved input levels (LSB = `inputs[0]`),
    /// returning one [`Logic`] per output bit. Any unknown input, or an
    /// unspecified row, yields all-`X`.
    pub fn eval(&self, inputs: &[Logic]) -> Vec<Logic> {
        match pack(inputs).and_then(|idx| self.rows.get(idx as usize).copied().flatten()) {
            Some(word) => unpack(word, self.outputs),
            None => vec![Logic::X; self.outputs],
        }
    }
}

/// The low-`width` all-ones mask (`width` in `1..=64`).
fn out_mask(width: usize) -> u64 {
    if width >= 64 {
        u64::MAX
    } else {
        (1u64 << width) - 1
    }
}

/// A boolean expression over named inputs, parsed from text.
///
/// Grammar (lowest to highest precedence): `|` (OR), `^` (XOR), `&` (AND), then
/// unary `!` (NOT), then a primary — an identifier, a `0`/`1` constant, or a
/// parenthesised expression. The word forms `and`/`or`/`xor`/`not` are accepted
/// as aliases. Evaluation uses the `X`-propagating [`Logic`] operators, so a
/// definite controlling input still forces the output (`0 & X = 0`).
///
/// ```
/// # use ohmforge_digital::hdl::Expr;
/// # use ohmforge_digital::Logic::{High, Low};
/// let e = Expr::parse("(a & !b) | c", &["a", "b", "c"]).unwrap();
/// assert_eq!(e.eval(&[High, Low, Low]), High); // a & !b
/// assert_eq!(e.eval(&[Low, Low, High]), High); // c
/// assert_eq!(e.eval(&[Low, High, Low]), Low);
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Expr {
    /// A literal `0` or `1`.
    Const(Logic),
    /// A reference to input `index` (resolved from a name at parse time).
    Var(usize),
    /// Logical NOT.
    Not(Box<Expr>),
    /// Logical AND.
    And(Box<Expr>, Box<Expr>),
    /// Logical OR.
    Or(Box<Expr>, Box<Expr>),
    /// Logical XOR.
    Xor(Box<Expr>, Box<Expr>),
}

/// Why parsing a boolean [`Expr`] failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExprError {
    /// An identifier that is not one of the declared input names.
    UnknownName(String),
    /// A syntax error (dangling operator, unbalanced parens, trailing input…).
    Syntax(String),
}

impl core::fmt::Display for ExprError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            ExprError::UnknownName(n) => write!(f, "unknown input name `{n}`"),
            ExprError::Syntax(m) => write!(f, "syntax error: {m}"),
        }
    }
}

impl std::error::Error for ExprError {}

impl Expr {
    /// Parse `src` as a boolean expression whose identifiers are resolved against
    /// `names` (index = position in the slice, so `names[0]` is input bit 0).
    pub fn parse(src: &str, names: &[&str]) -> Result<Expr, ExprError> {
        let tokens = lex(src)?;
        let mut p = Parser {
            tokens: &tokens,
            pos: 0,
            names,
        };
        let e = p.parse_or()?;
        if p.pos != p.tokens.len() {
            return Err(ExprError::Syntax("trailing tokens after expression".into()));
        }
        Ok(e)
    }

    /// Evaluate against resolved input levels (`values[Var(i)]`), propagating `X`.
    pub fn eval(&self, values: &[Logic]) -> Logic {
        match self {
            Expr::Const(l) => *l,
            Expr::Var(i) => values.get(*i).copied().unwrap_or(Logic::X).sensed(),
            Expr::Not(a) => a.eval(values).not(),
            Expr::And(a, b) => a.eval(values).and(b.eval(values)),
            Expr::Or(a, b) => a.eval(values).or(b.eval(values)),
            Expr::Xor(a, b) => a.eval(values).xor(b.eval(values)),
        }
    }
}

/// A lexical token of a boolean expression.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Tok {
    And,
    Or,
    Xor,
    Not,
    LParen,
    RParen,
    Zero,
    One,
    Ident(String),
}

/// Tokenise a boolean expression. Whitespace separates tokens; the symbol and
/// word operator forms are both accepted.
fn lex(src: &str) -> Result<Vec<Tok>, ExprError> {
    let mut out = Vec::new();
    // Iterate by `char`, not raw bytes: a multi-byte UTF-8 char must advance by
    // its full length and must never be sliced mid-codepoint. (A non-ASCII letter
    // becomes an identifier that later resolves to an UnknownName; a stray symbol
    // is a clean Syntax error — neither panics. Regression: CP-PLAT-M10.2 proptest.)
    let mut chars = src.char_indices().peekable();
    while let Some(&(start, c)) = chars.peek() {
        match c {
            c if c.is_whitespace() => {
                chars.next();
            }
            '&' => {
                out.push(Tok::And);
                chars.next();
            }
            '|' => {
                out.push(Tok::Or);
                chars.next();
            }
            '^' => {
                out.push(Tok::Xor);
                chars.next();
            }
            '!' | '~' => {
                out.push(Tok::Not);
                chars.next();
            }
            '(' => {
                out.push(Tok::LParen);
                chars.next();
            }
            ')' => {
                out.push(Tok::RParen);
                chars.next();
            }
            '0' => {
                out.push(Tok::Zero);
                chars.next();
            }
            '1' => {
                out.push(Tok::One);
                chars.next();
            }
            c if c.is_alphabetic() || c == '_' => {
                let mut end = start;
                while let Some(&(idx, d)) = chars.peek() {
                    if d.is_alphanumeric() || d == '_' {
                        end = idx + d.len_utf8();
                        chars.next();
                    } else {
                        break;
                    }
                }
                let word = &src[start..end];
                match word.to_ascii_lowercase().as_str() {
                    "and" => out.push(Tok::And),
                    "or" => out.push(Tok::Or),
                    "xor" => out.push(Tok::Xor),
                    "not" => out.push(Tok::Not),
                    _ => out.push(Tok::Ident(word.to_string())),
                }
            }
            other => {
                return Err(ExprError::Syntax(format!("unexpected character `{other}`")));
            }
        }
    }
    Ok(out)
}

/// A recursive-descent parser over a token slice.
struct Parser<'a> {
    tokens: &'a [Tok],
    pos: usize,
    names: &'a [&'a str],
}

impl Parser<'_> {
    fn peek(&self) -> Option<&Tok> {
        self.tokens.get(self.pos)
    }

    fn bump(&mut self) -> Option<&Tok> {
        let t = self.tokens.get(self.pos);
        if t.is_some() {
            self.pos += 1;
        }
        t
    }

    // or := xor ('|' xor)*  — lowest precedence.
    fn parse_or(&mut self) -> Result<Expr, ExprError> {
        let mut lhs = self.parse_xor()?;
        while matches!(self.peek(), Some(Tok::Or)) {
            self.bump();
            let rhs = self.parse_xor()?;
            lhs = Expr::Or(Box::new(lhs), Box::new(rhs));
        }
        Ok(lhs)
    }

    // xor := and ('^' and)*
    fn parse_xor(&mut self) -> Result<Expr, ExprError> {
        let mut lhs = self.parse_and()?;
        while matches!(self.peek(), Some(Tok::Xor)) {
            self.bump();
            let rhs = self.parse_and()?;
            lhs = Expr::Xor(Box::new(lhs), Box::new(rhs));
        }
        Ok(lhs)
    }

    // and := unary ('&' unary)*
    fn parse_and(&mut self) -> Result<Expr, ExprError> {
        let mut lhs = self.parse_unary()?;
        while matches!(self.peek(), Some(Tok::And)) {
            self.bump();
            let rhs = self.parse_unary()?;
            lhs = Expr::And(Box::new(lhs), Box::new(rhs));
        }
        Ok(lhs)
    }

    // unary := '!' unary | primary
    fn parse_unary(&mut self) -> Result<Expr, ExprError> {
        if matches!(self.peek(), Some(Tok::Not)) {
            self.bump();
            let inner = self.parse_unary()?;
            Ok(Expr::Not(Box::new(inner)))
        } else {
            self.parse_primary()
        }
    }

    // primary := ident | '0' | '1' | '(' or ')'
    fn parse_primary(&mut self) -> Result<Expr, ExprError> {
        // Clone the token off the slice so the mutable `self` borrow from `bump`
        // is released before we consult `self.names` / recurse.
        let tok = self.bump().cloned();
        match tok {
            Some(Tok::Zero) => Ok(Expr::Const(Logic::Low)),
            Some(Tok::One) => Ok(Expr::Const(Logic::High)),
            Some(Tok::Ident(name)) => {
                let idx = self
                    .names
                    .iter()
                    .position(|n| *n == name)
                    .ok_or(ExprError::UnknownName(name))?;
                Ok(Expr::Var(idx))
            }
            Some(Tok::LParen) => {
                let e = self.parse_or()?;
                match self.bump() {
                    Some(Tok::RParen) => Ok(e),
                    _ => Err(ExprError::Syntax("expected `)`".into())),
                }
            }
            Some(t) => Err(ExprError::Syntax(format!("unexpected token {t:?}"))),
            None => Err(ExprError::Syntax("unexpected end of expression".into())),
        }
    }
}

/// A read-only memory: address bits in, data bits out (a combinational lookup).
///
/// Addresses and data follow the crate [bit convention](self): address bit 0 is
/// the LSB, data bit 0 is the LSB. An out-of-range address reads as all-zero
/// words; an unknown address bit drives every data output `X`.
#[derive(Debug, Clone)]
pub struct Rom {
    addr_bits: usize,
    data_bits: usize,
    cells: Vec<u64>,
}

impl Rom {
    /// A ROM with `addr_bits` address lines and `data_bits`-wide words,
    /// initialised from `data` (word 0 at address 0). Extra `data` entries past
    /// `2^addr_bits` are ignored; missing cells read as zero.
    pub fn new(addr_bits: usize, data_bits: usize, data: &[u64]) -> Self {
        assert!(addr_bits <= 20, "ROM address space capped at 2^20 words");
        assert!((1..=64).contains(&data_bits), "data_bits must be 1..=64");
        let size = 1usize << addr_bits;
        let mask = out_mask(data_bits);
        let mut cells = vec![0u64; size];
        for (i, &w) in data.iter().take(size).enumerate() {
            cells[i] = w & mask;
        }
        Self {
            addr_bits,
            data_bits,
            cells,
        }
    }

    /// Number of address bits.
    pub fn addr_bits(&self) -> usize {
        self.addr_bits
    }
    /// Width of a data word, in bits.
    pub fn data_bits(&self) -> usize {
        self.data_bits
    }

    /// The stored word at `addr` (0 if out of range).
    pub fn word(&self, addr: usize) -> u64 {
        self.cells.get(addr).copied().unwrap_or(0)
    }

    /// Look up `addr_levels` (LSB first) and return the data word as logic
    /// levels. An unknown address bit yields all-`X`.
    pub fn eval(&self, addr_levels: &[Logic]) -> Vec<Logic> {
        match pack(addr_levels) {
            Some(addr) => unpack(self.word(addr as usize), self.data_bits),
            None => vec![Logic::X; self.data_bits],
        }
    }
}

/// A simple synchronous-free RAM: address, data-in and a write-enable in; the
/// addressed word out.
///
/// The combined input vector is laid out `[addr.., data_in.., we]` (all LSB
/// first), matching how [`Sim::add_ram`](crate::Sim::add_ram) wires it. The write
/// is **level-sensitive**: whenever `we` is high with a definite address and data,
/// the word is stored; the output always reflects the addressed word. An unknown
/// address drives the data outputs `X` and writes nothing.
#[derive(Debug, Clone)]
pub struct Ram {
    addr_bits: usize,
    data_bits: usize,
    cells: Vec<u64>,
}

impl Ram {
    /// A zero-initialised RAM with `addr_bits` address lines and `data_bits`-wide
    /// words.
    pub fn new(addr_bits: usize, data_bits: usize) -> Self {
        assert!(addr_bits <= 20, "RAM address space capped at 2^20 words");
        assert!((1..=64).contains(&data_bits), "data_bits must be 1..=64");
        Self {
            addr_bits,
            data_bits,
            cells: vec![0u64; 1usize << addr_bits],
        }
    }

    /// Number of address bits.
    pub fn addr_bits(&self) -> usize {
        self.addr_bits
    }
    /// Width of a data word, in bits.
    pub fn data_bits(&self) -> usize {
        self.data_bits
    }

    /// The stored word at `addr` (0 if out of range).
    pub fn word(&self, addr: usize) -> u64 {
        self.cells.get(addr).copied().unwrap_or(0)
    }

    /// Evaluate over the `[addr.., data_in.., we]` input vector, writing the
    /// addressed cell when `we` is high and returning the addressed word.
    pub fn eval(&mut self, inputs: &[Logic]) -> Vec<Logic> {
        let a = self.addr_bits;
        let d = self.data_bits;
        debug_assert_eq!(
            inputs.len(),
            a + d + 1,
            "RAM inputs are [addr, data_in, we]"
        );
        let addr = match pack(&inputs[..a]) {
            Some(addr) => addr as usize,
            None => return vec![Logic::X; d], // unknown address: no read, no write
        };
        let we = inputs[a + d].sensed();
        if we == Logic::High {
            if let Some(word) = pack(&inputs[a..a + d]) {
                if addr < self.cells.len() {
                    self.cells[addr] = word;
                }
            }
        }
        unpack(self.word(addr), d)
    }
}

#[cfg(test)]
mod tests {
    use super::{Expr, ExprError, Ram, Rom, TruthTable};
    use crate::Logic::{High, Low, X, Z};

    #[test]
    fn truth_table_two_bit_adder() {
        // Inputs [a0, a1, b0, b1] → outputs [s0, s1, s2] where s = a + b (0..6).
        let adder = TruthTable::from_fn(4, 3, |i| {
            let a = (i & 1) | ((i >> 1) & 1) << 1; // a0,a1
            let b = ((i >> 2) & 1) | ((i >> 3) & 1) << 1; // b0,b1
            (a + b) as u64
        });
        // 3 + 2 = 5: a=11 (a0=1,a1=1), b=01 (b0=0,b1=1) → sum 101.
        let got = adder.eval(&[High, High, Low, High]);
        assert_eq!(got, vec![High, Low, High], "3 + 2 = 5 (101)");
        // 1 + 1 = 2: a=01, b=01 → 010.
        assert_eq!(adder.eval(&[High, Low, High, Low]), vec![Low, High, Low]);
        // Any unknown input → all-X.
        assert_eq!(adder.eval(&[X, Low, High, Low]), vec![X, X, X]);
    }

    #[test]
    fn sparse_table_unspecified_rows_are_x() {
        let mut t = TruthTable::sparse(2, 1);
        t.set_row(0b11, 1); // only the all-ones row defined
        assert_eq!(t.eval(&[High, High]), vec![High]);
        assert_eq!(t.eval(&[Low, Low]), vec![X], "undefined row → X");
    }

    #[test]
    fn expr_parses_and_evaluates_with_precedence() {
        // ! binds tighter than &, & tighter than ^, ^ tighter than |.
        let e = Expr::parse("a & b | c ^ 1", &["a", "b", "c"]).unwrap();
        // Parsed as ((a & b) | (c ^ 1)).
        assert_eq!(e.eval(&[High, High, Low]), High); // a&b = 1
        assert_eq!(e.eval(&[Low, Low, Low]), High); // c^1 = 1
        assert_eq!(e.eval(&[Low, Low, High]), Low); // 0 | (1^1)=0
    }

    #[test]
    fn expr_word_aliases_and_not() {
        let e = Expr::parse("not a or (b and c)", &["a", "b", "c"]).unwrap();
        assert_eq!(e.eval(&[Low, Low, Low]), High); // not a
        assert_eq!(e.eval(&[High, High, High]), High); // b and c
        assert_eq!(e.eval(&[High, High, Low]), Low);
    }

    #[test]
    fn expr_propagates_x_but_definite_input_dominates() {
        let e = Expr::parse("a & b", &["a", "b"]).unwrap();
        assert_eq!(e.eval(&[Low, X]), Low, "0 & X = 0");
        assert_eq!(e.eval(&[High, X]), X, "1 & X = X");
        assert_eq!(e.eval(&[High, Z]), X, "floating treated as X");
    }

    #[test]
    fn expr_errors_on_unknown_name_and_syntax() {
        assert_eq!(
            Expr::parse("a & d", &["a", "b"]),
            Err(ExprError::UnknownName("d".into()))
        );
        assert!(matches!(
            Expr::parse("a &", &["a"]),
            Err(ExprError::Syntax(_))
        ));
        assert!(matches!(
            Expr::parse("(a", &["a"]),
            Err(ExprError::Syntax(_))
        ));
    }

    #[test]
    fn rom_looks_up_initialised_words() {
        // 2-bit address, 4-bit data.
        let rom = Rom::new(2, 4, &[0x1, 0x2, 0x4, 0x8]);
        assert_eq!(rom.eval(&[Low, Low]), super::unpack(0x1, 4)); // addr 0
        assert_eq!(rom.eval(&[High, Low]), super::unpack(0x2, 4)); // addr 1
        assert_eq!(rom.eval(&[Low, High]), super::unpack(0x4, 4)); // addr 2
        assert_eq!(rom.eval(&[High, High]), super::unpack(0x8, 4)); // addr 3
        assert_eq!(rom.eval(&[X, Low]), vec![X; 4], "unknown addr → X");
    }

    #[test]
    fn ram_writes_on_we_and_reads_back() {
        // 2-bit addr, 4-bit data. Inputs: [a0,a1, d0,d1,d2,d3, we].
        let mut ram = Ram::new(2, 4);
        // Write 0b1010 (=0xA) to address 0b10 (=2) with we high.
        let write = [High, Low, Low, High, Low, High, High];
        assert_eq!(ram.eval(&write), super::unpack(0xA, 4));
        // Read back at address 2 with we low: still 0xA.
        let read = [High, Low, Low, Low, Low, Low, Low];
        assert_eq!(ram.eval(&read), super::unpack(0xA, 4));
        // A different address still reads zero.
        let read0 = [Low, Low, Low, Low, Low, Low, Low];
        assert_eq!(ram.eval(&read0), vec![Low; 4]);
    }

    #[test]
    fn ram_does_not_write_with_we_low_or_unknown_addr() {
        let mut ram = Ram::new(2, 4);
        // we low: no write.
        ram.eval(&[Low, Low, High, High, High, High, Low]);
        assert_eq!(ram.word(0), 0);
        // unknown address: no write, output X.
        let out = ram.eval(&[X, Low, High, High, High, High, High]);
        assert_eq!(out, vec![X; 4]);
    }
}
