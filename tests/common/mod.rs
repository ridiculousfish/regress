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

/// A compiled regex which remembers a TestConfig.
#[derive(Debug, Clone)]
pub struct TestCompiledRegex {
    re: regress::Regex,
    tc: TestConfig,
}

impl TestCompiledRegex {
    /// Search for self in \p input, returning a list of all matches.
    pub fn matches(&'_ self, input: &'_ str, start: usize) -> Vec<regress::Match> {
        use regress::backends as rbe;
        match (self.tc.use_ascii(input), self.tc.backend) {
            (true, Backend::PikeVM) => {
                rbe::find::<rbe::PikeVMExecutor>(&self.re, input, start).collect()
            }

            (false, Backend::PikeVM) => {
                rbe::find::<rbe::PikeVMExecutor>(&self.re, input, start).collect()
            }

            (true, Backend::Backtracking) => {
                rbe::find_ascii::<rbe::BacktrackExecutor>(&self.re, input, start).collect()
            }

            (false, Backend::Backtracking) => {
                rbe::find::<rbe::BacktrackExecutor>(&self.re, input, start).collect()
            }
        }
    }

    /// Search for self in \p input, returning the first Match, or None if
    /// none.
    pub fn find(&self, input: &str) -> Option<regress::Match> {
        self.matches(input, 0).into_iter().next()
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
#[derive(Debug, Copy, Clone)]
enum Backend {
    PikeVM,
    Backtracking,
}

/// Description of how to test a regex.
#[derive(Debug, Copy, Clone)]
pub struct TestConfig {
    // Whether to prefer ASCII forms if the input is ASCII.
    ascii: bool,

    // Whether to optimize.
    optimize: bool,

    // Which backed to use.
    backend: Backend,
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

        let re = regress::Regex::with_flags(pattern, flags);
        assert!(
            re.is_ok(),
            "Failed to parse! flags: {} pattern: {}, error: {}",
            flags_str,
            pattern,
            re.unwrap_err()
        );
        TestCompiledRegex {
            re: re.unwrap(),
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

/// Invoke \p F with each test config, in turn.
pub fn test_with_configs<F>(func: F)
where
    F: Fn(TestConfig),
{
    // Note we wish to be able to determine the TestConfig from the line number.
    // Also note that optimizations are not supported for PikeVM backend, as it
    // doesn't implement Loop1CharBody.
    func(TestConfig {
        ascii: true,
        optimize: false,
        backend: Backend::PikeVM,
    });
    func(TestConfig {
        ascii: false,
        optimize: false,
        backend: Backend::PikeVM,
    });
    func(TestConfig {
        ascii: true,
        optimize: false,
        backend: Backend::Backtracking,
    });
    func(TestConfig {
        ascii: false,
        optimize: false,
        backend: Backend::Backtracking,
    });
    func(TestConfig {
        ascii: true,
        optimize: true,
        backend: Backend::Backtracking,
    });
    func(TestConfig {
        ascii: false,
        optimize: true,
        backend: Backend::Backtracking,
    });
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
    func(TestConfig {
        ascii: false,
        optimize: false,
        backend: Backend::PikeVM,
    });
    func(TestConfig {
        ascii: false,
        optimize: false,
        backend: Backend::Backtracking,
    });
    func(TestConfig {
        ascii: false,
        optimize: true,
        backend: Backend::Backtracking,
    });
}
