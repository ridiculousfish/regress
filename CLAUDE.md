# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

regress is a backtracking regular expression engine implemented in Rust that targets EcmaScript (JavaScript) regular expression syntax. It supports advanced features like backreferences, lookaround assertions, and variable-width lookbehind assertions with capture groups.

## Development Commands

### Building and Testing
- `cargo build` - Build the library
- `cargo build --release` - Build optimized release version
- `cargo test` - Run all tests
- `cargo test <test_name>` - Run specific test containing the name
- `cargo test --release` - Run tests in release mode for performance testing

### Tools
- `cargo run --bin regress-tool <pattern> <flags>` - Test patterns with the regress-tool
- `cargo run --bin regress-tool <pattern> --dump-phases` - Show compilation phases
- `cargo run --release --bin regress-tool <pattern> --bench <file>` - Benchmark against text file

### Workspace Structure
This is a Cargo workspace with three members:
- Main `regress` crate (the regex engine)
- `regress-tool` - CLI tool for testing and benchmarking
- `gen-unicode` - Unicode table generation utility

## Architecture Overview

### Core Components
- **Parser** (`parse.rs`) - Converts regex strings to intermediate representation (IR)
- **IR** (`ir.rs`) - Intermediate representation of regex patterns
- **Optimizer** (`optimizer.rs`) - Optimizes IR for better performance
- **Automata** (`automata/`) - Finite state machine implementations:
  - `nfa.rs` - Non-deterministic finite automaton
  - `dfa.rs` - Deterministic finite automaton
- **Execution Engines**:
  - **Classical Backtrack** (`classicalbacktrack.rs`) - Default backtracking engine; also the correctness oracle the other backends are cross-checked against
  - **PikeVM** (`pikevm.rs`) - Non-backtracking Thompson NFA simulation (enabled by the `backend-pikevm` default feature)
  - **Automata backends** (`automata/`) - Experimental NFA/DFA/TDFA engines behind the `nfa` feature, not yet on the public match path. See `src/automata/CLAUDE.md` for the map, plus the optional native-code `tdfa-jit` backend.

### Key Features
- **Multiple Input Formats**: UTF-8 (default), ASCII, UTF-16, UCS-2
- **Unicode Support**: Full Unicode property support via generated tables
- **EcmaScript Compliance**: Handles quirks like the 'u' flag behavior
- **Character Classes** (`charclasses.rs`) - Efficient character set matching
- **Capture Groups** - Full support including backreferences

### Testing
- Extensive test suite in `tests/` directory covering:
  - Pattern matching (`pattern_tests.rs`)
  - PCRE compatibility (`pcre_tests.rs`)
  - Unicode properties (`unicode_property_escapes.rs`)
  - Syntax errors (`syntax_error_tests.rs`)
  - Replacement operations (`replacement_tests.rs`)

### Features and Backends
- Default features: `["backend-pikevm", "std", "nfa"]`
- Optional UTF-16 support with `utf16` feature
- Experimental automata backends behind `nfa` (default-on); optional native-code TDFA via `tdfa-jit` (requires `std`, incompatible with `prohibit-unsafe`)
- Pattern trait implementation with `pattern` feature (nightly only)
- Safety options: `prohibit-unsafe`, `index-positions`

Use `cargo test` to run the comprehensive test suite that validates EcmaScript regex compatibility.