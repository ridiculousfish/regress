use regress::Regex;

fn main() {
    println!("Demonstrating the Unicode Syntax Lenient mode (L flag)");
    println!();

    // Example 1: Legacy octal escape
    println!("1. Legacy octal escape \\141 (represents 'a'):");

    // This works in non-unicode mode
    let re = Regex::new(r"\141").unwrap();
    println!(
        "   Non-unicode mode: Matches 'a': {}",
        re.find("a").is_some()
    );

    // This fails in strict unicode mode
    let result = Regex::with_flags(r"\141", "u");
    println!(
        "   Unicode mode: Parse result: {}",
        if result.is_ok() {
            "Success"
        } else {
            "Failed (syntax error)"
        }
    );

    // This works with the lenient flag
    let re = Regex::with_flags(r"\141", "L").unwrap();
    println!(
        "   Lenient mode (L): Matches 'a': {}",
        re.find("a").is_some()
    );

    println!();

    // Example 2: Control character escape
    println!("2. Control character escape \\cA (represents control-A, \\x01):");

    let re = Regex::new(r"\cA").unwrap();
    println!(
        "   Non-unicode mode: Matches \\x01: {}",
        re.find("\x01").is_some()
    );

    let result = Regex::with_flags(r"\cA", "u");
    println!(
        "   Unicode mode: Parse result: {}",
        if result.is_ok() {
            "Success"
        } else {
            "Failed (syntax error)"
        }
    );

    let re = Regex::with_flags(r"\cA", "L").unwrap();
    println!(
        "   Lenient mode (L): Matches \\x01: {}",
        re.find("\x01").is_some()
    );

    println!();

    // Example 3: Unicode case folding still works
    println!("3. Unicode case folding with μ (micro sign) and Μ (Greek capital mu):");

    let re = Regex::with_flags(r"μ", "i").unwrap();
    println!(
        "   Non-unicode mode + icase: Matches Μ: {}",
        re.find("Μ").is_some()
    );

    let re = Regex::with_flags(r"μ", "ui").unwrap();
    println!(
        "   Unicode mode + icase: Matches Μ: {}",
        re.find("Μ").is_some()
    );

    let re = Regex::with_flags(r"μ", "Li").unwrap();
    println!(
        "   Lenient mode (L) + icase: Matches Μ: {}",
        re.find("Μ").is_some()
    );

    println!();

    // Example 4: Unicode property escapes work with L flag
    println!("4. Unicode property escapes \\p{{L}} (matches letters):");

    let result = Regex::new(r"\p{L}");
    println!(
        "   Non-unicode mode: Parse result: {}",
        if result.is_ok() {
            "Success"
        } else {
            "Failed (no Unicode properties)"
        }
    );

    let re = Regex::with_flags(r"\p{L}", "u").unwrap();
    println!("   Unicode mode: Matches 'a': {}", re.find("a").is_some());

    let re = Regex::with_flags(r"\p{L}", "L").unwrap();
    println!(
        "   Lenient mode (L): Matches 'a': {}",
        re.find("a").is_some()
    );

    println!();

    // Example 5: Identity escapes for unrecognized characters
    println!("5. Identity escape for space (\\ ):");

    let re = Regex::new(r"\ ").unwrap();
    println!(
        "   Non-unicode mode: Matches space: {}",
        re.find(" ").is_some()
    );

    let result = Regex::with_flags(r"\ ", "u");
    println!(
        "   Unicode mode: Parse result: {}",
        if result.is_ok() {
            "Success"
        } else {
            "Failed (syntax error)"
        }
    );

    let re = Regex::with_flags(r"\ ", "L").unwrap();
    println!(
        "   Lenient mode (L): Matches space: {}",
        re.find(" ").is_some()
    );
}
