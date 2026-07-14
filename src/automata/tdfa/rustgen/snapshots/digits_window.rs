{
    // regress AoT-compiled matcher for pattern "[0-9][0-9][0-9]-[0-9][0-9][0-9][0-9]".
    use ::regress::__codegen as __rt;
    static __PREFILTER: __rt::PrefilterSpec = __rt::PrefilterSpec::Prefix {
        predicate: __rt::PredicateSpec::ByteBracket([0x0000, 0x0000, 0x0000, 0x03ff, 0x0000, 0x0000, 0x0000, 0x0000, 0x0000, 0x0000, 0x0000, 0x0000, 0x0000, 0x0000, 0x0000, 0x0000]),
        lit_window: ::core::option::Option::Some(__rt::LitWindowSpec { byte: b'-', lo: 3, hi: 3 }),
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
        let mut pos = start + 1;
        let mut state: u32 = 2;
        'scan: loop {
            match state {
                1 => {
                    if pos >= len {
                        break 'scan;
                    }
                    let b = input[pos];
                    pos += 1;
                    state = match b {
                        b'0'..=b'9' => 2,
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
                        b'0'..=b'9' => 3,
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
                        b'0'..=b'9' => 4,
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
                        b'-' => 5,
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
                        b'0'..=b'9' => 6,
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
                        b'0'..=b'9' => 7,
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
                        b'0'..=b'9' => 8,
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
                        b'0'..=b'9' => 9,
                        _ => break 'scan,
                    };
                }
                9 => {
                    acc = pos;
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
