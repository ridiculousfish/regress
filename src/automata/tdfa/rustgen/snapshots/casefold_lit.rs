{
    // regress AoT-compiled matcher for pattern "Sherlock".
    use ::regress::__codegen as __rt;
    static __PREFILTER: __rt::PrefilterSpec = __rt::PrefilterSpec::CaseFoldLiteral {
        sets: &[b"Hh", b"Ee", b"Rr", b"Ll", b"Oo", b"Cc"],
        prefix_lo: 1,
        prefix_hi: 2,
        teddy_needles: ::core::option::Option::Some(&[b"SHER", b"SHEr", b"SHeR", b"SHer", b"ShER", b"ShEr", b"SheR", b"Sher", b"sHER", b"sHEr", b"sHeR", b"sHer", b"shER", b"shEr", b"sheR", b"sher", b"\xc5\xbfHER", b"\xc5\xbfHEr", b"\xc5\xbfHeR", b"\xc5\xbfHer", b"\xc5\xbfhER", b"\xc5\xbfhEr", b"\xc5\xbfheR", b"\xc5\xbfher"]),
        cache: __rt::CaseFoldCache::new(),
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
        let mut pos = start;
        let mut state: u32 = 1;
        'scan: loop {
            match state {
                1 => {
                    if pos >= len {
                        break 'scan;
                    }
                    let b = input[pos];
                    pos += 1;
                    state = match b {
                        b'S' | b's' => 2,
                        0xc5 => 3,
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
                        b'H' | b'h' => 4,
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
                        0xbf => 2,
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
                        b'E' | b'e' => 5,
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
                        b'R' | b'r' => 6,
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
                        b'L' | b'l' => 7,
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
                        b'O' | b'o' => 8,
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
                        b'C' | b'c' => 9,
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
                        b'K' | b'k' => 10,
                        0xe2 => 11,
                        _ => break 'scan,
                    };
                }
                10 => {
                    acc = pos;
                    break 'scan;
                }
                11 => {
                    if pos >= len {
                        break 'scan;
                    }
                    let b = input[pos];
                    pos += 1;
                    state = match b {
                        0x84 => 12,
                        _ => break 'scan,
                    };
                }
                12 => {
                    if pos >= len {
                        break 'scan;
                    }
                    let b = input[pos];
                    pos += 1;
                    state = match b {
                        0xaa => 10,
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
    __rt::CompiledMatcher::from_parts(&__PREFILTER, __verify, 0usize, __GROUP_NAMES)
}
