{
    // regress AoT-compiled matcher for pattern "Sherlock Holmes$".
    use ::regress::__codegen as __rt;
    static __PREFILTER: __rt::PrefilterSpec = __rt::PrefilterSpec::Prefix {
        predicate: __rt::PredicateSpec::ByteSeq { bytes: b"Sherlock Holmes", finder: __rt::FinderCache::new() },
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
                        b' ' => 10,
                        _ => break 'scan,
                    };
                }
                10 => {
                    if pos >= len {
                        break 'scan;
                    }
                    let b = input[pos];
                    pos += 1;
                    state = match b {
                        b'H' => 11,
                        _ => break 'scan,
                    };
                }
                11 => {
                    if pos >= len {
                        break 'scan;
                    }
                    let b = input[pos];
                    pos += 1;
                    state = match b {
                        b'o' => 12,
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
                        b'l' => 13,
                        _ => break 'scan,
                    };
                }
                13 => {
                    if pos >= len {
                        break 'scan;
                    }
                    let b = input[pos];
                    pos += 1;
                    state = match b {
                        b'm' => 14,
                        _ => break 'scan,
                    };
                }
                14 => {
                    if pos >= len {
                        break 'scan;
                    }
                    let b = input[pos];
                    pos += 1;
                    state = match b {
                        b'e' => 15,
                        _ => break 'scan,
                    };
                }
                15 => {
                    if pos >= len {
                        break 'scan;
                    }
                    let b = input[pos];
                    pos += 1;
                    state = match b {
                        b's' => 16,
                        _ => break 'scan,
                    };
                }
                16 => {
                    if pos >= len {
                        acc = pos;
                    }
                    break 'scan;
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
