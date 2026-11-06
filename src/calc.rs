//! Calculator state machine, independent of the UI.
//!
//! Input builds an expression of numbers, operators and parentheses, which `=` evaluates with
//! C-like precedence: parentheses first, then `×` `÷` `MOD`, `+` `−`, `<<` `>>`, `AND`, `XOR`,
//! and `OR` last, each left to right.
//!
//! Numbers are shown and typed in a [`Base`]. Base 10 works with fractions; bases 2, 8 and 16
//! work with integers of a [`WordSize`], like a programmer's calculator: arithmetic wraps
//! around, and negative numbers are shown in two's complement.

use std::sync::Arc;

use serde::{Deserialize, Serialize};

/// Number base for typing and showing numbers.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Base {
    Bin,
    Oct,
    #[default]
    Dec,
    Hex,
}

impl Base {
    /// In the order the base selector shows them.
    pub const ALL: [Self; 4] = [Self::Hex, Self::Dec, Self::Oct, Self::Bin];

    pub const fn radix(self) -> u32 {
        match self {
            Self::Bin => 2,
            Self::Oct => 8,
            Self::Dec => 10,
            Self::Hex => 16,
        }
    }

    pub const fn label(self) -> &'static str {
        match self {
            Self::Bin => "BIN",
            Self::Oct => "OCT",
            Self::Dec => "DEC",
            Self::Hex => "HEX",
        }
    }

    /// Integer bases have no fractions, and work in a [`WordSize`].
    pub fn integer(self) -> bool {
        self != Self::Dec
    }
}

/// Width of integers in the integer bases.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum WordSize {
    Byte,
    Word,
    Dword,
    #[default]
    Qword,
}

impl WordSize {
    /// In the order the word size selector shows them.
    pub const ALL: [Self; 4] = [Self::Qword, Self::Dword, Self::Word, Self::Byte];

    pub const fn bits(self) -> u32 {
        match self {
            Self::Byte => 8,
            Self::Word => 16,
            Self::Dword => 32,
            Self::Qword => 64,
        }
    }

    pub const fn label(self) -> &'static str {
        match self {
            Self::Byte => "BYTE",
            Self::Word => "WORD",
            Self::Dword => "DWORD",
            Self::Qword => "QWORD",
        }
    }

    /// Largest unsigned value of this width.
    const fn mask(self) -> u64 {
        u64::MAX >> (64 - self.bits())
    }

    /// Keeps the low `bits` bits of `n`, read as a signed number.
    const fn wrap(self, n: i64) -> i64 {
        let shift = 64 - self.bits();
        (n << shift) >> shift
    }

    /// The bit pattern of `n`, as an unsigned number.
    pub const fn unsigned(self, n: i64) -> u64 {
        n.cast_unsigned() & self.mask()
    }
}

/// A number: a float in base 10, an integer in the integer bases.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub enum Value {
    Real(f64),
    Int(i64),
}

impl Value {
    #[expect(
        clippy::cast_precision_loss,
        reason = "base 10 is floating point; integers beyond 2^53 round"
    )]
    const fn real(self) -> f64 {
        match self {
            Self::Real(x) => x,
            Self::Int(n) => n as f64,
        }
    }

    /// The integer part, if it fits in 64 bits.
    fn int(self) -> Option<i64> {
        match self {
            Self::Int(n) => Some(n),
            Self::Real(x) => {
                /// 2^63, the first value past `i64::MAX`, and what `i64::MAX` rounds to.
                const LIMIT: f64 = 9_223_372_036_854_775_808.0;
                let x = x.trunc();
                // `as` saturates, so `LIMIT` maps back to `i64::MAX`.
                #[expect(clippy::cast_possible_truncation, reason = "checked to fit")]
                (-LIMIT..=LIMIT).contains(&x).then_some(x as i64)
            }
        }
    }

    /// Whether the number can be shown and saved: not infinite or NaN.
    const fn is_finite(self) -> bool {
        match self {
            Self::Real(x) => x.is_finite(),
            Self::Int(_) => true,
        }
    }
}

/// How the current base computes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Arith {
    Real,
    Int(WordSize),
}

impl Arith {
    const fn zero(self) -> Value {
        match self {
            Self::Real => Value::Real(0.0),
            Self::Int(_) => Value::Int(0),
        }
    }

    /// `value` in this arithmetic: fractions are dropped and integers wrap to the word size.
    /// `None` if it doesn't fit in 64 bits.
    fn convert(self, value: Value) -> Option<Value> {
        match self {
            Self::Real => Some(Value::Real(value.real())),
            Self::Int(word) => value.int().map(|n| Value::Int(word.wrap(n))),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Op {
    Add,
    Sub,
    Mul,
    Div,
    Mod,
    And,
    Or,
    Xor,
    Shl,
    Shr,
}

impl Op {
    pub const fn symbol(self) -> &'static str {
        match self {
            Self::Add => "+",
            Self::Sub => "−",
            Self::Mul => "×",
            Self::Div => "÷",
            Self::Mod => "MOD",
            Self::And => "AND",
            Self::Or => "OR",
            Self::Xor => "XOR",
            Self::Shl => "<<",
            Self::Shr => ">>",
        }
    }

    /// Operators only the integer bases offer.
    pub const fn integer_only(self) -> bool {
        !matches!(self, Self::Add | Self::Sub | Self::Mul | Self::Div)
    }

    /// Higher binds tighter.
    const fn precedence(self) -> u8 {
        match self {
            Self::Or => 0,
            Self::Xor => 1,
            Self::And => 2,
            Self::Shl | Self::Shr => 3,
            Self::Add | Self::Sub => 4,
            Self::Mul | Self::Div | Self::Mod => 5,
        }
    }

    /// `None` on division by zero, overflow, or a negative shift.
    fn apply(self, lhs: Value, rhs: Value, arith: Arith) -> Option<Value> {
        if let Arith::Int(word) = arith {
            return self.apply_int(lhs.int()?, rhs.int()?, word).map(Value::Int);
        }
        let (a, b) = (lhs.real(), rhs.real());
        let result = match self {
            Self::Add => a + b,
            Self::Sub => a - b,
            Self::Mul => a * b,
            Self::Div | Self::Mod if b == 0.0 => return None,
            Self::Div => a / b,
            Self::Mod => a % b,
            // Bitwise operators work on the integer parts.
            _ => Value::Int(self.apply_int(lhs.int()?, rhs.int()?, WordSize::Qword)?).real(),
        };
        result.is_finite().then_some(Value::Real(result))
    }

    /// Two's complement arithmetic in `word` bits; division truncates toward zero.
    fn apply_int(self, a: i64, b: i64, word: WordSize) -> Option<i64> {
        let result = match self {
            Self::Add => a.wrapping_add(b),
            Self::Sub => a.wrapping_sub(b),
            Self::Mul => a.wrapping_mul(b),
            Self::Div | Self::Mod if b == 0 => return None,
            Self::Div => a.wrapping_div(b),
            Self::Mod => a.wrapping_rem(b),
            Self::And => a & b,
            Self::Or => a | b,
            Self::Xor => a ^ b,
            Self::Shl | Self::Shr if b < 0 => return None,
            // Shifting everything out leaves 0, or the sign for `>>`.
            Self::Shl if b >= 64 => 0,
            Self::Shl => a << b,
            Self::Shr => a >> b.min(63),
        };
        Some(word.wrap(result))
    }
}

/// A finished calculation, as recorded in the history.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Calculation {
    /// E.g. `2 + 3 × 4 =`.
    /// Shared, so showing the history every frame doesn't copy it.
    pub expression: Arc<str>,
    pub result: Value,
    /// The base and word size the expression is written in.
    pub base: Base,
    pub word: WordSize,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Token {
    Num(Value),
    Op(Op),
    Open,
    Close,
}

/// Builds and evaluates an expression.
///
/// `tokens` is always a valid prefix of an expression, and is never left ending in a number
/// while `entry` is empty: a number being edited lives in `entry` until something follows it.
/// All numbers are of the current base's kind: [`Value::Real`] in base 10, [`Value::Int`]
/// otherwise.
#[derive(Clone, Default)]
pub struct Calculator {
    tokens: Vec<Token>,
    /// The number being typed, without grouping; empty if none. In the integer bases it is the
    /// unsigned bit pattern, e.g. `FF` for -1 in a byte.
    entry: String,
    /// The computed value `entry` shows (a result, a recalled value, a percentage), if any. It
    /// is kept exactly, as `entry` is rounded for display, and the next digit replaces it
    /// instead of appending.
    fixed: Option<Value>,
    /// After `=`: the expression that was evaluated. The result is in `entry`.
    evaluated: Option<Vec<Token>>,
    /// Last operator and right operand, so repeated `=` repeats the operation.
    last: Option<(Op, Value)>,
    error: bool,
    base: Base,
    word: WordSize,
    memory: Option<Value>,
}

impl Calculator {
    pub fn new() -> Self {
        Self::default()
    }

    /// Starts over, keeping the base, word size and memory.
    fn reset(&mut self) {
        *self = Self {
            base: self.base,
            word: self.word,
            memory: self.memory,
            ..Self::default()
        };
    }

    pub const fn base(&self) -> Base {
        self.base
    }

    pub const fn word(&self) -> WordSize {
        self.word
    }

    fn arith(&self) -> Arith {
        if self.base.integer() {
            Arith::Int(self.word)
        } else {
            Arith::Real
        }
    }

    /// Switches base, converting everything on screen. Moving to an integer base drops
    /// fractions and wraps to the word size; a value beyond 64 bits is an error.
    pub fn set_base(&mut self, base: Base) {
        if base == self.base {
            return;
        }
        let entry = (!self.entry.is_empty()).then(|| self.entry_value());
        let from = self.arith();
        self.base = base;
        let to = self.arith();
        if self.error {
            return;
        }
        if from != to {
            for token in &mut self.tokens {
                if let Token::Num(n) = token {
                    match to.convert(*n) {
                        Some(converted) => *n = converted,
                        None => self.error = true,
                    }
                }
            }
            // The evaluated expression is shown in the new base, unless a number doesn't fit.
            self.evaluated = self.evaluated.take().and_then(|tokens| {
                tokens
                    .into_iter()
                    .map(|t| match t {
                        Token::Num(n) => to.convert(n).map(Token::Num),
                        t => Some(t),
                    })
                    .collect()
            });
        }
        if let Some(value) = entry {
            match to.convert(value) {
                Some(value) if self.fixed.is_some() => self.set_entry(value),
                Some(value) => self.edit_entry(value),
                None => self.error = true,
            }
        }
        if self.error {
            self.clear_entry();
        } else {
            self.check_evaluated();
        }
    }

    /// Changes the word size of the integer bases, keeping the low bits of every number.
    pub fn set_word(&mut self, word: WordSize) {
        if word == self.word {
            return;
        }
        let entry = (!self.entry.is_empty()).then(|| self.entry_value());
        self.word = word;
        let Arith::Int(word) = self.arith() else {
            return;
        };
        if self.error {
            return;
        }
        let wrap = |token: &mut Token| {
            if let Token::Num(Value::Int(n)) = token {
                *n = word.wrap(*n);
            }
        };
        self.tokens.iter_mut().for_each(wrap);
        self.evaluated.iter_mut().flatten().for_each(wrap);
        if let Some(Value::Int(n)) = entry {
            let value = Value::Int(word.wrap(n));
            if self.fixed.is_some() {
                self.set_entry(value);
            } else {
                self.edit_entry(value);
            }
        }
        self.check_evaluated();
    }

    /// After a change of base or word size: forgets the evaluated expression unless it still
    /// computes the result on screen, e.g. not `7 ÷ 2 =` with a result of 3 in base 10, and
    /// recomputes the operation that repeated `=` repeats.
    fn check_evaluated(&mut self) {
        let arith = self.arith();
        let result = (!self.entry.is_empty()).then(|| self.entry_value());
        let expr = self
            .evaluated
            .as_deref()
            .and_then(parse)
            .filter(|e| result.is_some() && e.eval(arith) == result);
        self.last = expr.as_ref().and_then(|e| e.last_op(arith));
        if expr.is_none() {
            self.evaluated = None;
        }
    }

    /// The value on screen; `None` on error.
    pub fn value(&self) -> Option<Value> {
        if self.error {
            None
        } else if !self.entry.is_empty() {
            Some(self.entry_value())
        } else if self.tokens.is_empty() {
            Some(self.arith().zero())
        } else {
            evaluate(&self.tokens, self.arith())
                // E.g. a division by zero that isn't finished yet: show the last number.
                .or_else(|| {
                    self.tokens.iter().rev().find_map(|t| match t {
                        Token::Num(n) => Some(*n),
                        _ => None,
                    })
                })
                .or_else(|| Some(self.arith().zero()))
        }
    }

    /// The big number: what is being typed, or else the value of the expression so far.
    pub fn display(&self) -> String {
        match self.value() {
            None => "Error".into(),
            Some(_) if !self.entry.is_empty() => group(&self.entry, self.base),
            Some(value) => format_value(value, self.base, self.word),
        }
    }

    /// The value on screen without digit grouping, for the clipboard.
    pub fn copy_text(&self) -> Option<String> {
        self.value().map(|v| format_raw(v, self.base, self.word))
    }

    /// The line above the display: the expression being built, e.g. `2 + 3 × (4 −`, or the one
    /// just evaluated, e.g. `2 + 3 × 4 =`.
    pub fn pending_expression(&self) -> String {
        self.evaluated.as_ref().map_or_else(
            || format_tokens(&self.tokens, self.base, self.word),
            |tokens| format!("{} =", format_tokens(tokens, self.base, self.word)),
        )
    }

    /// The operator waiting for its right operand, if any.
    pub fn pending_op(&self) -> Option<Op> {
        match self.tokens.last() {
            Some(Token::Op(op)) if self.entry.is_empty() => Some(*op),
            _ => None,
        }
    }

    /// The number being typed.
    fn entry_value(&self) -> Value {
        if let Some(value) = self.fixed {
            return value;
        }
        match self.arith() {
            Arith::Real => Value::Real(self.entry.parse().unwrap_or(0.0)),
            Arith::Int(word) => {
                let bits = u64::from_str_radix(&self.entry, self.base.radix()).unwrap_or(0);
                Value::Int(word.wrap(bits.cast_signed()))
            }
        }
    }

    /// Replaces the number being typed with a computed value.
    fn set_entry(&mut self, value: Value) {
        self.entry = format_raw(value, self.base, self.word);
        self.fixed = Some(value);
    }

    /// Puts `value` up for editing, or as a computed value if its text isn't exact: rounded, or
    /// in scientific notation, which typed digits would extend into the exponent.
    fn edit_entry(&mut self, value: Value) {
        self.entry = format_raw(value, self.base, self.word);
        self.fixed = None;
        if self.entry.contains('e') || self.entry_value() != value {
            self.fixed = Some(value);
        }
    }

    fn clear_entry(&mut self) {
        self.entry.clear();
        self.fixed = None;
    }

    fn reset_if_error(&mut self) {
        if self.error {
            self.reset();
        }
    }

    /// Starts a new expression if the last one was just evaluated.
    fn start_new_if_evaluated(&mut self) {
        if self.evaluated.take().is_some() {
            self.last = None;
            self.clear_entry();
        }
    }

    /// Prepares `entry` for typing a number.
    fn begin_number(&mut self) {
        self.reset_if_error();
        self.start_new_if_evaluated();
        if self.fixed.is_some() {
            self.clear_entry();
        }
        self.multiply_after_group();
    }

    /// `(2 + 3)4` and `(2)(3)` multiply.
    fn multiply_after_group(&mut self) {
        if self.entry.is_empty() && self.tokens.last() == Some(&Token::Close) {
            self.tokens.push(Token::Op(Op::Mul));
        }
    }

    /// Moves `entry` into the expression. Returns whether there was one.
    fn commit_entry(&mut self) -> bool {
        if self.entry.is_empty() {
            return false;
        }
        self.tokens.push(Token::Num(self.entry_value()));
        self.clear_entry();
        true
    }

    fn depth(&self) -> usize {
        self.tokens.iter().fold(0, |depth, t| match t {
            Token::Open => depth + 1,
            Token::Close => depth - 1,
            _ => depth,
        })
    }

    /// Types a digit, 0 to 15; digits outside the base are ignored, and so are digits that
    /// would overflow the word size.
    pub fn digit(&mut self, d: u8) {
        self.push_digit(d);
    }

    /// [`Self::digit`], returning whether the digit was taken.
    fn push_digit(&mut self, d: u8) -> bool {
        let Some(ch) = char::from_digit(d.into(), self.base.radix()) else {
            return false;
        };
        self.begin_number();
        if self.entry == "0" {
            self.entry.clear();
        } else if self.entry == "-0" {
            self.entry.truncate(1);
        }
        let mut entry = self.entry.clone();
        entry.push(ch.to_ascii_uppercase());
        let fits = match self.arith() {
            Arith::Real => entry.chars().filter(char::is_ascii_digit).count() <= MAX_DEC_DIGITS,
            Arith::Int(word) => {
                u64::from_str_radix(&entry, self.base.radix()).is_ok_and(|n| n <= word.mask())
            }
        };
        if fits {
            self.entry = entry;
        }
        fits
    }

    pub fn decimal(&mut self) {
        if self.base.integer() {
            return;
        }
        self.begin_number();
        if self.entry.is_empty() {
            self.entry.push('0');
        }
        if !self.entry.contains('.') {
            self.entry.push('.');
        }
    }

    /// Ignores operators the base doesn't offer.
    pub fn operator(&mut self, op: Op) {
        if op.integer_only() && !self.base.integer() {
            return;
        }
        self.reset_if_error();
        // After `=`, the result in `entry` becomes the left operand.
        self.evaluated = None;
        self.last = None;
        if !self.commit_entry() {
            match self.tokens.last_mut() {
                // Operator pressed twice in a row: just swap it.
                Some(Token::Op(prev)) => {
                    *prev = op;
                    return;
                }
                // No left operand yet inside a group.
                Some(Token::Open) => return,
                Some(Token::Close | Token::Num(_)) => {}
                None => self.tokens.push(Token::Num(self.arith().zero())),
            }
        }
        self.tokens.push(Token::Op(op));
    }

    pub fn open_paren(&mut self) {
        self.reset_if_error();
        self.start_new_if_evaluated();
        if self.commit_entry() || self.tokens.last() == Some(&Token::Close) {
            self.tokens.push(Token::Op(Op::Mul));
        }
        self.tokens.push(Token::Open);
    }

    pub fn close_paren(&mut self) {
        if self.error || self.evaluated.is_some() || self.depth() == 0 {
            return;
        }
        // A group needs to end in an operand.
        if self.commit_entry() || self.tokens.last() == Some(&Token::Close) {
            self.tokens.push(Token::Close);
        }
    }

    /// Evaluates the expression, or repeats the last operation. Returns the calculation unless
    /// there was nothing to compute or it failed.
    pub fn equals(&mut self) -> Option<Calculation> {
        if self.error {
            return None;
        }
        let tokens = if self.evaluated.is_some() || self.tokens.is_empty() {
            let (op, rhs) = self.last?;
            vec![
                Token::Num(self.entry_value()),
                Token::Op(op),
                Token::Num(rhs),
            ]
        } else {
            self.commit_entry();
            let tokens = complete(&self.tokens);
            // E.g. just `(`.
            if tokens.is_empty() {
                return None;
            }
            tokens
        };
        let (base, word, arith) = (self.base, self.word, self.arith());
        let expression = format!("{} =", format_tokens(&tokens, base, word)).into();
        let parsed = parse(&tokens);

        self.tokens.clear();
        self.evaluated = Some(tokens);
        let result = parsed.as_ref().and_then(|e| e.eval(arith));
        if let Some(result) = result.filter(|r| r.is_finite()) {
            self.last = parsed.as_ref().and_then(|e| e.last_op(arith));
            self.set_entry(result);
            // A lone number, e.g. from `5 + =` or `(5) =`, is nothing computed.
            matches!(parsed, Some(Expr::Bin(..))).then_some(Calculation {
                expression,
                result,
                base,
                word,
            })
        } else {
            self.error = true;
            self.clear_entry();
            self.last = None;
            None
        }
    }

    /// Puts a previous result in place of the number being typed, converted to the current
    /// base and word size.
    pub fn recall(&mut self, value: Value) {
        self.reset_if_error();
        self.start_new_if_evaluated();
        if self.entry.is_empty() {
            self.multiply_after_group();
        }
        if let Some(value) = self.arith().convert(value) {
            self.set_entry(value);
        } else {
            self.error = true;
            self.clear_entry();
        }
    }

    pub fn clear(&mut self) {
        self.reset();
    }

    /// Deletes the last character typed, or else the last operator or parenthesis.
    pub fn backspace(&mut self) {
        if self.error {
            return self.clear();
        }
        if self.evaluated.is_some() {
            return;
        }
        if self.fixed.is_some() {
            self.clear_entry();
        } else if !self.entry.is_empty() {
            self.entry.pop();
            if self.entry == "-" {
                self.entry.clear();
            }
        } else {
            self.tokens.pop();
            // Keep editing the number now at the end.
            if let Some(&Token::Num(n)) = self.tokens.last() {
                self.tokens.pop();
                self.edit_entry(n);
            }
        }
    }

    pub fn negate(&mut self) {
        if self.base.integer() {
            return self.transform(i64::wrapping_neg);
        }
        if self.error {
            return;
        }
        if self.entry.is_empty() {
            // Nothing to negate after a group; otherwise start a negative number.
            if self.tokens.last() != Some(&Token::Close) {
                self.entry = "-0".into();
            }
            return;
        }
        // A negated result no longer matches the expression shown for it.
        self.evaluated = None;
        self.last = None;
        if let Some(rest) = self.entry.strip_prefix('-') {
            self.entry = rest.to_string();
        } else {
            self.entry.insert(0, '-');
        }
        if let Some(Value::Real(x)) = &mut self.fixed {
            *x = -*x;
        }
    }

    /// Bitwise NOT of the number on screen; integer bases only.
    pub fn not(&mut self) {
        self.transform(|n| !n);
    }

    /// Flips one bit of the number on screen; integer bases only.
    pub fn toggle_bit(&mut self, bit: u32) {
        if bit < self.word.bits() {
            self.transform(|n| n ^ (1 << bit));
        }
    }

    /// Replaces the number on screen with `f` of it, in an integer base.
    fn transform(&mut self, f: impl FnOnce(i64) -> i64) {
        let Arith::Int(word) = self.arith() else {
            return;
        };
        // After a group the number on screen is the whole expression, not an operand.
        if self.entry.is_empty() && self.tokens.last() == Some(&Token::Close) {
            return;
        }
        let Some(Value::Int(n)) = self.value() else {
            return;
        };
        // A changed result no longer matches the expression shown for it.
        self.evaluated = None;
        self.last = None;
        self.set_entry(Value::Int(word.wrap(f(n))));
    }

    /// `a + b%` means `a + a*b/100`; otherwise `b%` is `b/100`.
    pub fn percent(&mut self) {
        if self.error || self.entry.is_empty() || self.base.integer() {
            return;
        }
        let value = self.entry_value().real();
        let result = match self.tokens.split_last() {
            Some((Token::Op(Op::Add | Op::Sub), before)) => {
                // `a` is the operand group the `+` belongs to.
                let start = group_start(before);
                match evaluate(&before[start..], Arith::Real) {
                    Some(base) => base.real() * value / 100.0,
                    None => return,
                }
            }
            _ => value / 100.0,
        };
        self.evaluated = None;
        self.last = None;
        self.set_entry(Value::Real(result));
    }

    pub const fn has_memory(&self) -> bool {
        self.memory.is_some()
    }

    pub const fn memory_clear(&mut self) {
        self.memory = None;
    }

    /// MS: stores the number on screen.
    pub fn memory_store(&mut self) {
        if let Some(value) = self.value() {
            self.memory = Some(value);
            self.fix_entry();
        }
    }

    /// M+ and M−: adds the number on screen to memory, or subtracts it.
    pub fn memory_add(&mut self, subtract: bool) {
        let Some(value) = self.value() else {
            return;
        };
        let arith = self.arith();
        // A memory that doesn't fit the base is left alone rather than lost.
        let Some(memory) = self
            .memory
            .map_or_else(|| Some(arith.zero()), |m| arith.convert(m))
        else {
            return;
        };
        let op = if subtract { Op::Sub } else { Op::Add };
        if let Some(sum) = op.apply(memory, value, arith) {
            self.memory = Some(sum);
            self.fix_entry();
        }
    }

    /// MR: recalls the memory like a history entry.
    pub fn memory_recall(&mut self) {
        if let Some(value) = self.memory {
            self.recall(value);
        }
    }

    /// The next digit starts a new number.
    fn fix_entry(&mut self) {
        if !self.entry.is_empty() {
            self.fixed = Some(self.entry_value());
        }
    }

    /// Types pasted text such as `2 + 3`, `0xFF AND 0F`, `2 × -3` or `1,234.5`. Separators, a
    /// `0x`, `0o` or `0b` prefix matching the base, and a trailing `=` are skipped. Returns false
    /// and changes nothing if the text has anything else, a number too long for the base, or
    /// makes the expression too long.
    pub fn paste(&mut self, text: &str) -> bool {
        let Some(inputs) = parse_paste(text, self.base) else {
            return false;
        };
        if inputs.is_empty() {
            return false;
        }
        // Typed on a copy, to change nothing if a digit doesn't fit.
        let mut next = self.clone();
        // A pasted number replaces the one being typed.
        if matches!(inputs.first(), Some(Input::Digit(_) | Input::Decimal)) {
            next.fix_entry();
        }
        for input in &inputs {
            match *input {
                Input::Digit(d) => {
                    if !next.push_digit(d) {
                        return false;
                    }
                }
                Input::Decimal => next.decimal(),
                Input::Negate => next.negate(),
                Input::Op(op) => next.operator(op),
                Input::Open => next.open_paren(),
                Input::Close => next.close_paren(),
            }
            // Bounds the work, and the parser's recursion.
            if next.tokens.len() > MAX_PASTE_TOKENS {
                return false;
            }
        }
        *self = next;
        true
    }
}

/// Most digits a typed base 10 number may have; `f64` holds about 16.
const MAX_DEC_DIGITS: usize = 16;

/// Longest expression a paste may build.
const MAX_PASTE_TOKENS: usize = 1000;

/// A key, as read from pasted text.
enum Input {
    Digit(u8),
    Decimal,
    Negate,
    Op(Op),
    Open,
    Close,
}

/// Ends a number that a `-` before it negates, if there is one.
fn end_negated(inputs: &mut Vec<Input>, negate: &mut bool) -> Option<()> {
    if std::mem::take(negate) {
        // The `-` must be followed by a number.
        matches!(inputs.last(), Some(Input::Digit(_) | Input::Decimal)).then_some(())?;
        inputs.push(Input::Negate);
    }
    Some(())
}

fn parse_paste(text: &str, base: Base) -> Option<Vec<Input>> {
    let chars: Vec<char> = text
        .trim()
        .trim_end_matches('=')
        .to_uppercase()
        .chars()
        .collect();
    let prefix = match base {
        Base::Bin => Some('B'),
        Base::Oct => Some('O'),
        Base::Dec => None,
        Base::Hex => Some('X'),
    };
    let words = [
        ("AND", Op::And),
        ("OR", Op::Or),
        ("XOR", Op::Xor),
        ("MOD", Op::Mod),
    ];
    let mut inputs = Vec::new();
    // Set by a `-` where an operand is expected, as in `2 × -3`: it negates the next number.
    let mut negate = false;
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        let rest = &chars[i..];
        // Only at the start of a word or number.
        let starts_word = i == 0 || !chars[i - 1].is_alphanumeric();
        let ends_at = |len: usize| chars.get(i + len).is_none_or(|c| !c.is_alphanumeric());
        // Word operators and shifts are for the integer bases only.
        if starts_word
            && let Some(&(word, op)) = words.iter().find(|(word, _)| {
                rest.iter().take(word.len()).copied().eq(word.chars()) && ends_at(word.len())
            })
        {
            base.integer().then_some(())?;
            end_negated(&mut inputs, &mut negate)?;
            inputs.push(Input::Op(op));
            i += word.len();
            continue;
        }
        if starts_word && c == '0' && prefix.is_some() && rest.get(1).copied() == prefix {
            i += 2;
            continue;
        }
        if rest.starts_with(&['<', '<']) || rest.starts_with(&['>', '>']) {
            base.integer().then_some(())?;
            end_negated(&mut inputs, &mut negate)?;
            inputs.push(Input::Op(if c == '<' { Op::Shl } else { Op::Shr }));
            i += 2;
            continue;
        }
        if let Some(d) = c.to_digit(base.radix()).and_then(|d| u8::try_from(d).ok()) {
            inputs.push(Input::Digit(d));
            i += 1;
            continue;
        }
        let input = match c {
            _ if c.is_whitespace() => None,
            ',' | '_' | '\'' => None,
            '.' if base == Base::Dec => Some(Input::Decimal),
            '+' => Some(Input::Op(Op::Add)),
            '-' | '−' if !negate && matches!(inputs.last(), Some(Input::Op(_) | Input::Open)) => {
                negate = true;
                None
            }
            '-' | '−' => Some(Input::Op(Op::Sub)),
            '*' | '×' => Some(Input::Op(Op::Mul)),
            '/' | '÷' => Some(Input::Op(Op::Div)),
            '%' if base.integer() => Some(Input::Op(Op::Mod)),
            '&' => Some(Input::Op(Op::And)),
            '|' => Some(Input::Op(Op::Or)),
            '^' => Some(Input::Op(Op::Xor)),
            '(' => Some(Input::Open),
            ')' => Some(Input::Close),
            _ => return None,
        };
        if let Some(Input::Op(op)) = &input
            && op.integer_only()
            && !base.integer()
        {
            return None;
        }
        if let Some(input) = input {
            if !matches!(input, Input::Decimal) {
                end_negated(&mut inputs, &mut negate)?;
            }
            inputs.push(input);
        }
        i += 1;
    }
    end_negated(&mut inputs, &mut negate)?;
    Some(inputs)
}

/// Index just after the innermost unclosed `(`, or 0.
fn group_start(tokens: &[Token]) -> usize {
    let mut depth = 0;
    for (i, t) in tokens.iter().enumerate().rev() {
        match t {
            Token::Close => depth += 1,
            Token::Open if depth == 0 => return i + 1,
            Token::Open => depth -= 1,
            _ => {}
        }
    }
    0
}

/// Drops a dangling operator or `(` and closes open groups.
fn complete(tokens: &[Token]) -> Vec<Token> {
    let mut tokens = tokens.to_vec();
    while matches!(tokens.last(), Some(Token::Op(_) | Token::Open)) {
        tokens.pop();
    }
    let depth = tokens.iter().fold(0i32, |depth, t| match t {
        Token::Open => depth + 1,
        Token::Close => depth - 1,
        _ => depth,
    });
    tokens.extend((0..depth).map(|_| Token::Close));
    tokens
}

/// Value of a possibly unfinished expression.
fn evaluate(tokens: &[Token], arith: Arith) -> Option<Value> {
    parse(&complete(tokens))?.eval(arith)
}

fn format_tokens(tokens: &[Token], base: Base, word: WordSize) -> String {
    let mut s = String::new();
    let mut prev = None;
    for &t in tokens {
        if prev.is_some() && prev != Some(Token::Open) && t != Token::Close {
            s.push(' ');
        }
        match t {
            Token::Num(n) => s.push_str(&format_value(n, base, word)),
            Token::Op(op) => s.push_str(op.symbol()),
            Token::Open => s.push('('),
            Token::Close => s.push(')'),
        }
        prev = Some(t);
    }
    s
}

enum Expr {
    Num(Value),
    Bin(Box<Self>, Op, Box<Self>),
}

impl Expr {
    /// `None` on division by zero or overflow.
    fn eval(&self, arith: Arith) -> Option<Value> {
        match self {
            Self::Num(n) => Some(*n),
            Self::Bin(lhs, op, rhs) => op.apply(lhs.eval(arith)?, rhs.eval(arith)?, arith),
        }
    }

    /// The top-level operator and the value of its right operand, for repeated `=`.
    fn last_op(&self, arith: Arith) -> Option<(Op, Value)> {
        match self {
            Self::Bin(_, op, rhs) => rhs.eval(arith).map(|rhs| (*op, rhs)),
            Self::Num(_) => None,
        }
    }
}

/// Parses a complete expression of numbers and parenthesized groups joined by operators,
/// by precedence climbing.
fn parse(tokens: &[Token]) -> Option<Expr> {
    let mut parser = Parser { tokens, pos: 0 };
    let expr = parser.expr(0)?;
    (parser.pos == tokens.len()).then_some(expr)
}

struct Parser<'a> {
    tokens: &'a [Token],
    pos: usize,
}

impl Parser<'_> {
    fn next(&mut self) -> Option<Token> {
        let token = *self.tokens.get(self.pos)?;
        self.pos += 1;
        Some(token)
    }

    /// Operands joined by operators that bind at least as tightly as `min`.
    fn expr(&mut self, min: u8) -> Option<Expr> {
        let mut lhs = self.factor()?;
        while let Some(&Token::Op(op)) = self.tokens.get(self.pos)
            && op.precedence() >= min
        {
            self.pos += 1;
            let rhs = self.expr(op.precedence() + 1)?;
            lhs = Expr::Bin(Box::new(lhs), op, Box::new(rhs));
        }
        Some(lhs)
    }

    fn factor(&mut self) -> Option<Expr> {
        match self.next()? {
            Token::Num(n) => Some(Expr::Num(n)),
            Token::Open => {
                let inner = self.expr(0)?;
                (self.next()? == Token::Close).then_some(inner)
            }
            _ => None,
        }
    }
}

/// Formats a number in `base`, with digit grouping: `1,234.5`, `FFFF FFFF`, `1010 1010`.
pub fn format_value(value: Value, base: Base, word: WordSize) -> String {
    group(&format_raw(value, base, word), base)
}

/// Formats a number in `base` without grouping. Integer bases show the bit pattern in `word`
/// bits, so negative numbers appear in two's complement.
fn format_raw(value: Value, base: Base, word: WordSize) -> String {
    let bits = || word.unsigned(value.int().unwrap_or(0));
    match base {
        Base::Dec => match value {
            Value::Real(x) => format_number(x),
            Value::Int(n) => n.to_string(),
        },
        Base::Bin => format!("{:b}", bits()),
        Base::Oct => format!("{:o}", bits()),
        Base::Hex => format!("{:X}", bits()),
    }
}

/// Inserts digit separators into a number formatted in `base`: thousands commas in base 10,
/// spaces between groups of 4 (hex, binary) or 3 (octal) digits otherwise.
pub fn group(s: &str, base: Base) -> String {
    let (sign, digits) = s.strip_prefix('-').map_or(("", s), |rest| ("-", rest));
    // Leave scientific notation alone.
    if digits.contains('e') {
        return s.into();
    }
    let (int, frac) = digits.split_at(digits.find('.').unwrap_or(digits.len()));
    let (size, sep) = match base {
        Base::Dec => (3, ','),
        Base::Oct => (3, ' '),
        Base::Bin | Base::Hex => (4, ' '),
    };
    let mut out = String::from(sign);
    for (i, c) in int.chars().enumerate() {
        if i > 0 && (int.len() - i) % size == 0 {
            out.push(sep);
        }
        out.push(c);
    }
    out.push_str(frac);
    out
}

fn format_number(n: f64) -> String {
    if n == 0.0 {
        return "0".into();
    }
    let magnitude = n.abs();
    if !(1e-9..1e16).contains(&magnitude) {
        return format!("{n:e}");
    }
    // Round to 12 significant digits to hide float noise like 0.1 + 0.2.
    // `magnitude` is within 1e-9..1e16, so its exponent is within -9..16.
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "small and clamped to 0..=15"
    )]
    let decimals = (11 - magnitude.log10().floor() as i32).clamp(0, 15) as usize;
    let s = format!("{n:.decimals$}");
    if s.contains('.') {
        s.trim_end_matches('0').trim_end_matches('.').to_string()
    } else {
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn press(c: &mut Calculator, keys: &str) {
        for k in keys.chars() {
            match k {
                '0'..='9' => c.digit(k as u8 - b'0'),
                // Uppercase only: `b` and `c` are backspace and clear.
                'A'..='F' => c.digit(k as u8 - b'A' + 10),
                '.' => c.decimal(),
                '+' => c.operator(Op::Add),
                '-' => c.operator(Op::Sub),
                '*' => c.operator(Op::Mul),
                '/' => c.operator(Op::Div),
                '%' => c.percent(),
                'm' => c.operator(Op::Mod),
                '&' => c.operator(Op::And),
                '|' => c.operator(Op::Or),
                '^' => c.operator(Op::Xor),
                '<' => c.operator(Op::Shl),
                '>' => c.operator(Op::Shr),
                '~' => c.not(),
                '(' => c.open_paren(),
                ')' => c.close_paren(),
                '=' => {
                    c.equals();
                }
                'n' => c.negate(),
                'b' => c.backspace(),
                'c' => c.clear(),
                ' ' => {}
                _ => panic!("bad key {k}"),
            }
        }
    }

    fn run(keys: &str) -> String {
        let mut c = Calculator::new();
        press(&mut c, keys);
        c.display()
    }

    /// The line above the display after pressing `keys`.
    fn expression(keys: &str) -> String {
        let mut c = Calculator::new();
        press(&mut c, keys);
        c.pending_expression()
    }

    #[test]
    fn basics() {
        assert_eq!(run(""), "0");
        assert_eq!(run("12+30="), "42");
        assert_eq!(run("7-10="), "-3");
        assert_eq!(run("6*7="), "42");
        assert_eq!(run("1/4="), "0.25");
        assert_eq!(run("0.1+0.2="), "0.3");
    }

    #[test]
    fn precedence() {
        assert_eq!(run("2+3*4="), "14");
        assert_eq!(run("10-4/2="), "8");
        assert_eq!(run("2*3+4*5="), "26");
        assert_eq!(run("8/4/2="), "1");
        assert_eq!(run("10-3-2="), "5");
    }

    #[test]
    fn parentheses() {
        assert_eq!(run("(2+3)*4="), "20");
        assert_eq!(run("2*(3+4)="), "14");
        assert_eq!(run("((1+2)*(3+4))/7="), "3");
        // Open groups are closed and a dangling operator is dropped.
        assert_eq!(run("2*(3+4="), "14");
        assert_eq!(run("2*(3+="), "6");
        // A closing parenthesis needs a matching group and an operand.
        assert_eq!(run("2)+1="), "3");
        assert_eq!(run("(2+)3="), "5");
        assert_eq!(run("()1="), "1");
    }

    #[test]
    fn implicit_multiplication() {
        assert_eq!(run("2(3+1)="), "8");
        assert_eq!(run("(2+1)3="), "9");
        assert_eq!(run("(2)(3)="), "6");
    }

    #[test]
    fn expression_stays_on_screen() {
        assert_eq!(expression("2+3*"), "2 + 3 ×");
        assert_eq!(expression("2*(3+4"), "2 × (3 +");
        assert_eq!(expression("2*(3+4)"), "2 × (3 + 4)");
        assert_eq!(expression("2+3*4="), "2 + 3 × 4 =");
        assert_eq!(expression("2*(3+4="), "2 × (3 + 4) =");
        assert_eq!(expression("2n*3"), "-2 ×");
    }

    #[test]
    fn display_previews_value() {
        assert_eq!(run("2+3*"), "5");
        assert_eq!(run("2+3*4"), "4");
        assert_eq!(run("(2+3)"), "5");
        assert_eq!(run("8/0+"), "0");
    }

    #[test]
    fn after_equals() {
        // A digit starts a new expression; an operator continues from the result.
        assert_eq!(run("2+3=7"), "7");
        assert_eq!(expression("2+3=7"), "");
        assert_eq!(run("2+3=*2="), "10");
        assert_eq!(run("2+3=(1+1)="), "2");
    }

    #[test]
    fn repeated_equals() {
        assert_eq!(run("2+3==="), "11");
        assert_eq!(expression("2+3=="), "5 + 3 =");
        // Repeats the last top-level operation: `+ 12`.
        assert_eq!(run("2+3*4=="), "26");
    }

    #[test]
    fn operator_swap() {
        assert_eq!(run("5+*2="), "10");
        assert_eq!(run("+5="), "5");
        assert_eq!(run("(*2)="), "2");
    }

    #[test]
    fn divide_by_zero() {
        assert_eq!(run("5/0="), "Error");
        assert_eq!(run("1+5/(2-2)="), "Error");
        assert_eq!(run("5/0=3+4="), "7");
    }

    #[test]
    fn editing() {
        assert_eq!(run("123b"), "12");
        assert_eq!(run("1bb"), "0");
        assert_eq!(run("5n"), "-5");
        assert_eq!(run("5nn"), "5");
        assert_eq!(run("n5"), "-5");
        assert_eq!(run("2*5n="), "-10");
        assert_eq!(run("1..5"), "1.5");
        assert_eq!(run(".5"), "0.5");
        assert_eq!(run("0007"), "7");
    }

    #[test]
    fn backspace_edits_expression() {
        // Deleting an operator resumes editing the number before it.
        assert_eq!(run("2+b5="), "25");
        assert_eq!(expression("2*(3b"), "2 × (");
        assert_eq!(run("2*(3bb+4="), "6");
        assert_eq!(run("(2+3)b*4="), "14");
        // Nothing to delete after `=`.
        assert_eq!(run("2+3=b"), "5");
    }

    #[test]
    fn percent() {
        assert_eq!(run("50%"), "0.5");
        assert_eq!(run("200+10%="), "220");
        assert_eq!(run("200*10%="), "20");
        assert_eq!(run("200+10%+5="), "225");
        // The base is the group the `+` belongs to.
        assert_eq!(run("2*(100+10%)="), "220");
        assert_eq!(run("2*100+10%="), "220");
    }

    #[test]
    fn equals_reports_calculation() {
        let mut c = Calculator::new();
        press(&mut c, "12*3+4");
        let calc = c.equals().unwrap();
        assert_eq!(calc.result, Value::Real(40.0));
        assert_eq!(&*calc.expression, "12 × 3 + 4 =");

        // Repeated `=` reports the repeated operation.
        let calc = c.equals().unwrap();
        assert_eq!(&*calc.expression, "40 + 4 =");
        assert_eq!(calc.result, Value::Real(44.0));
    }

    #[test]
    fn equals_reports_nothing_without_operation() {
        let mut c = Calculator::new();
        press(&mut c, "5");
        assert_eq!(c.equals(), None);
        press(&mut c, "/0");
        assert_eq!(c.equals(), None);
        assert_eq!(c.display(), "Error");
    }

    #[test]
    fn recall() {
        let mut c = Calculator::new();
        c.recall(Value::Real(42.0));
        assert_eq!(c.display(), "42");
        // Typing replaces a recalled value rather than appending to it.
        c.digit(7);
        assert_eq!(c.display(), "7");

        // A recalled value is a valid operand.
        let mut c = Calculator::new();
        press(&mut c, "2*");
        c.recall(Value::Real(21.0));
        press(&mut c, "+");
        assert_eq!(c.display(), "42");

        // After a group, a recalled value multiplies.
        let mut c = Calculator::new();
        press(&mut c, "(1+1)");
        c.recall(Value::Real(3.0));
        press(&mut c, "=");
        assert_eq!(c.display(), "6");

        // Recall clears an error.
        let mut c = Calculator::new();
        press(&mut c, "1/0=");
        c.recall(Value::Real(3.0));
        assert_eq!(c.display(), "3");
    }

    fn in_base(base: Base, keys: &str) -> Calculator {
        let mut c = Calculator::new();
        c.set_base(base);
        press(&mut c, keys);
        c
    }

    fn in_word(base: Base, word: WordSize, keys: &str) -> Calculator {
        let mut c = Calculator::new();
        c.set_base(base);
        c.set_word(word);
        press(&mut c, keys);
        c
    }

    #[test]
    fn bases() {
        assert_eq!(in_base(Base::Hex, "FF+1=").display(), "100");
        assert_eq!(in_base(Base::Bin, "101*11=").display(), "1111");
        assert_eq!(in_base(Base::Oct, "7+1=").display(), "10");
        assert_eq!(
            in_base(Base::Hex, "2*(A+B").pending_expression(),
            "2 × (A +"
        );
        assert_eq!(in_base(Base::Hex, "A+5=").pending_expression(), "A + 5 =");
    }

    #[test]
    fn integer_bases() {
        // Division truncates toward zero.
        assert_eq!(in_base(Base::Bin, "111/10=").display(), "11");
        assert_eq!(in_word(Base::Hex, WordSize::Byte, "7n/2=").display(), "FD");
        // No decimal point or percent.
        assert_eq!(in_base(Base::Oct, "1.5").display(), "15");
        assert_eq!(in_base(Base::Hex, "50%").display(), "50");
        // Digits outside the base are ignored.
        assert_eq!(in_base(Base::Bin, "1021").display(), "101");
        assert_eq!(in_base(Base::Dec, "1A").display(), "1");
        // Bitwise operators are for integer bases only.
        assert_eq!(in_base(Base::Dec, "6&3=").display(), "63");
    }

    #[test]
    fn word_sizes() {
        // Negative numbers are shown in two's complement.
        assert_eq!(in_word(Base::Hex, WordSize::Byte, "An").display(), "F6");
        assert_eq!(in_word(Base::Hex, WordSize::Word, "1-2=").display(), "FFFF");
        assert_eq!(in_base(Base::Hex, "1-2=").display(), "FFFF FFFF FFFF FFFF");
        // Arithmetic wraps around.
        assert_eq!(in_word(Base::Hex, WordSize::Byte, "FF+2=").display(), "1");
        assert_eq!(
            in_base(Base::Hex, "7FFFFFFFFFFFFFFF+1=").display(),
            "8000 0000 0000 0000"
        );
        // Typing stops at the word size.
        assert_eq!(in_word(Base::Hex, WordSize::Byte, "123").display(), "12");
        assert_eq!(in_word(Base::Oct, WordSize::Byte, "3777").display(), "377");
        assert_eq!(
            in_word(Base::Bin, WordSize::Byte, "111111111").display(),
            "1111 1111"
        );

        // Narrowing keeps the low bits.
        let mut c = in_base(Base::Hex, "1234");
        c.set_word(WordSize::Byte);
        assert_eq!(c.display(), "34");
        // The value is signed: FF in a byte is -1, and stays -1 when widened.
        let mut c = in_word(Base::Hex, WordSize::Byte, "FF");
        c.set_word(WordSize::Word);
        assert_eq!(c.display(), "FFFF");
        c.set_base(Base::Dec);
        assert_eq!(c.display(), "-1");
    }

    #[test]
    fn bitwise() {
        assert_eq!(in_base(Base::Hex, "F0|F=").display(), "FF");
        assert_eq!(in_base(Base::Hex, "FF&F0=").display(), "F0");
        assert_eq!(in_base(Base::Hex, "FF^F=").display(), "F0");
        assert_eq!(in_base(Base::Hex, "1<4=").display(), "10");
        assert_eq!(in_base(Base::Hex, "F0>4=").display(), "F");
        assert_eq!(in_base(Base::Hex, "11m4=").display(), "1");
        assert_eq!(in_base(Base::Hex, "5m0=").display(), "Error");
        assert_eq!(in_base(Base::Hex, "1<n=").display(), "Error");
        // `>>` keeps the sign; shifting everything out of `<<` leaves 0.
        assert_eq!(in_word(Base::Hex, WordSize::Byte, "80>1=").display(), "C0");
        assert_eq!(in_word(Base::Hex, WordSize::Byte, "1<8=").display(), "0");
        // NOT works on the number on screen.
        assert_eq!(in_word(Base::Hex, WordSize::Byte, "F~").display(), "F0");
        assert_eq!(in_word(Base::Hex, WordSize::Byte, "~").display(), "FF");
        assert_eq!(
            in_word(Base::Hex, WordSize::Byte, "F+~").pending_expression(),
            "F +"
        );
        assert_eq!(in_word(Base::Hex, WordSize::Byte, "F+~=").display(), "FF");
        // C precedence: `|` < `^` < `&` < shifts < `+` < `×`.
        assert_eq!(in_base(Base::Hex, "1|2&3=").display(), "3");
        assert_eq!(in_base(Base::Hex, "1+1<2=").display(), "8");
        assert_eq!(in_base(Base::Hex, "6^3&1=").display(), "7");
        assert_eq!(in_base(Base::Hex, "2*3m4=").display(), "2");
        assert_eq!(
            in_base(Base::Hex, "F0>4=").pending_expression(),
            "F0 >> 4 ="
        );
    }

    #[test]
    fn toggling_bits() {
        let mut c = in_word(Base::Bin, WordSize::Byte, "");
        c.toggle_bit(7);
        assert_eq!(c.display(), "1000 0000");
        c.toggle_bit(0);
        assert_eq!(c.display(), "1000 0001");
        c.toggle_bit(8);
        assert_eq!(c.display(), "1000 0001");
        // Typing replaces the toggled value.
        press(&mut c, "1");
        assert_eq!(c.display(), "1");

        // After an operator, the toggled number is the next operand.
        let mut c = in_base(Base::Hex, "5+");
        c.toggle_bit(0);
        assert_eq!(c.display(), "4");
        press(&mut c, "=");
        assert_eq!(c.display(), "9");
    }

    #[test]
    fn switching_base() {
        // The number being typed is converted, and typing continues in the new base.
        let mut c = in_base(Base::Dec, "255");
        c.set_base(Base::Hex);
        assert_eq!(c.display(), "FF");
        press(&mut c, "1");
        assert_eq!(c.display(), "FF1");
        c.set_base(Base::Bin);
        assert_eq!(c.display(), "1111 1111 0001");

        // The expression is shown in the new base.
        let mut c = in_base(Base::Dec, "10+5=");
        c.set_base(Base::Hex);
        assert_eq!(c.pending_expression(), "A + 5 =");
        assert_eq!(c.display(), "F");
        press(&mut c, "=");
        assert_eq!(c.display(), "14");

        // Integer bases drop fractions.
        let mut c = in_base(Base::Dec, "7.9");
        c.set_base(Base::Oct);
        assert_eq!(c.display(), "7");
        let mut c = in_base(Base::Dec, "1.5+2");
        c.set_base(Base::Hex);
        assert_eq!(c.pending_expression(), "1 +");
        press(&mut c, "=");
        assert_eq!(c.display(), "3");
        // A result whose expression had fractions keeps only the result.
        let mut c = in_base(Base::Dec, "2.5*2=");
        c.set_base(Base::Hex);
        assert_eq!(c.display(), "5");
        assert_eq!(c.pending_expression(), "");

        // Negative numbers convert to two's complement and back.
        let mut c = in_base(Base::Dec, "5n");
        c.set_word(WordSize::Byte);
        c.set_base(Base::Hex);
        assert_eq!(c.display(), "FB");
        c.set_base(Base::Dec);
        assert_eq!(c.display(), "-5");

        // Numbers wrap to the word size; beyond 64 bits is an error.
        let mut c = in_base(Base::Dec, "300");
        c.set_word(WordSize::Byte);
        c.set_base(Base::Hex);
        assert_eq!(c.display(), "2C");
        let mut c = in_base(Base::Dec, "99999999999999999999");
        assert_eq!(c.display(), "9,999,999,999,999,999");
        press(&mut c, "*1000=");
        c.set_base(Base::Hex);
        assert_eq!(c.display(), "Error");

        // Clearing keeps the base.
        let mut c = in_base(Base::Hex, "Fc");
        assert_eq!(c.base(), Base::Hex);
        press(&mut c, "A");
        assert_eq!(c.display(), "A");
    }

    #[test]
    fn calculation_records_base() {
        let mut c = in_base(Base::Bin, "11+1");
        let calc = c.equals().unwrap();
        assert_eq!(&*calc.expression, "11 + 1 =");
        assert_eq!(calc.base, Base::Bin);
        assert_eq!(calc.result, Value::Int(4));

        // Recalled values are converted to the current base.
        c.set_base(Base::Hex);
        c.recall(Value::Real(255.0));
        assert_eq!(c.display(), "FF");
        c.set_word(WordSize::Byte);
        c.recall(Value::Int(-1));
        assert_eq!(c.display(), "FF");
        c.set_base(Base::Dec);
        c.recall(Value::Int(i64::MAX));
        assert_eq!(c.display(), "9.223372036854776e18");
    }

    #[test]
    fn memory() {
        let mut c = Calculator::new();
        assert!(!c.has_memory());
        press(&mut c, "5");
        c.memory_store();
        // Typing after MS starts a new number.
        press(&mut c, "3");
        assert_eq!(c.display(), "3");
        c.memory_add(false);
        c.memory_add(false);
        press(&mut c, "c2");
        c.memory_add(true);
        press(&mut c, "c");
        // Memory survives clearing.
        c.memory_recall();
        assert_eq!(c.display(), "9");
        press(&mut c, "*2=");
        assert_eq!(c.display(), "18");
        c.memory_clear();
        assert!(!c.has_memory());

        // M+ on an empty memory stores.
        let mut c = in_base(Base::Hex, "A");
        c.memory_add(false);
        c.memory_recall();
        assert_eq!(c.display(), "A");
    }

    #[test]
    fn paste() {
        let mut c = Calculator::new();
        assert!(c.paste("2 + 3 × 4"));
        assert_eq!(c.equals().unwrap().result, Value::Real(14.0));
        let mut c = Calculator::new();
        assert!(c.paste("1,234.5"));
        assert_eq!(c.display(), "1,234.5");
        // A pasted number replaces the one being typed.
        press(&mut c, "c12");
        assert!(c.paste("34"));
        assert_eq!(c.display(), "34");
        // Text that isn't a calculation changes nothing.
        assert!(!c.paste("hello"));
        assert!(!c.paste("FF"));
        assert!(!c.paste("6 AND 3"));
        assert_eq!(c.display(), "34");

        let mut c = in_base(Base::Hex, "");
        assert!(c.paste("0xFF and 0F="));
        assert_eq!(c.pending_expression(), "FF AND");
        press(&mut c, "=");
        assert_eq!(c.display(), "F");
        let mut c = in_base(Base::Bin, "");
        assert!(c.paste("0b1010 << 1"));
        press(&mut c, "=");
        assert_eq!(c.display(), "1 0100");
        let mut c = in_base(Base::Hex, "");
        assert!(c.paste("BAD"));
        assert_eq!(c.display(), "BAD");
    }

    #[test]
    fn results_keep_full_precision() {
        // The display rounds a result, but calculating with it doesn't.
        assert_eq!(run("1/3=*3="), "1");
        assert_eq!(run("10/3=*3="), "10");
        assert_eq!(run("0.000001/3=*3="), "0.000001");
    }

    #[test]
    fn backspace_into_scientific_notation() {
        // 16 nines round to 1e16, whose exponent digits mustn't extend.
        let mut c = Calculator::new();
        press(&mut c, "(9999999999999999+b");
        assert_eq!(c.display(), "1e16");
        press(&mut c, "000");
        assert_eq!(c.display(), "0");
    }

    #[test]
    fn equals_with_nothing_to_compute() {
        assert_eq!(run("(="), "0");
        let mut c = Calculator::new();
        press(&mut c, "5+");
        assert_eq!(c.equals(), None);
        assert_eq!(c.display(), "5");
    }

    #[test]
    fn switching_drops_stale_expression() {
        // An expression that no longer computes the result is dropped.
        let mut c = in_base(Base::Hex, "7/2=");
        c.set_base(Base::Dec);
        assert_eq!(c.display(), "3");
        assert_eq!(c.pending_expression(), "");
        let mut c = in_word(Base::Hex, WordSize::Byte, "7F+1=");
        c.set_word(WordSize::Word);
        assert_eq!(c.display(), "FF80");
        assert_eq!(c.pending_expression(), "");
        // The largest number survives a trip through base 10.
        let mut c = in_base(Base::Hex, "7FFFFFFFFFFFFFFF");
        c.set_base(Base::Dec);
        c.set_base(Base::Hex);
        assert_eq!(c.display(), "7FFF FFFF FFFF FFFF");
    }

    #[test]
    fn paste_negative_numbers() {
        // A `-` after an operator or `(` negates the number after it.
        let mut c = Calculator::new();
        assert!(c.paste("2 × -3"));
        assert_eq!(c.equals().unwrap().result, Value::Real(-6.0));
        let mut c = Calculator::new();
        assert!(c.paste("(-5)"));
        assert_eq!(c.pending_expression(), "(-5)");
        let mut c = in_base(Base::Hex, "");
        assert!(c.paste("10/-2"));
        press(&mut c, "=");
        assert_eq!(c.display(), "FFFF FFFF FFFF FFF8");
        assert!(!c.paste("2*-(3)"));
    }

    #[test]
    fn paste_rejects_what_does_not_fit() {
        let mut c = in_word(Base::Hex, WordSize::Byte, "1");
        assert!(!c.paste("1FF"));
        assert_eq!(c.display(), "1");
        assert!(!Calculator::new().paste(&"(".repeat(2000)));
    }

    #[test]
    fn memory_too_big_for_base_is_kept() {
        let mut c = Calculator::new();
        c.recall(Value::Real(1e20));
        c.memory_store();
        press(&mut c, "c");
        c.set_base(Base::Hex);
        press(&mut c, "5");
        c.memory_add(false);
        c.set_base(Base::Dec);
        c.memory_recall();
        assert_eq!(c.display(), "1e20");
    }

    #[test]
    fn grouping() {
        assert_eq!(run("1234567"), "1,234,567");
        assert_eq!(run("1234.5678"), "1,234.5678");
        assert_eq!(run("123n"), "-123");
        assert_eq!(run("1234n"), "-1,234");
        assert_eq!(run("1000."), "1,000.");
        assert_eq!(expression("1000+2000="), "1,000 + 2,000 =");
        assert_eq!(in_base(Base::Oct, "1234567").display(), "1 234 567");
        assert_eq!(in_base(Base::Hex, "12345").display(), "1 2345");
        assert_eq!(group("1e20", Base::Dec), "1e20");
        // The clipboard gets the number without grouping.
        assert_eq!(
            in_base(Base::Hex, "12345").copy_text().as_deref(),
            Some("12345")
        );
        assert_eq!(run("1/0=").as_str(), "Error");
        assert_eq!(in_base(Base::Dec, "1/0=").copy_text(), None);
    }

    #[test]
    fn formatting() {
        assert_eq!(format_number(1e20), "1e20");
        assert_eq!(format_number(1.0 / 3.0), "0.333333333333");
        assert_eq!(format_number(-2.5), "-2.5");
    }
}
