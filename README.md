# regress - REGex in Rust with EcmaScript Syntax

oh no why

## Introduction

regress is a backtracking regular expression engine implemented in Rust, which targets JavaScript regular expression syntax. See [the crate documentation](https://docs.rs/regress) for more.

It's fast, Unicode-aware, has few dependencies, and has a big test suite. It makes fewer guarantees than the `regex` crate but it enables more syntactic features, such as backreferences and lookaround assertions.

### Fun Tools

The `regress-tool` binary can be used for some fun.

You can see how things get compiled with the `dump-phases` cli flag:

    > cargo run 'x{3,4}' 'i' --dump-phases

You can run a little benchmark too, for example:

    > cargo run --release -- 'abcd' 'i' --bench ~/3200.txt

## Want to contribute?

This was my first Rust program so no doubt there is room for improvement.

There's lots of stuff still missing, maybe you want to contribute?

### Currently Missing Features

- An API for replacing a string while substituting in capture groups (e.g. with `$1`)
- An API for escaping a string to make it a literal
- Implementing `std::str::pattern::Pattern`

### Missing Performance Optimizations

- Anchored matches like `^abc` still perform a string search. We should compute whether the whole regex is anchored, and optimize matching if so.
- Non-greedy loops like `.*?` will eagerly compute their maximum match. This doesn't affect correctness but it does mean they may match more than they should.
- Pure literal searches should use Boyer-Moore or etc.
- There are lots of vectorization opportunities.
