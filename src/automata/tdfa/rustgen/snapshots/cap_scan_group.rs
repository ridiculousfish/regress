{
    // regress AoT-compiled matcher for pattern "([a-z]+)[0-9]".
    use ::regress::__codegen as __rt;
    static __PREFILTER: __rt::PrefilterSpec = __rt::PrefilterSpec::Scan;
    static __GROUP_NAMES: &[&str] = &[];
    #[allow(unused_mut, unused_assignments)]
    fn __verify(
        input: &[u8],
        start: usize,
        caps: &mut [usize],
    ) -> ::core::option::Option<(usize, usize)> {
        static __CLASSES: [u8; 256] = [
            0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
            0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
            0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
            1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 2, 2, 2, 2, 2, 2,
            2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2,
            2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2,
            2, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3,
            3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 4, 4, 4, 4, 4,
            5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5,
            6, 6, 6, 6, 6, 6, 6, 6, 6, 6, 6, 6, 6, 6, 6, 6,
            7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7,
            7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7,
            8, 8, 9, 9, 9, 9, 9, 9, 9, 9, 9, 9, 9, 9, 9, 9,
            9, 9, 9, 9, 9, 9, 9, 9, 9, 9, 9, 9, 9, 9, 9, 9,
            10, 11, 11, 11, 11, 11, 11, 11, 11, 11, 11, 11, 11, 12, 13, 13,
            14, 15, 15, 15, 16, 17, 17, 17, 17, 17, 17, 17, 17, 17, 17, 17,
        ];
        let len = input.len();
        let mut pos = start;
        let mut m = [usize::MAX; 6];
        let mut acc_end = usize::MAX;
        let mut acc_state = u32::MAX;
        if start == 0 {
            m[0] = pos;
            m[1] = pos;
        } else {
            m[0] = pos;
            m[1] = pos;
            m[2] = pos;
            m[3] = pos;
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
                    match __CLASSES[b as usize] {
                        0 => {
                            m[0] = pos;
                            m[1] = pos;
                            m[4] = pos;
                            state = 1;
                        }
                        1 | 2 | 4 => {
                            m[0] = pos;
                            m[1] = pos;
                            state = 1;
                        }
                        3 => {
                            m[2] = pos;
                            m[3] = pos;
                            state = 2;
                        }
                        9 => {
                            state = 3;
                        }
                        10 => {
                            state = 4;
                        }
                        11 | 13 => {
                            state = 5;
                        }
                        12 => {
                            state = 6;
                        }
                        14 => {
                            state = 7;
                        }
                        15 => {
                            state = 8;
                        }
                        16 => {
                            state = 9;
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
                    match __CLASSES[b as usize] {
                        0 | 2 | 4 => {
                            m[0] = pos;
                            m[1] = pos;
                            state = 1;
                        }
                        1 => {
                            m[3] = pos;
                            state = 10;
                        }
                        3 => {
                            m[2] = pos;
                            m[4] = pos;
                            state = 11;
                        }
                        9 => {
                            state = 3;
                        }
                        10 => {
                            state = 4;
                        }
                        11 | 13 => {
                            state = 5;
                        }
                        12 => {
                            state = 6;
                        }
                        14 => {
                            state = 7;
                        }
                        15 => {
                            state = 8;
                        }
                        16 => {
                            state = 9;
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
                        0x80..=0xbf => {
                            m[0] = pos;
                            m[1] = pos;
                            state = 1;
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
                        0xa0..=0xbf => {
                            state = 3;
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
                        0x80..=0xbf => {
                            state = 3;
                        }
                        _ => {
                            break 'scan;
                        }
                    }
                }
                6 => {
                    if pos >= len {
                        break 'scan;
                    }
                    let b = input[pos];
                    pos += 1;
                    match b {
                        0x80..=0x9f => {
                            state = 3;
                        }
                        _ => {
                            break 'scan;
                        }
                    }
                }
                7 => {
                    if pos >= len {
                        break 'scan;
                    }
                    let b = input[pos];
                    pos += 1;
                    match b {
                        0x90..=0xbf => {
                            state = 5;
                        }
                        _ => {
                            break 'scan;
                        }
                    }
                }
                8 => {
                    if pos >= len {
                        break 'scan;
                    }
                    let b = input[pos];
                    pos += 1;
                    match b {
                        0x80..=0xbf => {
                            state = 5;
                        }
                        _ => {
                            break 'scan;
                        }
                    }
                }
                9 => {
                    if pos >= len {
                        break 'scan;
                    }
                    let b = input[pos];
                    pos += 1;
                    match b {
                        0x80..=0x8f => {
                            state = 5;
                        }
                        _ => {
                            break 'scan;
                        }
                    }
                }
                10 => {
                    acc_end = pos;
                    acc_state = 10;
                    break 'scan;
                }
                11 => {
                    if pos >= len {
                        break 'scan;
                    }
                    let b = input[pos];
                    pos += 1;
                    match __CLASSES[b as usize] {
                        0 | 2 | 4 => {
                            m[0] = pos;
                            m[1] = pos;
                            state = 1;
                        }
                        1 => {
                            m[3] = pos;
                            state = 10;
                        }
                        3 => {
                            m[2] = pos;
                            m[3] = m[4];
                            m[4] = pos;
                            m[5] = pos;
                            state = 11;
                        }
                        9 => {
                            state = 3;
                        }
                        10 => {
                            state = 4;
                        }
                        11 | 13 => {
                            state = 5;
                        }
                        12 => {
                            state = 6;
                        }
                        14 => {
                            state = 7;
                        }
                        15 => {
                            state = 8;
                        }
                        16 => {
                            state = 9;
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
            10 => {
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
