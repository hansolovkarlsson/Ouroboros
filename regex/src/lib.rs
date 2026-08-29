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
//! Not supported, deliberately: back-references, `{n,m}` counted repetition,
//! POSIX class names (`[:alpha:]`), and submatch capture. Each is a real
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
/// Backtracking stack entries. The matcher's worst case is a fixed array of
/// this many `(pc, sp)` pairs (4 bytes each, so 2 KB) rather than unbounded
/// host recursion. A greedy `*` pushes one entry per byte it consumes, so this
/// has to exceed the longest text a caller will match - `grep`'s line buffer is
/// 256 bytes, leaving room for several stacked repeats on one line.
pub const MAX_STACK: usize = 512;
/// Longest text one match attempt accepts. Positions are `u16` on the
/// backtracking stack, so a longer text reports [`Match::Limit`] rather than
/// wrapping around silently.
pub const MAX_TEXT: usize = u16::MAX as usize;
/// Instructions executed per match attempt before giving up as too complex.
/// With empty-body repeats rejected at compile time the engine cannot loop
/// forever, so this is a backstop against *exponential* backtracking
/// (`(a|aa)*b`), not against non-termination.
pub const MAX_STEPS: u32 = 20_000;

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

    /// `[abc]`, `[a-z]`, `[^...]`. A `]` immediately after the (optional) `^`
    /// is a literal `]`, and a `-` first or last is a literal `-` - the POSIX
    /// rules, which exist precisely so those two characters remain typable.
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
                let hi = match self.peek() {
                    None => return Err(Error::UnterminatedClass),
                    Some(h) => h,
                };
                self.pos += 1;
                let (lo, hi) = if lo <= hi { (lo, hi) } else { (hi, lo) };
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
        self.n_classes += 1;
        self.push(Node::Class((self.n_classes - 1) as u8))
    }
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
        let mut start = 0usize;
        loop {
            match self.run(text, start, fold) {
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
    fn run(&self, text: &[u8], start: usize, fold: bool) -> Match {
        let mut stack = [(0u16, 0u16); MAX_STACK];
        let mut sp_top = 0usize;
        let mut pc = 0u16;
        let mut sp = start as u16;
        let mut steps = 0u32;

        loop {
            // Execute straight-line until this thread matches or fails.
            let failed = loop {
                steps += 1;
                if steps > MAX_STEPS {
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
                        let mut ok = bits[(b >> 3) as usize] & (1 << (b & 7)) != 0;
                        if fold && !ok {
                            // Try the other case, so [a-z] with -i also accepts
                            // 'A' without rewriting the compiled bitmap.
                            let alt = if b.is_ascii_lowercase() {
                                b - 32
                            } else if b.is_ascii_uppercase() {
                                b + 32
                            } else {
                                b
                            };
                            ok = bits[(alt >> 3) as usize] & (1 << (alt & 7)) != 0;
                        }
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
        // A reversed range is accepted as the range it obviously means.
        assert!(m("[z-a]", "m"));
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
}
