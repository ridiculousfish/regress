//! `regex!` — regress regexes compiled to Rust ahead of time.
//!
//! `regex!("pattern")` / `regex!("pattern", "flags")` runs the
//! [regress](https://docs.rs/regress) TDFA pipeline at macro-expansion time
//! and expands to unrolled Rust control flow implementing the matcher — no
//! parsing, compilation, or automaton interpretation at runtime. The
//! expression evaluates to a `CompiledMatcher` whose `find` / `find_iter` /
//! `find_from` mirror `regress::Regex` and produce `regress::Match` values.
//!
//! Users must also depend on `regress` with its `codegen` feature enabled —
//! the generated code links against `regress`'s runtime support module.
//!
//! ```toml
//! [dependencies]
//! regress = { version = "...", features = ["codegen"] }
//! regress-macro = "..."
//! ```
//!
//! ```ignore
//! use regress_macro::regex;
//!
//! static RE: regress::__codegen::CompiledMatcher = regex!(r"Sherlock\w+");
//! let re = regex!("[0-9][0-9][0-9]-[0-9][0-9][0-9][0-9]");
//! if let Some(m) = re.find("call 555-1234 now") {
//!     assert_eq!(m.range, 5..13);
//! }
//! ```
//!
//! Patterns the ahead-of-time compiler cannot handle (backreferences,
//! lookaround, word boundaries, and constructs outside the supported TDFA
//! tier) are **compile errors** — use `regress::Regex` for those. The `u`
//! (unicode) flag is always in effect. Supported flags: `i`, `m`, `s`, `u`,
//! `v`.

use proc_macro::TokenStream;
use syn::parse::{Parse, ParseStream};
use syn::{LitStr, Token};

struct RegexInput {
    pattern: LitStr,
    flags: Option<LitStr>,
}

impl Parse for RegexInput {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let pattern: LitStr = input.parse().map_err(|e| {
            syn::Error::new(
                e.span(),
                "regex! expects a string literal pattern: regex!(\"pat\") or regex!(\"pat\", \"flags\")",
            )
        })?;
        let flags = if input.peek(Token![,]) {
            input.parse::<Token![,]>()?;
            if input.is_empty() {
                None // trailing comma
            } else {
                Some(input.parse::<LitStr>().map_err(|e| {
                    syn::Error::new(e.span(), "regex! flags must be a string literal")
                })?)
            }
        } else {
            None
        };
        if !input.is_empty() {
            return Err(input.error("unexpected tokens after regex! arguments"));
        }
        Ok(RegexInput { pattern, flags })
    }
}

/// Compile a regex to Rust source at macro-expansion time. See the crate docs.
#[proc_macro]
pub fn regex(input: TokenStream) -> TokenStream {
    let input = syn::parse_macro_input!(input as RegexInput);
    let pattern = input.pattern.value();
    let flags_str = input.flags.as_ref().map(|f| f.value()).unwrap_or_default();

    // `regress::Flags::from` silently ignores unknown flag characters; a
    // compile-time macro can do better and reject typos.
    if let Some(bad) = flags_str.chars().find(|c| !matches!(c, 'i' | 'm' | 's' | 'u' | 'v')) {
        let span = input.flags.as_ref().map_or_else(|| input.pattern.span(), LitStr::span);
        return syn::Error::new(span, format!("regex!: unsupported flag `{bad}` (supported: imsuv)"))
            .to_compile_error()
            .into();
    }

    match regress::codegen::compile_to_rust(&pattern, regress::Flags::from(flags_str.as_str())) {
        Ok(src) => src
            .parse()
            .expect("regress-macro: emitted source failed to tokenize (emitter bug)"),
        Err(e) => syn::Error::new(input.pattern.span(), format!("regex!: {e}"))
            .to_compile_error()
            .into(),
    }
}
