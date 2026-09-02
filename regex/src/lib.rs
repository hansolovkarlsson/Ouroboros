//! A small POSIX-ERE regular-expression engine for Ouroboros.
//!
//! This is the shared matching logic behind `/bin/grep`'s patterns, written the
//! way the rest of this system's shared logic is: **pure** (no I/O, no syscalls,
//! no heap, no `alloc`) so it links for `aarch64-unknown-none` *and* runs under
//! the host test runner - `cargo test -p regex --target aarch64-apple-darwin`.
//! A regex engine is exactly the kind of code where a foreign observer pays for
//! itself, and a pure crate is the cheapest one available (the lesson the
//! `accounts` crate wrote down).
//!
//! ## Syntax (POSIX ERE, the `egrep` dialect)
//!
//! | form | meaning |
//! |------|---------|
//! | `c` | the literal byte `c` |
//! | `.` | any single byte |
//! | `[abc]` `[a-z]` `[^0-9]` | character class; `]` first or `-` last is literal |
//! | `^` `$` | start / end of the text |
//! | `e*` `e+` `e?` | zero-or-more, one-or-more, zero-or-one |
//! | `ab` | concatenation |
//! | <code>a&#124;b</code> | alternation |
//! | `(e)` | grouping (no capture - nothing reads submatches yet) |
//! | `\c` | literal `c`; `\n` and `\t` are newline and tab |
//!
//! Alternation binds loosest, then concatenation, then the postfix repeats -
//! so `^a|b$` is "starts with `a`" or "ends with `b`", not `^(a|b)$`.
//!
//! POSIX class names work inside a bracket expression: `[[:alpha:]]`,
//! `[[:digit:]_]`, `[^[:space:]]`. All twelve are supported (`alnum`, `alpha`,
//! `blank`, `cntrl`, `digit`, `graph`, `lower`, `print`, `punct`, `space`,
//! `upper`, `xdigit`), and an unknown name is an ERROR rather than a fall back
//! to the literal characters. Note the POSIX form needs BOTH brackets - a bare
//! `[:alpha:]` is one bracket expression containing `:`, `a`, `l`, `p` and `h`,
//! which is what POSIX leaves undefined and what this engine does.
//!
//! Not supported, deliberately: back-references, `{n,m}` counted repetition,
//! and submatch capture. Each is a real
//! addition rather than a tweak, and nothing wants them yet.
//!
//! ## Shape: parse to an AST, emit a program, run it on an explicit stack
//!
//! Three fixed-size arrays, all inside the [`Regex`] value:
//!
//! 1. **Parse** the pattern into [`MAX_NODES`] AST nodes (recursive descent;
//!    recursion depth is paren nesting, capped at [`MAX_DEPTH`]).
//! 2. **Emit** those nodes as [`MAX_PROG`] VM instructions with absolute jump
//!    targets. Emitting from an AST (rather than patching a growing program in
//!    place) is what keeps absolute targets valid - nothing is ever shifted.
//! 3. **Run** the program with an *explicit* backtracking stack of
//!    [`MAX_STACK`] `(pc, sp)` pairs, not host recursion. That is the
//!    load-bearing choice for this OS: a recursive matcher's depth grows with
//!    the *input* (`a*` over a 256-byte line is 256 frames deep), and a
//!    userland program here has a 32 KB guarded stack. An explicit stack makes
//!    the worst case a fixed 2 KB array instead.
//!
//! ## Bounded, and honest about it
//!
//! Two different hazards, handled differently:
//!
//! - **Non-termination** is designed out. A `*`/`+` whose body can match the
//!   empty string (`(a*)*`, `(a|)*`) is the one construct that lets a
//!   backtracking matcher spin without making progress, so it is **rejected at
//!   compile time** ([`Error::EmptyRepeat`]). POSIX allows such patterns; this
//!   engine refuses them, and in exchange every accepted pattern is guaranteed
//!   to terminate - each iteration of a repeat consumes at least one byte, so
//!   iterations are bounded by the text length.
//! - **Exponential backtracking** is still possible on accepted patterns
//!   (`(a|aa)+b` against a long run of `a`s), so the matcher carries a step
//!   budget ([`MAX_STEPS`]) and a finite stack ([`MAX_STACK`]). When either
//!   runs out the answer is [`Match::Limit`], **not a silent "no match"** - the
//!   caller decides what to report. `grep` says so on stderr rather than
//!   quietly dropping a line that might have matched.

#![cfg_attr(not(test), no_std)]

/// Longest pattern accepted (bytes).
pub const MAX_PATTERN: usize = 128;
/// Most AST nodes a pattern may parse into.
pub const MAX_NODES: usize = 128;
/// Most VM instructions a pattern may compile to.
pub const MAX_PROG: usize = 192;
/// Most character classes (`[...]`) in one pattern; each costs a 256-bit bitmap.
pub const MAX_CLASSES: usize = 8;
/// Deepest `(` nesting accepted - bounds the parser's recursion.
pub const MAX_DEPTH: usize = 16;
/// Backtracking stack entries: `(pc, sp)` pairs, 4 bytes each, so 4 KB - a
/// fixed array rather than unbounded host recursion.
///
/// **Sized from the real worst case, which is not one entry per byte.** Every
/// executed `Split` pushes, and nothing pops while a scan is succeeding, so a
/// repeat wrapping an *alternation* pushes one entry per branch per byte:
/// `^(a|b|c)*$` costs ~3 per byte. At 512 this failed on a 170-byte line -
/// well inside `grep`'s own 256-byte buffer - and reported `Match::Limit` for a
/// pattern nobody would call pathological. 1024 covers ~4 branches across a full
/// line; deeper nesting still reports `Limit`, honestly, rather than guessing.
pub const MAX_STACK: usize = 1024;
/// Longest text one match attempt accepts. Positions are `u16` on the
/// backtracking stack, so a longer text reports [`Match::Limit`] rather than
/// wrapping around silently.
pub const MAX_TEXT: usize = u16::MAX as usize;
/// Instructions executed per match attempt before giving up as too complex.
/// With empty-body repeats rejected at compile time the engine cannot loop
/// forever, so this is a backstop against *exponential* backtracking
/// (`(a|aa)*b`), not against non-termination.
pub const MAX_STEPS: u32 = 20_000;

/// Total instructions across *all* start offsets of one unanchored search. The
/// per-offset [`MAX_STEPS`] keeps each attempt honest; this keeps the whole
/// search finite without making an ordinary pattern's budget shrink as the line
/// grows (which is what a single shared budget did).
pub const MAX_TOTAL_STEPS: u32 = 2_000_000;

/// Why a pattern could not be compiled. Every variant is a user error in the
/// pattern except [`Error::TooComplex`], which is this engine's fixed limits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// The pattern is longer than [`MAX_PATTERN`].
    TooLong,
    /// The pattern needs more nodes/instructions/classes/nesting than the
    /// fixed arrays hold.
    TooComplex,
    /// A `(` with no `)`, or a `)` with no `(`.
    UnbalancedParen,
    /// A `[` with no closing `]`.
    UnterminatedClass,
    /// A `\` at the very end of the pattern.
    TrailingBackslash,
    /// `*`, `+` or `?` with nothing before it to repeat.
    NothingToRepeat,
    /// A character-class range whose endpoints are the wrong way round
    /// (`[z-a]`) - almost always a typo, and never worth guessing at.
    BadRange,
    /// `[[:nosuch:]]` - a POSIX class name that does not exist. Refused rather
    /// than read as the literal characters, which is what a parser that does
    /// not know the syntax would do: `[[:alfa:]]` would then quietly match `a`,
    /// `l`, `f`, `:` and `[`, and report success on text containing none of the
    /// letters intended.
    UnknownClassName,
    /// A `*` or `+` applied to something that can match the empty string
    /// (`(a*)*`, `(a|)*`). POSIX allows it; this engine refuses it, because a
    /// loop body that consumes nothing is the one way a backtracking matcher
    /// can spin forever. Rejecting it is what makes every accepted pattern
    /// terminate: each iteration of a repeat now consumes at least one byte, so
    /// the number of iterations is bounded by the text length. Write the
    /// pattern without the nested repeat (`a*` rather than `(a*)*`).
    EmptyRepeat,
}

impl Error {
    /// A short, printable explanation - callers here have no `core::fmt`
    /// (formatting machinery is what drags in the unlinkable relocations).
    pub fn message(&self) -> &'static [u8] {
        match self {
            Error::TooLong => b"pattern too long",
            Error::TooComplex => b"pattern too complex",
            Error::UnbalancedParen => b"unbalanced parenthesis",
            Error::UnterminatedClass => b"unterminated [ ] class",
            Error::TrailingBackslash => b"trailing backslash",
            Error::NothingToRepeat => b"nothing to repeat",
            Error::BadRange => b"character-class range is reversed",
            Error::UnknownClassName => b"unknown [:name:] character class",
            Error::EmptyRepeat => b"repeat of a possibly-empty expression",
        }
    }
}

/// The outcome of a match attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Match {
    /// The pattern matches somewhere in the text.
    Yes,
    /// It does not.
    No,
    /// The step budget or backtracking stack ran out before either could be
    /// established. Distinct from [`Match::No`] on purpose: "I could not tell"
    /// is not "no", and a caller that reports it as one is lying quietly.
    Limit,
}

// --- AST ------------------------------------------------------------------

/// One parsed node. Children are indices into the node array (no heap, no
/// `Box`), and `Empty` is the identity for concatenation.
#[derive(Clone, Copy)]
enum Node {
    Empty,
    Char(u8),
    Any,
    Class(u8),
    Bol,
    Eol,
    Cat(u16, u16),
    Alt(u16, u16),
    Star(u16),
    Plus(u16),
    Quest(u16),
}

// --- VM -------------------------------------------------------------------

/// One compiled instruction. `Split` is the only branching one: try `a`, and
/// keep `b` for when that fails.
#[derive(Clone, Copy)]
enum Inst {
    Char(u8),
    Any,
    Class(u8),
    Bol,
    Eol,
    Split(u16, u16),
    Jmp(u16),
    Match,
}

/// A compiled pattern: fixed arrays, `Copy`-free but cheap to hold on a stack
/// frame (~2 KB).
pub struct Regex {
    prog: [Inst; MAX_PROG],
    prog_len: usize,
    /// One 256-bit membership bitmap per `[...]` class.
    classes: [[u8; 32]; MAX_CLASSES],
    /// Whether each class was written negated (`[^...]`). Needed only for
    /// case-insensitive matching: folding *widens* a positive class but must
    /// *narrow* a negated one - see the `Inst::Class` arm.
    class_negated: [bool; MAX_CLASSES],
    n_classes: usize,
    /// An empty pattern matches every text (grep's behaviour).
    empty: bool,
}

/// Parser state: the pattern, a cursor, and the node arena.
struct Parser<'a> {
    pat: &'a [u8],
    pos: usize,
    nodes: [Node; MAX_NODES],
    n_nodes: usize,
    classes: [[u8; 32]; MAX_CLASSES],
    class_negated: [bool; MAX_CLASSES],
    n_classes: usize,
}

impl<'a> Parser<'a> {
    fn new(pat: &'a [u8]) -> Self {
        Parser {
            pat,
            pos: 0,
            nodes: [Node::Empty; MAX_NODES],
            n_nodes: 0,
            classes: [[0u8; 32]; MAX_CLASSES],
            class_negated: [false; MAX_CLASSES],
            n_classes: 0,
        }
    }

    fn peek(&self) -> Option<u8> {
        self.pat.get(self.pos).copied()
    }

    fn push(&mut self, n: Node) -> Result<u16, Error> {
        if self.n_nodes >= MAX_NODES {
            return Err(Error::TooComplex);
        }
        self.nodes[self.n_nodes] = n;
        self.n_nodes += 1;
        Ok((self.n_nodes - 1) as u16)
    }

    /// `alt := concat ('|' concat)*`
    fn parse_alt(&mut self, depth: usize) -> Result<u16, Error> {
        let mut left = self.parse_concat(depth)?;
        while self.peek() == Some(b'|') {
            self.pos += 1;
            let right = self.parse_concat(depth)?;
            left = self.push(Node::Alt(left, right))?;
        }
        Ok(left)
    }

    /// `concat := repeat*` - stops at `|`, `)`, or end of pattern.
    fn parse_concat(&mut self, depth: usize) -> Result<u16, Error> {
        let mut acc: Option<u16> = None;
        loop {
            match self.peek() {
                None | Some(b'|') | Some(b')') => break,
                _ => {}
            }
            let piece = self.parse_repeat(depth)?;
            acc = Some(match acc {
                None => piece,
                Some(l) => self.push(Node::Cat(l, piece))?,
            });
        }
        match acc {
            Some(n) => Ok(n),
            None => self.push(Node::Empty),
        }
    }

    /// Can this node match without consuming a byte? Used to reject `*`/`+`
    /// over an empty-matching body, which is what would let the matcher loop
    /// without making progress. Zero-width assertions (`^`, `$`) count as
    /// nullable - `(^)*` loops just as happily as `(a*)*`.
    fn nullable(&self, n: u16) -> bool {
        match self.nodes[n as usize] {
            Node::Empty | Node::Bol | Node::Eol => true,
            Node::Char(_) | Node::Any | Node::Class(_) => false,
            Node::Cat(a, b) => self.nullable(a) && self.nullable(b),
            Node::Alt(a, b) => self.nullable(a) || self.nullable(b),
            Node::Star(_) | Node::Quest(_) => true,
            Node::Plus(a) => self.nullable(a),
        }
    }

    /// `repeat := atom ('*' | '+' | '?')*` - stacked postfixes are applied in
    /// order, so `a+?` is "one-or-more, optionally".
    fn parse_repeat(&mut self, depth: usize) -> Result<u16, Error> {
        let mut atom = self.parse_atom(depth)?;
        loop {
            match self.peek() {
                Some(b'*') => {
                    self.pos += 1;
                    if self.nullable(atom) {
                        return Err(Error::EmptyRepeat);
                    }
                    atom = self.push(Node::Star(atom))?;
                }
                Some(b'+') => {
                    self.pos += 1;
                    if self.nullable(atom) {
                        return Err(Error::EmptyRepeat);
                    }
                    atom = self.push(Node::Plus(atom))?;
                }
                Some(b'?') => {
                    self.pos += 1;
                    atom = self.push(Node::Quest(atom))?;
                }
                _ => break,
            }
        }
        Ok(atom)
    }

    fn parse_atom(&mut self, depth: usize) -> Result<u16, Error> {
        let c = match self.peek() {
            Some(c) => c,
            None => return self.push(Node::Empty),
        };
        match c {
            b'*' | b'+' | b'?' => Err(Error::NothingToRepeat),
            b'(' => {
                if depth + 1 > MAX_DEPTH {
                    return Err(Error::TooComplex);
                }
                self.pos += 1;
                let inner = self.parse_alt(depth + 1)?;
                if self.peek() != Some(b')') {
                    return Err(Error::UnbalancedParen);
                }
                self.pos += 1;
                Ok(inner)
            }
            b')' => Err(Error::UnbalancedParen),
            b'.' => {
                self.pos += 1;
                self.push(Node::Any)
            }
            b'^' => {
                self.pos += 1;
                self.push(Node::Bol)
            }
            b'$' => {
                self.pos += 1;
                self.push(Node::Eol)
            }
            b'[' => self.parse_class(),
            b'\\' => {
                self.pos += 1;
                match self.peek() {
                    None => Err(Error::TrailingBackslash),
                    Some(e) => {
                        self.pos += 1;
                        let lit = match e {
                            b'n' => b'\n',
                            b't' => b'\t',
                            other => other,
                        };
                        self.push(Node::Char(lit))
                    }
                }
            }
            other => {
                self.pos += 1;
                self.push(Node::Char(other))
            }
        }
    }

    /// `[abc]`, `[a-z]`, `[^...]`, `[[:alpha:]]`. A `]` immediately after the
    /// (optional) `^` is a literal `]`, and a `-` first or last is a literal
    /// `-` - the POSIX rules, which exist precisely so those two characters
    /// remain typable.
    fn parse_class(&mut self) -> Result<u16, Error> {
        self.pos += 1; // consume '['
        if self.n_classes >= MAX_CLASSES {
            return Err(Error::TooComplex);
        }
        let mut bits = [0u8; 32];
        let mut negate = false;
        if self.peek() == Some(b'^') {
            negate = true;
            self.pos += 1;
        }
        let mut first = true;
        loop {
            let c = match self.peek() {
                None => return Err(Error::UnterminatedClass),
                Some(c) => c,
            };
            if c == b']' && !first {
                self.pos += 1;
                break;
            }
            // `[:name:]` - a POSIX class name. Only meaningful INSIDE a bracket
            // expression, which is why it is handled here and not in `atom`.
            if c == b'[' && self.pat.get(self.pos + 1).copied() == Some(b':') {
                self.pos += 2; // consume "[:"
                let name_start = self.pos;
                loop {
                    match (self.pat.get(self.pos), self.pat.get(self.pos + 1)) {
                        (Some(b':'), Some(b']')) => break,
                        (None, _) => return Err(Error::UnterminatedClass),
                        _ => self.pos += 1,
                    }
                }
                let name = &self.pat[name_start..self.pos];
                self.pos += 2; // consume ":]"
                // `posix_class` decides on the NAME, so the first byte settles
                // whether the name is known; the loop then just collects. One
                // function answers both questions, so there is no second list
                // of names to drift out of step with this one.
                for b in 0..=u8::MAX {
                    match posix_class(name, b) {
                        None => return Err(Error::UnknownClassName),
                        Some(true) => bits[(b >> 3) as usize] |= 1 << (b & 7),
                        Some(false) => {}
                    }
                }
                first = false;
                continue;
            }
            first = false;
            self.pos += 1;
            // An escape inside a class is the literal byte.
            let lo = if c == b'\\' {
                match self.peek() {
                    None => return Err(Error::TrailingBackslash),
                    Some(e) => {
                        self.pos += 1;
                        match e {
                            b'n' => b'\n',
                            b't' => b'\t',
                            other => other,
                        }
                    }
                }
            } else {
                c
            };
            // A range, unless the '-' is the last character before ']'.
            if self.peek() == Some(b'-') && self.pat.get(self.pos + 1).copied() != Some(b']') {
                self.pos += 1; // consume '-'
                // The upper endpoint takes an escape exactly as the lower one
                // does. Reading it raw made `[\t-\n]` compile as 0x09..=0x5C
                // ('\\') plus a literal 'n' - a class matching most of ASCII
                // instead of two control characters, and silently.
                let hi = match self.peek() {
                    None => return Err(Error::UnterminatedClass),
                    Some(b'\\') => {
                        self.pos += 1;
                        match self.peek() {
                            None => return Err(Error::TrailingBackslash),
                            Some(e) => match e {
                                b'n' => b'\n',
                                b't' => b'\t',
                                other => other,
                            },
                        }
                    }
                    Some(h) => h,
                };
                self.pos += 1;
                // REJECT rather than silently swap. `[z-a]` is a typo, and this
                // crate's contract (which grep states out loud) is that a bad
                // pattern is an error, not a fallback to something else that
                // happens to compile.
                if lo > hi {
                    return Err(Error::BadRange);
                }
                for b in lo..=hi {
                    bits[(b >> 3) as usize] |= 1 << (b & 7);
                }
            } else {
                bits[(lo >> 3) as usize] |= 1 << (lo & 7);
            }
        }
        if negate {
            for b in bits.iter_mut() {
                *b = !*b;
            }
        }
        self.classes[self.n_classes] = bits;
        self.class_negated[self.n_classes] = negate;
        self.n_classes += 1;
        self.push(Node::Class((self.n_classes - 1) as u8))
    }
}

/// Whether byte `b` belongs to the POSIX character class `name`, or `None` if
/// `name` is not one of the twelve.
///
/// COMPUTED from `core`'s own `is_ascii_*` predicates rather than transcribed
/// as twelve 32-byte bit tables. A table of 384 hex bytes is a transcription
/// task, and a wrong bit in one would be invisible: the class would simply
/// match slightly the wrong set, in a direction no test looks at unless it was
/// written for that byte.
///
/// TWO PLACES WHERE POSIX AND `core` DISAGREE, both handled explicitly:
///
/// - `is_ascii_whitespace` EXCLUDES vertical tab (0x0B); POSIX `[:space:]`
///   includes it. Six characters, not five.
/// - POSIX `[:print:]` is `[:graph:]` plus the space; `core` has no `print`.
///
/// `[:punct:]` is `is_ascii_punctuation`, which is the POSIX set (printable,
/// neither space nor alphanumeric) - checked against the count, not assumed.
fn posix_class(name: &[u8], b: u8) -> Option<bool> {
    Some(match name {
        b"alnum" => b.is_ascii_alphanumeric(),
        b"alpha" => b.is_ascii_alphabetic(),
        b"blank" => b == b' ' || b == b'\t',
        b"cntrl" => b.is_ascii_control(),
        b"digit" => b.is_ascii_digit(),
        b"graph" => b.is_ascii_graphic(),
        b"lower" => b.is_ascii_lowercase(),
        b"print" => b.is_ascii_graphic() || b == b' ',
        b"punct" => b.is_ascii_punctuation(),
        b"space" => b.is_ascii_whitespace() || b == 0x0B,
        b"upper" => b.is_ascii_uppercase(),
        b"xdigit" => b.is_ascii_hexdigit(),
        _ => return None,
    })
}

/// Emitter state: the growing program.
struct Emitter {
    prog: [Inst; MAX_PROG],
    len: usize,
}

impl Emitter {
    fn emit(&mut self, i: Inst) -> Result<usize, Error> {
        if self.len >= MAX_PROG {
            return Err(Error::TooComplex);
        }
        self.prog[self.len] = i;
        self.len += 1;
        Ok(self.len - 1)
    }

    /// Emit `node`'s instructions. Jump targets are absolute; because every
    /// branch target is patched *after* the code it points past is emitted,
    /// nothing ever has to be moved.
    fn gen(&mut self, nodes: &[Node], n: u16) -> Result<(), Error> {
        match nodes[n as usize] {
            Node::Empty => Ok(()),
            Node::Char(c) => self.emit(Inst::Char(c)).map(|_| ()),
            Node::Any => self.emit(Inst::Any).map(|_| ()),
            Node::Class(i) => self.emit(Inst::Class(i)).map(|_| ()),
            Node::Bol => self.emit(Inst::Bol).map(|_| ()),
            Node::Eol => self.emit(Inst::Eol).map(|_| ()),
            Node::Cat(a, b) => {
                self.gen(nodes, a)?;
                self.gen(nodes, b)
            }
            Node::Alt(a, b) => {
                //   split A, B
                // A: <a>
                //   jmp END
                // B: <b>
                // END:
                let split = self.emit(Inst::Split(0, 0))?;
                self.gen(nodes, a)?;
                let jmp = self.emit(Inst::Jmp(0))?;
                let b_start = self.len;
                self.gen(nodes, b)?;
                self.prog[split] = Inst::Split(split as u16 + 1, b_start as u16);
                self.prog[jmp] = Inst::Jmp(self.len as u16);
                Ok(())
            }
            Node::Star(a) => {
                // L: split BODY, END
                // BODY: <a>
                //    jmp L
                // END:
                let split = self.emit(Inst::Split(0, 0))?;
                self.gen(nodes, a)?;
                self.emit(Inst::Jmp(split as u16))?;
                self.prog[split] = Inst::Split(split as u16 + 1, self.len as u16);
                Ok(())
            }
            Node::Plus(a) => {
                // BODY: <a>
                //    split BODY, END
                // END:
                let body = self.len;
                self.gen(nodes, a)?;
                let split = self.emit(Inst::Split(0, 0))?;
                self.prog[split] = Inst::Split(body as u16, self.len as u16);
                Ok(())
            }
            Node::Quest(a) => {
                //   split BODY, END
                // BODY: <a>
                // END:
                let split = self.emit(Inst::Split(0, 0))?;
                self.gen(nodes, a)?;
                self.prog[split] = Inst::Split(split as u16 + 1, self.len as u16);
                Ok(())
            }
        }
    }
}

/// ASCII lower-case fold of one byte.
fn fold_byte(b: u8) -> u8 {
    if b.is_ascii_uppercase() {
        b + 32
    } else {
        b
    }
}

impl Regex {
    /// Compile an ERE pattern. An empty pattern matches every text, as `grep`'s
    /// empty pattern does.
    pub fn compile(pattern: &[u8]) -> Result<Regex, Error> {
        if pattern.len() > MAX_PATTERN {
            return Err(Error::TooLong);
        }
        if pattern.is_empty() {
            return Ok(Regex {
                prog: [Inst::Match; MAX_PROG],
                prog_len: 1,
                classes: [[0u8; 32]; MAX_CLASSES],
                class_negated: [false; MAX_CLASSES],
                n_classes: 0,
                empty: true,
            });
        }
        let mut p = Parser::new(pattern);
        let root = p.parse_alt(0)?;
        if p.pos != p.pat.len() {
            // parse_alt stops at an unmatched ')'
            return Err(Error::UnbalancedParen);
        }
        let mut e = Emitter { prog: [Inst::Match; MAX_PROG], len: 0 };
        e.gen(&p.nodes[..p.n_nodes], root)?;
        e.emit(Inst::Match)?;
        Ok(Regex {
            prog: e.prog,
            prog_len: e.len,
            classes: p.classes,
            class_negated: p.class_negated,
            n_classes: p.n_classes,
            empty: false,
        })
    }

    /// Does the pattern match anywhere in `text`? With `fold`, matching is
    /// ASCII case-insensitive (`grep -i`).
    ///
    /// Unanchored: the program is run from each start offset in turn, which is
    /// what makes `^` an ordinary instruction (it simply fails at any offset
    /// but 0) instead of a special case in the search loop.
    pub fn is_match(&self, text: &[u8], fold: bool) -> Match {
        if self.empty {
            return Match::Yes;
        }
        if text.len() > MAX_TEXT {
            return Match::Limit;
        }
        // The stack is allocated ONCE for the whole search, not per start offset.
        // `run` used to declare it locally, so an unanchored scan over a
        // non-matching 256-byte line zeroed 4 KB per offset - a quarter of a
        // megabyte of memset to decide one line.
        let mut stack = [(0u16, 0u16); MAX_STACK];
        // The budget is PER START OFFSET, with a separate total cap below.
        //
        // Sharing one budget across every offset looked tighter and was wrong in
        // practice: an unanchored search retries at each offset, so a pattern
        // like `.*zzz` against a non-matching line spends budget linearly in the
        // line length and exhausted 20,000 steps by ~99 bytes - well inside
        // grep's 256-byte line - reporting `Limit` for an entirely ordinary
        // pattern. grep then *drops* that line under both polarities, so `-v`
        // silently stops printing long lines it was meant to print. A per-offset
        // budget keeps the answer correct for real patterns; MAX_TOTAL_STEPS
        // keeps the whole search finite for pathological ones.
        let mut total = 0u32;
        let mut start = 0usize;
        loop {
            let mut steps = 0u32;
            let outcome = self.run(text, start, fold, &mut stack, &mut steps);
            total = total.saturating_add(steps);
            if total > MAX_TOTAL_STEPS {
                return Match::Limit;
            }
            match outcome {
                Match::Yes => return Match::Yes,
                Match::Limit => return Match::Limit,
                Match::No => {}
            }
            if start >= text.len() {
                return Match::No;
            }
            start += 1;
        }
    }

    /// Run the program from one start offset, backtracking on an explicit
    /// stack. Returns [`Match::Limit`] if the step budget or the stack runs
    /// out - never a silent "no".
    fn run(
        &self,
        text: &[u8],
        start: usize,
        fold: bool,
        stack: &mut [(u16, u16); MAX_STACK],
        steps: &mut u32,
    ) -> Match {
        let mut sp_top = 0usize;
        let mut pc = 0u16;
        let mut sp = start as u16;

        loop {
            // Execute straight-line until this thread matches or fails.
            let failed = loop {
                *steps += 1;
                if *steps > MAX_STEPS {
                    return Match::Limit;
                }
                match self.prog[pc as usize] {
                    Inst::Match => return Match::Yes,
                    Inst::Char(c) => {
                        let at = sp as usize;
                        let ok = at < text.len()
                            && if fold {
                                fold_byte(text[at]) == fold_byte(c)
                            } else {
                                text[at] == c
                            };
                        if !ok {
                            break true;
                        }
                        pc += 1;
                        sp += 1;
                    }
                    Inst::Any => {
                        if sp as usize >= text.len() {
                            break true;
                        }
                        pc += 1;
                        sp += 1;
                    }
                    Inst::Class(i) => {
                        let at = sp as usize;
                        if at >= text.len() {
                            break true;
                        }
                        let bits = &self.classes[i as usize];
                        let b = text[at];
                        let member = |c: u8| bits[(c >> 3) as usize] & (1 << (c & 7)) != 0;
                        let ok = if !fold {
                            member(b)
                        } else {
                            // Case folding asks "does EITHER case match the class
                            // the user wrote". For a positive class the stored
                            // bitmap *is* that class, so either case suffices.
                            // For a NEGATED class the bitmap is already the
                            // complement, so "either case is outside the original
                            // set" means BOTH cases must be inside the stored one.
                            // Testing either way round there made `[^a]` match 'a'
                            // under -i - a negated class matching the very
                            // character it was written to exclude.
                            let alt = if b.is_ascii_lowercase() {
                                b - 32
                            } else if b.is_ascii_uppercase() {
                                b + 32
                            } else {
                                b
                            };
                            if self.class_negated[i as usize] {
                                member(b) && member(alt)
                            } else {
                                member(b) || member(alt)
                            }
                        };
                        if !ok {
                            break true;
                        }
                        pc += 1;
                        sp += 1;
                    }
                    Inst::Bol => {
                        if sp != 0 {
                            break true;
                        }
                        pc += 1;
                    }
                    Inst::Eol => {
                        if sp as usize != text.len() {
                            break true;
                        }
                        pc += 1;
                    }
                    Inst::Jmp(t) => pc = t,
                    Inst::Split(a, b) => {
                        if sp_top >= MAX_STACK {
                            return Match::Limit;
                        }
                        stack[sp_top] = (b, sp);
                        sp_top += 1;
                        pc = a;
                    }
                }
            };
            if failed {
                if sp_top == 0 {
                    return Match::No;
                }
                sp_top -= 1;
                let (bpc, bsp) = stack[sp_top];
                pc = bpc;
                sp = bsp;
            }
        }
    }

    /// Number of compiled instructions - for tests and diagnostics.
    pub fn prog_len(&self) -> usize {
        self.prog_len
    }

    /// Number of character classes the pattern used.
    pub fn class_count(&self) -> usize {
        self.n_classes
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Compile and match, asserting the pattern is valid.
    fn m(pat: &str, text: &str) -> bool {
        match Regex::compile(pat.as_bytes()).unwrap().is_match(text.as_bytes(), false) {
            Match::Yes => true,
            Match::No => false,
            Match::Limit => panic!("hit the engine limit on {pat:?} / {text:?}"),
        }
    }

    fn mi(pat: &str, text: &str) -> bool {
        match Regex::compile(pat.as_bytes()).unwrap().is_match(text.as_bytes(), true) {
            Match::Yes => true,
            Match::No => false,
            Match::Limit => panic!("hit the engine limit"),
        }
    }

    fn err(pat: &str) -> Error {
        match Regex::compile(pat.as_bytes()) {
            Err(e) => e,
            Ok(_) => panic!("{pat:?} compiled but should not have"),
        }
    }

    #[test]
    fn literals_and_substring_search() {
        assert!(m("abc", "abc"));
        assert!(m("abc", "xxabcxx")); // unanchored
        assert!(!m("abc", "ab"));
        assert!(!m("abc", "abx"));
        // An empty pattern matches anything, including empty text (grep's rule).
        assert!(m("", "anything"));
        assert!(m("", ""));
    }

    #[test]
    fn dot_matches_any_byte() {
        assert!(m("a.c", "abc"));
        assert!(m("a.c", "axc"));
        assert!(m("a.c", "a c"));
        assert!(!m("a.c", "ac")); // dot must consume something
        assert!(!m(".", "")); // nothing to consume
    }

    #[test]
    fn star_plus_quest() {
        assert!(m("ab*c", "ac"));
        assert!(m("ab*c", "abc"));
        assert!(m("ab*c", "abbbbc"));
        assert!(!m("ab+c", "ac"));
        assert!(m("ab+c", "abc"));
        assert!(m("ab+c", "abbc"));
        assert!(m("ab?c", "ac"));
        assert!(m("ab?c", "abc"));
        assert!(!m("ab?c", "abbc"));
        // Greedy star must give bytes back so the rest can match.
        assert!(m("a.*c", "abcxxc"));
        assert!(m(".*", ""));
    }

    #[test]
    fn anchors() {
        assert!(m("^abc", "abcdef"));
        assert!(!m("^abc", "xabcdef"));
        assert!(m("abc$", "xxabc"));
        assert!(!m("abc$", "abcx"));
        assert!(m("^abc$", "abc"));
        assert!(!m("^abc$", "abcd"));
        assert!(m("^$", ""));
        assert!(!m("^$", "x"));
    }

    #[test]
    fn character_classes() {
        assert!(m("[abc]", "b"));
        assert!(!m("[abc]", "d"));
        assert!(m("[a-z]+", "hello"));
        assert!(!m("^[a-z]+$", "Hello"));
        assert!(m("[^0-9]", "a"));
        assert!(!m("^[^0-9]+$", "a1"));
        assert!(m("[0-9][0-9]", "x42x"));
        // ']' first is a literal; '-' last is a literal.
        assert!(m("[]]", "]"));
        assert!(m("[a-]", "-"));
        assert!(m("[a-]", "a"));
        // A reversed range is a typo, and is refused rather than guessed at.
        assert_eq!(err("[z-a]"), Error::BadRange);
    }

    #[test]
    fn alternation_and_groups() {
        assert!(m("cat|dog", "hotdog"));
        assert!(m("cat|dog", "cats"));
        assert!(!m("cat|dog", "bird"));
        assert!(m("^(cat|dog)$", "cat"));
        assert!(m("^(cat|dog)$", "dog"));
        assert!(!m("^(cat|dog)$", "catdog"));
        assert!(m("(ab)+c", "ababc"));
        assert!(!m("^(ab)+c$", "abac"));
        assert!(m("a(b|c)*d", "abcbcd"));
        assert!(m("^(a|b|c)$", "b")); // three-way
        // Alternation binds loosest: "^a|b$" is "^a" or "b$".
        assert!(m("^a|b$", "axx"));
        assert!(m("^a|b$", "xxb"));
        assert!(!m("^a|b$", "xax"));
    }

    #[test]
    fn escapes() {
        assert!(m(r"a\.c", "a.c"));
        assert!(!m(r"a\.c", "abc"));
        assert!(m(r"\*", "*"));
        assert!(m(r"\\", r"\"));
        assert!(m(r"a\|b", "a|b"));
        assert!(m(r"\(x\)", "(x)"));
        assert!(m(r"a\tb", "a\tb"));
        assert!(m(r"a\nb", "a\nb"));
        // An escape of an ordinary character is just that character.
        assert!(m(r"\q", "q"));
    }

    #[test]
    fn case_folding() {
        assert!(mi("hello", "HELLO"));
        assert!(mi("HeLLo", "hello"));
        assert!(!m("hello", "HELLO"));
        // Folding reaches into classes too.
        assert!(mi("[a-z]+", "ABC"));
        assert!(mi("^[A-Z]+$", "abc"));
        assert!(!mi("[0-9]", "a"));
    }

    #[test]
    fn negated_classes_narrow_under_folding() {
        // The bug this guards: folding used to widen every class, so a negated
        // one matched the character it was written to exclude.
        assert!(!mi("[^a]", "a"));
        assert!(!mi("[^a]", "A"));
        assert!(mi("[^a]", "b"));
        assert!(!mi("^[^a-z]+$", "ABC")); // -i: A..Z fold into the excluded set
        assert!(mi("^[^a-z]+$", "123"));
        // ...while a positive class still widens, as it must.
        assert!(mi("[a-z]", "A"));
        assert!(mi("^[a-z]+$", "AbC"));
        // Case-insensitive matching of a class with no letters is unaffected.
        assert!(!mi("[^0-9]", "5"));
        assert!(mi("[^0-9]", "x"));
    }

    #[test]
    fn class_range_endpoints_both_take_escapes() {
        // `[\t-\n]` is tab..newline - not 0x09..=0x5C plus a stray 'n', which
        // is what reading the upper endpoint raw produced (and which matched
        // most of ASCII, silently).
        assert!(m(r"[\t-\n]", "\t"));
        assert!(m(r"[\t-\n]", "\n"));
        assert!(!m(r"[\t-\n]", "A"));
        assert!(!m(r"[\t-\n]", "5"));
        assert!(!m(r"[\t-\n]", "n"));
        // An escaped ']' as the upper endpoint closes nothing - it is the range's
        // end. ('Z'..']' is 0x5A..0x5D, so it covers '[' and '\'.)
        assert!(m(r"[Z-\]]", "["));
        assert!(m(r"[Z-\]]", "\\"));
        assert!(!m(r"[Z-\]]", "a"));
        // ...and `[a-\]]` really is reversed ('a' is 0x61, ']' is 0x5D), so it is
        // refused rather than quietly swapped into something that compiles.
        assert_eq!(err(r"[a-\]]"), Error::BadRange);
    }

    #[test]
    fn ordinary_patterns_decide_non_matching_lines() {
        // A MATCH short-circuits, so only non-matching input actually spends the
        // budget - which is why the stack test below could not catch the shared
        // budget making these return Limit at ~99 bytes.
        for pat in [".*zzz", "[a-z]*zzz", "a*b", "^(a|b|c)*$x"] {
            let re = Regex::compile(pat.as_bytes()).unwrap();
            for n in [8usize, 99, 170, 256] {
                let text = vec![b'a'; n];
                assert_eq!(re.is_match(&text, false), Match::No, "{pat} at n = {n}");
            }
        }
    }

    #[test]
    fn ordinary_patterns_fit_the_backtracking_stack() {
        // A repeat around an alternation pushes ~one entry per branch per byte,
        // so this is the shape that used to exhaust the stack well inside grep's
        // own 256-byte line buffer and report Limit for a normal pattern.
        let re = Regex::compile(b"^(a|b|c)*$").unwrap();
        for n in [8usize, 64, 170, 256] {
            let text = vec![b'a'; n];
            assert_eq!(re.is_match(&text, false), Match::Yes, "n = {n}");
        }
    }

    #[test]
    fn realistic_patterns() {
        assert!(m("^[a-z]+:[0-9]+$", "root:0"));
        assert!(!m("^[a-z]+:[0-9]+$", "root:x"));
        assert!(m("^#", "# a comment"));
        assert!(m(r"\.txt$", "notes.txt"));
        assert!(!m(r"\.txt$", "notes.txtx"));
        assert!(m("(ERROR|WARN|FATAL)", "2026 WARN disk"));
        assert!(m("^ *$", "     ")); // blank-ish line
        assert!(m("a{", "a{")); // no counted repetition: '{' is a literal
    }

    #[test]
    fn syntax_errors_are_reported_not_guessed() {
        assert_eq!(err("(ab"), Error::UnbalancedParen);
        assert_eq!(err("ab)"), Error::UnbalancedParen);
        assert_eq!(err("[abc"), Error::UnterminatedClass);
        assert_eq!(err(r"ab\"), Error::TrailingBackslash);
        assert_eq!(err("*ab"), Error::NothingToRepeat);
        assert_eq!(err("+"), Error::NothingToRepeat);
        // A repeat whose body can match empty is refused (see Error::EmptyRepeat).
        assert_eq!(err("(a*)*"), Error::EmptyRepeat);
        assert_eq!(err("(|a)*"), Error::EmptyRepeat);
        assert_eq!(err("(a?)+"), Error::EmptyRepeat);
        assert_eq!(err("(^)*"), Error::EmptyRepeat);
        // ...but a body that must consume something is fine, however nested.
        assert!(Regex::compile(b"(ab?)*").is_ok());
        assert!(Regex::compile(b"((a|b)c)+").is_ok());
        // A pattern longer than the cap is refused rather than truncated.
        let long = [b'a'; MAX_PATTERN + 1];
        assert!(matches!(Regex::compile(&long), Err(Error::TooLong)));
    }

    #[test]
    fn pathological_patterns_terminate() {
        // The classic exponential backtracker, (a*)*b, can't even be built here:
        // an empty-matching loop body is rejected, which is precisely what
        // guarantees every accepted pattern terminates.
        assert_eq!(err("(a*)*b"), Error::EmptyRepeat);

        // A pattern that IS accepted can still be exponential to backtrack -
        // (a|aa)+b over a run of a's with no b. The requirement is only that it
        // terminates, and that it never claims a clean "no" when it merely ran
        // out of budget.
        let re = Regex::compile(b"(a|aa)+b").unwrap();
        let text = [b'a'; 40];
        assert!(matches!(re.is_match(&text, false), Match::No | Match::Limit));
        // With the 'b' actually there, the first greedy path finds it at once.
        let mut ok = [b'a'; 41];
        ok[40] = b'b';
        assert_eq!(re.is_match(&ok, false), Match::Yes);
    }

    #[test]
    fn long_input_does_not_need_deep_recursion() {
        // A recursive matcher would be ~256 frames deep here; the explicit
        // stack makes it a fixed array. (The real point is that this runs at
        // all on a 32 KB guarded userland stack.)
        let re = Regex::compile(b"^a*$").unwrap();
        let text = [b'a'; 256];
        assert_eq!(re.is_match(&text, false), Match::Yes);
        let re2 = Regex::compile(b"x").unwrap();
        assert_eq!(re2.is_match(&text, false), Match::No);
    }

    #[test]
    fn compiles_within_the_fixed_arrays() {
        let re = Regex::compile(b"^(cat|dog)+[0-9]*\\.txt$").unwrap();
        assert!(re.prog_len() <= MAX_PROG);
        assert_eq!(re.class_count(), 1);
    }

    // ---- POSIX character classes -------------------------------------------

    /// How many of the 256 bytes `[[:name:]]` accepts.
    ///
    /// The COUNT is the assertion that a wrong class set cannot survive.
    /// Spot-checking `m("[[:alpha:]]", "a")` passes for any class containing
    /// `a` - alnum, lower, print, graph and xdigit all do - so it distinguishes
    /// almost nothing. A cardinality pins the whole set with one number.
    fn class_size(name: &str) -> usize {
        // A fixed buffer: this crate is `no_std`, so its tests have no `Vec`.
        let mut pat = [0u8; 24];
        let n = name.as_bytes();
        pat[..3].copy_from_slice(b"[[:");
        pat[3..3 + n.len()].copy_from_slice(n);
        pat[3 + n.len()..6 + n.len()].copy_from_slice(b":]]");
        let re = Regex::compile(&pat[..6 + n.len()]).unwrap();
        (0..=u8::MAX)
            .filter(|&b| re.is_match(&[b], false) == Match::Yes)
            .count()
    }

    #[test]
    fn posix_classes_have_the_right_cardinalities() {
        assert_eq!(class_size("upper"), 26);
        assert_eq!(class_size("lower"), 26);
        assert_eq!(class_size("alpha"), 52);
        assert_eq!(class_size("digit"), 10);
        assert_eq!(class_size("alnum"), 62);
        assert_eq!(class_size("xdigit"), 22); // 0-9 A-F a-f
        assert_eq!(class_size("blank"), 2); // space, tab
        assert_eq!(class_size("cntrl"), 33); // 0x00..=0x1F and 0x7F
        assert_eq!(class_size("graph"), 94); // 0x21..=0x7E
        assert_eq!(class_size("print"), 95); // graph + space
        assert_eq!(class_size("punct"), 32); // graph - alnum
        assert_eq!(class_size("space"), 6); // space \t \n \v \f \r
    }

    /// The one place `core` and POSIX disagree, and it is off by exactly this
    /// byte: `is_ascii_whitespace` omits the vertical tab. Written as its own
    /// test because the cardinality above would also pass if `space` held six
    /// entirely different bytes.
    #[test]
    fn posix_space_includes_the_vertical_tab() {
        assert!(m("[[:space:]]", "\x0b"));
        for c in [" ", "\t", "\n", "\x0c", "\r"] {
            assert!(m("[[:space:]]", c), "space should contain {c:?}");
        }
        assert!(!m("[[:space:]]", "a"));
    }

    /// Every class must be DISJOINT from the bytes it excludes, not merely
    /// contain the ones it includes.
    #[test]
    fn posix_classes_exclude_what_they_should() {
        assert!(!m("[[:alpha:]]", "1"));
        assert!(!m("[[:digit:]]", "a"));
        assert!(!m("[[:upper:]]", "a"));
        assert!(!m("[[:lower:]]", "A"));
        assert!(!m("[[:punct:]]", "a"));
        assert!(!m("[[:punct:]]", " "));
        assert!(!m("[[:cntrl:]]", "a"));
        assert!(!m("[[:graph:]]", " "));
        assert!(m("[[:print:]]", " "));
        assert!(!m("[[:xdigit:]]", "g"));
    }

    #[test]
    fn posix_classes_compose_with_the_rest_of_a_bracket_expression() {
        assert!(m("^[[:digit:]]+$", "2026"));
        assert!(m("^[[:alpha:][:digit:]_]+$", "a_9Z"));
        assert!(!m("^[[:alpha:][:digit:]_]+$", "a-9"));
        assert!(m("^[[:digit:]abc]+$", "1a2b"));
        // Negation applies to the assembled set, not to each piece.
        assert!(m("^[^[:digit:]]+$", "abc"));
        assert!(!m("^[^[:digit:]]+$", "ab1"));
        // And they still work under the case-insensitive flag.
        assert!(mi("^[[:lower:]]+$", "ABC"));
    }

    #[test]
    fn an_unknown_class_name_is_an_error_not_a_literal() {
        // The whole point: `[[:alfa:]]` must NOT quietly become the letters
        // a, l, f plus punctuation.
        assert_eq!(Regex::compile(b"[[:alfa:]]").err(), Some(Error::UnknownClassName));
        assert_eq!(Regex::compile(b"[[::]]").err(), Some(Error::UnknownClassName));
        assert_eq!(Regex::compile(b"[[:ALPHA:]]").err(), Some(Error::UnknownClassName));
    }

    #[test]
    fn an_unterminated_class_name_is_an_error() {
        assert_eq!(Regex::compile(b"[[:alpha]").err(), Some(Error::UnterminatedClass));
        assert_eq!(Regex::compile(b"[[:alpha").err(), Some(Error::UnterminatedClass));
    }

    /// POSIX leaves the single-bracket form undefined; this engine reads it as
    /// an ordinary bracket expression. Pinned by a test so the behaviour is a
    /// decision rather than an accident, and so that changing it later is a
    /// visible change.
    #[test]
    fn a_bare_colon_form_is_an_ordinary_bracket_expression() {
        assert!(m("[:alpha:]", "a"));
        assert!(m("[:alpha:]", ":"));
        assert!(!m("[:alpha:]", "z"));
    }
}
