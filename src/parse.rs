//! Parser from regex patterns to IR

use crate::{
    api, charclasses,
    codepointset::{CodePointSet, Interval},
    ir,
    types::{
        BracketContents, CaptureGroupID, CaptureGroupName, CharacterClassType, MAX_CAPTURE_GROUPS,
        MAX_LOOPS,
    },
    unfold_char,
    unicode::{PropertyEscape, UnicodePropertyName, UnicodePropertyValue},
    util::to_char_sat,
    CASE_MATCHER,
};
use core::{fmt, iter::Peekable};
use icu_collections::codepointinvlist::CodePointInversionListBuilder;
#[cfg(feature = "std")]
use std::collections::HashMap;
#[cfg(not(feature = "std"))]
use {
    alloc::{
        boxed::Box,
        string::{String, ToString},
        vec::Vec,
    },
    hashbrown::HashMap,
};

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

#[cfg(feature = "std")]
impl std::error::Error for Error {}

enum ClassAtom {
    CodePoint(u32),
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
        ClassAtom::CodePoint(c) => bc.cps.add_one(c),
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
struct Parser<I>
where
    I: Iterator<Item = u32>,
{
    /// The remaining input.
    input: Peekable<I>,

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

impl<I> Parser<I>
where
    I: Iterator<Item = u32> + Clone,
{
    /// Consume a character, returning it.
    fn consume<C: Into<u32>>(&mut self, c: C) -> u32 {
        let nc = self.input.next();
        core::debug_assert!(nc == Some(c.into()), "char was not next");
        nc.unwrap()
    }

    /// If our contents begin with the char c, consume it from our contents
    /// and return true. Otherwise return false.
    fn try_consume<C: Into<u32>>(&mut self, c: C) -> bool {
        self.input.next_if_eq(&c.into()).is_some()
    }

    /// If our contents begin with the string \p s, consume it from our contents
    /// and return true. Otherwise return false.
    fn try_consume_str(&mut self, s: &str) -> bool {
        let mut cursor = self.input.clone();
        for c1 in s.chars() {
            if cursor.next() != Some(c1 as u32) {
                return false;
            }
        }
        self.input = cursor;
        true
    }

    /// Peek at the next character.
    fn peek(&mut self) -> Option<u32> {
        self.input.peek().copied()
    }

    /// \return the next character.
    fn next(&mut self) -> Option<u32> {
        self.input.next()
    }

    fn try_parse(&mut self) -> Result<ir::Regex, Error> {
        self.parse_capture_groups()?;

        // Parse a catenation. If we consume everything, it's success. If there's
        // something left, it's an error (for example, an excess closing paren).
        let body = self.consume_disjunction()?;
        match self.input.peek().copied() {
            Some(c) if c == ')' as u32 => error("Unbalanced parenthesis"),
            Some(c) => error(format!(
                "Unexpected char: {}",
                char::from_u32(c)
                    .map(String::from)
                    .unwrap_or_else(|| format!("\\u{c:04X}"))
            )),
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
            match to_char_sat(c) {
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
                        quantifier_allowed = false;
                        self.has_lookbehind = true;
                        result.push(self.consume_lookaround_assertion(LookaroundParams {
                            negate: false,
                            backwards: true,
                        })?);
                    } else if self.try_consume_str("(?<!") {
                        // Negative lookbehind.
                        quantifier_allowed = false;
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

                ']' if self.flags.unicode => {
                    return error("Unbalanced bracket");
                }

                _ => {
                    // It's an error if this parses successfully as a quantifier.
                    // It's also an error if this is an invalid quantifier (with 'u' flag).
                    // Note this covers *, +, ? as well.
                    let saved = self.input.clone();
                    if self.try_consume_quantifier()?.is_some() {
                        return error("Nothing to repeat");
                    }
                    self.input = saved;
                    let cc = c;
                    self.consume(cc);

                    if self.flags.icase {
                        result.push(unfold_char(char::from_u32(cc).unwrap()))
                    } else {
                        result.push(ir::Node::Char(cc))
                    }
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
            match self.peek().map(to_char_sat) {
                None => {
                    return error("Unbalanced bracket");
                }
                Some(']') => {
                    self.consume(']');
                    if self.flags.icase {
                        let intervals = result.cps.intervals().to_vec();
                        for interval in intervals {
                            for code_point in interval.first..=interval.last {
                                let c = char::from_u32(code_point).unwrap();
                                let simple = CASE_MATCHER.simple_fold(c);
                                let mut builder = CodePointInversionListBuilder::new();
                                CASE_MATCHER.add_case_closure_to(c, &mut builder);
                                for unfolded in builder.build().iter_chars() {
                                    if CASE_MATCHER.simple_fold(unfolded) == simple {
                                        result.cps.add_one(unfolded.into());
                                    }
                                }
                            }
                        }
                    }
                    return Ok(ir::Node::Bracket(result));
                }
                _ => {}
            }

            // Parse a code point or character class.
            let Some(first) = self.try_consume_bracket_class_atom()? else {
                continue;
            };

            // Check for a dash; we may have a range.
            if !self.try_consume('-') {
                add_class_atom(&mut result, first);
                continue;
            }

            let Some(second) = self.try_consume_bracket_class_atom()? else {
                // No second atom. For example: [a-].
                add_class_atom(&mut result, first);
                add_class_atom(&mut result, ClassAtom::CodePoint(u32::from('-')));
                continue;
            };

            // Ranges must also be in order: z-a is invalid.
            // ES6 21.2.2.15.1 "If i > j, throw a SyntaxError exception"
            if let (ClassAtom::CodePoint(c1), ClassAtom::CodePoint(c2)) = (&first, &second) {
                if c1 > c2 {
                    return error(
                        "Range values reversed, start char code is greater than end char code.",
                    );
                }
                result.cps.add(Interval {
                    first: *c1,
                    last: *c2,
                });
                continue;
            }

            if self.flags.unicode {
                return error("Invalid character range");
            }

            // If it does not match a range treat as any match single characters.
            add_class_atom(&mut result, first);
            add_class_atom(&mut result, ClassAtom::CodePoint(u32::from('-')));
            add_class_atom(&mut result, second);
        }
    }

    fn try_consume_bracket_class_atom(&mut self) -> Result<Option<ClassAtom>, Error> {
        let c = self.peek();
        if c.is_none() {
            return Ok(None);
        }
        let c = c.unwrap();
        match to_char_sat(c) {
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
                match to_char_sat(ec) {
                    // ES6 21.2.2.12 CharacterClassEscape.
                    'd' | 'D' | 's' | 'S' | 'w' | 'W' => {
                        self.consume(ec);
                        let class_type = match to_char_sat(ec) {
                            'd' | 'D' => CharacterClassType::Digits,
                            's' | 'S' => CharacterClassType::Spaces,
                            'w' | 'W' => CharacterClassType::Words,
                            _ => panic!("Unreachable"),
                        };
                        Ok(Some(ClassAtom::CharacterClass {
                            class_type,
                            positive: (ec == 'd' as u32 || ec == 's' as u32 || ec == 'w' as u32),
                        }))
                    }
                    'b' => {
                        // "Return the CharSet containing the single character <BS> U+0008
                        // (BACKSPACE)"
                        self.consume('b');
                        Ok(Some(ClassAtom::CodePoint(u32::from('\x08'))))
                    }

                    '-' => {
                        // ES6 21.2.1 ClassEscape: \- escapes - in Unicode
                        // expressions.
                        self.consume('-');
                        Ok(Some(ClassAtom::CodePoint(u32::from('-'))))
                    }

                    // TODO: implement property escape in brackets
                    //                    'p' | 'P' => {
                    //                        self.consume(ec);
                    //
                    //                        let property_escape = self.try_consume_unicode_property_escape()?;
                    //                        let negate = ec == 'P';
                    //
                    //                        Ok(Some(ClassAtom::UnicodePropertyEscape {
                    //                            property_escape,
                    //                            negate,
                    //                        }))
                    //                    }
                    _ => {
                        let cc = self.consume_character_escape()?;
                        Ok(Some(ClassAtom::CodePoint(cc)))
                    }
                }
            }

            _ => Ok(Some(ClassAtom::CodePoint(self.consume(c)))),
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
        match char::from_u32(c) {
            Some('+') => {
                self.consume('+');
                Ok(Some(ir::Quantifier {
                    min: 1,
                    max: core::usize::MAX,
                    greedy: true,
                }))
            }
            Some('*') => {
                self.consume('*');
                Ok(Some(ir::Quantifier {
                    min: 0,
                    max: core::usize::MAX,
                    greedy: true,
                }))
            }
            Some('?') => {
                self.consume('?');
                Ok(Some(ir::Quantifier {
                    min: 0,
                    max: 1,
                    greedy: true,
                }))
            }
            Some('{') => {
                if let Some(quantifier) = self.try_consume_braced_quantifier() {
                    Ok(Some(quantifier))
                } else if self.flags.unicode {
                    // if there was a brace '{' that doesn't parse into a valid quantifier,
                    // it's not valid with the unicode flag
                    error("Invalid quantifier")
                } else {
                    Ok(None)
                }
            }
            _ => Ok(None),
        }
    }

    fn try_consume_braced_quantifier(&mut self) -> Option<ir::Quantifier> {
        // if parsed input is actually invalid, keep the previous one for rollback
        let pre_input = self.input.clone();
        self.consume('{');
        let optmin = self.try_consume_decimal_integer_literal();
        if optmin.is_none() {
            // not a valid quantifier, rollback consumption
            self.input = pre_input;
            return None;
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
            // not a valid quantifier, rollback consumption
            self.input = pre_input;
            return None;
        }
        Some(quant)
    }

    /// ES6 11.8.3 DecimalIntegerLiteral.
    /// If the value would overflow, usize::MAX is returned.
    /// All decimal digits are consumed regardless.
    fn try_consume_decimal_integer_literal(&mut self) -> Option<usize> {
        let mut result: usize = 0;
        let mut char_count = 0;
        while let Some(c) = self.peek() {
            if let Some(digit) = char::from_u32(c).and_then(|c| char::to_digit(c, 10)) {
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

    fn consume_character_escape(&mut self) -> Result<u32, Error> {
        let c = self.peek().expect("Should have a character");
        match to_char_sat(c) {
            'f' => {
                self.consume('f');
                Ok(0xC)
            }
            'n' => {
                self.consume('n');
                Ok(0xA)
            }
            'r' => {
                self.consume('r');
                Ok(0xD)
            }
            't' => {
                self.consume('t');
                Ok(0x9)
            }
            'v' => {
                self.consume('v');
                Ok(0xB)
            }
            'c' => {
                // Control escape.
                self.consume('c');
                if let Some(nc) = self.next().and_then(char::from_u32) {
                    if nc.is_ascii_lowercase() || nc.is_ascii_uppercase() {
                        return Ok((nc as u32) % 32);
                    }
                }
                error("Invalid character escape")
            }
            '0' => {
                // CharacterEscape :: "0 [lookahead != DecimalDigit]"
                self.consume('0');
                match self.peek().and_then(char::from_u32) {
                    Some(c) if c.is_ascii_digit() => error("Invalid character escape"),
                    _ => Ok(0x0),
                }
            }

            'x' => {
                // HexEscapeSequence :: x HexDigit HexDigit
                // See ES6 11.8.3 HexDigit
                let hex_to_digit = |c: char| c.to_digit(16);
                self.consume('x');
                let x1 = self.next().and_then(char::from_u32).and_then(hex_to_digit);
                let x2 = self.next().and_then(char::from_u32).and_then(hex_to_digit);
                match (x1, x2) {
                    (Some(x1), Some(x2)) => Ok(x1 * 16 + x2),
                    _ => error("Invalid character escape"),
                }
            }

            'u' => {
                // Unicode escape
                self.consume('u');
                if let Some(c) = self.try_escape_unicode_sequence() {
                    Ok(c)
                } else {
                    error("Invalid unicode escape")
                }
            }

            // Only syntax characters and / participate in IdentityEscape in Unicode regexp.
            '^' | '$' | '\\' | '.' | '*' | '+' | '?' | '(' | ')' | '[' | ']' | '{' | '}' | '|'
            | '/' => Ok(self.consume(c)),

            // TODO: currently we reject numeric characters in IdentityEscape to help some PCRE
            // tests pass.
            // Specifically a regex of the form [\p{Nd}]: in non-Unicode mode this is not a
            // character property test and is expected to parse as just a bracket where \p is
            // IdentityEscaped to p.
            c if (!self.flags.unicode && !c.is_ascii_digit()) || c.is_ascii_alphabetic() => {
                Ok(self.consume(c))
            }

            _ => error("Invalid character escape"),
        }
    }

    fn consume_atom_escape(&mut self) -> Result<ir::Node, Error> {
        let nc = self.peek();
        if nc.is_none() {
            return error("Incomplete escape");
        }
        let c = nc.unwrap();
        match to_char_sat(c) {
            'b' | 'B' => {
                self.consume(c);
                Ok(ir::Node::WordBoundary {
                    invert: c == 'B' as u32,
                })
            }

            'd' | 'D' => {
                self.consume(c);
                Ok(make_bracket_class(
                    CharacterClassType::Digits,
                    c == 'd' as u32,
                ))
            }

            's' | 'S' => {
                self.consume(c);
                Ok(make_bracket_class(
                    CharacterClassType::Spaces,
                    c == 's' as u32,
                ))
            }

            'w' | 'W' => {
                self.consume(c);
                Ok(make_bracket_class(
                    CharacterClassType::Words,
                    c == 'w' as u32,
                ))
            }

            'p' | 'P' => {
                self.consume(c);

                let property_escape = self.try_consume_unicode_property_escape()?;
                let negate = c == 'P' as u32;

                Ok(ir::Node::UnicodePropertyEscape {
                    property_escape,
                    negate,
                })
            }

            '1'..='9' => {
                let val = self.try_consume_decimal_integer_literal().unwrap();

                // This is a backreference.
                // Note we limit backreferences to u32 but the value may exceed that.
                if val <= self.group_count_max as usize {
                    if val > MAX_CAPTURE_GROUPS {
                        return error(format!("Backreference \\{} too large", val));
                    }
                    let group = val as u32;
                    self.max_backref = core::cmp::max(self.max_backref, group);
                    Ok(ir::Node::BackRef(group))
                } else {
                    error("Invalid character escape")
                }
            }

            'k' => {
                self.consume('k');

                // The sequence `\k` must be the start of a backreference to a named capture group.
                if let Some(group_name) = self.try_consume_named_capture_group_name() {
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
            _ => {
                let c = self.consume_character_escape()?;

                if self.flags.icase {
                    Ok(unfold_char(char::from_u32(c).unwrap()))
                } else {
                    Ok(ir::Node::Char(c))
                }
            }
        }
    }

    #[allow(clippy::branches_sharing_code)]
    fn try_escape_unicode_sequence(&mut self) -> Option<u32> {
        let mut orig_input = self.input.clone();

        // Support \u{X..X} (Unicode CodePoint)
        if self.try_consume('{') {
            let mut s = String::new();
            loop {
                match self.next().and_then(char::from_u32) {
                    Some('}') => break,
                    Some(c) => s.push(c),
                    None => {
                        // Surrogates not supported in code point escapes.
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
                        Some(u)
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
                if let Some(c) = self.next().and_then(char::from_u32) {
                    s.push(c);
                } else {
                    // Surrogates are not hex digits.
                    self.input = orig_input;
                    return None;
                }
            }
            match u16::from_str_radix(&s, 16) {
                Ok(u) => {
                    if (0xD800..=0xDBFF).contains(&u) {
                        // Found a high surrogate. Try to parse a low surrogate next
                        // to see if we can rebuild the original `char`

                        if !self.try_consume_str("\\u") {
                            return Some(u as u32);
                        }
                        orig_input = self.input.clone();

                        // A poor man's try block to handle the backtracking
                        // in a single place instead of every time we want to return.
                        // This allows us to use `?` within the inner block without returning
                        // from the entire parent function.
                        let result = (|| {
                            let mut s = String::new();
                            for _ in 0..4 {
                                let c = self.next().and_then(char::from_u32)?;
                                s.push(c);
                            }

                            let uu = u16::from_str_radix(&s, 16).ok()?;
                            let ch = char::decode_utf16([u, uu]).next()?.ok()?;
                            Some(u32::from(ch))
                        })();

                        result.or_else(|| {
                            self.input = orig_input;
                            Some(u as u32)
                        })
                    } else {
                        // If `u` is not a surrogate or is a low surrogate we can directly return it,
                        // since all paired low surrogates should have been handled above.
                        Some(u as u32)
                    }
                }
                _ => {
                    self.input = orig_input;
                    None
                }
            }
        }
    }

    fn try_consume_named_capture_group_name(&mut self) -> Option<String> {
        if !self.try_consume('<') {
            return None;
        }

        let orig_input = self.input.clone();
        let mut group_name = String::new();

        if let Some(mut c) = self.next().and_then(char::from_u32) {
            if c == '\\' && self.try_consume('u') {
                if let Some(escaped) = self.try_escape_unicode_sequence().and_then(char::from_u32) {
                    c = escaped;
                } else {
                    self.input = orig_input;
                    return None;
                }
            }

            if icu_properties::sets::id_start().contains(c) || c == '$' || c == '_' {
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
            if let Some(mut c) = self.next().and_then(char::from_u32) {
                if c == '\\' && self.try_consume('u') {
                    if let Some(escaped) =
                        self.try_escape_unicode_sequence().and_then(char::from_u32)
                    {
                        c = escaped;
                    } else {
                        self.input = orig_input;
                        return None;
                    }
                }

                if c == '>' {
                    break;
                }

                if icu_properties::sets::id_continue().contains(c) || c == '$' || c == '_' || c == '\u{200C}' /* <ZWNJ> */ || c == '\u{200D}'
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
    fn parse_capture_groups(&mut self) -> Result<(), Error> {
        let orig_input = self.input.clone();

        loop {
            match self.next().map(to_char_sat) {
                Some('\\') => {
                    self.next();
                    continue;
                }
                Some('[') => loop {
                    match self.next().map(to_char_sat) {
                        Some('\\') => {
                            self.next();
                            continue;
                        }
                        Some(']') => break,
                        Some(_) => continue,
                        None => break,
                    }
                },
                Some('(') => {
                    if self.try_consume_str("?") {
                        if let Some(name) = self.try_consume_named_capture_group_name() {
                            if self
                                .named_group_indices
                                .insert(name, self.group_count_max)
                                .is_some()
                            {
                                return error("Duplicate capture group name");
                            }
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

        Ok(())
    }

    fn try_consume_unicode_property_escape(&mut self) -> Result<PropertyEscape, Error> {
        if !self.try_consume('{') {
            return error("Invalid character at property escape start");
        }

        let mut buffer = String::new();
        let mut name = None;

        while let Some(c) = self.peek().and_then(char::from_u32) {
            match c {
                '}' => {
                    self.consume(c);
                    if let Some(value) = UnicodePropertyValue::from_str(&buffer, name) {
                        return Ok(PropertyEscape::new(name, value));
                    } else {
                        return error("Invalid property name");
                    }
                }
                '=' if name.is_none() => {
                    self.consume(c);
                    let Some(n) = UnicodePropertyName::from_str(&buffer) else {
                        return error("Invalid property name");
                    };
                    name = Some(n);
                    buffer.clear();
                }
                c if c.is_ascii_alphanumeric() || c == '_' => {
                    self.consume(c);
                    buffer.push(c);
                }
                _ => {
                    return error("Invalid property name");
                }
            }
        }

        error("Invalid property name")
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
pub fn try_parse<I>(pattern: I, flags: api::Flags) -> Result<ir::Regex, Error>
where
    I: Iterator<Item = u32> + Clone,
{
    // for q in 0..=0x10FFFF {
    //     if let Some(c) = core::char::from_u32(q) {
    //         let cc = folds::fold(c);
    //         if (c as u32) > 127 && (cc as u32) < 127 {
    //             println!("Bad CP: {}", q);
    //         }
    //     }
    // }

    let mut p = Parser {
        input: pattern.peekable(),
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
