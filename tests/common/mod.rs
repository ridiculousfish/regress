#![allow(clippy::uninlined_format_args)]

/// Test that \p pattern fails to parse with default flags.
pub fn test_parse_fails(pattern: &str) {
    let res = regress::Regex::new(pattern);
    assert!(res.is_err(), "Pattern should not have parsed: {}", pattern);
}

/// Test that \p pattern fails to parse with flags.
pub fn test_parse_fails_flags(pattern: &str, flags: &str) {
    let res = regress::Regex::with_flags(pattern, flags);
    assert!(res.is_err(), "Pattern should not have parsed: {}", pattern);
}

/// Format a Match by inserting commas between all capture groups.
fn format_match(r: &regress::Match, input: &str) -> String {
    let mut result = input[r.range()].to_string();
    for cg in r.captures.iter() {
        result.push(',');
        if let Some(cg) = cg {
            result.push_str(&input[cg.clone()])
        }
    }
    result
}

/// Encode a string as UTF16.
pub fn to_utf16(input: &str) -> Vec<u16> {
    input.encode_utf16().collect()
}

/// Given a range of a string encoded as UTF16, return the corresponding
/// range in the original string (UTF-8).
pub fn range_from_utf16(utf16: &[u16], r: regress::Range) -> regress::Range {
    use std::char::decode_utf16;
    // Figure out start.
    let start_utf8: usize = decode_utf16(utf16[0..r.start].iter().copied())
        .map(|r| r.expect("Invalid UTF16").len_utf8())
        .sum();
    let len_utf8: usize = decode_utf16(utf16[r].iter().copied())
        .map(|r| r.expect("Invalid UTF16").len_utf8())
        .sum();
    start_utf8..(start_utf8 + len_utf8)
}

pub trait StringTestHelpers {
    /// "Fluent" style helper for testing that a String is equal to a str.
    fn test_eq(&self, s: &str);
}

impl StringTestHelpers for String {
    fn test_eq(&self, rhs: &str) {
        assert_eq!(self.as_str(), rhs)
    }
}

pub trait VecTestHelpers {
    /// "Fluent" style helper for testing that a Vec<&str> is equal to a
    /// Vec<&str>.
    fn test_eq(&self, rhs: Vec<&str>);
}

impl VecTestHelpers for Vec<&str> {
    fn test_eq(&self, rhs: Vec<&str>) {
        assert_eq!(*self, rhs)
    }
}

/// Skip protocol for "this backend can't handle this pattern":
///
/// - The FA backends (`Backend::Tnfa`, `Backend::Tdfa`) don't yet support
///   every IR construct (anchors, lookarounds, backreferences, word
///   boundaries, etc.). When `Nfa::try_from` or `Tdfa::try_from` returns
///   `Err`, `build_test_artifacts` prints a `SKIP …` line to stderr and
///   raises a panic carrying a `String` payload that starts with
///   `SKIP_BACKEND_PREFIX` followed by the failure reason.
/// - `run_tc` wraps each `func(tc)` call in `catch_unwind`. If the panic
///   payload downcasts to a `String` starting with that prefix, it's
///   silently swallowed and the harness moves on to the next TestConfig.
///   Any other panic payload resumes — i.e. real test assertion failures
///   propagate normally.
/// - Bytecode backends (PikeVM, Backtracking) never raise this panic, so
///   the catch_unwind is a no-op for them.
///
/// The payload is a `String` rather than a marker struct because the
/// default panic hook only formats `&'static str` and `String` payloads —
/// other types print as "Box<dyn Any>", which is noise in stderr when SKIPs
/// fire. Embedding the reason in the panic message means each SKIP prints
/// the full context (`<SKIP: backend=… pattern=… err=…>`) before being
/// absorbed, so RUST_BACKTRACE=1 output is self-explanatory.
///
/// Why panic+catch rather than `Result` propagation: test functions are
/// `Fn(TestConfig)` containing many independent assertions; non-locally
/// exiting after one unsupported pattern would otherwise require threading
/// `?` through every `compilef` call site across the test corpus. The
/// catch is narrow (only this marker prefix) so other panics still surface.
const SKIP_BACKEND_PREFIX: &str = "<SKIP: FA backend can't compile this pattern";

fn skip_backend(backend: Backend, pattern: &str, reason: &str) -> ! {
    eprintln!(
        "SKIP {:?}: {} for pattern={:?}",
        backend, reason, pattern
    );
    std::panic::panic_any(format!(
        "{} backend={:?} pattern={:?} reason={}>",
        SKIP_BACKEND_PREFIX, backend, pattern, reason
    ))
}

/// Drive parse → optimize → emit ourselves so the harness can hold the
/// `CompiledRegex` directly (for the bytecode executors) plus the parallel
/// `Nfa`/`Tdfa` built from the same IR. Avoids the bytecode engines having
/// to reach into `Regex` internals.
///
/// If the selected backend is Tnfa or Tdfa and the FA build fails, logs a
/// SKIP line and raises the skip-marker panic — `test_with_configs`
/// catches that and skips the rest of the test function for this TestConfig.
fn build_test_artifacts(
    pattern: &str,
    flags: regress::Flags,
    backend: Backend,
) -> (
    regress::backends::CompiledRegex,
    Option<regress::backends::Nfa>,
    Option<regress::backends::Tdfa>,
) {
    use regress::backends;
    let mut ire = backends::try_parse(pattern.chars().map(u32::from), flags)
        .expect("Pattern failed to parse (already validated)");
    if !flags.no_opt {
        backends::optimize(&mut ire);
    }
    let cr = backends::emit(&ire);

    if !matches!(backend, Backend::Tnfa | Backend::Tdfa) {
        return (cr, None, None);
    }
    let nfa = match backends::Nfa::try_from(&ire) {
        Ok(n) => n,
        Err(e) => skip_backend(backend, pattern, &format!("NFA build failed: {:?}", e)),
    };
    if matches!(backend, Backend::Tnfa) {
        return (cr, Some(nfa), None);
    }
    // Backend::Tdfa
    match backends::Tdfa::try_from(&nfa) {
        Ok(t) => (cr, Some(nfa), Some(t)),
        Err(e) => skip_backend(backend, pattern, &format!("TDFA build failed: {:?}", e)),
    }
}

/// A compiled regex which remembers a TestConfig.
#[derive(Debug)]
pub struct TestCompiledRegex {
    #[allow(dead_code)] // only read by the utf16/ucs2 paths
    re: regress::Regex,
    cr: regress::backends::CompiledRegex,
    nfa: Option<regress::backends::Nfa>,
    tdfa: Option<regress::backends::Tdfa>,
    tc: TestConfig,
}

impl TestCompiledRegex {
    /// Search for self in \p input, returning a list of all matches.
    #[track_caller]
    pub fn matches(&'_ self, input: &'_ str, start: usize) -> Vec<regress::Match> {
        use regress::backends as rbe;
        #[cfg(feature = "utf16")]
        {
            // We don't test the PikeVM backend with UTF16 or UCS2.
            if self.tc.encoding == Encoding::Utf16 {
                return self.match_utf16(input, start);
            } else if self.tc.encoding == Encoding::Ucs2 {
                // Don't test with UCS-2 if the input contains a surrogate pair,
                // as the tests expect these to pass. UCS-2 is tested separately
                // in other places.
                if input.chars().any(|c| c > '\u{FFFF}') {
                    return self.match_utf16(input, start);
                }
                return self.match_ucs2(input, start);
            }
        }
        match (self.tc.use_ascii(input), self.tc.backend) {
            (true, Backend::PikeVM) => {
                rbe::find::<rbe::PikeVMExecutor>(&self.cr, input, start).collect()
            }

            (false, Backend::PikeVM) => {
                rbe::find::<rbe::PikeVMExecutor>(&self.cr, input, start).collect()
            }

            (true, Backend::Backtracking) => {
                rbe::find_ascii::<rbe::BacktrackExecutor>(&self.cr, input, start).collect()
            }

            (false, Backend::Backtracking) => {
                rbe::find::<rbe::BacktrackExecutor>(&self.cr, input, start).collect()
            }

            (_, Backend::Tnfa) => match &self.nfa {
                Some(nfa) => rbe::find::<rbe::NfaExecutor>(nfa, input, start).collect(),
                None => Vec::new(),
            },

            (_, Backend::Tdfa) => match &self.tdfa {
                Some(tdfa) => rbe::find::<rbe::TdfaExecutor>(tdfa, input, start).collect(),
                None => Vec::new(),
            },
        }
    }

    /// Encode a string as UTF16, and match against it as UTF16.
    /// 'start' is given as the byte offset into the UTF8 string.
    #[cfg(feature = "utf16")]
    #[track_caller]
    pub fn match_utf16(&self, input: &str, start: usize) -> Vec<regress::Match> {
        // convert the input and start to UTF16.
        let u16_start = input[..start].chars().map(char::len_utf16).sum();
        let u16_input = to_utf16(input);
        let mut matches: Vec<_> = self.re.find_from_utf16(&u16_input, u16_start).collect();
        // Convert any ranges back to UTF8.
        for matc in matches.iter_mut() {
            matc.range = range_from_utf16(&u16_input, matc.range());
            for r in matc.captures.iter_mut().flatten() {
                *r = range_from_utf16(&u16_input, r.clone());
            }
        }
        matches
    }

    /// Encode a string as UTF16, and match against it as UCS2.
    #[cfg(feature = "utf16")]
    #[track_caller]
    pub fn match_ucs2(&self, input: &str, start: usize) -> Vec<regress::Match> {
        let u16_start = input[..start].chars().map(char::len_utf16).sum();
        let u16_input = to_utf16(input);
        let mut matches: Vec<_> = self.re.find_from_ucs2(&u16_input, u16_start).collect();
        // Convert any ranges back to UTF8.
        for matc in matches.iter_mut() {
            matc.range = range_from_utf16(&u16_input, matc.range());
            for r in matc.captures.iter_mut().flatten() {
                *r = range_from_utf16(&u16_input, r.clone());
            }
        }
        matches
    }

    /// Search for self in \p input, returning the first Match, or None if
    /// none.
    pub fn find(&self, input: &str) -> Option<regress::Match> {
        self.matches(input, 0).into_iter().next()
    }

    /// Like find(), but for UTF-16.
    #[cfg(feature = "utf16")]
    pub fn find_utf16(&self, input: &str) -> Option<regress::Match> {
        self.match_utf16(input, 0).into_iter().next()
    }

    /// Like find(), but for UCS2.
    #[cfg(feature = "utf16")]
    pub fn find_ucs2(&self, input: &str) -> Option<regress::Match> {
        self.match_ucs2(input, 0).into_iter().next()
    }

    /// Match against a string, returning the first formatted match.
    #[track_caller]
    pub fn match1f(&self, input: &str) -> String {
        match self.find(input) {
            Some(m) => format_match(&m, input),
            None => panic!("Failed to match {}", input),
        }
    }

    /// Match against a string, returning the string of the named capture group given.
    pub fn match1_named_group(&self, input: &str, group: &str) -> String {
        match self.find(input) {
            Some(m) => match m.named_group(group) {
                Some(r) => match input.get(r.clone()) {
                    Some(str) => str.to_string(),
                    None => panic!("Cannot get range from string input {:?}", r),
                },
                None => panic!("Named capture group does not exist {}", group),
            },
            None => panic!("Failed to match {}", input),
        }
    }

    /// Match against a string, returning the match as a Vec containing None
    /// for unmatched groups, or the matched strings.
    pub fn match1_vec<'b>(&self, input: &'b str) -> Vec<Option<&'b str>> {
        let mut result = Vec::new();
        let m: regress::Match = self.find(input).expect("Failed to match");
        result.push(Some(&input[m.range()]));
        for cr in m.captures {
            result.push(cr.map(|r| &input[r]));
        }
        result
    }

    /// Test that matching against \p input fails.
    #[track_caller]
    pub fn test_fails(&self, input: &str) {
        assert!(self.find(input).is_none(), "Should not have matched")
    }

    /// Test that matching against \p input succeeds.
    #[track_caller]
    pub fn test_succeeds(&self, input: &str) {
        assert!(self.find(input).is_some(), "Should have matched")
    }

    /// Return a list of all non-overlapping total match ranges from a given
    /// start.
    pub fn match_all_from(&'_ self, input: &'_ str, start: usize) -> Vec<regress::Range> {
        self.matches(input, start)
            .into_iter()
            .map(move |m| m.range())
            .collect()
    }

    /// Return a list of all non-overlapping matches.
    pub fn match_all<'b>(&self, input: &'b str) -> Vec<&'b str> {
        self.matches(input, 0)
            .into_iter()
            .map(move |m| &input[m.range()])
            .collect()
    }

    /// Collect all matches into a String, separated by commas.
    pub fn run_global_match(&self, input: &str) -> String {
        self.matches(input, 0)
            .into_iter()
            .map(move |m| format_match(&m, input))
            .collect::<Vec<String>>()
            .join(",")
    }
}

/// Our backend types.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
enum Backend {
    PikeVM,
    Backtracking,
    Tnfa,
    Tdfa,
}

/// Our encoding types.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
enum Encoding {
    Utf8,
    Utf16,
    Ucs2,
}

/// Description of how to test a regex.
#[derive(Debug, Copy, Clone)]
pub struct TestConfig {
    // Whether to prefer ASCII forms if the input is ASCII.
    ascii: bool,

    // Whether to optimize.
    optimize: bool,

    // Which backend to use.
    backend: Backend,

    // Which encoding to use. Only used if utf16 is enabled.
    #[allow(dead_code)]
    encoding: Encoding,
}

impl TestConfig {
    /// Whether to use ASCII for this input.
    pub fn use_ascii(&self, s: &str) -> bool {
        self.ascii && s.is_ascii()
    }

    /// Compile a pattern to a regex, with default flags.
    pub fn compile(&self, pattern: &str) -> TestCompiledRegex {
        self.compilef(pattern, "")
    }

    /// Compile a pattern to a regex, with given flags.
    #[track_caller]
    pub fn compilef(&self, pattern: &str, flags_str: &str) -> TestCompiledRegex {
        let mut flags = regress::Flags::from(flags_str);
        flags.no_opt = !self.optimize;

        // Validate parse via the public API so error reporting matches what
        // users would see, then drive the pipeline ourselves to obtain a
        // CompiledRegex we own (without reaching into Regex internals).
        if let Err(e) = regress::Regex::with_flags(pattern, flags) {
            panic!(
                "Failed to parse! flags: {} pattern: {}, error: {}",
                flags_str, pattern, e
            );
        }
        let (cr, nfa, tdfa) = build_test_artifacts(pattern, flags, self.backend);
        let re = regress::Regex::from(cr.clone());
        TestCompiledRegex {
            re,
            cr,
            nfa,
            tdfa,
            tc: *self,
        }
    }

    /// Test that \p pattern and \p flags successfully parses, and matches
    /// \p input.
    #[track_caller]
    pub fn test_match_succeeds(&self, pattern: &str, flags_str: &str, input: &str) {
        let cr = self.compilef(pattern, flags_str);
        cr.test_succeeds(input)
    }

    /// Test that \p pattern and \p flags successfully parses, and does not
    /// match \p input.
    pub fn test_match_fails(&self, pattern: &str, flags_str: &str, input: &str) {
        let cr = self.compilef(pattern, flags_str);
        cr.test_fails(input)
    }
}

/// Invoke `func` with `tc`, catching the skip-marker panic that the
/// harness raises when an FA backend can't compile some pattern in the
/// test body. Bytecode backends (PikeVM, Backtracking) never raise it,
/// so the catch is effectively a no-op for them.
fn run_tc<F: Fn(TestConfig)>(func: &F, tc: TestConfig) {
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| func(tc)));
    if let Err(payload) = result {
        if let Some(s) = payload.downcast_ref::<String>() {
            if s.starts_with(SKIP_BACKEND_PREFIX) {
                return;
            }
        }
        std::panic::resume_unwind(payload);
    }
}

/// Invoke \p F with each test config, in turn.
pub fn test_with_configs<F>(func: F)
where
    F: Fn(TestConfig),
{
    let encoding = Encoding::Utf8;
    // Note we wish to be able to determine the TestConfig from the line number.
    // Also note that optimizations are not supported for PikeVM backend, as it
    // doesn't implement Loop1CharBody.
    run_tc(&func, TestConfig {
        ascii: true,
        optimize: false,
        backend: Backend::PikeVM,
        encoding,
    });
    run_tc(&func, TestConfig {
        ascii: false,
        optimize: false,
        backend: Backend::PikeVM,
        encoding,
    });
    run_tc(&func, TestConfig {
        ascii: true,
        optimize: false,
        backend: Backend::Backtracking,
        encoding,
    });
    run_tc(&func, TestConfig {
        ascii: false,
        optimize: false,
        backend: Backend::Backtracking,
        encoding,
    });
    run_tc(&func, TestConfig {
        ascii: true,
        optimize: true,
        backend: Backend::Backtracking,
        encoding,
    });
    run_tc(&func, TestConfig {
        ascii: false,
        optimize: true,
        backend: Backend::Backtracking,
        encoding,
    });

    // NFA/TDFA backends: byte-based, no ascii/utf16/ucs2 variants needed.
    run_tc(&func, TestConfig {
        ascii: false,
        optimize: false,
        backend: Backend::Tnfa,
        encoding: Encoding::Utf8,
    });
    run_tc(&func, TestConfig {
        ascii: false,
        optimize: false,
        backend: Backend::Tdfa,
        encoding: Encoding::Utf8,
    });

    // UTF16 and UCS2.
    if cfg!(feature = "utf16") {
        run_tc(&func, TestConfig {
            ascii: false,
            optimize: false,
            backend: Backend::Backtracking,
            encoding: Encoding::Utf16,
        });

        run_tc(&func, TestConfig {
            ascii: false,
            optimize: true,
            backend: Backend::Backtracking,
            encoding: Encoding::Utf16,
        });

        run_tc(&func, TestConfig {
            ascii: false,
            optimize: false,
            backend: Backend::Backtracking,
            encoding: Encoding::Ucs2,
        });

        run_tc(&func, TestConfig {
            ascii: false,
            optimize: true,
            backend: Backend::Backtracking,
            encoding: Encoding::Ucs2,
        });
    }
}

/// Invoke `F` with each test config.
/// Exclude ASCII tests.
pub fn test_with_configs_no_ascii<F>(func: F)
where
    F: Fn(TestConfig),
{
    // Note we wish to be able to determine the TestConfig from the line number.
    // Also note that optimizations are not supported for PikeVM backend, as it
    // doesn't implement Loop1CharBody.
    run_tc(&func, TestConfig {
        ascii: false,
        optimize: false,
        backend: Backend::PikeVM,
        encoding: Encoding::Utf8,
    });
    run_tc(&func, TestConfig {
        ascii: false,
        optimize: false,
        backend: Backend::Backtracking,
        encoding: Encoding::Utf8,
    });
    run_tc(&func, TestConfig {
        ascii: false,
        optimize: true,
        backend: Backend::Backtracking,
        encoding: Encoding::Utf8,
    });

    // NFA/TDFA backends: byte-based, no ascii/utf16/ucs2 variants needed.
    run_tc(&func, TestConfig {
        ascii: false,
        optimize: false,
        backend: Backend::Tnfa,
        encoding: Encoding::Utf8,
    });
    run_tc(&func, TestConfig {
        ascii: false,
        optimize: false,
        backend: Backend::Tdfa,
        encoding: Encoding::Utf8,
    });

    // UTF16 and UCS2.
    if cfg!(feature = "utf16") {
        run_tc(&func, TestConfig {
            ascii: false,
            optimize: false,
            backend: Backend::Backtracking,
            encoding: Encoding::Utf16,
        });

        run_tc(&func, TestConfig {
            ascii: false,
            optimize: true,
            backend: Backend::Backtracking,
            encoding: Encoding::Utf16,
        });

        run_tc(&func, TestConfig {
            ascii: false,
            optimize: false,
            backend: Backend::Backtracking,
            encoding: Encoding::Ucs2,
        });

        run_tc(&func, TestConfig {
            ascii: false,
            optimize: true,
            backend: Backend::Backtracking,
            encoding: Encoding::Ucs2,
        });
    }
}
