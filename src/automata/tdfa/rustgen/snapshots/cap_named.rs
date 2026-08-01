{
    // regress AoT-compiled matcher for pattern "Dr (?<name>[A-Z]\\w+)".
    use ::regress::__codegen as __rt;
    static __PREFILTER: __rt::PrefilterSpec = __rt::PrefilterSpec::Prefix {
        predicate: __rt::PredicateSpec::ByteSeq { bytes: b"Dr ", finder: __rt::FinderCache::new() },
        lit_window: ::core::option::Option::None,
    };
    static __GROUP_NAMES: &[&str] = &["name"];
    #[allow(unused_mut, unused_assignments)]
    fn __verify(
        input: &[u8],
        start: usize,
        caps: &mut [usize],
    ) -> ::core::option::Option<(usize, usize)> {
        let len = input.len();
        let mut pos = start;
        let mut m = [usize::MAX; 4];
        let mut acc_end = usize::MAX;
        let mut acc_state = u32::MAX;
        if start == 0 {
            m[0] = pos;
        } else {
            m[0] = pos;
            m[1] = pos;
        }
        let mut state: u32 = 1;
        'scan: loop {
            match state {
                1 => {
                    if pos >= len {
                        break 'scan;
                    }
                    let b = input[pos];
                    pos += 1;
                    match b {
                        b'D' => {
                            state = 2;
                        }
                        _ => {
                            break 'scan;
                        }
                    }
                }
                2 => {
                    if pos >= len {
                        break 'scan;
                    }
                    let b = input[pos];
                    pos += 1;
                    match b {
                        b'r' => {
                            state = 3;
                        }
                        _ => {
                            break 'scan;
                        }
                    }
                }
                3 => {
                    if pos >= len {
                        break 'scan;
                    }
                    let b = input[pos];
                    pos += 1;
                    match b {
                        b' ' => {
                            m[1] = pos;
                            m[2] = pos;
                            state = 4;
                        }
                        _ => {
                            break 'scan;
                        }
                    }
                }
                4 => {
                    if pos >= len {
                        break 'scan;
                    }
                    let b = input[pos];
                    pos += 1;
                    match b {
                        b'A'..=b'Z' => {
                            state = 5;
                        }
                        _ => {
                            break 'scan;
                        }
                    }
                }
                5 => {
                    if pos >= len {
                        break 'scan;
                    }
                    let b = input[pos];
                    pos += 1;
                    match b {
                        b'0'..=b'9' | b'A'..=b'Z' | b'_' | b'a'..=b'z' => {
                            m[2] = pos;
                            m[3] = pos;
                            state = 6;
                        }
                        _ => {
                            break 'scan;
                        }
                    }
                }
                6 => {
                    let p0 = pos;
                    while pos < len && matches!(input[pos], b'0'..=b'9' | b'A'..=b'Z' | b'_' | b'a'..=b'z') {
                        pos += 1;
                    }
                    if pos != p0 {
                        m[2] = pos;
                        m[3] = pos;
                    }
                    acc_end = pos;
                    acc_state = 6;
                    if pos >= len {
                        break 'scan;
                    }
                    let b = input[pos];
                    pos += 1;
                    match b {
                        b'0'..=b'9' | b'A'..=b'Z' | b'_' | b'a'..=b'z' => {
                            m[2] = pos;
                            m[3] = pos;
                            state = 6;
                        }
                        _ => {
                            break 'scan;
                        }
                    }
                }
                _ => ::core::unreachable!(),
            }
        }
        if acc_state == u32::MAX {
            return ::core::option::Option::None;
        }
        for c in caps.iter_mut() {
            *c = usize::MAX;
        }
        match acc_state {
            6 => {
                caps[0] = m[1];
                caps[1] = m[2];
                let fs = m[0];
                let fe = m[3];
                ::core::option::Option::Some((
                    if fs == usize::MAX { 0 } else { fs },
                    if fe == usize::MAX { acc_end } else { fe },
                ))
            }
            _ => ::core::unreachable!(),
        }
    }
    __rt::CompiledMatcher::from_parts(&__PREFILTER, __verify, 1usize, __GROUP_NAMES, __rt::MatcherTier::Unrolled)
}
