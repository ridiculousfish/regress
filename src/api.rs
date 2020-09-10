use crate::classicalbacktrack;
use crate::emit;
use crate::exec;
use crate::indexing;
use crate::insn::CompiledRegex;
use crate::optimizer;
use crate::parse;

#[cfg(feature = "backend-pikevm")]
use crate::pikevm;

use std::fmt;

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

    /// If set, dump the IR before optimization.
    #[cfg(feature = "dump-phases")]
    pub dump_init_ir: bool,

    /// If set, dump the IR after optimization.
    #[cfg(feature = "dump-phases")]
    pub dump_opt_ir: bool,

    /// If set, dump the emitted bytecode.
    #[cfg(feature = "dump-phases")]
    pub dump_bytecode: bool,
}

impl Flags {
    /// Construct a Flags from a string, using JavaScript field names.
    /// 'i' means to ignore case, 'm' means multiline.
    /// Note the 'g' flag implies a stateful regex and is not supported.
    /// Other flags are not implemented and are ignored.
    pub fn from(s: &str) -> Self {
        let mut result = Self::default();
        for c in s.chars() {
            match c {
                'm' => {
                    result.multiline = true;
                }
                'i' => {
                    result.icase = true;
                }
                's' => {
                    result.dot_all = true;
                }
                _ => {
                    // Silently skip unsupported flags.
                }
            }
        }
        result
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
        Ok(())
    }
}

/// Range is used to express the extent of a match, as indexes into the input
/// string.
pub type Range = std::ops::Range<usize>;

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
    pub total_range: Range,

    /// The list of captures. This has length equal to the number of capturing
    /// groups in the regex. For each capture, if the value is None, that group
    /// did not match (for example, it was in a not-taken branch of an
    /// alternation). If the value is Some, the group did match with the
    /// enclosed range.
    pub captures: Vec<Option<Range>>,
}

impl Match {
    /// Access a group by index, using the convention of Python's group()
    /// function. Index 0 is the total match, index 1 is the first capture
    /// group.
    pub fn group(&self, idx: usize) -> Option<Range> {
        if idx == 0 {
            Some(self.total_range.clone())
        } else {
            self.captures[idx - 1].clone()
        }
    }

    /// Return the total range. This is a convenience function to work around
    /// the fact that Range does not support Copy.
    pub fn total(&self) -> Range {
        self.total_range.clone()
    }

    /// Returns an iterator over the capture groups of a Match
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
    fn new(mat: &'m Match) -> Self {
        Self {
            mat,
            i: 0,
            max: mat.captures.len() + 1,
        }
    }
}

impl<'m> Iterator for Groups<'m> {
    type Item = Option<Range>;
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

/// A Regex is the compiled version of a pattern.
#[derive(Debug, Clone)]
pub struct Regex {
    cr: CompiledRegex,
}

impl Regex {
    /// Construct a regex by parsing `pattern` using the default flags.
    /// An Error may be returned if the syntax is invalid.
    /// Note that this is rather expensive; prefer to cache a Regex which is
    /// intended to be used more than once.
    pub fn new(pattern: &str) -> Result<Regex, Error> {
        Self::newf(pattern, Flags::default())
    }

    /// Construct a regex by parsing `pattern` with `flags`.
    /// An Error may be returned if the syntax is invalid.
    //
    /// Note it is preferable to cache a Regex which is intended to be used more
    /// than once, as the parse may be expensive. For example:
    pub fn newf(pattern: &str, flags: Flags) -> Result<Regex, Error> {
        let mut ire = parse::try_parse(pattern, flags)?;

        #[cfg(feature = "dump-phases")]
        {
            if flags.dump_init_ir {
                println!("Unoptimized IR:\n{}", ire);
            }
        }

        if !flags.no_opt {
            optimizer::optimize(&mut ire);

            #[cfg(feature = "dump-phases")]
            {
                if flags.dump_opt_ir {
                    println!("Optimized IR:\n{}", ire);
                }
            }
        }
        let cr = emit::emit(&ire);
        #[cfg(feature = "dump-phases")]
        {
            if flags.dump_bytecode {
                println!("Bytecode:\n{:?}", cr);
            }
        }
        Ok(Regex { cr })
    }

    /// Searches `text` to find the first match.
    pub fn find(&self, text: &str) -> Option<Match> {
        self.find_iter(text).next()
    }

    /// Searches `text`, returning an iterator over non-overlapping matches.
    /// Note that the resulting Iterator borrows both the regex `'r` and the
    /// input string as `'t`.
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
    ///   let t1 = re.find(&text[1..]).unwrap().total();
    ///   assert!(t1 == (2..3));
    ///   let t2 = re.find_from(text, 1).next().unwrap().total();
    ///   assert!(t2 == (1..2));
    ///   ```
    pub fn find_from<'r, 't>(&'r self, text: &'t str, start: usize) -> Matches<'r, 't> {
        backends::find(self, text, start)
    }

    /// Searches `text` to find the first match.
    /// The input text is expected to be ascii-only: only ASCII case-folding is
    /// supported.
    pub fn find_ascii(&self, text: &str) -> Option<Match> {
        self.find_iter_ascii(text).next()
    }

    /// Searches `text`, returning an iterator over non-overlapping matches.
    /// The input text is expected to be ascii-only: only ASCII case-folding is
    /// supported.
    pub fn find_iter_ascii<'r, 't>(&'r self, text: &'t str) -> AsciiMatches<'r, 't> {
        self.find_from_ascii(text, 0)
    }

    /// Returns an iterator for matches found in 'text' starting at byte index
    /// `start`.
    pub fn find_from_ascii<'r, 't>(&'r self, text: &'t str, start: usize) -> AsciiMatches<'r, 't> {
        backends::find(self, text, start)
    }
}

// Support for using regress with different regex backends.
// Currently there is only the classical backtracking, and PikeVM.
#[doc(hidden)]
pub mod backends {
    use super::exec;
    use super::indexing;
    use super::Regex;

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
