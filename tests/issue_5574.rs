use regress::{Flags, Regex};

#[test]
fn backtracks_across_nested_counted_repetition() {
    for no_opt in [false, true] {
        let re = Regex::with_flags(
            r"(?:(?:a+)+){2}",
            Flags {
                no_opt,
                ..Flags::default()
            },
        )
        .unwrap();
        assert_eq!(re.find("aa").map(|m| m.range()), Some(0..2));
        #[cfg(feature = "utf16")]
        assert_eq!(
            re.find_from_ucs2(&[b'a' as u16, b'a' as u16], 0)
                .next()
                .map(|m| m.range()),
            Some(0..2)
        );
    }
}
