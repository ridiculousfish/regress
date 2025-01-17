use crate::classicalbacktrack;
use crate::emit;
use crate::exec;
use crate::indexing;
use crate::insn::CompiledRegex;
use crate::optimizer;
use crate::parse;

#[cfg(feature = "utf16")]
use crate::{
    classicalbacktrack::MatchAttempter,
    indexing::{InputIndexer, Ucs2Input, Utf16Input},
};

#[cfg(feature = "backend-pikevm")]
use crate::pikevm;
use crate::util::to_char_sat;

use core::{fmt, str::FromStr};
#[cfg(feature = "std")]
#[cfg(not(feature = "std"))]
use {
    alloc::{string::String, vec::Vec},
    hashbrown::{hash_map::Iter, HashMap},
};

pub use parse::Error;

/// Flags used to control regex parsing.
/// The default flags are case-sensitive, not-multiline, and optimizing.
#[derive(Debug, Copy, Clone, Default)]
pub struct Flags {
    /// If set, make the regex case-insensitive.
    /// Equivalent to the 'i' flag in JavaScript.
    pub icase: bool,

    /// If set, ^ and $ match at line separators, not just the input boundaries.
    /// Equivalent to the 'm' flag in JavaScript.
    pub multiline: bool,

    /// If set, . matches at line separators as well as any other character.
    /// Equivalent to the 'm' flag in JavaScript.
    pub dot_all: bool,

    /// If set, disable regex IR passes.
    pub no_opt: bool,

    /// If set, the regex is interpreted as a Unicode regex.
    /// Equivalent to the 'u' flag in JavaScript.
    pub unicode: bool,

    /// If set, the regex is interpreted as a UnicodeSets regex.
    /// Equivalent to the 'v' flag in JavaScript.
    pub unicode_sets: bool,
}

impl Flags {
    /// Construct a Flags from a Unicode codepoints iterator, using JavaScript field names.
    /// 'i' means to ignore case, 'm' means multiline, 'u' means unicode.
    /// Note the 'g' flag implies a stateful regex and is not supported.
    /// Other flags are not implemented and are ignored.
    #[inline]
    pub fn new<T: Iterator<Item = u32>>(chars: T) -> Self {
        let mut result = Self::default();
        for c in chars {
            match to_char_sat(c) {
                'm' => {
                    result.multiline = true;
                }
                'i' => {
                    result.icase = true;
                }
                's' => {
                    result.dot_all = true;
                }
                'u' => {
                    result.unicode = true;
                }
                'v' => {
                    result.unicode_sets = true;
                }
                _ => {
                    // Silently skip unsupported flags.
                }
            }
        }
        result
    }
}

impl From<&str> for Flags {
    /// Construct a Flags from a string, using JavaScript field names.
    ///
    /// See also: [`Flags::new`].
    #[inline]
    fn from(s: &str) -> Self {
        Self::new(s.chars().map(u32::from))
    }
}

impl fmt::Display for Flags {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        if self.multiline {
            f.write_str("m")?;
        }
        if self.icase {
            f.write_str("i")?;
        }
        if self.dot_all {
            f.write_str("s")?;
        }
        if self.unicode {
            f.write_str("u")?;
        }
        Ok(())
    }
}

/// Range is used to express the extent of a match, as indexes into the input
/// string.
pub type Range = core::ops::Range<usize>;

/// An iterator type which yields `Match`es found in a string.
pub type Matches<'r, 't> = exec::Matches<backends::DefaultExecutor<'r, 't>>;

/// An iterator type which yields `Match`es found in a string, supporting ASCII
/// only.
pub type AsciiMatches<'r, 't> = exec::Matches<backends::DefaultAsciiExecutor<'r, 't>>;

/// A Match represents a portion of a string which was found to match a Regex.
#[derive(Debug, Clone)]
pub struct Match {
    /// The total range of the match. Note this may be empty, if the regex
    /// matched an empty string.
    pub range: Range,

    /// The list of captures. This has length equal to the number of capturing
    /// groups in the regex. For each capture, if the value is None, that group
    /// did not match (for example, it was in a not-taken branch of an
    /// alternation). If the value is Some, the group did match with the
    /// enclosed range.
    pub captures: Vec<Option<Range>>,

    // A list of capture group names. This is either:
    //   - Empty, if there were no named capture groups.
    //   - A list of names with length `captures.len()`, corresponding to the
    //     capture group names in order. Groups without names have an empty string.
    pub(crate) group_names: Box<[Box<str>]>,
}

impl Match {
    /// Access a group by index, using the convention of Python's group()
    /// function. Index 0 is the total match, index 1 is the first capture
    /// group.
    #[inline]
    pub fn group(&self, idx: usize) -> Option<Range> {
        if idx == 0 {
            Some(self.range.clone())
        } else {
            self.captures[idx - 1].clone()
        }
    }

    /// Access a named group by name.
    #[inline]
    pub fn named_group(&self, name: &str) -> Option<Range> {
        // Empty strings are used as sentinels to indicate unnamed group.
        if name.is_empty() {
            return None;
        }
        let pos = self.group_names.iter().position(|s| s.as_ref() == name)?;
        self.captures[pos].clone()
    }

    /// Return an iterator over the named groups of a Match.
    #[inline]
    pub fn named_groups(&self) -> NamedGroups {
        NamedGroups::new(self)
    }

    /// Returns the range over the starting and ending byte offsets of the match in the haystack.
    ///
    /// This is a convenience function to work around
    /// the fact that Range does not support Copy.
    #[inline]
    pub fn range(&self) -> Range {
        self.range.clone()
    }

    /// Returns the starting byte offset of the match in the haystack.
    #[inline]
    pub fn start(&self) -> usize {
        self.range.start
    }

    /// Returns the ending byte offset of the match in the haystack.
    #[inline]
    pub fn end(&self) -> usize {
        self.range.end
    }

    /// Return an iterator over a Match. The first returned value is the total
    /// match, and subsequent values represent the capture groups.
    #[inline]
    pub fn groups(&self) -> Groups {
        Groups::new(self)
    }
}

/// An iterator over the capture groups of a [`Match`]
///
/// This struct is created by the [`groups`] method on [`Match`].
///
/// [`Match`]: ../struct.Match.html
/// [`groups`]: ../struct.Match.html#method.groups
#[derive(Clone)]
pub struct Groups<'m> {
    mat: &'m Match,
    i: usize,
    max: usize,
}

impl<'m> Groups<'m> {
    #[inline]
    fn new(mat: &'m Match) -> Self {
        Self {
            mat,
            i: 0,
            max: mat.captures.len() + 1,
        }
    }
}

impl Iterator for Groups<'_> {
    type Item = Option<Range>;

    #[inline]
    fn next(&mut self) -> Option<Self::Item> {
        let i = self.i;
        if i < self.max {
            self.i += 1;
            Some(self.mat.group(i))
        } else {
            None
        }
    }
}

/// An iterator over the named capture groups of a [`Match`]
///
/// This struct is created by the [`named_groups`] method on [`Match`].
///
/// [`Match`]: ../struct.Match.html
/// [`named_groups`]: ../struct.Match.html#method.named_groups
#[derive(Clone)]
pub struct NamedGroups<'m> {
    mat: &'m Match,
    next_group_name_idx: usize,
}

impl<'m> NamedGroups<'m> {
    #[inline]
    fn new(mat: &'m Match) -> Self {
        Self {
            mat,
            next_group_name_idx: 0,
        }
    }
}

impl<'m> Iterator for NamedGroups<'m> {
    type Item = (&'m str, Option<Range>);

    #[inline]
    fn next(&mut self) -> Option<Self::Item> {
        // Increment next_group_name_idx until we find a non-empty name.
        debug_assert!(self.next_group_name_idx <= self.mat.group_names.len());
        let end = self.mat.group_names.len();
        let mut idx = self.next_group_name_idx;
        while idx < end && self.mat.group_names[idx].is_empty() {
            idx += 1;
        }
        if idx == end {
            return None;
        }
        let name = self.mat.group_names[idx].as_ref();
        let range = self.mat.captures[idx].clone();
        self.next_group_name_idx = idx + 1;
        Some((name, range))
    }
}

/// A Regex is the compiled version of a pattern.
#[derive(Debug, Clone)]
pub struct Regex {
    cr: CompiledRegex,
}

impl From<CompiledRegex> for Regex {
    fn from(cr: CompiledRegex) -> Self {
        Self { cr }
    }
}

impl Regex {
    /// Construct a regex by parsing `pattern` using the default flags.
    /// An Error may be returned if the syntax is invalid.
    /// Note that this is rather expensive; prefer to cache a Regex which is
    /// intended to be used more than once.
    #[inline]
    pub fn new(pattern: &str) -> Result<Regex, Error> {
        Self::with_flags(pattern, Flags::default())
    }

    /// Construct a regex by parsing `pattern` with `flags`.
    /// An Error may be returned if the syntax is invalid.
    //
    /// Note it is preferable to cache a Regex which is intended to be used more
    /// than once, as the parse may be expensive. For example:
    #[inline]
    pub fn with_flags<F>(pattern: &str, flags: F) -> Result<Regex, Error>
    where
        F: Into<Flags>,
    {
        Self::from_unicode(pattern.chars().map(u32::from), flags)
    }

    /// Construct a regex by parsing `pattern` with `flags`, where
    /// `pattern` is an iterator of `u32` Unicode codepoints.
    /// An Error may be returned if the syntax is invalid.
    /// This allows parsing regular expressions from exotic strings in
    /// other encodings, such as UTF-16 or UTF-32.
    pub fn from_unicode<I, F>(pattern: I, flags: F) -> Result<Regex, Error>
    where
        I: Iterator<Item = u32> + Clone,
        F: Into<Flags>,
    {
        let flags = flags.into();
        let mut ire = parse::try_parse(pattern, flags)?;
        if !flags.no_opt {
            optimizer::optimize(&mut ire);
        }
        let cr = emit::emit(&ire);
        Ok(Regex { cr })
    }

    /// Searches `text` to find the first match.
    #[inline]
    pub fn find(&self, text: &str) -> Option<Match> {
        self.find_iter(text).next()
    }

    /// Searches `text`, returning an iterator over non-overlapping matches.
    /// Note that the resulting Iterator borrows both the regex `'r` and the
    /// input string as `'t`.
    #[inline]
    pub fn find_iter<'r, 't>(&'r self, text: &'t str) -> Matches<'r, 't> {
        self.find_from(text, 0)
    }

    /// Returns an iterator for matches found in 'text' starting at byte index
    /// `start`. Note this may be different from passing a sliced `text` in
    /// the case of lookbehind assertions.
    /// Example:
    ///
    ///  ```rust
    ///   use regress::Regex;
    ///   let text = "xyxy";
    ///   let re = Regex::new(r"(?<=x)y").unwrap();
    ///   let t1 = re.find(&text[1..]).unwrap().range();
    ///   assert!(t1 == (2..3));
    ///   let t2 = re.find_from(text, 1).next().unwrap().range();
    ///   assert!(t2 == (1..2));
    ///   ```
    #[inline]
    pub fn find_from<'r, 't>(&'r self, text: &'t str, start: usize) -> Matches<'r, 't> {
        backends::find(self, text, start)
    }

    /// Searches `text` to find the first match.
    /// The input text is expected to be ascii-only: only ASCII case-folding is
    /// supported.
    #[inline]
    pub fn find_ascii(&self, text: &str) -> Option<Match> {
        self.find_iter_ascii(text).next()
    }

    /// Searches `text`, returning an iterator over non-overlapping matches.
    /// The input text is expected to be ascii-only: only ASCII case-folding is
    /// supported.
    #[inline]
    pub fn find_iter_ascii<'r, 't>(&'r self, text: &'t str) -> AsciiMatches<'r, 't> {
        self.find_from_ascii(text, 0)
    }

    /// Returns an iterator for matches found in 'text' starting at byte index
    /// `start`.
    #[inline]
    pub fn find_from_ascii<'r, 't>(&'r self, text: &'t str, start: usize) -> AsciiMatches<'r, 't> {
        backends::find(self, text, start)
    }

    /// Returns an iterator for matches found in 'text' starting at index `start`.
    #[cfg(feature = "utf16")]
    pub fn find_from_utf16<'r, 't>(
        &'r self,
        text: &'t [u16],
        start: usize,
    ) -> exec::Matches<super::classicalbacktrack::BacktrackExecutor<'r, indexing::Utf16Input<'t>>>
    {
        let input = Utf16Input::new(text, self.cr.flags.unicode);
        exec::Matches::new(
            super::classicalbacktrack::BacktrackExecutor::new(
                input,
                MatchAttempter::new(&self.cr, input.left_end()),
            ),
            start,
        )
    }

    /// Returns an iterator for matches found in 'text' starting at index `start`.
    #[cfg(feature = "utf16")]
    pub fn find_from_ucs2<'r, 't>(
        &'r self,
        text: &'t [u16],
        start: usize,
    ) -> exec::Matches<super::classicalbacktrack::BacktrackExecutor<'r, indexing::Ucs2Input<'t>>>
    {
        let input = Ucs2Input::new(text, self.cr.flags.unicode);
        exec::Matches::new(
            super::classicalbacktrack::BacktrackExecutor::new(
                input,
                MatchAttempter::new(&self.cr, input.left_end()),
            ),
            start,
        )
    }
}

impl FromStr for Regex {
    type Err = Error;

    /// Attempts to parse a string into a regular expression
    #[inline]
    fn from_str(s: &str) -> Result<Self, Error> {
        Self::new(s)
    }
}

// Support for using regress with different regex backends.
// Currently there is only the classical backtracking, and PikeVM.
#[doc(hidden)]
pub mod backends {
    use super::exec;
    use super::indexing;
    use super::Regex;
    pub use crate::emit::emit;
    pub use crate::optimizer::optimize;
    pub use crate::parse::try_parse;

    /// An Executor using the classical backtracking algorithm.
    pub type BacktrackExecutor<'r, 't> =
        super::classicalbacktrack::BacktrackExecutor<'r, indexing::Utf8Input<'t>>;

    /// A Executor using the PikeVM executor.
    #[cfg(feature = "backend-pikevm")]
    pub type PikeVMExecutor<'r, 't> = super::pikevm::PikeVMExecutor<'r, indexing::Utf8Input<'t>>;

    /// An alias type to the default Executor.
    pub type DefaultExecutor<'r, 't> = BacktrackExecutor<'r, 't>;

    /// An alias type to the default executor's ASCII form.
    pub type DefaultAsciiExecutor<'r, 't> =
        <DefaultExecutor<'r, 't> as exec::Executor<'r, 't>>::AsAscii;

    /// Searches `text`, returning an iterator over non-overlapping matches.
    pub fn find<'r, 't, Executor: exec::Executor<'r, 't>>(
        re: &'r Regex,
        text: &'t str,
        start: usize,
    ) -> exec::Matches<Executor> {
        exec::Matches::new(Executor::new(&re.cr, text), start)
    }

    /// Searches `text`, returning an iterator over non-overlapping matches.
    /// This is a convenience method to avoid E0223.
    pub fn find_ascii<'r, 't, Executor: exec::Executor<'r, 't>>(
        re: &'r Regex,
        text: &'t str,
        start: usize,
    ) -> exec::Matches<Executor::AsAscii> {
        find::<Executor::AsAscii>(re, text, start)
    }
}
