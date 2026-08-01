{
    // regress AoT-compiled matcher for pattern "a.{12}b".
    use ::regress::__codegen as __rt;
    static __PREFILTER: __rt::PrefilterSpec = __rt::PrefilterSpec::Prefix {
        predicate: __rt::PredicateSpec::ByteSet1([b'a']),
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
            0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 2, 2, 3, 4, 4,
            4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4,
            4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4,
            4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4,
            4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4,
            4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4,
            4, 5, 6, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7,
            7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7,
            8, 9, 9, 9, 9, 9, 9, 9, 9, 9, 9, 9, 9, 9, 9, 9,
            10, 10, 10, 10, 10, 10, 10, 10, 10, 10, 10, 10, 10, 10, 10, 10,
            11, 11, 11, 11, 11, 11, 11, 11, 12, 12, 13, 13, 13, 13, 13, 13,
            13, 13, 13, 13, 13, 13, 13, 13, 13, 13, 13, 13, 13, 13, 13, 13,
            14, 14, 15, 15, 15, 15, 15, 15, 15, 15, 15, 15, 15, 15, 15, 15,
            15, 15, 15, 15, 15, 15, 15, 15, 15, 15, 15, 15, 15, 15, 15, 15,
            16, 17, 18, 19, 19, 19, 19, 19, 19, 19, 19, 19, 19, 20, 21, 21,
            22, 23, 23, 23, 24, 25, 25, 25, 25, 25, 25, 25, 25, 25, 25, 25,
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
                        b'a' => 2,
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
                        0 | 2 | 4 | 5 | 6 | 7 => 3,
                        15 => 4,
                        16 => 5,
                        17 | 19 | 21 => 6,
                        18 => 7,
                        20 => 8,
                        22 => 9,
                        23 => 10,
                        24 => 11,
                        _ => break 'scan,
                    };
                }
                3 => {
                    if pos >= len {
                        break 'scan;
                    }
                    let b = input[pos];
                    pos += 1;
                    state = match __CLASSES[b as usize] {
                        0 | 2 | 4 | 5 | 6 | 7 => 13,
                        15 => 14,
                        16 => 15,
                        17 | 19 | 21 => 16,
                        18 => 17,
                        20 => 18,
                        22 => 19,
                        23 => 20,
                        24 => 21,
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
                        0x80..=0xbf => 3,
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
                        0xa0..=0xbf => 4,
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
                        0x80..=0xbf => 4,
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
                        0x80 => 12,
                        0x81..=0xbf => 4,
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
                        0x80..=0x9f => 4,
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
                        0x90..=0xbf => 6,
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
                        0x80..=0xbf => 6,
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
                        0x80..=0x8f => 6,
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
                        0x80..=0xa7 | 0xaa..=0xbf => 3,
                        _ => break 'scan,
                    };
                }
                13 => {
                    if pos >= len {
                        break 'scan;
                    }
                    let b = input[pos];
                    pos += 1;
                    state = match __CLASSES[b as usize] {
                        0 | 2 | 4 | 5 | 6 | 7 => 23,
                        15 => 24,
                        16 => 25,
                        17 | 19 | 21 => 26,
                        18 => 27,
                        20 => 28,
                        22 => 29,
                        23 => 30,
                        24 => 31,
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
                        0x80..=0xbf => 13,
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
                        0xa0..=0xbf => 14,
                        _ => break 'scan,
                    };
                }
                16 => {
                    if pos >= len {
                        break 'scan;
                    }
                    let b = input[pos];
                    pos += 1;
                    state = match b {
                        0x80..=0xbf => 14,
                        _ => break 'scan,
                    };
                }
                17 => {
                    if pos >= len {
                        break 'scan;
                    }
                    let b = input[pos];
                    pos += 1;
                    state = match b {
                        0x80 => 22,
                        0x81..=0xbf => 14,
                        _ => break 'scan,
                    };
                }
                18 => {
                    if pos >= len {
                        break 'scan;
                    }
                    let b = input[pos];
                    pos += 1;
                    state = match b {
                        0x80..=0x9f => 14,
                        _ => break 'scan,
                    };
                }
                19 => {
                    if pos >= len {
                        break 'scan;
                    }
                    let b = input[pos];
                    pos += 1;
                    state = match b {
                        0x90..=0xbf => 16,
                        _ => break 'scan,
                    };
                }
                20 => {
                    if pos >= len {
                        break 'scan;
                    }
                    let b = input[pos];
                    pos += 1;
                    state = match b {
                        0x80..=0xbf => 16,
                        _ => break 'scan,
                    };
                }
                21 => {
                    if pos >= len {
                        break 'scan;
                    }
                    let b = input[pos];
                    pos += 1;
                    state = match b {
                        0x80..=0x8f => 16,
                        _ => break 'scan,
                    };
                }
                22 => {
                    if pos >= len {
                        break 'scan;
                    }
                    let b = input[pos];
                    pos += 1;
                    state = match b {
                        0x80..=0xa7 | 0xaa..=0xbf => 13,
                        _ => break 'scan,
                    };
                }
                23 => {
                    if pos >= len {
                        break 'scan;
                    }
                    let b = input[pos];
                    pos += 1;
                    state = match __CLASSES[b as usize] {
                        0 | 2 | 4 | 5 | 6 | 7 => 33,
                        15 => 34,
                        16 => 35,
                        17 | 19 | 21 => 36,
                        18 => 37,
                        20 => 38,
                        22 => 39,
                        23 => 40,
                        24 => 41,
                        _ => break 'scan,
                    };
                }
                24 => {
                    if pos >= len {
                        break 'scan;
                    }
                    let b = input[pos];
                    pos += 1;
                    state = match b {
                        0x80..=0xbf => 23,
                        _ => break 'scan,
                    };
                }
                25 => {
                    if pos >= len {
                        break 'scan;
                    }
                    let b = input[pos];
                    pos += 1;
                    state = match b {
                        0xa0..=0xbf => 24,
                        _ => break 'scan,
                    };
                }
                26 => {
                    if pos >= len {
                        break 'scan;
                    }
                    let b = input[pos];
                    pos += 1;
                    state = match b {
                        0x80..=0xbf => 24,
                        _ => break 'scan,
                    };
                }
                27 => {
                    if pos >= len {
                        break 'scan;
                    }
                    let b = input[pos];
                    pos += 1;
                    state = match b {
                        0x80 => 32,
                        0x81..=0xbf => 24,
                        _ => break 'scan,
                    };
                }
                28 => {
                    if pos >= len {
                        break 'scan;
                    }
                    let b = input[pos];
                    pos += 1;
                    state = match b {
                        0x80..=0x9f => 24,
                        _ => break 'scan,
                    };
                }
                29 => {
                    if pos >= len {
                        break 'scan;
                    }
                    let b = input[pos];
                    pos += 1;
                    state = match b {
                        0x90..=0xbf => 26,
                        _ => break 'scan,
                    };
                }
                30 => {
                    if pos >= len {
                        break 'scan;
                    }
                    let b = input[pos];
                    pos += 1;
                    state = match b {
                        0x80..=0xbf => 26,
                        _ => break 'scan,
                    };
                }
                31 => {
                    if pos >= len {
                        break 'scan;
                    }
                    let b = input[pos];
                    pos += 1;
                    state = match b {
                        0x80..=0x8f => 26,
                        _ => break 'scan,
                    };
                }
                32 => {
                    if pos >= len {
                        break 'scan;
                    }
                    let b = input[pos];
                    pos += 1;
                    state = match b {
                        0x80..=0xa7 | 0xaa..=0xbf => 23,
                        _ => break 'scan,
                    };
                }
                33 => {
                    if pos >= len {
                        break 'scan;
                    }
                    let b = input[pos];
                    pos += 1;
                    state = match __CLASSES[b as usize] {
                        0 | 2 | 4 | 5 | 6 | 7 => 43,
                        15 => 44,
                        16 => 45,
                        17 | 19 | 21 => 46,
                        18 => 47,
                        20 => 48,
                        22 => 49,
                        23 => 50,
                        24 => 51,
                        _ => break 'scan,
                    };
                }
                34 => {
                    if pos >= len {
                        break 'scan;
                    }
                    let b = input[pos];
                    pos += 1;
                    state = match b {
                        0x80..=0xbf => 33,
                        _ => break 'scan,
                    };
                }
                35 => {
                    if pos >= len {
                        break 'scan;
                    }
                    let b = input[pos];
                    pos += 1;
                    state = match b {
                        0xa0..=0xbf => 34,
                        _ => break 'scan,
                    };
                }
                36 => {
                    if pos >= len {
                        break 'scan;
                    }
                    let b = input[pos];
                    pos += 1;
                    state = match b {
                        0x80..=0xbf => 34,
                        _ => break 'scan,
                    };
                }
                37 => {
                    if pos >= len {
                        break 'scan;
                    }
                    let b = input[pos];
                    pos += 1;
                    state = match b {
                        0x80 => 42,
                        0x81..=0xbf => 34,
                        _ => break 'scan,
                    };
                }
                38 => {
                    if pos >= len {
                        break 'scan;
                    }
                    let b = input[pos];
                    pos += 1;
                    state = match b {
                        0x80..=0x9f => 34,
                        _ => break 'scan,
                    };
                }
                39 => {
                    if pos >= len {
                        break 'scan;
                    }
                    let b = input[pos];
                    pos += 1;
                    state = match b {
                        0x90..=0xbf => 36,
                        _ => break 'scan,
                    };
                }
                40 => {
                    if pos >= len {
                        break 'scan;
                    }
                    let b = input[pos];
                    pos += 1;
                    state = match b {
                        0x80..=0xbf => 36,
                        _ => break 'scan,
                    };
                }
                41 => {
                    if pos >= len {
                        break 'scan;
                    }
                    let b = input[pos];
                    pos += 1;
                    state = match b {
                        0x80..=0x8f => 36,
                        _ => break 'scan,
                    };
                }
                42 => {
                    if pos >= len {
                        break 'scan;
                    }
                    let b = input[pos];
                    pos += 1;
                    state = match b {
                        0x80..=0xa7 | 0xaa..=0xbf => 33,
                        _ => break 'scan,
                    };
                }
                43 => {
                    if pos >= len {
                        break 'scan;
                    }
                    let b = input[pos];
                    pos += 1;
                    state = match __CLASSES[b as usize] {
                        0 | 2 | 4 | 5 | 6 | 7 => 53,
                        15 => 54,
                        16 => 55,
                        17 | 19 | 21 => 56,
                        18 => 57,
                        20 => 58,
                        22 => 59,
                        23 => 60,
                        24 => 61,
                        _ => break 'scan,
                    };
                }
                44 => {
                    if pos >= len {
                        break 'scan;
                    }
                    let b = input[pos];
                    pos += 1;
                    state = match b {
                        0x80..=0xbf => 43,
                        _ => break 'scan,
                    };
                }
                45 => {
                    if pos >= len {
                        break 'scan;
                    }
                    let b = input[pos];
                    pos += 1;
                    state = match b {
                        0xa0..=0xbf => 44,
                        _ => break 'scan,
                    };
                }
                46 => {
                    if pos >= len {
                        break 'scan;
                    }
                    let b = input[pos];
                    pos += 1;
                    state = match b {
                        0x80..=0xbf => 44,
                        _ => break 'scan,
                    };
                }
                47 => {
                    if pos >= len {
                        break 'scan;
                    }
                    let b = input[pos];
                    pos += 1;
                    state = match b {
                        0x80 => 52,
                        0x81..=0xbf => 44,
                        _ => break 'scan,
                    };
                }
                48 => {
                    if pos >= len {
                        break 'scan;
                    }
                    let b = input[pos];
                    pos += 1;
                    state = match b {
                        0x80..=0x9f => 44,
                        _ => break 'scan,
                    };
                }
                49 => {
                    if pos >= len {
                        break 'scan;
                    }
                    let b = input[pos];
                    pos += 1;
                    state = match b {
                        0x90..=0xbf => 46,
                        _ => break 'scan,
                    };
                }
                50 => {
                    if pos >= len {
                        break 'scan;
                    }
                    let b = input[pos];
                    pos += 1;
                    state = match b {
                        0x80..=0xbf => 46,
                        _ => break 'scan,
                    };
                }
                51 => {
                    if pos >= len {
                        break 'scan;
                    }
                    let b = input[pos];
                    pos += 1;
                    state = match b {
                        0x80..=0x8f => 46,
                        _ => break 'scan,
                    };
                }
                52 => {
                    if pos >= len {
                        break 'scan;
                    }
                    let b = input[pos];
                    pos += 1;
                    state = match b {
                        0x80..=0xa7 | 0xaa..=0xbf => 43,
                        _ => break 'scan,
                    };
                }
                53 => {
                    if pos >= len {
                        break 'scan;
                    }
                    let b = input[pos];
                    pos += 1;
                    state = match __CLASSES[b as usize] {
                        0 | 2 | 4 | 5 | 6 | 7 => 63,
                        15 => 64,
                        16 => 65,
                        17 | 19 | 21 => 66,
                        18 => 67,
                        20 => 68,
                        22 => 69,
                        23 => 70,
                        24 => 71,
                        _ => break 'scan,
                    };
                }
                54 => {
                    if pos >= len {
                        break 'scan;
                    }
                    let b = input[pos];
                    pos += 1;
                    state = match b {
                        0x80..=0xbf => 53,
                        _ => break 'scan,
                    };
                }
                55 => {
                    if pos >= len {
                        break 'scan;
                    }
                    let b = input[pos];
                    pos += 1;
                    state = match b {
                        0xa0..=0xbf => 54,
                        _ => break 'scan,
                    };
                }
                56 => {
                    if pos >= len {
                        break 'scan;
                    }
                    let b = input[pos];
                    pos += 1;
                    state = match b {
                        0x80..=0xbf => 54,
                        _ => break 'scan,
                    };
                }
                57 => {
                    if pos >= len {
                        break 'scan;
                    }
                    let b = input[pos];
                    pos += 1;
                    state = match b {
                        0x80 => 62,
                        0x81..=0xbf => 54,
                        _ => break 'scan,
                    };
                }
                58 => {
                    if pos >= len {
                        break 'scan;
                    }
                    let b = input[pos];
                    pos += 1;
                    state = match b {
                        0x80..=0x9f => 54,
                        _ => break 'scan,
                    };
                }
                59 => {
                    if pos >= len {
                        break 'scan;
                    }
                    let b = input[pos];
                    pos += 1;
                    state = match b {
                        0x90..=0xbf => 56,
                        _ => break 'scan,
                    };
                }
                60 => {
                    if pos >= len {
                        break 'scan;
                    }
                    let b = input[pos];
                    pos += 1;
                    state = match b {
                        0x80..=0xbf => 56,
                        _ => break 'scan,
                    };
                }
                61 => {
                    if pos >= len {
                        break 'scan;
                    }
                    let b = input[pos];
                    pos += 1;
                    state = match b {
                        0x80..=0x8f => 56,
                        _ => break 'scan,
                    };
                }
                62 => {
                    if pos >= len {
                        break 'scan;
                    }
                    let b = input[pos];
                    pos += 1;
                    state = match b {
                        0x80..=0xa7 | 0xaa..=0xbf => 53,
                        _ => break 'scan,
                    };
                }
                63 => {
                    if pos >= len {
                        break 'scan;
                    }
                    let b = input[pos];
                    pos += 1;
                    state = match __CLASSES[b as usize] {
                        0 | 2 | 4 | 5 | 6 | 7 => 73,
                        15 => 74,
                        16 => 75,
                        17 | 19 | 21 => 76,
                        18 => 77,
                        20 => 78,
                        22 => 79,
                        23 => 80,
                        24 => 81,
                        _ => break 'scan,
                    };
                }
                64 => {
                    if pos >= len {
                        break 'scan;
                    }
                    let b = input[pos];
                    pos += 1;
                    state = match b {
                        0x80..=0xbf => 63,
                        _ => break 'scan,
                    };
                }
                65 => {
                    if pos >= len {
                        break 'scan;
                    }
                    let b = input[pos];
                    pos += 1;
                    state = match b {
                        0xa0..=0xbf => 64,
                        _ => break 'scan,
                    };
                }
                66 => {
                    if pos >= len {
                        break 'scan;
                    }
                    let b = input[pos];
                    pos += 1;
                    state = match b {
                        0x80..=0xbf => 64,
                        _ => break 'scan,
                    };
                }
                67 => {
                    if pos >= len {
                        break 'scan;
                    }
                    let b = input[pos];
                    pos += 1;
                    state = match b {
                        0x80 => 72,
                        0x81..=0xbf => 64,
                        _ => break 'scan,
                    };
                }
                68 => {
                    if pos >= len {
                        break 'scan;
                    }
                    let b = input[pos];
                    pos += 1;
                    state = match b {
                        0x80..=0x9f => 64,
                        _ => break 'scan,
                    };
                }
                69 => {
                    if pos >= len {
                        break 'scan;
                    }
                    let b = input[pos];
                    pos += 1;
                    state = match b {
                        0x90..=0xbf => 66,
                        _ => break 'scan,
                    };
                }
                70 => {
                    if pos >= len {
                        break 'scan;
                    }
                    let b = input[pos];
                    pos += 1;
                    state = match b {
                        0x80..=0xbf => 66,
                        _ => break 'scan,
                    };
                }
                71 => {
                    if pos >= len {
                        break 'scan;
                    }
                    let b = input[pos];
                    pos += 1;
                    state = match b {
                        0x80..=0x8f => 66,
                        _ => break 'scan,
                    };
                }
                72 => {
                    if pos >= len {
                        break 'scan;
                    }
                    let b = input[pos];
                    pos += 1;
                    state = match b {
                        0x80..=0xa7 | 0xaa..=0xbf => 63,
                        _ => break 'scan,
                    };
                }
                73 => {
                    if pos >= len {
                        break 'scan;
                    }
                    let b = input[pos];
                    pos += 1;
                    state = match __CLASSES[b as usize] {
                        0 | 2 | 4 | 5 | 6 | 7 => 83,
                        15 => 84,
                        16 => 85,
                        17 | 19 | 21 => 86,
                        18 => 87,
                        20 => 88,
                        22 => 89,
                        23 => 90,
                        24 => 91,
                        _ => break 'scan,
                    };
                }
                74 => {
                    if pos >= len {
                        break 'scan;
                    }
                    let b = input[pos];
                    pos += 1;
                    state = match b {
                        0x80..=0xbf => 73,
                        _ => break 'scan,
                    };
                }
                75 => {
                    if pos >= len {
                        break 'scan;
                    }
                    let b = input[pos];
                    pos += 1;
                    state = match b {
                        0xa0..=0xbf => 74,
                        _ => break 'scan,
                    };
                }
                76 => {
                    if pos >= len {
                        break 'scan;
                    }
                    let b = input[pos];
                    pos += 1;
                    state = match b {
                        0x80..=0xbf => 74,
                        _ => break 'scan,
                    };
                }
                77 => {
                    if pos >= len {
                        break 'scan;
                    }
                    let b = input[pos];
                    pos += 1;
                    state = match b {
                        0x80 => 82,
                        0x81..=0xbf => 74,
                        _ => break 'scan,
                    };
                }
                78 => {
                    if pos >= len {
                        break 'scan;
                    }
                    let b = input[pos];
                    pos += 1;
                    state = match b {
                        0x80..=0x9f => 74,
                        _ => break 'scan,
                    };
                }
                79 => {
                    if pos >= len {
                        break 'scan;
                    }
                    let b = input[pos];
                    pos += 1;
                    state = match b {
                        0x90..=0xbf => 76,
                        _ => break 'scan,
                    };
                }
                80 => {
                    if pos >= len {
                        break 'scan;
                    }
                    let b = input[pos];
                    pos += 1;
                    state = match b {
                        0x80..=0xbf => 76,
                        _ => break 'scan,
                    };
                }
                81 => {
                    if pos >= len {
                        break 'scan;
                    }
                    let b = input[pos];
                    pos += 1;
                    state = match b {
                        0x80..=0x8f => 76,
                        _ => break 'scan,
                    };
                }
                82 => {
                    if pos >= len {
                        break 'scan;
                    }
                    let b = input[pos];
                    pos += 1;
                    state = match b {
                        0x80..=0xa7 | 0xaa..=0xbf => 73,
                        _ => break 'scan,
                    };
                }
                83 => {
                    if pos >= len {
                        break 'scan;
                    }
                    let b = input[pos];
                    pos += 1;
                    state = match __CLASSES[b as usize] {
                        0 | 2 | 4 | 5 | 6 | 7 => 93,
                        15 => 94,
                        16 => 95,
                        17 | 19 | 21 => 96,
                        18 => 97,
                        20 => 98,
                        22 => 99,
                        23 => 100,
                        24 => 101,
                        _ => break 'scan,
                    };
                }
                84 => {
                    if pos >= len {
                        break 'scan;
                    }
                    let b = input[pos];
                    pos += 1;
                    state = match b {
                        0x80..=0xbf => 83,
                        _ => break 'scan,
                    };
                }
                85 => {
                    if pos >= len {
                        break 'scan;
                    }
                    let b = input[pos];
                    pos += 1;
                    state = match b {
                        0xa0..=0xbf => 84,
                        _ => break 'scan,
                    };
                }
                86 => {
                    if pos >= len {
                        break 'scan;
                    }
                    let b = input[pos];
                    pos += 1;
                    state = match b {
                        0x80..=0xbf => 84,
                        _ => break 'scan,
                    };
                }
                87 => {
                    if pos >= len {
                        break 'scan;
                    }
                    let b = input[pos];
                    pos += 1;
                    state = match b {
                        0x80 => 92,
                        0x81..=0xbf => 84,
                        _ => break 'scan,
                    };
                }
                88 => {
                    if pos >= len {
                        break 'scan;
                    }
                    let b = input[pos];
                    pos += 1;
                    state = match b {
                        0x80..=0x9f => 84,
                        _ => break 'scan,
                    };
                }
                89 => {
                    if pos >= len {
                        break 'scan;
                    }
                    let b = input[pos];
                    pos += 1;
                    state = match b {
                        0x90..=0xbf => 86,
                        _ => break 'scan,
                    };
                }
                90 => {
                    if pos >= len {
                        break 'scan;
                    }
                    let b = input[pos];
                    pos += 1;
                    state = match b {
                        0x80..=0xbf => 86,
                        _ => break 'scan,
                    };
                }
                91 => {
                    if pos >= len {
                        break 'scan;
                    }
                    let b = input[pos];
                    pos += 1;
                    state = match b {
                        0x80..=0x8f => 86,
                        _ => break 'scan,
                    };
                }
                92 => {
                    if pos >= len {
                        break 'scan;
                    }
                    let b = input[pos];
                    pos += 1;
                    state = match b {
                        0x80..=0xa7 | 0xaa..=0xbf => 83,
                        _ => break 'scan,
                    };
                }
                93 => {
                    if pos >= len {
                        break 'scan;
                    }
                    let b = input[pos];
                    pos += 1;
                    state = match __CLASSES[b as usize] {
                        0 | 2 | 4 | 5 | 6 | 7 => 103,
                        15 => 104,
                        16 => 105,
                        17 | 19 | 21 => 106,
                        18 => 107,
                        20 => 108,
                        22 => 109,
                        23 => 110,
                        24 => 111,
                        _ => break 'scan,
                    };
                }
                94 => {
                    if pos >= len {
                        break 'scan;
                    }
                    let b = input[pos];
                    pos += 1;
                    state = match b {
                        0x80..=0xbf => 93,
                        _ => break 'scan,
                    };
                }
                95 => {
                    if pos >= len {
                        break 'scan;
                    }
                    let b = input[pos];
                    pos += 1;
                    state = match b {
                        0xa0..=0xbf => 94,
                        _ => break 'scan,
                    };
                }
                96 => {
                    if pos >= len {
                        break 'scan;
                    }
                    let b = input[pos];
                    pos += 1;
                    state = match b {
                        0x80..=0xbf => 94,
                        _ => break 'scan,
                    };
                }
                97 => {
                    if pos >= len {
                        break 'scan;
                    }
                    let b = input[pos];
                    pos += 1;
                    state = match b {
                        0x80 => 102,
                        0x81..=0xbf => 94,
                        _ => break 'scan,
                    };
                }
                98 => {
                    if pos >= len {
                        break 'scan;
                    }
                    let b = input[pos];
                    pos += 1;
                    state = match b {
                        0x80..=0x9f => 94,
                        _ => break 'scan,
                    };
                }
                99 => {
                    if pos >= len {
                        break 'scan;
                    }
                    let b = input[pos];
                    pos += 1;
                    state = match b {
                        0x90..=0xbf => 96,
                        _ => break 'scan,
                    };
                }
                100 => {
                    if pos >= len {
                        break 'scan;
                    }
                    let b = input[pos];
                    pos += 1;
                    state = match b {
                        0x80..=0xbf => 96,
                        _ => break 'scan,
                    };
                }
                101 => {
                    if pos >= len {
                        break 'scan;
                    }
                    let b = input[pos];
                    pos += 1;
                    state = match b {
                        0x80..=0x8f => 96,
                        _ => break 'scan,
                    };
                }
                102 => {
                    if pos >= len {
                        break 'scan;
                    }
                    let b = input[pos];
                    pos += 1;
                    state = match b {
                        0x80..=0xa7 | 0xaa..=0xbf => 93,
                        _ => break 'scan,
                    };
                }
                103 => {
                    if pos >= len {
                        break 'scan;
                    }
                    let b = input[pos];
                    pos += 1;
                    state = match __CLASSES[b as usize] {
                        0 | 2 | 4 | 5 | 6 | 7 => 113,
                        15 => 114,
                        16 => 115,
                        17 | 19 | 21 => 116,
                        18 => 117,
                        20 => 118,
                        22 => 119,
                        23 => 120,
                        24 => 121,
                        _ => break 'scan,
                    };
                }
                104 => {
                    if pos >= len {
                        break 'scan;
                    }
                    let b = input[pos];
                    pos += 1;
                    state = match b {
                        0x80..=0xbf => 103,
                        _ => break 'scan,
                    };
                }
                105 => {
                    if pos >= len {
                        break 'scan;
                    }
                    let b = input[pos];
                    pos += 1;
                    state = match b {
                        0xa0..=0xbf => 104,
                        _ => break 'scan,
                    };
                }
                106 => {
                    if pos >= len {
                        break 'scan;
                    }
                    let b = input[pos];
                    pos += 1;
                    state = match b {
                        0x80..=0xbf => 104,
                        _ => break 'scan,
                    };
                }
                107 => {
                    if pos >= len {
                        break 'scan;
                    }
                    let b = input[pos];
                    pos += 1;
                    state = match b {
                        0x80 => 112,
                        0x81..=0xbf => 104,
                        _ => break 'scan,
                    };
                }
                108 => {
                    if pos >= len {
                        break 'scan;
                    }
                    let b = input[pos];
                    pos += 1;
                    state = match b {
                        0x80..=0x9f => 104,
                        _ => break 'scan,
                    };
                }
                109 => {
                    if pos >= len {
                        break 'scan;
                    }
                    let b = input[pos];
                    pos += 1;
                    state = match b {
                        0x90..=0xbf => 106,
                        _ => break 'scan,
                    };
                }
                110 => {
                    if pos >= len {
                        break 'scan;
                    }
                    let b = input[pos];
                    pos += 1;
                    state = match b {
                        0x80..=0xbf => 106,
                        _ => break 'scan,
                    };
                }
                111 => {
                    if pos >= len {
                        break 'scan;
                    }
                    let b = input[pos];
                    pos += 1;
                    state = match b {
                        0x80..=0x8f => 106,
                        _ => break 'scan,
                    };
                }
                112 => {
                    if pos >= len {
                        break 'scan;
                    }
                    let b = input[pos];
                    pos += 1;
                    state = match b {
                        0x80..=0xa7 | 0xaa..=0xbf => 103,
                        _ => break 'scan,
                    };
                }
                113 => {
                    if pos >= len {
                        break 'scan;
                    }
                    let b = input[pos];
                    pos += 1;
                    state = match b {
                        b'b' => 123,
                        _ => break 'scan,
                    };
                }
                114 => {
                    if pos >= len {
                        break 'scan;
                    }
                    let b = input[pos];
                    pos += 1;
                    state = match b {
                        0x80..=0xbf => 113,
                        _ => break 'scan,
                    };
                }
                115 => {
                    if pos >= len {
                        break 'scan;
                    }
                    let b = input[pos];
                    pos += 1;
                    state = match b {
                        0xa0..=0xbf => 114,
                        _ => break 'scan,
                    };
                }
                116 => {
                    if pos >= len {
                        break 'scan;
                    }
                    let b = input[pos];
                    pos += 1;
                    state = match b {
                        0x80..=0xbf => 114,
                        _ => break 'scan,
                    };
                }
                117 => {
                    if pos >= len {
                        break 'scan;
                    }
                    let b = input[pos];
                    pos += 1;
                    state = match b {
                        0x80 => 122,
                        0x81..=0xbf => 114,
                        _ => break 'scan,
                    };
                }
                118 => {
                    if pos >= len {
                        break 'scan;
                    }
                    let b = input[pos];
                    pos += 1;
                    state = match b {
                        0x80..=0x9f => 114,
                        _ => break 'scan,
                    };
                }
                119 => {
                    if pos >= len {
                        break 'scan;
                    }
                    let b = input[pos];
                    pos += 1;
                    state = match b {
                        0x90..=0xbf => 116,
                        _ => break 'scan,
                    };
                }
                120 => {
                    if pos >= len {
                        break 'scan;
                    }
                    let b = input[pos];
                    pos += 1;
                    state = match b {
                        0x80..=0xbf => 116,
                        _ => break 'scan,
                    };
                }
                121 => {
                    if pos >= len {
                        break 'scan;
                    }
                    let b = input[pos];
                    pos += 1;
                    state = match b {
                        0x80..=0x8f => 116,
                        _ => break 'scan,
                    };
                }
                122 => {
                    if pos >= len {
                        break 'scan;
                    }
                    let b = input[pos];
                    pos += 1;
                    state = match b {
                        0x80..=0xa7 | 0xaa..=0xbf => 113,
                        _ => break 'scan,
                    };
                }
                123 => {
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
    __rt::CompiledMatcher::from_parts(&__PREFILTER, __verify, 0usize, __GROUP_NAMES, __rt::MatcherTier::Unrolled)
}
