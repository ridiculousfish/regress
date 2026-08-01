{
    // regress AoT-compiled matcher for pattern "Sherlock\\w+".
    use ::regress::__codegen as __rt;
    static __PREFILTER: __rt::PrefilterSpec = __rt::PrefilterSpec::Prefix {
        predicate: __rt::PredicateSpec::ByteSeq { bytes: b"Sherlock", finder: __rt::FinderCache::new() },
        lit_window: ::core::option::Option::None,
    };
    static __GROUP_NAMES: &[&str] = &[];
    #[allow(unused_mut)]
    fn __verify(
        input: &[u8],
        start: usize,
        _caps: &mut [usize],
    ) -> ::core::option::Option<(usize, usize)> {
        let len = input.len();
        let mut acc = usize::MAX;
        let mut pos = start + 8;
        let mut state: u32 = 9;
        'scan: loop {
            match state {
                1 => {
                    if pos >= len {
                        break 'scan;
                    }
                    let b = input[pos];
                    pos += 1;
                    state = match b {
                        b'S' => 2,
                        _ => break 'scan,
                    };
                }
                2 => {
                    if pos >= len {
                        break 'scan;
                    }
                    let b = input[pos];
                    pos += 1;
                    state = match b {
                        b'h' => 3,
                        _ => break 'scan,
                    };
                }
                3 => {
                    if pos >= len {
                        break 'scan;
                    }
                    let b = input[pos];
                    pos += 1;
                    state = match b {
                        b'e' => 4,
                        _ => break 'scan,
                    };
                }
                4 => {
                    if pos >= len {
                        break 'scan;
                    }
                    let b = input[pos];
                    pos += 1;
                    state = match b {
                        b'r' => 5,
                        _ => break 'scan,
                    };
                }
                5 => {
                    if pos >= len {
                        break 'scan;
                    }
                    let b = input[pos];
                    pos += 1;
                    state = match b {
                        b'l' => 6,
                        _ => break 'scan,
                    };
                }
                6 => {
                    if pos >= len {
                        break 'scan;
                    }
                    let b = input[pos];
                    pos += 1;
                    state = match b {
                        b'o' => 7,
                        _ => break 'scan,
                    };
                }
                7 => {
                    if pos >= len {
                        break 'scan;
                    }
                    let b = input[pos];
                    pos += 1;
                    state = match b {
                        b'c' => 8,
                        _ => break 'scan,
                    };
                }
                8 => {
                    if pos >= len {
                        break 'scan;
                    }
                    let b = input[pos];
                    pos += 1;
                    state = match b {
                        b'k' => 9,
                        _ => break 'scan,
                    };
                }
                9 => {
                    if pos >= len {
                        break 'scan;
                    }
                    let b = input[pos];
                    pos += 1;
                    state = match b {
                        b'0'..=b'9' | b'A'..=b'Z' | b'_' | b'a'..=b'z' => 10,
                        _ => break 'scan,
                    };
                }
                10 => {
                    static __PEEL_FAST: __rt::ScanFast = __rt::ScanFast::AsciiRanges{count:4,pairs:[48,57,65,90,95,95,97,122],bm0:287948901175001088,bm1:576460745995190270};
                    static __PEEL_BM: [u64; 4] = [287948901175001088, 576460745995190270, 0, 0];
                    pos = __rt::scan_fast(&__PEEL_FAST, &__PEEL_BM, input, pos);
                    acc = pos;
                    if pos >= len {
                        break 'scan;
                    }
                    let b = input[pos];
                    pos += 1;
                    state = match b {
                        b'0'..=b'9' | b'A'..=b'Z' | b'_' | b'a'..=b'z' => 10,
                        _ => break 'scan,
                    };
                }
                _ => ::core::unreachable!(),
            }
        }
        if acc == usize::MAX {
            ::core::option::Option::None
        } else {
            ::core::option::Option::Some((start, acc))
        }
    }
    __rt::CompiledMatcher::from_parts(&__PREFILTER, __verify, 0usize, __GROUP_NAMES, __rt::MatcherTier::Unrolled)
}
