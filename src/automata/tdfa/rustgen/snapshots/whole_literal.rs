{
    // regress AoT-compiled matcher for pattern "Holmes".
    use ::regress::__codegen as __rt;
    static __PREFILTER: __rt::PrefilterSpec = __rt::PrefilterSpec::WholeLiteral {
        bytes: b"Holmes",
        finder: __rt::FinderCache::new(),
    };
    static __GROUP_NAMES: &[&str] = &[];
    fn __verify(
        _input: &[u8],
        _start: usize,
        _caps: &mut [usize],
    ) -> ::core::option::Option<(usize, usize)> {
        ::core::option::Option::None
    }
    __rt::CompiledMatcher::from_parts(&__PREFILTER, __verify, 0usize, __GROUP_NAMES)
}
