{
    // regress AoT-compiled matcher for pattern "(\\w+)@(\\w+)".
    use ::regress::__codegen as __rt;
    static __PREFILTER: __rt::PrefilterSpec = __rt::PrefilterSpec::ReverseInner {
        literal: b"@",
        finder: __rt::FinderCache::new(),
        dfa: __rt::ReverseDfaSpec {
            start: 1,
            num_classes: 9,
            byte_to_class: &[
                0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
                0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
                0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
                1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 2, 2, 2, 2, 2, 2,
                2, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3,
                3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 4, 4, 4, 4, 5,
                6, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7,
                7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 8, 8, 8, 8, 8,
                8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8,
                8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8,
                8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8,
                8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8,
                8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8,
                8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8,
                8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8,
                8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8,
            ],
            transitions: &[
                0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 2, 0, 2, 0, 2, 0,
                2, 0, 0, 2, 0, 2, 0, 2, 0, 2, 0,
            ],
            accepting: &[false, false, true],
        },
    };
    static __GROUP_NAMES: &[&str] = &[];
    #[allow(unused_mut, unused_assignments)]
    fn __verify(
        input: &[u8],
        start: usize,
        caps: &mut [usize],
    ) -> ::core::option::Option<(usize, usize)> {
        let len = input.len();
        let mut pos = start;
        let mut m0 = usize::MAX;
        let mut m1 = usize::MAX;
        let mut m2 = usize::MAX;
        let mut m3 = usize::MAX;
        let mut m4 = usize::MAX;
        let mut m5 = usize::MAX;
        let mut acc_end = usize::MAX;
        let mut acc_state = u32::MAX;
        if start == 0 {
            m1 = pos;
            m0 = pos;
        } else {
            m1 = pos;
            m0 = pos;
            m3 = pos;
            m2 = pos;
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
                        b'0'..=b'9' => {
                            m2 = pos;
                            m4 = pos;
                            state = 2;
                        }
                        b'A'..=b'Z' => {
                            m2 = pos;
                            m5 = pos;
                            state = 2;
                        }
                        b'_' | b'a'..=b'z' => {
                            m2 = pos;
                            state = 2;
                        }
                        _ => {
                            break 'scan;
                        }
                    }
                }
                2 => {
                    let p0 = pos;
                    while pos < len && matches!(input[pos], b'0'..=b'9' | b'A'..=b'Z' | b'_' | b'a'..=b'z') {
                        pos += 1;
                    }
                    if pos != p0 {
                        m2 = pos;
                    }
                    if pos >= len {
                        break 'scan;
                    }
                    let b = input[pos];
                    pos += 1;
                    match b {
                        b'0'..=b'9' | b'A'..=b'Z' | b'_' | b'a'..=b'z' => {
                            m2 = pos;
                            state = 2;
                        }
                        b'@' => {
                            m3 = pos;
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
                        b'0'..=b'9' | b'A'..=b'Z' | b'_' | b'a'..=b'z' => {
                            m5 = pos;
                            m4 = pos;
                            state = 4;
                        }
                        _ => {
                            break 'scan;
                        }
                    }
                }
                4 => {
                    let p0 = pos;
                    while pos < len && matches!(input[pos], b'0'..=b'9' | b'A'..=b'Z' | b'_' | b'a'..=b'z') {
                        pos += 1;
                    }
                    if pos != p0 {
                        m5 = pos;
                        m4 = pos;
                    }
                    acc_end = pos;
                    acc_state = 4;
                    if pos >= len {
                        break 'scan;
                    }
                    let b = input[pos];
                    pos += 1;
                    match b {
                        b'0'..=b'9' | b'A'..=b'Z' | b'_' | b'a'..=b'z' => {
                            m5 = pos;
                            m4 = pos;
                            state = 4;
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
            4 => {
                caps[0] = m1;
                caps[1] = m2;
                caps[2] = m3;
                caps[3] = m4;
                let fs = m0;
                let fe = m5;
                ::core::option::Option::Some((
                    if fs == usize::MAX { 0 } else { fs },
                    if fe == usize::MAX { acc_end } else { fe },
                ))
            }
            _ => ::core::unreachable!(),
        }
    }
    __rt::CompiledMatcher::from_parts(&__PREFILTER, __verify, 2usize, __GROUP_NAMES)
}
