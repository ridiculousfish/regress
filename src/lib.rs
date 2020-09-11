/*!

# regress - REGex in Rust with EcmaScript Syntax

This crate provides a regular expression engine which targets EcmaScript (aka JavaScript) regular expression syntax.

# Example: test if a string contains a match

```rust
use regress::Regex;
let re = Regex::new(r"\d{4}").unwrap();
let matched = re.find("2020-20-05").is_some();
assert!(matched);
```

# Example: iterating over matches

Here we use a backreference to find doubled characters:

```rust
use regress::Regex;
let re = Regex::new(r"(\w)\1").unwrap();
let text = "Frankly, Miss Piggy, I don't give a hoot!";
for m in re.find_iter(text) {
    println!("{}", &text[m.total()])
}
// Output: ss
// Output: gg
// Output: oo

```

# Example: using capture groups

Capture groups are available in the `Match` object produced by a successful match.
A capture group is a range of byte indexes into the original string.

```rust
use regress::Regex;
let re = Regex::new(r"(\d{4})").unwrap();
let text = "Today is 2020-20-05";
let m = re.find(text).unwrap();
let group = m.group(1).unwrap();
println!("Year: {}", &text[group]);
// Output: Year: 2020
```

# Supported Syntax

regress targets ES 2018 syntax. You can refer to the many resources about JavaScript regex syntax.

There are some features which have yet to be implemented:

- Named capture groups like `(?<count>\d+)`
- Named character classes liks `[[:alpha:]]`
- Unicode property escapes like `\p{Sc}`

Note the parser assumes the `u` (Unicode) flag, as the non-Unicode path is tied to JS's UCS-2 string encoding and the semantics cannot be usefully expressed in Rust.

# Unicode remarks

regress supports Unicode case folding. For example:

```rust
use regress::{Regex, Flags};
let re = Regex::with_flags("\u{00B5}", Flags::from("i")).unwrap();
assert!(re.find("\u{03BC}").is_some());
```

Here the U+00B5 (micro sign) was case-insensitively matched against U+03BC (small letter mu).

regress does NOT perform normalization. For example,  e-with-accute-accent can be precomposed or decomposed, and these are treated as not equivalent:

```rust
use regress::{Regex, Flags};
let re = Regex::new("\u{00E9}").unwrap();
assert!(re.find("\u{0065}\u{0301}").is_none());
```

This agrees with JavaScript semantics. Perform any required normalization before regex matching.

## Ascii matching

regress has an "ASCII mode" which treats each 8-bit quantity as a separate character.
This may provide improved performance if you do not need Unicode semantics, because it can avoid decoding UTF-8 and has simpler (ASCII-only) case-folding.

Example:

```rust
use regress::{Flags, Regex};
let re = Regex::with_flags("BC", Flags::from("i")).unwrap();
assert!(re.find("abcd").is_some());
```


# Comparison to regex crate

regress supports features that regex does not, in particular backreferences and zero-width lookaround assertions.
However the regex crate provides linear-time matching guarantees, while regress does not. This difference is due
to the architecture: regex uses finite automata while regress uses "classical backtracking."

# Comparison to fancy-regex crate

fancy-regex wraps the regex crate and extends it with PCRE-style syntactic features. regress has more complete support for these features: backreferences may be case-insensitive, and lookbehinds may be arbitrary-width.

# Architecture

regress has a parser, intermediate representation, optimizer which acts on the IR, bytecode emitter, and two bytecode interpreters, referred to as "backends".

The major interpreter is the "classical backtracking" which uses an explicit backtracking stack, similar to JS implementations. There is also the "PikeVM" pseudo-toy backend which is mainly used for testing and verification.
*/

#![warn(clippy::all)]

pub use crate::api::*;

mod api;
mod bytesearch;
mod charclasses;
mod classicalbacktrack;
mod codepointset;
mod cursor;
mod emit;
mod exec;
mod folds;
mod foldtable;
mod indexing;
mod insn;
mod ir;
mod matchers;
mod optimizer;
mod parse;
mod scm;
mod startpredicate;
mod types;
mod util;

#[cfg(feature = "backend-pikevm")]
mod pikevm;
