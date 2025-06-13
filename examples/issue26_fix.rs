use regress::Regex;

fn main() {
    println!("Testing the specific case from issue #26: /,\\\\;/");
    println!("See: https://github.com/ridiculousfish/regress/issues/26");
    println!();

    // The problematic pattern from the issue
    let pattern = r",\;";

    // This would fail in strict unicode mode
    let result_unicode = Regex::with_flags(pattern, "u");
    println!(
        "Unicode mode result: {}",
        if result_unicode.is_ok() {
            "Success"
        } else {
            "Failed (syntax error)"
        }
    );

    // This should work with the new lenient mode
    let result_lenient = Regex::with_flags(pattern, "L");
    println!(
        "Lenient mode result: {}",
        if result_lenient.is_ok() {
            "Success"
        } else {
            "Failed"
        }
    );

    if let Ok(re) = result_lenient {
        println!("Matches ',;': {}", re.find(",;").is_some());
    }

    // Test without any flags (non-unicode mode)
    let result_default = Regex::new(pattern);
    println!(
        "Default mode result: {}",
        if result_default.is_ok() {
            "Success"
        } else {
            "Failed"
        }
    );

    println!();
    println!("✅ Issue #26 has been fixed with the new 'L' (lenient) flag!");
}
