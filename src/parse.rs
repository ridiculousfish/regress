//! Parser from regex patterns to IR

use crate::api;
use crate::charclasses;
use crate::codepointset::{CodePointSet, Interval};
use crate::folds;
use crate::ir;
use crate::types::{
    BracketContents, CaptureGroupID, CaptureGroupName, CharacterClassType, MAX_CAPTURE_GROUPS,
    MAX_LOOPS,
};
use crate::unicode::{is_id_continue, is_id_start};
use std::collections::HashMap;
use std::{error::Error as StdError, fmt, iter::Peekable};

/// Represents an error encountered during regex compilation.
///
/// The text contains a human-readable error message.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Error {
    pub text: String,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str(&self.text)
    }
}

impl StdError for Error {}

enum ClassAtom {
    CodePoint(char),
    CharacterClass {
        class_type: CharacterClassType,
        positive: bool,
    },
}

fn error<S, T>(text: S) -> Result<T, Error>
where
    S: ToString,
{
    Err(Error {
        text: text.to_string(),
    })
}

fn make_cat(nodes: ir::NodeList) -> ir::Node {
    match nodes.len() {
        0 => ir::Node::Empty,
        1 => nodes.into_iter().next().unwrap(),
        _ => ir::Node::Cat(nodes),
    }
}

fn make_alt(nodes: ir::NodeList) -> ir::Node {
    let mut mright = None;
    for node in nodes.into_iter().rev() {
        match mright {
            None => mright = Some(node),
            Some(right) => mright = Some(ir::Node::Alt(Box::new(node), Box::new(right))),
        }
    }
    mright.unwrap_or(ir::Node::Empty)
}

/// \return a CodePointSet for a given character escape (positive or negative).
/// See ES9 21.2.2.12.
fn codepoints_from_class(ct: CharacterClassType, positive: bool) -> CodePointSet {
    let mut cps;
    match ct {
        CharacterClassType::Digits => {
            cps = CodePointSet::from_sorted_disjoint_intervals(charclasses::DIGITS.to_vec())
        }
        CharacterClassType::Words => {
            cps = CodePointSet::from_sorted_disjoint_intervals(charclasses::WORD_CHARS.to_vec())
        }
        CharacterClassType::Spaces => {
            cps = CodePointSet::from_sorted_disjoint_intervals(charclasses::WHITESPACE.to_vec());
            for &iv in charclasses::LINE_TERMINATOR.iter() {
                cps.add(iv)
            }
        }
    };
    if !positive {
        cps = cps.inverted()
    }
    cps
}

/// \return a Bracket for a given character escape (positive or negative).
fn make_bracket_class(ct: CharacterClassType, positive: bool) -> ir::Node {
    ir::Node::Bracket(BracketContents {
        invert: false,
        cps: codepoints_from_class(ct, positive),
    })
}

fn add_class_atom(bc: &mut BracketContents, atom: ClassAtom) {
    match atom {
        ClassAtom::CodePoint(c) => bc.cps.add_one(c as u32),
        ClassAtom::CharacterClass {
            class_type,
            positive,
        } => {
            bc.cps.add_set(codepoints_from_class(class_type, positive));
        }
    }
}

struct LookaroundParams {
    negate: bool,
    backwards: bool,
}

/// Represents the state used to parse a regex.
struct Parser<'a> {
    /// The remaining input.
    input: Peekable<std::str::Chars<'a>>,

    /// Flags used.
    flags: api::Flags,

    /// Number of loops.
    loop_count: u32,

    /// Number of capturing groups.
    group_count: CaptureGroupID,

    /// Maximum number of capturing groups.
    group_count_max: u32,

    /// Named capture group references.
    named_group_indices: HashMap<CaptureGroupName, u32>,

    /// Maximum backreference encountered.
    /// Note that values larger than will fit are early errors.
    max_backref: u32,

    /// Whether a lookbehind was encountered.
    has_lookbehind: bool,
}

impl<'a> Parser<'a> {
    /// Consume a character, returning it.
    fn consume(&mut self, c: char) -> char {
        let nc = self.input.next();
        std::debug_assert!(nc == Some(c), "char was not next");
        nc.unwrap()
    }

    /// If our contents begin with the char c, consume it from our contents
    /// and return true. Otherwise return false.
    fn try_consume(&mut self, c: char) -> bool {
        let mut cursor = self.input.clone();
        if cursor.next() == Some(c) {
            self.input = cursor;
            true
        } else {
            false
        }
    }

    /// If our contents begin with the string \p s, consume it from our contents
    /// and return true. Otherwise return false.
    fn try_consume_str(&mut self, s: &str) -> bool {
        let mut cursor = self.input.clone();
        for c1 in s.chars() {
            if cursor.next() != Some(c1) {
                return false;
            }
        }
        self.input = cursor;
        true
    }

    /// Peek at the next character.
    fn peek(&mut self) -> Option<char> {
        self.input.peek().copied()
    }

    /// \return the next character.
    fn next(&mut self) -> Option<char> {
        self.input.next()
    }

    fn try_parse(&mut self) -> Result<ir::Regex, Error> {
        self.parse_capture_groups();

        // Parse a catenation. If we consume everything, it's success. If there's
        // something left, it's an error (for example, an excess closing paren).
        let body = self.consume_disjunction()?;
        match self.input.peek() {
            Some(')') => error("Unbalanced parenthesis"),
            Some(c) => error(format!("Unexpected char: {}", c)),
            None => self.finalize(ir::Regex {
                node: make_cat(vec![body, ir::Node::Goal]),
                flags: self.flags,
            }),
        }
    }

    /// ES6 21.2.2.3 Disjunction.
    fn consume_disjunction(&mut self) -> Result<ir::Node, Error> {
        let mut terms = vec![self.consume_term()?];
        while self.try_consume('|') {
            terms.push(self.consume_term()?)
        }
        Ok(make_alt(terms))
    }

    /// ES6 21.2.2.5 Term.
    fn consume_term(&mut self) -> Result<ir::Node, Error> {
        let mut result: Vec<ir::Node> = Vec::new();
        loop {
            let start_group = self.group_count;
            let start_offset = result.len();
            let mut quantifier_allowed = true;

            let nc = self.peek();
            if nc.is_none() {
                return Ok(make_cat(result));
            }
            let c = nc.unwrap();
            match c {
                // A concatenation is terminated by closing parens or vertical bar (alternations).
                ')' | '|' => break,
                '^' => {
                    self.consume('^');
                    result.push(ir::Node::Anchor(ir::AnchorType::StartOfLine));
                    quantifier_allowed = false;
                }

                '$' => {
                    self.consume('$');
                    result.push(ir::Node::Anchor(ir::AnchorType::EndOfLine));
                    quantifier_allowed = false;
                }

                '\\' => {
                    self.consume('\\');
                    result.push(self.consume_atom_escape()?);
                }

                '.' => {
                    self.consume('.');
                    result.push(if self.flags.dot_all {
                        ir::Node::MatchAny
                    } else {
                        ir::Node::MatchAnyExceptLineTerminator
                    });
                }

                '(' => {
                    if self.try_consume_str("(?=") {
                        // Positive lookahead.
                        quantifier_allowed = false;
                        result.push(self.consume_lookaround_assertion(LookaroundParams {
                            negate: false,
                            backwards: false,
                        })?);
                    } else if self.try_consume_str("(?!") {
                        // Negative lookahead.
                        quantifier_allowed = false;
                        result.push(self.consume_lookaround_assertion(LookaroundParams {
                            negate: true,
                            backwards: false,
                        })?);
                    } else if self.try_consume_str("(?<=") {
                        // Positive lookbehind.
                        self.has_lookbehind = true;
                        result.push(self.consume_lookaround_assertion(LookaroundParams {
                            negate: false,
                            backwards: true,
                        })?);
                    } else if self.try_consume_str("(?<!") {
                        // Negative lookbehind.
                        self.has_lookbehind = true;
                        result.push(self.consume_lookaround_assertion(LookaroundParams {
                            negate: true,
                            backwards: true,
                        })?);
                    } else if self.try_consume_str("(?:") {
                        // Non-capturing group.
                        result.push(self.consume_disjunction()?);
                    } else {
                        // Capturing group.
                        self.consume('(');
                        let group = self.group_count;
                        if self.group_count as usize >= MAX_CAPTURE_GROUPS {
                            return error("Capture group count limit exceeded");
                        }
                        self.group_count += 1;

                        // Parse capture group name.
                        if self.try_consume_str("?") {
                            let group_name = if let Some(group_name) =
                                self.try_consume_named_capture_group_name()
                            {
                                group_name
                            } else {
                                return error("Invalid token at named capture group identifier");
                            };
                            let contents = self.consume_disjunction()?;
                            result.push(ir::Node::NamedCaptureGroup(
                                Box::new(contents),
                                group,
                                group_name,
                            ))
                        } else {
                            let contents = self.consume_disjunction()?;
                            result.push(ir::Node::CaptureGroup(Box::new(contents), group))
                        }
                    }
                    if !self.try_consume(')') {
                        return error("Unbalanced parenthesis");
                    }
                }

                '[' => {
                    result.push(self.consume_bracket()?);
                }

                ']' => {
                    return error("Unbalanced bracket");
                }

                c => {
                    // It's an error if this parses successfully as a quantifier.
                    // Note this covers *, +, ? as well.
                    let saved = self.input.clone();
                    if let Ok(Some(_)) = self.try_consume_quantifier() {
                        return error("Nothing to repeat");
                    }
                    self.input = saved;

                    let mut cc = self.consume(c);
                    if self.flags.icase {
                        cc = folds::fold(cc)
                    }
                    result.push(ir::Node::Char {
                        c: cc,
                        icase: self.flags.icase,
                    })
                }
            }

            // We just parsed a term; try parsing a quantifier.
            if let Some(quant) = self.try_consume_quantifier()? {
                if !quantifier_allowed {
                    return error("Quantifier not allowed here");
                }
                // Validate the quantifier.
                // Note we don't want to do this as part of parsing the quantiifer in some cases
                // an incomplete quantifier is not recognized as a quantifier, e.g. `/{3/` is
                // valid.
                if quant.min > quant.max {
                    return error("Invalid quantifier");
                }
                let quantifee = result.split_off(start_offset);
                if self.loop_count as usize >= MAX_LOOPS {
                    return error("Loop count limit exceeded");
                }
                self.loop_count += 1;
                result.push(ir::Node::Loop {
                    loopee: Box::new(make_cat(quantifee)),
                    quant,
                    enclosed_groups: start_group..self.group_count,
                });
            }
        }
        Ok(make_cat(result))
    }

    /// ES6 21.2.2.13 CharacterClass.
    fn consume_bracket(&mut self) -> Result<ir::Node, Error> {
        self.consume('[');
        let invert = self.try_consume('^');
        let mut result = BracketContents {
            invert,
            cps: CodePointSet::default(),
        };

        loop {
            match self.peek() {
                None => {
                    return error("Unbalanced bracket");
                }
                Some(']') => {
                    self.consume(']');
                    if self.flags.icase {
                        result.cps = folds::fold_code_points(result.cps);
                    }
                    return Ok(ir::Node::Bracket(result));
                }
                _ => {}
            }

            // Parse a code point or character class.
            let first = self.try_consume_bracket_class_atom()?;
            if first.is_none() {
                continue;
            }

            // Check for a dash; we may have a range.
            if !self.try_consume('-') {
                add_class_atom(&mut result, first.unwrap());
                continue;
            }

            let second = self.try_consume_bracket_class_atom()?;
            if second.is_none() {
                // No second atom. For example: [a-].
                add_class_atom(&mut result, first.unwrap());
                add_class_atom(&mut result, ClassAtom::CodePoint('-'));
                continue;
            }

            // Ranges can't contain character classes: [\d-z] is invalid.
            // Ranges must also be in order: z-a is invalid.
            // ES6 21.2.2.15.1 "If i > j, throw a SyntaxError exception"
            match (first.unwrap(), second.unwrap()) {
                (ClassAtom::CodePoint(c1), ClassAtom::CodePoint(c2)) if c1 <= c2 => {
                    result.cps.add(Interval {
                        first: c1 as u32,
                        last: c2 as u32,
                    })
                }
                _ => {
                    return error("Invalid character range");
                }
            }
        }
    }

    fn try_consume_bracket_class_atom(&mut self) -> Result<Option<ClassAtom>, Error> {
        let c = self.peek();
        if c.is_none() {
            return Ok(None);
        }
        match c.unwrap() {
            // End of bracket.
            ']' => Ok(None),

            // Escape sequence.
            '\\' => {
                self.consume('\\');
                let next = self.peek();
                if next.is_none() {
                    return error("Unterminated escape");
                }
                let ec = next.unwrap();
                match ec {
                    // ES6 21.2.2.12 CharacterClassEscape.
                    'd' | 'D' | 's' | 'S' | 'w' | 'W' => {
                        self.consume(ec);
                        let class_type = match ec {
                            'd' | 'D' => CharacterClassType::Digits,
                            's' | 'S' => CharacterClassType::Spaces,
                            'w' | 'W' => CharacterClassType::Words,
                            _ => panic!("Unreachable"),
                        };
                        Ok(Some(ClassAtom::CharacterClass {
                            class_type,
                            positive: (ec == 'd' || ec == 's' || ec == 'w'),
                        }))
                    }
                    'b' => {
                        // "Return the CharSet containing the single character <BS> U+0008
                        // (BACKSPACE)"
                        self.consume('b');
                        Ok(Some(ClassAtom::CodePoint('\x08')))
                    }

                    '-' => {
                        // ES6 21.2.1 ClassEscape: \- escapes - in Unicode
                        // expressions.
                        self.consume('-');
                        Ok(Some(ClassAtom::CodePoint('-')))
                    }

                    _ => {
                        let cc = self.consume_character_escape()?;
                        Ok(Some(ClassAtom::CodePoint(cc)))
                    }
                }
            }

            c => Ok(Some(ClassAtom::CodePoint(self.consume(c)))),
        }
    }

    fn try_consume_quantifier(&mut self) -> Result<Option<ir::Quantifier>, Error> {
        if let Some(mut quant) = self.try_consume_quantifier_prefix()? {
            quant.greedy = !self.try_consume('?');
            Ok(Some(quant))
        } else {
            Ok(None)
        }
    }

    fn try_consume_quantifier_prefix(&mut self) -> Result<Option<ir::Quantifier>, Error> {
        let nc = self.peek();
        if nc.is_none() {
            return Ok(None);
        }
        let c = nc.unwrap();
        match c {
            '+' => {
                self.consume('+');
                Ok(Some(ir::Quantifier {
                    min: 1,
                    max: std::usize::MAX,
                    greedy: true,
                }))
            }
            '*' => {
                self.consume('*');
                Ok(Some(ir::Quantifier {
                    min: 0,
                    max: std::usize::MAX,
                    greedy: true,
                }))
            }
            '?' => {
                self.consume('?');
                Ok(Some(ir::Quantifier {
                    min: 0,
                    max: 1,
                    greedy: true,
                }))
            }
            '{' => {
                self.consume('{');
                let optmin = self.try_consume_decimal_integer_literal();
                if optmin.is_none() {
                    return error("Invalid quantifier");
                }
                let mut quant = ir::Quantifier {
                    min: optmin.unwrap(),
                    max: optmin.unwrap(),
                    greedy: true,
                };
                if self.try_consume(',') {
                    if let Some(max) = self.try_consume_decimal_integer_literal() {
                        // Like {3,4}
                        quant.max = max;
                    } else {
                        // Like {3,}
                        quant.max = usize::max_value();
                    }
                } else {
                    // Like {3}.
                }
                if !self.try_consume('}') {
                    return error("Invalid quantifier");
                }
                Ok(Some(quant))
            }
            _ => Ok(None),
        }
    }

    /// ES6 11.8.3 DecimalIntegerLiteral.
    /// If the value would overflow, usize::MAX is returned.
    /// All decimal digits are consumed regardless.
    fn try_consume_decimal_integer_literal(&mut self) -> Option<usize> {
        let mut result: usize = 0;
        let mut char_count = 0;
        while let Some(c) = self.peek() {
            if let Some(digit) = char::to_digit(c, 10) {
                self.consume(c);
                char_count += 1;
                result = result.saturating_mul(10);
                result = result.saturating_add(digit as usize);
            } else {
                break;
            }
        }
        if char_count > 0 {
            Some(result)
        } else {
            None
        }
    }

    fn consume_lookaround_assertion(
        &mut self,
        params: LookaroundParams,
    ) -> Result<ir::Node, Error> {
        let start_group = self.group_count;
        let contents = self.consume_disjunction()?;
        let end_group = self.group_count;
        Ok(ir::Node::LookaroundAssertion {
            negate: params.negate,
            backwards: params.backwards,
            start_group,
            end_group,
            contents: Box::new(contents),
        })
    }

    fn consume_character_escape(&mut self) -> Result<char, Error> {
        let u2c = |c| Ok(std::char::from_u32(c).expect("Invalid char"));
        let c = self.peek().expect("Should have a character");
        match c {
            'f' => {
                self.consume('f');
                u2c(0xC)
            }
            'n' => {
                self.consume('n');
                u2c(0xA)
            }
            'r' => {
                self.consume('r');
                u2c(0xD)
            }
            't' => {
                self.consume('t');
                u2c(0x9)
            }
            'v' => {
                self.consume('v');
                u2c(0xB)
            }
            'c' => {
                // Control escape.
                self.consume('c');
                if let Some(nc) = self.next() {
                    if ('a' <= nc && nc <= 'z') || ('A' <= nc && nc <= 'Z') {
                        return u2c((nc as u32) % 32);
                    }
                }
                error("Invalid character escape")
            }
            '0' => {
                // CharacterEscape :: "0 [lookahead != DecimalDigit]"
                self.consume('0');
                match self.peek() {
                    Some(c) if ('0'..='9').contains(&c) => error("Invalid character escape"),
                    _ => u2c(0x0),
                }
            }

            '1'..='7' => {
                // LegacyOctalEscapeSequence
                if self.flags.unicode {
                    error("Invalid character escape")
                } else if let Some(c) = self.try_consume_legacy_octal_escape_sequence() {
                    Ok(c)
                } else {
                    error("invalid octal escape")
                }
            }

            'x' => {
                // HexEscapeSequence :: x HexDigit HexDigit
                // See ES6 11.8.3 HexDigit
                let hex_to_digit = |c: char| c.to_digit(16);
                self.consume('x');
                let x1 = self.next().and_then(hex_to_digit);
                let x2 = self.next().and_then(hex_to_digit);
                match (x1, x2) {
                    (Some(x1), Some(x2)) => u2c(x1 * 16 + x2),
                    _ => error("Invalid character escape"),
                }
            }

            'u' => {
                // Unicode escape
                self.consume('u');
                if let Some(c) = self.try_escape_unicode_sequence() {
                    Ok(c)
                } else if self.flags.unicode {
                    error("Invalid unicode escape")
                } else {
                    Ok('u')
                }
            }

            // Only syntax characters and / participate in IdentityEscape in Unicode regexp.
            '^' | '$' | '\\' | '.' | '*' | '+' | '?' | '(' | ')' | '[' | ']' | '{' | '}' | '|'
            | '/' => Ok(self.consume(c)),

            c => {
                if self.flags.unicode {
                    error("Invalid character escape")
                } else {
                    Ok(self.consume(c))
                }
            }
        }
    }

    fn consume_atom_escape(&mut self) -> Result<ir::Node, Error> {
        let nc = self.peek();
        if nc.is_none() {
            return error("Incomplete escape");
        }
        let c = nc.unwrap();
        match c {
            'b' | 'B' => {
                self.consume(c);
                Ok(ir::Node::WordBoundary { invert: c == 'B' })
            }

            'd' | 'D' => {
                self.consume(c);
                Ok(make_bracket_class(CharacterClassType::Digits, c == 'd'))
            }

            's' | 'S' => {
                self.consume(c);
                Ok(make_bracket_class(CharacterClassType::Spaces, c == 's'))
            }

            'w' | 'W' => {
                self.consume(c);
                Ok(make_bracket_class(CharacterClassType::Words, c == 'w'))
            }

            '1'..='9' => {
                let orig_input = self.input.clone();
                let val = self.try_consume_decimal_integer_literal().unwrap();

                // This is a backreference.
                // Note we limit backreferences to u32 but the value may exceed that.
                if val <= self.group_count_max as usize {
                    if val > MAX_CAPTURE_GROUPS {
                        return error(format!("Backreference \\{} too large", val));
                    }
                    let group = val as u32;
                    self.max_backref = std::cmp::max(self.max_backref, group);
                    Ok(ir::Node::BackRef(group))
                } else if self.flags.unicode {
                    error("Invalid character escape")
                } else if c == '8' || c == '9' {
                    self.input = orig_input;
                    Ok(ir::Node::Char {
                        c: self.consume(c),
                        icase: self.flags.icase,
                    })
                } else {
                    self.input = orig_input;
                    if let Some(c) = self.try_consume_legacy_octal_escape_sequence() {
                        Ok(ir::Node::Char {
                            c,
                            icase: self.flags.icase,
                        })
                    } else {
                        error("invalid octal escape")
                    }
                }
            }

            'k' => {
                self.consume(c);

                // The sequence `\k` must be the start of a backreference to a named capture group, when:
                // 1. the `unicode` flag is set or
                // 2. there exists no named capture group in the regexp
                // Otherwise `\k` is parsed as the `k` literal character.
                if !self.flags.unicode && self.named_group_indices.is_empty() {
                    Ok(ir::Node::Char {
                        c: 'k',
                        icase: self.flags.icase,
                    })
                } else if let Some(group_name) = self.try_consume_named_capture_group_name() {
                    if let Some(index) = self.named_group_indices.get(&group_name) {
                        Ok(ir::Node::BackRef(*index + 1))
                    } else {
                        error(format!(
                            "Backreference to invalid named capture group: {}",
                            &group_name
                        ))
                    }
                } else {
                    error("Unexpected end of named backreference")
                }
            }
            _ => Ok(ir::Node::Char {
                c: self.consume_character_escape()?,
                icase: self.flags.icase,
            }),
        }
    }

    fn try_escape_unicode_sequence(&mut self) -> Option<char> {
        let orig_input = self.input.clone();

        // Support \u{X..X} (Unicode CodePoint)
        if self.try_consume('{') {
            let mut s = String::new();
            loop {
                match self.next() {
                    Some('}') => break,
                    Some(c) => s.push(c),
                    None => {
                        self.input = orig_input;
                        return None;
                    }
                }
            }

            match u32::from_str_radix(&s, 16) {
                Ok(u) => {
                    if u > 0x10_FFFF {
                        self.input = orig_input;
                        None
                    } else {
                        char::from_u32(u as u32)
                    }
                }
                _ => {
                    self.input = orig_input;
                    None
                }
            }
        } else {
            // Hex4Digits
            let mut s = String::new();
            for _ in 0..4 {
                if let Some(c) = self.next() {
                    s.push(c);
                } else {
                    self.input = orig_input;
                    return None;
                }
            }
            match u16::from_str_radix(&s, 16) {
                Ok(u) => {
                    if (0xDC00..=0xDFFF).contains(&u) || (0xD800..=0xDB7F).contains(&u) {
                        // Low/High Surrogates
                        if !self.try_consume_str("\\u") {
                            return String::from_utf16_lossy(&[u]).chars().next();
                        }

                        let mut s = String::new();
                        for _ in 0..4 {
                            if let Some(c) = self.next() {
                                s.push(c);
                            } else {
                                self.input = orig_input;
                                return None;
                            }
                        }
                        match u16::from_str_radix(&s, 16) {
                            Ok(uu) => match String::from_utf16(&[u, uu]) {
                                Ok(s) => s.chars().next(),
                                _ => {
                                    self.input = orig_input;
                                    None
                                }
                            },
                            _ => {
                                self.input = orig_input;
                                None
                            }
                        }
                    } else {
                        String::from_utf16_lossy(&[u]).chars().next()
                    }
                }
                _ => {
                    self.input = orig_input;
                    None
                }
            }
        }
    }

    fn try_consume_legacy_octal_escape_sequence(&mut self) -> Option<char> {
        let first = self.next()?;
        let mut s = String::from(first);

        if let Some(c) = self.peek() {
            if ('0'..='7').contains(&c) {
                self.consume(c);
                s.push(c);

                if ('0'..='3').contains(&first) {
                    if let Some(c) = self.peek() {
                        if ('0'..='7').contains(&c) {
                            self.consume(c);
                            s.push(c);
                        }
                    }
                }
            }
        }

        match u32::from_str_radix(&s, 8) {
            Ok(i) => char::from_u32(i),
            Err(_) => None,
        }
    }

    fn try_consume_named_capture_group_name(&mut self) -> Option<String> {
        if !self.try_consume('<') {
            return None;
        }

        let orig_input = self.input.clone();
        let mut group_name = String::new();

        if let Some(mut c) = self.next() {
            if self.try_consume('u') {
                if let Some(escaped) = self.try_escape_unicode_sequence() {
                    c = escaped;
                } else {
                    self.input = orig_input;
                    return None;
                }
            }

            if is_id_start(c) || c == '$' || c == '_' {
                group_name.push(c);
            } else {
                self.input = orig_input;
                return None;
            }
        } else {
            self.input = orig_input;
            return None;
        }

        loop {
            if let Some(mut c) = self.next() {
                if self.try_consume('u') {
                    if let Some(escaped) = self.try_escape_unicode_sequence() {
                        c = escaped;
                    } else {
                        self.input = orig_input;
                        return None;
                    }
                }

                if c == '>' {
                    break;
                }

                if is_id_continue(c) || c == '$' || c == '_' || c == '\u{200C}' /* <ZWNJ> */ || c == '\u{200D}'
                /* <ZWJ> */
                {
                    group_name.push(c);
                } else {
                    self.input = orig_input;
                    return None;
                }
            } else {
                self.input = orig_input;
                return None;
            }
        }

        Some(group_name)
    }

    // Quickly parse all capture groups.
    fn parse_capture_groups(&mut self) {
        let orig_input = self.input.clone();

        loop {
            let c = self.input.next();
            match c {
                Some('[') => loop {
                    let c = self.input.next();
                    match c {
                        Some(']') => break,
                        Some(_) => continue,
                        None => break,
                    }
                },
                Some('(') => {
                    if self.try_consume_str("?") {
                        if let Some(name) = self.try_consume_named_capture_group_name() {
                            self.named_group_indices.insert(name, self.group_count_max);
                        }
                    }
                    self.group_count_max = if self.group_count_max + 1 > MAX_CAPTURE_GROUPS as u32 {
                        MAX_CAPTURE_GROUPS as u32
                    } else {
                        self.group_count_max + 1
                    };
                }
                Some(_) => continue,
                None => break,
            }
        }

        self.input = orig_input;
    }

    fn finalize(&self, mut re: ir::Regex) -> Result<ir::Regex, Error> {
        debug_assert!(self.loop_count <= MAX_LOOPS as u32);
        debug_assert!(self.group_count as usize <= MAX_CAPTURE_GROUPS);
        if self.has_lookbehind {
            ir::walk_mut(false, &mut re.node, &mut ir::Node::reverse_cats);
        }
        Ok(re)
    }
}

/// Try parsing a given pattern.
/// Return the resulting IR regex, or an error.
pub fn try_parse(pattern: &str, flags: api::Flags) -> Result<ir::Regex, Error> {
    // for q in 0..=0x10FFFF {
    //     if let Some(c) = std::char::from_u32(q) {
    //         let cc = folds::fold(c);
    //         if (c as u32) > 127 && (cc as u32) < 127 {
    //             println!("Bad CP: {}", q);
    //         }
    //     }
    // }

    let mut p = Parser {
        input: pattern.chars().peekable(),
        flags,
        loop_count: 0,
        group_count: 0,
        named_group_indices: HashMap::new(),
        group_count_max: 0,
        max_backref: 0,
        has_lookbehind: false,
    };
    p.try_parse()
}
