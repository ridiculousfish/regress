{
    // regress AoT-compiled matcher for pattern "T[aeiou0-9%#!=<>@]+".
    use ::regress::__codegen as __rt;
    static __PREFILTER: __rt::PrefilterSpec = __rt::PrefilterSpec::Prefix {
        predicate: __rt::PredicateSpec::ByteSet1([b'T']),
        lit_window: ::core::option::Option::None,
    };
    static __GROUP_NAMES: &[&str] = &[];
    #[allow(unused_mut)]
    fn __verify(
        input: &[u8],
        start: usize,
        _caps: &mut [usize],
    ) -> ::core::option::Option<(usize, usize)> {
        static __CLASSES: [u8; 256] = [
            0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
            0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
            0, 1, 2, 3, 4, 5, 6, 6, 6, 6, 6, 6, 6, 6, 6, 6,
            7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 8, 8, 9, 9, 9, 10,
            11, 12, 12, 12, 12, 12, 12, 12, 12, 12, 12, 12, 12, 12, 12, 12,
            12, 12, 12, 12, 13, 14, 14, 14, 14, 14, 14, 14, 14, 14, 14, 14,
            14, 15, 16, 16, 16, 17, 18, 18, 18, 19, 20, 20, 20, 20, 20, 21,
            22, 22, 22, 22, 22, 23, 24, 24, 24, 24, 24, 24, 24, 24, 24, 24,
            24, 24, 24, 24, 24, 24, 24, 24, 24, 24, 24, 24, 24, 24, 24, 24,
            24, 24, 24, 24, 24, 24, 24, 24, 24, 24, 24, 24, 24, 24, 24, 24,
            24, 24, 24, 24, 24, 24, 24, 24, 24, 24, 24, 24, 24, 24, 24, 24,
            24, 24, 24, 24, 24, 24, 24, 24, 24, 24, 24, 24, 24, 24, 24, 24,
            24, 24, 24, 24, 24, 24, 24, 24, 24, 24, 24, 24, 24, 24, 24, 24,
            24, 24, 24, 24, 24, 24, 24, 24, 24, 24, 24, 24, 24, 24, 24, 24,
            24, 24, 24, 24, 24, 24, 24, 24, 24, 24, 24, 24, 24, 24, 24, 24,
            24, 24, 24, 24, 24, 24, 24, 24, 24, 24, 24, 24, 24, 24, 24, 24,
        ];
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
                        b'T' => 2,
                        _ => break 'scan,
                    };
                }
                2 => {
                    if pos >= len {
                        break 'scan;
                    }
                    let b = input[pos];
                    pos += 1;
                    state = match __CLASSES[b as usize] {
                        1 | 3 | 5 | 7 | 9 | 11 | 15 | 17 | 19 | 21 | 23 => 3,
                        _ => break 'scan,
                    };
                }
                3 => {
                    acc = pos;
                    if pos >= len {
                        break 'scan;
                    }
                    let b = input[pos];
                    pos += 1;
                    state = match __CLASSES[b as usize] {
                        1 | 3 | 5 | 7 | 9 | 11 | 15 | 17 | 19 | 21 | 23 => 3,
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
