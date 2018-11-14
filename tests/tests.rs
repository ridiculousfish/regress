// Work around dead code warnings: rust-lang issue #46379
pub mod common;

// Work around dead code warnings: rust-lang issue #46379
use common::*;

fn test_zero_length_matches_tc(tc: TestConfig) {
    tc.compile(".*?").match_all("a").test_eq(vec!["", ""]);
    tc.compile(".*?")
        .match_all("\u{0251}")
        .test_eq(vec!["", ""]);
}

#[test]
fn test_zero_length_matches() {
    test_with_configs(test_zero_length_matches_tc)
}

fn non_matching_captures_tc(tc: TestConfig) {
    let re_match = tc
        .compile("aa(b)?aa")
        .find("aaaa")
        .expect("Should have matched");
    assert!(
        re_match.captures.len() == 1,
        "Capture list should have size 1"
    );
    assert!(
        re_match.captures[0] == None,
        "Captured value should be missing"
    );
}

#[test]
fn non_matching_captures() {
    test_with_configs(non_matching_captures_tc)
}

fn test_multiline_tc(tc: TestConfig) {
    tc.compilef(r"^abc", "").match1f("abc").test_eq("abc");
    tc.compile(r"^def").test_fails("abc\ndef");
    tc.compilef(r"^def", "m").match1f("abc\ndef").test_eq("def");
    tc.compilef(r"^def", "m")
        .match1f("abc\n\rdef")
        .test_eq("def");

    tc.compile(r"(a*)^(a*)$").test_fails("aa\raaa");
    tc.compilef(r"(a*)^(a*)$", "m")
        .match1f("aa\raaa")
        .test_eq("aa,,aa");
    tc.compilef(r"[ab]$", "").match1f("a\rb").test_eq("b");
    tc.compilef(r"[ab]$", "m").match1f("a\rb").test_eq("a");

    tc.compilef(r"^\d", "m")
        .match_all("aaa\n789\r\nccc\r\n345")
        .test_eq(vec!["7", "3"]);
    tc.compilef(r"\d$", "m")
        .match_all("aaa789\n789\r\nccc10\r\n345")
        .test_eq(vec!["9", "9", "0", "5"]);
}

#[test]
fn test_multiline() {
    test_with_configs(test_multiline_tc)
}

fn test_lookbehinds_tc(tc: TestConfig) {
    tc.compilef(r"(?<=efg)..", "")
        .match1f("abcdefghijk123456")
        .test_eq("hi");
    tc.compilef(r"(?<=\d{3}).*", "")
        .match1f("abcdefghijk123456")
        .test_eq("456");
    tc.test_match_succeeds(r"(?<=\d{3}.*)", "", "abcdefghijk123456");
    tc.compilef(r"(?<![a-z])..", "")
        .match1f("abcdefghijk123456")
        .test_eq("ab");
    tc.compilef(r"(?<![a-z])\d{2}", "")
        .match1f("abcdefghijk123456")
        .test_eq("23");
    tc.compilef(r"(?<=x{3,4})\d", "")
        .match1f("1yxx2xxx3xxxx4xxxxx5xxxxxx6xxxxxxx7xxxxxxxx8")
        .test_eq("3");
    tc.compilef(r"(?<=(?:xx){3})\d", "")
        .match1f("1yxx2xxx3xxxx4xxxxx5xxxxxx6xxxxxxx7xxxxxxxx8")
        .test_eq("6");
    tc.compilef(r"(?<=(x*))\1$", "")
        .match1f("xxxxxxxx")
        .test_eq("xxxx,xxxx");
    tc.test_match_fails(r"(?<!(x*))\1$", "", "xxxxxxxx");
    tc.compilef(r"(?<!$ab)\d", "")
        .match1f("ab1ab2")
        .test_eq("1");
    tc.compilef(r"(?<!^ab)\d", "")
        .match1f("ab1ab2")
        .test_eq("2");

    tc.compilef(r"(?<=x)y", "")
        .match_all_from("xyxy", 1)
        .into_iter()
        .map(|r| format!("{}..{}", r.start, r.end))
        .collect::<Vec<_>>()
        .join(",")
        .test_eq("1..2,3..4");
}

#[test]
fn test_lookbehinds() {
    test_with_configs(test_lookbehinds_tc)
}

#[test]
fn test_lookbehinds_mjsunit() {
    test_with_configs(test_lookbehinds_mjsunit_tc)
}

#[cfg_attr(rustfmt, rustfmt_skip)]
fn test_lookbehinds_mjsunit_tc(tc: TestConfig) {
    // alternations.js
    tc.compilef(r".*(?<=(..|...|....))(.*)", "").match1f("xabcd").test_eq("xabcd,cd,");
    tc.compilef(r".*(?<=(xx|...|....))(.*)", "").match1f("xabcd").test_eq("xabcd,bcd,");
    tc.compilef(r".*(?<=(xx|...))(.*)", "").match1f("xxabcd").test_eq("xxabcd,bcd,");
    tc.compilef(r".*(?<=(xx|xxx))(.*)", "").match1f("xxabcd").test_eq("xxabcd,xx,abcd");

    // back-references-to-captures.js
    tc.compilef(r"(?<=\1(\w))d", "i").match1f("abcCd").test_eq("d,C");
    tc.compilef(r"(?<=\1([abx]))d", "").match1f("abxxd").test_eq("d,x");
    tc.compilef(r"(?<=\1(\w+))c", "").match1f("ababc").test_eq("c,ab");
    tc.compilef(r"(?<=\1(\w+))c", "i").match1f("ababc").test_eq("c,ab");
    tc.compilef(r"(?<=\1(\w+))c", "").match1f("ababbc").test_eq("c,b");
    tc.test_match_fails(r"(?<=\1(\w+))c", "", "ababdc");
    tc.compilef(r"(?<=(\w+)\1)c", "").match1f("ababc").test_eq("c,abab");

    // back-references.js
    tc.compilef("(.)(?<=(\\1\\1))", "").match1f("abb").test_eq("b,b,bb");
    tc.compilef("(.)(?<=(\\1\\1))", "i").match1f("abB").test_eq("B,B,bB");
    tc.compilef("((\\w)\\w)(?<=\\1\\2\\1)", "i").match1f("aabAaBa").test_eq("aB,aB,a");
    tc.compilef("(\\w(\\w))(?<=\\1\\2\\1)", "i").match1f("aabAaBa").test_eq("Ba,Ba,a");
    tc.compilef("(?=(\\w))(?<=(\\1)).", "i").match1f("abaBbAa").test_eq("b,b,B");
    tc.compilef("(?<=(.))(\\w+)(?=\\1)", "").match1f("  'foo'  ").test_eq("foo,',foo");
    tc.compilef("(?<=(.))(\\w+)(?=\\1)", "").match1f("  \"foo\"  ").test_eq("foo,\",foo");
    tc.compilef("(.)(?<=\\1\\1\\1)", "").match1f("abbb").test_eq("b,b");
    tc.compilef("(..)(?<=\\1\\1\\1)", "").match1f("fababab").test_eq("ab,ab");
    tc.compilef("(?<=(.))(\\w+)(?=\\1)", "").test_fails("  .foo\"  ");
    tc.compilef("(.)(?<=\\1\\1\\1)", "").test_fails("ab");
    tc.compilef("(.)(?<=\\1\\1\\1)", "").test_fails("abb");
    tc.compilef("(..)(?<=\\1\\1\\1)", "").test_fails("ab");
    tc.compilef("(..)(?<=\\1\\1\\1)", "").test_fails("abb");
    tc.compilef("(..)(?<=\\1\\1\\1)", "").test_fails("aabb");
    tc.compilef("(..)(?<=\\1\\1\\1)", "").test_fails("abab");
    tc.compilef("(..)(?<=\\1\\1\\1)", "").test_fails("fabxbab");
    tc.compilef("(..)(?<=\\1\\1\\1)", "").test_fails("faxabab");

    // do-not-backtrack.js
    tc.compilef("(?<=([abc]+)).\\1", "").test_fails("abcdbc");

    // greedy-loop.js
    tc.compilef("(?<=(b+))c", "").match1f("abbbbbbc").test_eq("c,bbbbbb");
    tc.compilef("(?<=(b\\d+))c", "").match1f("ab1234c").test_eq("c,b1234");
    tc.compilef("(?<=((?:b\\d{2})+))c", "").match1f("ab12b23b34c").test_eq("c,b12b23b34");

    // misc.js
    tc.compilef("(?<=$abc)def", "").test_fails("abcdef");
    tc.compilef("^f.o(?<=foo)$", "").test_fails("fno");
    tc.compilef("^foo(?<!foo)$", "").test_fails("foo");
    tc.compilef("^f.o(?<!foo)$", "").test_fails("foo");
    tc.compilef("^foo(?<=foo)$", "").match1f("foo").test_eq("foo");
    tc.compilef("^f.o(?<=foo)$", "").match1f("foo").test_eq("foo");
    tc.compilef("^f.o(?<!foo)$", "").match1f("fno").test_eq("fno");
    tc.compilef("^foooo(?<=fo+)$", "").match1f("foooo").test_eq("foooo");
    tc.compilef("^foooo(?<=fo*)$", "").match1f("foooo").test_eq("foooo");
    tc.compilef("(abc\\1)", "").match1f("abc").test_eq("abc,abc");
    tc.compilef("(abc\\1)", "").match1f("abc\u{1234}").test_eq("abc,abc");
    tc.compilef("(abc\\1)", "i").match1f("abc").test_eq("abc,abc");
    tc.compilef("(abc\\1)", "i").match1f("abc\u{1234}").test_eq("abc,abc");

    // mutual-recursive.js
    tc.compilef("(?<=a(.\\2)b(\\1)).{4}", "").match1f("aabcacbc").test_eq("cacb,a,");
    tc.compilef("(?<=a(\\2)b(..\\1))b", "").match1f("aacbacb").test_eq("b,ac,ac");
    tc.compilef("(?<=(?:\\1b)(aa)).", "").match1f("aabaax").test_eq("x,aa");
    tc.compilef("(?<=(?:\\1|b)(aa)).", "").match1f("aaaax").test_eq("x,aa");

    // negative.js
    tc.compilef("(?<!abc)\\w\\w\\w", "").match1f("abcdef").test_eq("abc");
    tc.compilef("(?<!a.c)\\w\\w\\w", "").match1f("abcdef").test_eq("abc");
    tc.compilef("(?<!a\\wc)\\w\\w\\w", "").match1f("abcdef").test_eq("abc");
    tc.compilef("(?<!a[a-z])\\w\\w\\w", "").match1f("abcdef").test_eq("abc");
    tc.compilef("(?<!a[a-z]{2})\\w\\w\\w", "").match1f("abcdef").test_eq("abc");
    tc.compilef("(?<!abc)def", "").test_fails("abcdef");
    tc.compilef("(?<!a.c)def", "").test_fails("abcdef");
    tc.compilef("(?<!a\\wc)def", "").test_fails("abcdef");
    tc.compilef("(?<!a[a-z][a-z])def", "").test_fails("abcdef");
    tc.compilef("(?<!a[a-z]{2})def", "").test_fails("abcdef");
    tc.compilef("(?<!a{1}b{1})cde", "").test_fails("abcdef");
    tc.compilef("(?<!a{1}[a-z]{2})def", "").test_fails("abcdef");

    // nested-lookaround.js
    tc.compilef("(?<=ab(?=c)\\wd)\\w\\w", "").match1f("abcdef").test_eq("ef");
    tc.compilef("(?<=a(?=([^a]{2})d)\\w{3})\\w\\w", "").match1f("abcdef").test_eq("ef,bc");
    tc.compilef("(?<=a(?=([bc]{2}(?<!a{2}))d)\\w{3})\\w\\w", "").match1f("abcdef").test_eq("ef,bc");
    tc.compilef("^faaao?(?<=^f[oa]+(?=o))", "").match1f("faaao").test_eq("faaa");
    tc.compilef("(?<=a(?=([bc]{2}(?<!a*))d)\\w{3})\\w\\w", "").test_fails("abcdef");

    // simple-fixed-length.js
    tc.compilef("^.(?<=a)", "").test_fails("b");
    tc.compilef("^f\\w\\w(?<=\\woo)", "").test_fails("boo");
    tc.compilef("^f\\w\\w(?<=\\woo)", "").test_fails("fao");
    tc.compilef("^f\\w\\w(?<=\\woo)", "").test_fails("foa");
    tc.compilef("^.(?<=a)", "").match1f("a").test_eq("a");
    tc.compilef("^f..(?<=.oo)", "").match1f("foo1").test_eq("foo");
    tc.compilef("^f\\w\\w(?<=\\woo)", "").match1f("foo2").test_eq("foo");
    tc.compilef("(?<=abc)\\w\\w\\w", "").match1f("abcdef").test_eq("def");
    tc.compilef("(?<=a.c)\\w\\w\\w", "").match1f("abcdef").test_eq("def");
    tc.compilef("(?<=a\\wc)\\w\\w\\w", "").match1f("abcdef").test_eq("def");
    tc.compilef("(?<=a[a-z])\\w\\w\\w", "").match1f("abcdef").test_eq("cde");
    tc.compilef("(?<=a[a-z][a-z])\\w\\w\\w", "").match1f("abcdef").test_eq("def");
    tc.compilef("(?<=a[a-z]{2})\\w\\w\\w", "").match1f("abcdef").test_eq("def");
    tc.compilef("(?<=a{1})\\w\\w\\w", "").match1f("abcdef").test_eq("bcd");
    tc.compilef("(?<=a{1}b{1})\\w\\w\\w", "").match1f("abcdef").test_eq("cde");
    tc.compilef("(?<=a{1}[a-z]{2})\\w\\w\\w", "").match1f("abcdef").test_eq("def");

    // start-of-line.js
    tc.compilef("(?<=^[^a-c]{3})def", "").test_fails("abcdef");
    tc.compilef("\"^foooo(?<=^o+)$", "").test_fails("foooo");
    tc.compilef("\"^foooo(?<=^o*)$", "").test_fails("foooo");
    tc.compilef("(?<=^abc)def", "").match1f("abcdef").test_eq("def");
    tc.compilef("(?<=^[a-c]{3})def", "").match1f("abcdef").test_eq("def");
    tc.compilef("(?<=^[a-c]{3})def", "m").match1f("xyz\nabcdef").test_eq("def");
    tc.compilef("(?<=^)\\w+", "m").run_global_match("ab\ncd\nefg").test_eq("ab,cd,efg");
    tc.compilef("\\w+(?<=$)", "m").run_global_match("ab\ncd\nefg").test_eq("ab,cd,efg");
    tc.compilef("(?<=^)\\w+(?<=$)", "m").run_global_match("ab\ncd\nefg").test_eq("ab,cd,efg");
    tc.compilef("^foo(?<=^fo+)$", "").match1f("foo").test_eq("foo");
    tc.compilef("^foooo(?<=^fo*)", "").match1f("foooo").test_eq("foooo");
    tc.compilef("^(f)oo(?<=^\\1o+)$", "").match1f("foo").test_eq("foo,f");
    tc.compilef("^(f)oo(?<=^\\1o+)$", "i").match1f("foo").test_eq("foo,f");
    tc.compilef("^(f)oo(?<=^\\1o+).$", "i").match1f("foo\u{1234}").test_eq("foo\u{1234},f");
    tc.compilef("(?<=^\\w+)def", "").match1f("abcdefdef").test_eq("def");
    tc.compilef("(?<=^\\w+)def", "").run_global_match("abcdefdef").test_eq("def,def");

    // variable-length.js
    tc.compilef("(?<=[a|b|c]*)[^a|b|c]{3}", "").match1f("abcdef").test_eq("def");
    tc.compilef("(?<=\\w*)[^a|b|c]{3}", "").match1f("abcdef").test_eq("def");

    // word-boundary.js
    tc.compilef("(?<=\\b)[d-f]{3}", "").match1f("abc def").test_eq("def");
    tc.compilef("(?<=\\B)\\w{3}", "").match1f("ab cdef").test_eq("def");
    tc.compilef("(?<=\\B)(?<=c(?<=\\w))\\w{3}", "").match1f("ab cdef").test_eq("def");
    tc.compilef("(?<=\\b)[d-f]{3}", "").test_fails("abcdef");
}

#[test]
fn run_misc_tests() {
    test_with_configs(run_misc_tests_tc)
}

fn run_misc_tests_tc(tc: TestConfig) {
    tc.compilef(r"(a+)(?!(\1))", "")
        .match1f("aaaaaa")
        .test_eq("aaaaaa,aaaaaa,");
    tc.compilef(r"\1(a)", "").match1f("aa").test_eq("a,a"); // see 15.10.2.11_A1_T5 from test262
    tc.compilef(r"((a)|(b))*?c", "")
        .match1f("abc")
        .test_eq("abc,b,,b");

    tc.compilef(r"(?=(a+))", "")
        .match1f("baaabac")
        .test_eq(",aaa");
    tc.compilef(r"(?=(a+))a*b\1", "")
        .match1f("baaabac")
        .test_eq("aba,a");
    assert_eq!(
        tc.compile(r"(.*?)a(?!(a+)b\2c)\2(.*)")
            .match1_vec("baaabaac"),
        vec![Some("baaabaac"), Some("ba"), None, Some("abaac")]
    );
    tc.compilef(r"\0", "").match1f("abc\0def").test_eq("\0");
}

#[test]
fn run_regexp_capture_test() {
    test_with_configs(run_regexp_capture_test_tc)
}

fn run_regexp_capture_test_tc(tc: TestConfig) {
    // regexp-captures.js
    tc.test_match_succeeds(r"^(((N({)?)|(R)|(U)|(V)|(B)|(H)|(n((n)|(r)|(v)|(h))?)|(r(r)?)|(v)|(b((n)|(b))?)|(h))|((Y)|(A)|(E)|(o(u)?)|(p(u)?)|(q(u)?)|(s)|(t)|(u)|(w)|(x(u)?)|(y)|(z)|(a((T)|(A)|(L))?)|(c)|(e)|(f(u)?)|(g(u)?)|(i)|(j)|(l)|(m(u)?)))+", "", "Avtnennan gunzvmu pubExnY nEvln vaTxh rmuhguhaTxnY");

    // regexp-capture.js
    assert_eq!(
        tc.compilef("(x)?\\1y", "").match1_vec("y"),
        vec![Some("y"), None]
    );
    assert_eq!(
        tc.compilef("(x)?y", "").match1_vec("y"),
        vec![Some("y"), None]
    );
    assert_eq!(
        tc.compilef("(x)?\\1y", "").match1_vec("y"),
        vec![Some("y"), None]
    );
    assert_eq!(
        tc.compilef("(x)?y", "").match1_vec("y"),
        vec![Some("y"), None]
    );
    assert_eq!(
        tc.compilef("(x)?\\1y", "").match1_vec("y"),
        vec![Some("y"), None]
    );
    assert_eq!(
        tc.compilef("^(b+|a){1,2}?bc", "").match1_vec("bbc"),
        vec![Some("bbc"), Some("b")]
    );
    assert_eq!(
        tc.compilef("((\\3|b)\\2(a)){2,}", "")
            .match1_vec("bbaababbabaaaaabbaaaabba"),
        vec![Some("bbaa"), Some("a"), Some(""), Some("a")]
    );

    tc.test_match_fails(r"((a|i|A|I|u|o|U|O)(s|c|b|c|d|f|g|h|j|k|l|m|n|p|q|r|s|t|v|w|x|y|z|B|C|D|F|G|H|J|K|L|M|N|P|Q|R|S|T|V|W|X|Y|Z)*) de\/da([.,!?\s]|$)", "", "");
}

#[test]
fn run_regexp_unicode_burns_test() {
    test_with_configs(run_regexp_unicode_burns_test_tc)
}

#[allow(unreachable_code)]
fn run_regexp_unicode_burns_test_tc(tc: TestConfig) {
    // These tests are extracted from regexp-capture-3.js.
    // All of these are cases where a naive engine would enter an infinite loop.
    // Termination is success. These depend on the v8 optimization where a regex
    // is known to only match Unicode strings, and so cannot match an ascii-only
    // string. We do not yet have this optimization so these tests are disabled.
    let _ = tc;
    return;
    let input = "The truth about forever is that it is happening right now";
    tc.compilef("(((.*)*)*x)\u{100}", "").match1f(input);
    tc.compilef("(((.*)*)*\u{100})foo", "").match1f(input);
    tc.compilef("\u{100}(((.*)*)*x)", "").match1f(input);
    tc.compilef("(((.*)*)*x)\u{100}", "").match1f(input);
    tc.compilef("[\u{107}\u{103}\u{100}](((.*)*)*x)", "")
        .match1f(input);
    tc.compilef("(((.*)*)*x)[\u{107}\u{103}\u{100}]", "")
        .match1f(input);
    tc.compilef("[^\\x00-\\xff](((.*)*)*x)", "").match1f(input);
    tc.compilef("(((.*)*)*x)[^\\x00-\\xff]", "").match1f(input);
    tc.compilef("(?!(((.*)*)*x)\u{100})foo", "").match1f(input);
    tc.compilef("(?!(((.*)*)*x))\u{100}", "").match1f(input);
    tc.compilef("(?=(((.*)*)*x)\u{100})foo", "").match1f(input);
    tc.compilef("(?=(((.*)*)*x))\u{100}", "").match1f(input);
    tc.compilef("(?=\u{100})(((.*)*)*x)", "").match1f(input);
    tc.compilef("(\u{e6}|\u{f8}|\u{100})(((.*)*)*x)", "")
        .match1f(input);
    tc.compilef("(a|b|(((.*)*)*x))\u{100}", "").match1f(input);
    tc.compilef("(a|(((.*)*)*x)\u{103}|(((.*)*)*x)\u{100})", "")
        .match1f(input);
}

#[test]
fn run_regexp_lookahead_tests() {
    test_with_configs(run_regexp_lookahead_tests_tc)
}

#[cfg_attr(rustfmt, rustfmt_skip)]
fn run_regexp_lookahead_tests_tc(tc: TestConfig) {
    // From regexp-lookahead.js
    tc.test_match_succeeds(r#"^(?=a)"#, "", "a");
    tc.test_match_fails(r#"^(?=a)"#, "", "b");
    tc.compilef(r#"^(?=a)"#, "").match1f("a").test_eq(""); 
    tc.test_match_succeeds(r#"^(?=\woo)f\w"#, "", "foo");
    tc.test_match_fails(r#"^(?=\woo)f\w"#, "", "boo");
    tc.test_match_fails(r#"^(?=\woo)f\w"#, "", "fao");
    tc.test_match_fails(r#"^(?=\woo)f\w"#, "", "foa");
    tc.compilef(r#"^(?=\woo)f\w"#, "").match1f("foo").test_eq("fo"); 
    tc.test_match_succeeds(r#"(?=\w).(?=\W)"#, "", r#".a! "#);
    tc.test_match_fails(r#"(?=\w).(?=\W)"#, "", r#".! "#);
    tc.test_match_succeeds(r#"(?=\w).(?=\W)"#, "", r#".ab! "#);
    tc.compilef(r#"(?=\w).(?=\W)"#, "").match1f(r#".ab! "#).test_eq("b"); 
    tc.test_match_succeeds(r#"(?=f(?=[^f]o)).."#, "", r#", foo!"#);
    tc.test_match_fails(r#"(?=f(?=[^f]o)).."#, "", r#", fo!"#);
    tc.test_match_fails(r#"(?=f(?=[^f]o)).."#, "", ", ffo");
    tc.compilef(r#"(?=f(?=[^f]o)).."#, "").match1f(r#", foo!"#).test_eq("fo"); 
    tc.test_match_succeeds(r#"^[^'"]*(?=(['"])).*\1(\w+)\1"#, "", "  'foo' ");
    tc.test_match_succeeds(r#"^[^'"]*(?=(['"])).*\1(\w+)\1"#, "", r#"  "foo" "#);
    tc.test_match_fails(r#"^[^'"]*(?=(['"])).*\1(\w+)\1"#, "", r#" " 'foo' "#);
    tc.test_match_fails(r#"^[^'"]*(?=(['"])).*\1(\w+)\1"#, "", r#" ' "foo" "#);
    tc.test_match_fails(r#"^[^'"]*(?=(['"])).*\1(\w+)\1"#, "", r#"  'foo" "#);
    tc.test_match_fails(r#"^[^'"]*(?=(['"])).*\1(\w+)\1"#, "", r#"  "foo' "#);
    tc.compilef(r#"^[^'"]*(?=(['"])).*\1(\w+)\1"#, "").match1f("  'foo' ").test_eq("  'foo',',foo"); 
    tc.compilef(r#"^[^'"]*(?=(['"])).*\1(\w+)\1"#, "").match1f(r#"  "foo" "#).test_eq(r#"  "foo",",foo"#); 
    tc.test_match_succeeds(r#"^(?:(?=(.))a|b)\1$"#, "", "aa");
    tc.test_match_succeeds(r#"^(?:(?=(.))a|b)\1$"#, "", "b");
    tc.test_match_fails(r#"^(?:(?=(.))a|b)\1$"#, "", "bb");
    tc.test_match_fails(r#"^(?:(?=(.))a|b)\1$"#, "", "a");
    tc.compilef(r#"^(?:(?=(.))a|b)\1$"#, "").match1f("aa").test_eq("aa,a"); 
    tc.compilef(r#"^(?:(?=(.))a|b)\1$"#, "").match1f("b").test_eq("b,"); 
    tc.test_match_succeeds(r#"^(?=(.)(?=(.)\1\2)\2\1)\1\2"#, "", "abab");
    tc.test_match_succeeds(r#"^(?=(.)(?=(.)\1\2)\2\1)\1\2"#, "", "ababxxxxxxxx");
    tc.test_match_fails(r#"^(?=(.)(?=(.)\1\2)\2\1)\1\2"#, "", "aba");
    tc.compilef(r#"^(?=(.)(?=(.)\1\2)\2\1)\1\2"#, "").match1f("abab").test_eq("ab,a,b"); 
    tc.test_match_succeeds(r#"^(?:(?=(.))a|b|c)$"#, "", "a");
    tc.test_match_succeeds(r#"^(?:(?=(.))a|b|c)$"#, "", "b");
    tc.test_match_succeeds(r#"^(?:(?=(.))a|b|c)$"#, "", "c");
    tc.test_match_fails(r#"^(?:(?=(.))a|b|c)$"#, "", "d");
    tc.compilef(r#"^(?:(?=(.))a|b|c)$"#, "").match1f("a").test_eq("a,a"); 
    tc.compilef(r#"^(?:(?=(.))a|b|c)$"#, "").match1f("b").test_eq("b,"); 
    tc.compilef(r#"^(?:(?=(.))a|b|c)$"#, "").match1f("c").test_eq("c,"); 
    tc.compilef(r#"^(?=(b))b"#, "").match1f("b").test_eq("b,b"); 
    tc.compilef(r#"^(?:(?=(b))|a)b"#, "").match1f("ab").test_eq("ab,"); 
    tc.compilef(r#"^(?:(?=(b)(?:(?=(c))|d))|)bd"#, "").match1f("bd").test_eq("bd,b,"); 
    tc.test_match_succeeds(r#"(?!x)."#, "", "y");
    tc.test_match_fails(r#"(?!x)."#, "", "x");
    tc.compilef(r#"(?!x)."#, "").match1f("y").test_eq("y"); 
    tc.test_match_succeeds(r#"(?!(\d))|\d"#, "", "4");
    tc.compilef(r#"(?!(\d))|\d"#, "").match1f("4").test_eq("4,"); 
    tc.compilef(r#"(?!(\d))|\d"#, "").match1f("x").test_eq(","); 
    tc.test_match_succeeds(r#"^(?=(x)(?=(y)))"#, "", "xy");
    tc.test_match_fails(r#"^(?=(x)(?=(y)))"#, "", "xz");
    tc.compilef(r#"^(?=(x)(?=(y)))"#, "").match1f("xy").test_eq(",x,y"); 
    tc.test_match_succeeds(r#"^(?!(x)(?!(y)))"#, "", "xy");
    tc.test_match_fails(r#"^(?!(x)(?!(y)))"#, "", "xz");
    tc.compilef(r#"^(?!(x)(?!(y)))"#, "").match1f("xy").test_eq(",,"); 
    tc.test_match_succeeds(r#"^(?=(x)(?!(y)))"#, "", "xz");
    tc.test_match_fails(r#"^(?=(x)(?!(y)))"#, "", "xy");
    tc.compilef(r#"^(?=(x)(?!(y)))"#, "").match1f("xz").test_eq(",x,"); 
    tc.test_match_succeeds(r#"^(?!(x)(?=(y)))"#, "", "xz");
    tc.test_match_fails(r#"^(?!(x)(?=(y)))"#, "", "xy");
    tc.compilef(r#"^(?!(x)(?=(y)))"#, "").match1f("xz").test_eq(",,"); 
    tc.test_match_succeeds(r#"^(?=(x)(?!(y)(?=(z))))"#, "", "xaz");
    tc.test_match_succeeds(r#"^(?=(x)(?!(y)(?=(z))))"#, "", "xya");
    tc.test_match_fails(r#"^(?=(x)(?!(y)(?=(z))))"#, "", "xyz");
    tc.test_match_fails(r#"^(?=(x)(?!(y)(?=(z))))"#, "", "a");
    tc.compilef(r#"^(?=(x)(?!(y)(?=(z))))"#, "").match1f("xaz").test_eq(",x,,"); 
    tc.compilef(r#"^(?=(x)(?!(y)(?=(z))))"#, "").match1f("xya").test_eq(",x,,"); 
    tc.test_match_succeeds(r#"^(?!(x)(?=(y)(?!(z))))"#, "", "a");
    tc.test_match_succeeds(r#"^(?!(x)(?=(y)(?!(z))))"#, "", "xa");
    tc.test_match_succeeds(r#"^(?!(x)(?=(y)(?!(z))))"#, "", "xyz");
    tc.test_match_fails(r#"^(?!(x)(?=(y)(?!(z))))"#, "", "xya");
    tc.compilef(r#"^(?!(x)(?=(y)(?!(z))))"#, "").match1f("a").test_eq(",,,"); 
    tc.compilef(r#"^(?!(x)(?=(y)(?!(z))))"#, "").match1f("xa").test_eq(",,,"); 
    tc.compilef(r#"^(?!(x)(?=(y)(?!(z))))"#, "").match1f("xyz").test_eq(",,,");     
}

#[test]
fn run_regexp_loop_capture_tests() {
    test_with_configs(run_regexp_loop_capture_tests_tc)
}

fn run_regexp_loop_capture_tests_tc(tc: TestConfig) {
    // From regexp-loop-capture.js
    assert_eq!(
        tc.compile(r"(?:(a)|(b)|(c))+").match1_vec("abc"),
        vec![Some("abc"), None, None, Some("c")]
    );
    assert_eq!(
        tc.compile(r"(?:(a)|b)*").match1_vec("ab"),
        vec![Some("ab"), None]
    );
}

#[test]
fn run_regexp_multiline_tests() {
    test_with_configs(run_regexp_multiline_tests_tc)
}

fn run_regexp_multiline_tests_tc(tc: TestConfig) {
    // From regexp-multiline.js
    tc.test_match_succeeds("^bar", "", "bar");
    tc.test_match_succeeds("^bar", "", "bar\nfoo");
    tc.compilef("^bar", "").test_fails("foo\nbar");
    tc.test_match_succeeds("^bar", "m", "bar");
    tc.test_match_succeeds("^bar", "m", "bar\nfoo");
    tc.test_match_succeeds("^bar", "m", "foo\nbar");
    tc.test_match_succeeds("bar$", "", "bar");
    tc.compilef("bar$", "").test_fails("bar\nfoo");
    tc.test_match_succeeds("bar$", "", "foo\nbar");
    tc.test_match_succeeds("bar$", "m", "bar");
    tc.test_match_succeeds("bar$", "m", "bar\nfoo");
    tc.test_match_succeeds("bar$", "m", "foo\nbar");
    tc.compilef("^bxr", "").test_fails("bar");
    tc.compilef("^bxr", "").test_fails("bar\nfoo");
    tc.compilef("^bxr", "m").test_fails("bar");
    tc.compilef("^bxr", "m").test_fails("bar\nfoo");
    tc.compilef("^bxr", "m").test_fails("foo\nbar");
    tc.compilef("bxr$", "").test_fails("bar");
    tc.compilef("bxr$", "").test_fails("foo\nbar");
    tc.compilef("bxr$", "m").test_fails("bar");
    tc.compilef("bxr$", "m").test_fails("bar\nfoo");
    tc.compilef("bxr$", "m").test_fails("foo\nbar");
    tc.test_match_succeeds("^.*$", "", "");
    tc.test_match_succeeds("^.*$", "", "foo");
    tc.compilef("^.*$", "").test_fails("\n");
    tc.test_match_succeeds("^.*$", "m", "\n");
    tc.test_match_succeeds("^[\\s]*$", "", " ");
    tc.test_match_succeeds("^[\\s]*$", "", "\n");
    tc.test_match_succeeds("^[^]*$", "", "");
    tc.test_match_succeeds("^[^]*$", "", "foo");
    tc.test_match_succeeds("^[^]*$", "", "\n");
    tc.test_match_succeeds("^([()\\s]|.)*$", "", "()\n()");
    tc.test_match_succeeds("^([()\\n]|.)*$", "", "()\n()");
    tc.compilef("^([()]|.)*$", "").test_fails("()\n()");
    tc.test_match_succeeds("^([()]|.)*$", "m", "()\n()");
    tc.test_match_succeeds("^([()]|.)*$", "m", "()\n");
    tc.test_match_succeeds("^[()]*$", "m", "()\n.");
    tc.test_match_succeeds("^[\\].]*$", "", "...]...");
    tc.compilef("^a$", "").test_fails("A");
    tc.test_match_succeeds("^a$", "i", "A");
    tc.compilef("^A$", "").test_fails("a");
    tc.test_match_succeeds("^A$", "i", "a");
    tc.compilef("^[a]$", "").test_fails("A");
    tc.test_match_succeeds("^[a]$", "i", "A");
    tc.compilef("^[A]$", "").test_fails("a");
    tc.test_match_succeeds("^[A]$", "i", "a");
    tc.compilef("^\u{e5}$", "").test_fails("\u{c5}");
    tc.test_match_succeeds("^\u{e5}$", "i", "\u{c5}");
    tc.compilef("^\u{c5}$", "").test_fails("\u{e5}");
    tc.test_match_succeeds("^\u{c5}$", "i", "\u{e5}");
    tc.compilef("^[\u{e5}]$", "").test_fails("\u{c5}");
    tc.test_match_succeeds("^[\u{e5}]$", "i", "\u{c5}");
    tc.compilef("^[\u{c5}]$", "").test_fails("\u{e5}");
    tc.test_match_succeeds("^[\u{c5}]$", "i", "\u{e5}");
    tc.compilef("^\u{413}$", "").test_fails("\u{433}");
    tc.test_match_succeeds("^\u{413}$", "i", "\u{433}");
    tc.compilef("^\u{433}$", "").test_fails("\u{413}");
    tc.test_match_succeeds("^\u{433}$", "i", "\u{413}");
    tc.compilef("^[\u{413}]$", "").test_fails("\u{433}");
    tc.test_match_succeeds("^[\u{413}]$", "i", "\u{433}");
    tc.compilef("^[\u{433}]$", "").test_fails("\u{413}");
    tc.test_match_succeeds("^[\u{433}]$", "i", "\u{413}");
}

#[test]
fn run_regexp_standalones() {
    test_with_configs(run_regexp_standalones_tc)
}

#[cfg_attr(rustfmt, rustfmt_skip)]
fn run_regexp_standalones_tc(tc: TestConfig) {
    // From regexp-standalones.js
    tc.compilef(r"^\d", "m").run_global_match("aaa\n789\r\nccc\r\n345").test_eq("7,3");
    tc.compilef(r"\d$", "m").run_global_match("aaa\n789\r\nccc\r\n345").test_eq("9,5");

    tc.compilef(r"^\d", "m").run_global_match("aaa\n789\r\nccc\r\nddd").test_eq("7");
    tc.compilef(r"\d$", "m").run_global_match("aaa\n789\r\nccc\r\nddd").test_eq("9");

    tc.compilef(r"[\S]+", "").match1f("\u{00BF}\u{00CD}\u{00BB}\u{00A7}").test_eq("\u{00BF}\u{00CD}\u{00BB}\u{00A7}");
    tc.compilef(r"[\S]+", "").match1f("\u{00BF}\u{00CD} \u{00BB}\u{00A7}").test_eq("\u{00BF}\u{00CD}");

    tc.compilef(r"[\S]+", "").match1f("\u{4e00}\u{ac00}\u{4e03}\u{4e00}").test_eq("\u{4e00}\u{ac00}\u{4e03}\u{4e00}");
    tc.compilef(r"[\S]+", "").match1f("\u{4e00}\u{ac00} \u{4e03}\u{4e00}").test_eq("\u{4e00}\u{ac00}");
}

#[test]
fn run_regexp_regexp() {
    test_with_configs(run_regexp_regexp_tc)
}

#[cfg_attr(rustfmt, rustfmt_skip)]
fn run_regexp_regexp_tc(tc: TestConfig) {
    // From regexp.js
    tc.compilef("[\u{0}]", "").match1f("[\u{0}]").test_eq("\u{0}");
    tc.compilef("[\u{0}]", "").match1f("[\u{0}]").test_eq("\u{0}");
    tc.compilef("^.", "m").run_global_match("aA\nbB\rcC\r\ndD\u{2028}eE\u{2029}fF").test_eq("a,b,c,d,e,f");
    tc.compilef(".$", "m").run_global_match("aA\nbB\rcC\r\ndD\u{2028}eE\u{2029}fF").test_eq("A,B,C,D,E,F");
    tc.compilef("^[^]", "m").run_global_match("aA\nbB\rcC\r\ndD\u{2028}eE\u{2029}fF").test_eq("a,b,c,\n,d,e,f");
    tc.compilef("[^]$", "m").run_global_match("aA\nbB\rcC\r\ndD\u{2028}eE\u{2029}fF").test_eq("A,B,C,\r,D,E,F");
    tc.test_match_succeeds("\\ca", "", "\u{1}");
    tc.compilef("\\ca", "").test_fails("\\ca");
    tc.compilef("\\ca", "").test_fails("ca");
    // Skipping Unicode-unsavvy \c[a/]
    // Skipping Unicode-unsavvy \c[a/]
    tc.test_match_succeeds("^[\\cM]$", "", "\r");
    tc.compilef("^[\\cM]$", "").test_fails("M");
    tc.compilef("^[\\cM]$", "").test_fails("c");
    tc.compilef("^[\\cM]$", "").test_fails("\\");
    tc.compilef("^[\\cM]$", "").test_fails("\u{3}");
    // Skipping Unicode-unsavvy ^[\c]]$
    // Skipping Unicode-unsavvy ^[\c]]$
    // Skipping Unicode-unsavvy ^[\c]]$
    // Skipping Unicode-unsavvy ^[\c]]$
    // Skipping Unicode-unsavvy ^[\c1]$
    // Skipping Unicode-unsavvy ^[\c1]$
    // Skipping Unicode-unsavvy ^[\c1]$
    // Skipping Unicode-unsavvy ^[\c1]$
    // Skipping Unicode-unsavvy ^[\c_]$
    // Skipping Unicode-unsavvy ^[\c_]$
    // Skipping Unicode-unsavvy ^[\c_]$
    // Skipping Unicode-unsavvy ^[\c_]$
    // Skipping Unicode-unsavvy ^[\c$]$
    // Skipping Unicode-unsavvy ^[\c$]$
    // Skipping Unicode-unsavvy ^[\c$]$
    // Skipping Unicode-unsavvy ^[\c$]$
    // Skipping Unicode-unsavvy ^[Z-\c-e]*$
    tc.test_match_succeeds("\\s", "", "\u{2028}");
    tc.test_match_succeeds("\\s", "", "\u{2029}");
    tc.test_match_succeeds("\\s", "", "\u{feff}");
    tc.compilef("\\S", "").test_fails("\u{2028}");
    tc.compilef("\\S", "").test_fails("\u{2029}");
    tc.compilef("\\S", "").test_fails("\u{feff}");
    // Skipping Unicode-unsavvy [\s-:]
    // Skipping Unicode-unsavvy [\s-:]
    // Skipping Unicode-unsavvy [\s-:]
    // Skipping Unicode-unsavvy [\s-:]
    // Skipping Unicode-unsavvy [\s-:]
    // Skipping Unicode-unsavvy [\s-:]
    // Skipping Unicode-unsavvy [\s-:]
    // Skipping Unicode-unsavvy [\S-:]
    // Skipping Unicode-unsavvy [\S-:]
    // Skipping Unicode-unsavvy [\S-:]
    // Skipping Unicode-unsavvy [\S-:]
    // Skipping Unicode-unsavvy [\S-:]
    // Skipping Unicode-unsavvy [\S-:]
    // Skipping Unicode-unsavvy [\S-:]
    // Skipping Unicode-unsavvy [^\s-:]
    // Skipping Unicode-unsavvy [^\s-:]
    // Skipping Unicode-unsavvy [^\s-:]
    // Skipping Unicode-unsavvy [^\s-:]
    // Skipping Unicode-unsavvy [^\s-:]
    // Skipping Unicode-unsavvy [^\s-:]
    // Skipping Unicode-unsavvy [^\s-:]
    // Skipping Unicode-unsavvy [^\S-:]
    // Skipping Unicode-unsavvy [^\S-:]
    // Skipping Unicode-unsavvy [^\S-:]
    // Skipping Unicode-unsavvy [^\S-:]
    // Skipping Unicode-unsavvy [^\S-:]
    // Skipping Unicode-unsavvy [^\S-:]
    // Skipping Unicode-unsavvy [^\S-:]
    tc.compilef("[\\s]", "").test_fails("-");
    tc.compilef("[\\s]", "").test_fails(":");
    tc.test_match_succeeds("[\\s]", "", " ");
    tc.test_match_succeeds("[\\s]", "", "\u{9}");
    tc.test_match_succeeds("[\\s]", "", "\n");
    tc.compilef("[\\s]", "").test_fails("a");
    tc.compilef("[\\s]", "").test_fails("Z");
    tc.test_match_succeeds("[^\\s]", "", "-");
    tc.test_match_succeeds("[^\\s]", "", ":");
    tc.compilef("[^\\s]", "").test_fails(" ");
    tc.compilef("[^\\s]", "").test_fails("\u{9}");
    tc.compilef("[^\\s]", "").test_fails("\n");
    tc.test_match_succeeds("[^\\s]", "", "a");
    tc.test_match_succeeds("[^\\s]", "", "Z");
    tc.test_match_succeeds("[\\S]", "", "-");
    tc.test_match_succeeds("[\\S]", "", ":");
    tc.compilef("[\\S]", "").test_fails(" ");
    tc.compilef("[\\S]", "").test_fails("\u{9}");
    tc.compilef("[\\S]", "").test_fails("\n");
    tc.test_match_succeeds("[\\S]", "", "a");
    tc.test_match_succeeds("[\\S]", "", "Z");
    tc.compilef("[^\\S]", "").test_fails("-");
    tc.compilef("[^\\S]", "").test_fails(":");
    tc.test_match_succeeds("[^\\S]", "", " ");
    tc.test_match_succeeds("[^\\S]", "", "\u{9}");
    tc.test_match_succeeds("[^\\S]", "", "\n");
    tc.compilef("[^\\S]", "").test_fails("a");
    tc.compilef("[^\\S]", "").test_fails("Z");
    tc.test_match_succeeds("[\\s\\S]", "", "-");
    tc.test_match_succeeds("[\\s\\S]", "", ":");
    tc.test_match_succeeds("[\\s\\S]", "", " ");
    tc.test_match_succeeds("[\\s\\S]", "", "\u{9}");
    tc.test_match_succeeds("[\\s\\S]", "", "\n");
    tc.test_match_succeeds("[\\s\\S]", "", "a");
    tc.test_match_succeeds("[\\s\\S]", "", "Z");
    tc.compilef("[^\\s\\S]", "").test_fails("-");
    tc.compilef("[^\\s\\S]", "").test_fails(":");
    tc.compilef("[^\\s\\S]", "").test_fails(" ");
    tc.compilef("[^\\s\\S]", "").test_fails("\u{9}");
    tc.compilef("[^\\s\\S]", "").test_fails("\n");
    tc.compilef("[^\\s\\S]", "").test_fails("a");
    tc.compilef("[^\\s\\S]", "").test_fails("Z");
    // Skipping Unicode-unsavvy [\s-0-9]
    // Skipping Unicode-unsavvy [\s-0-9]
    // Skipping Unicode-unsavvy [\s-0-9]
    // Skipping Unicode-unsavvy [\s-0-9]
    // Skipping Unicode-unsavvy [\s-0-9]
    // Skipping Unicode-unsavvy [\s-0-9]
    tc.compilef("^\\d+", "").test_fails("asdf\n123");
    tc.test_match_succeeds("^\\d+", "m", "asdf\n123");
    tc.compilef("\\d+$", "").test_fails("123\nasdf");
    tc.test_match_succeeds("\\d+$", "m", "123\nasdf");
    tc.compilef("^.*", "m").run_global_match("a\n\rb").test_eq("a,,b");
    tc.compilef("()foo$\\1", "").test_fails("football");
    tc.compilef("foo$(?=ball)", "").test_fails("football");
    tc.compilef("foo$(?!bar)", "").test_fails("football");
    tc.test_match_succeeds("()foo$\\1", "", "foo");
    tc.test_match_succeeds("foo$(?=(ball)?)", "", "foo");
    tc.test_match_succeeds("()foo$(?!bar)", "", "foo");
    tc.compilef("(x?)foo$\\1", "").test_fails("football");
    tc.compilef("foo$(?=ball)", "").test_fails("football");
    tc.compilef("foo$(?!bar)", "").test_fails("football");
    tc.test_match_succeeds("(x?)foo$\\1", "", "foo");
    tc.test_match_succeeds("foo$(?=(ball)?)", "", "foo");
    tc.test_match_succeeds("foo$(?!bar)", "", "foo");
    tc.compilef("f(o)\\b\\1", "").test_fails("foo");
    tc.test_match_succeeds("f(o)\\B\\1", "", "foo");
    tc.compilef("x(...)\\1", "i").test_fails("xaaaaa");
    tc.test_match_succeeds("x((?:))\\1\\1x", "i", "xx");
    tc.test_match_succeeds("x(?:...|(...))\\1x", "i", "xabcx");
    tc.test_match_succeeds("x(?:...|(...))\\1x", "i", "xabcABCx");
    tc.compilef("^(.)\\1$", "i").test_fails("\u{0} ");
    tc.compilef("^(.)\\1$", "i").test_fails("\u{1}!");
    tc.compilef("^(.)\\1$", "i").test_fails("\u{2}\"");
    tc.compilef("^(.)\\1$", "i").test_fails("\u{3}#");
    tc.compilef("^(.)\\1$", "i").test_fails("\u{4}$");
    tc.compilef("^(.)\\1$", "i").test_fails("\u{5}%");
    tc.compilef("^(.)\\1$", "i").test_fails("\u{6}&");
    tc.compilef("^(.)\\1$", "i").test_fails("\u{7}'");
    tc.compilef("^(.)\\1$", "i").test_fails("\u{8}(");
    tc.compilef("^(.)\\1$", "i").test_fails("\u{9})");
    tc.compilef("^(.)\\1$", "i").test_fails("\n*");
    tc.compilef("^(.)\\1$", "i").test_fails("\u{b}+");
    tc.compilef("^(.)\\1$", "i").test_fails("\u{c},");
    tc.compilef("^(.)\\1$", "i").test_fails("\r-");
    tc.compilef("^(.)\\1$", "i").test_fails("\u{e}.");
    tc.compilef("^(.)\\1$", "i").test_fails("\u{f}/");
    tc.compilef("^(.)\\1$", "i").test_fails("\u{10}0");
    tc.compilef("^(.)\\1$", "i").test_fails("\u{11}1");
    tc.compilef("^(.)\\1$", "i").test_fails("\u{12}2");
    tc.compilef("^(.)\\1$", "i").test_fails("\u{13}3");
    tc.compilef("^(.)\\1$", "i").test_fails("\u{14}4");
    tc.compilef("^(.)\\1$", "i").test_fails("\u{15}5");
    tc.compilef("^(.)\\1$", "i").test_fails("\u{16}6");
    tc.compilef("^(.)\\1$", "i").test_fails("\u{17}7");
    tc.compilef("^(.)\\1$", "i").test_fails("\u{18}8");
    tc.compilef("^(.)\\1$", "i").test_fails("\u{19}9");
    tc.compilef("^(.)\\1$", "i").test_fails("\u{1a}:");
    tc.compilef("^(.)\\1$", "i").test_fails("\u{1b};");
    tc.compilef("^(.)\\1$", "i").test_fails("\u{1c}<");
    tc.compilef("^(.)\\1$", "i").test_fails("\u{1d}=");
    tc.compilef("^(.)\\1$", "i").test_fails("\u{1e}>");
    tc.compilef("^(.)\\1$", "i").test_fails("\u{1f}?");
    tc.compilef("^(.)\\1$", "i").test_fails(" \u{0}");
    tc.compilef("^(.)\\1$", "i").test_fails("!\u{1}");
    tc.compilef("^(.)\\1$", "i").test_fails("\"\u{2}");
    tc.compilef("^(.)\\1$", "i").test_fails("#\u{3}");
    tc.compilef("^(.)\\1$", "i").test_fails("$\u{4}");
    tc.compilef("^(.)\\1$", "i").test_fails("%\u{5}");
    tc.compilef("^(.)\\1$", "i").test_fails("&\u{6}");
    tc.compilef("^(.)\\1$", "i").test_fails("'\u{7}");
    tc.compilef("^(.)\\1$", "i").test_fails("(\u{8}");
    tc.compilef("^(.)\\1$", "i").test_fails(")\u{9}");
    tc.compilef("^(.)\\1$", "i").test_fails("*\n");
    tc.compilef("^(.)\\1$", "i").test_fails("+\u{b}");
    tc.compilef("^(.)\\1$", "i").test_fails(",\u{c}");
    tc.compilef("^(.)\\1$", "i").test_fails("-\r");
    tc.compilef("^(.)\\1$", "i").test_fails(".\u{e}");
    tc.compilef("^(.)\\1$", "i").test_fails("/\u{f}");
    tc.compilef("^(.)\\1$", "i").test_fails("0\u{10}");
    tc.compilef("^(.)\\1$", "i").test_fails("1\u{11}");
    tc.compilef("^(.)\\1$", "i").test_fails("2\u{12}");
    tc.compilef("^(.)\\1$", "i").test_fails("3\u{13}");
    tc.compilef("^(.)\\1$", "i").test_fails("4\u{14}");
    tc.compilef("^(.)\\1$", "i").test_fails("5\u{15}");
    tc.compilef("^(.)\\1$", "i").test_fails("6\u{16}");
    tc.compilef("^(.)\\1$", "i").test_fails("7\u{17}");
    tc.compilef("^(.)\\1$", "i").test_fails("8\u{18}");
    tc.compilef("^(.)\\1$", "i").test_fails("9\u{19}");
    tc.compilef("^(.)\\1$", "i").test_fails(":\u{1a}");
    tc.compilef("^(.)\\1$", "i").test_fails(";\u{1b}");
    tc.compilef("^(.)\\1$", "i").test_fails("<\u{1c}");
    tc.compilef("^(.)\\1$", "i").test_fails("=\u{1d}");
    tc.compilef("^(.)\\1$", "i").test_fails(">\u{1e}");
    tc.compilef("^(.)\\1$", "i").test_fails("?\u{1f}");
    tc.compilef("^(.)\\1$", "i").test_fails("@`");
    tc.test_match_succeeds("^(.)\\1$", "i", "Aa");
    tc.test_match_succeeds("^(.)\\1$", "i", "Bb");
    tc.test_match_succeeds("^(.)\\1$", "i", "Cc");
    tc.test_match_succeeds("^(.)\\1$", "i", "Dd");
    tc.test_match_succeeds("^(.)\\1$", "i", "Ee");
    tc.test_match_succeeds("^(.)\\1$", "i", "Ff");
    tc.test_match_succeeds("^(.)\\1$", "i", "Gg");
    tc.test_match_succeeds("^(.)\\1$", "i", "Hh");
    tc.test_match_succeeds("^(.)\\1$", "i", "Ii");
    tc.test_match_succeeds("^(.)\\1$", "i", "Jj");
    tc.test_match_succeeds("^(.)\\1$", "i", "Kk");
    tc.test_match_succeeds("^(.)\\1$", "i", "Ll");
    tc.test_match_succeeds("^(.)\\1$", "i", "Mm");
    tc.test_match_succeeds("^(.)\\1$", "i", "Nn");
    tc.test_match_succeeds("^(.)\\1$", "i", "Oo");
    tc.test_match_succeeds("^(.)\\1$", "i", "Pp");
    tc.test_match_succeeds("^(.)\\1$", "i", "Qq");
    tc.test_match_succeeds("^(.)\\1$", "i", "Rr");
    tc.test_match_succeeds("^(.)\\1$", "i", "Ss");
    tc.test_match_succeeds("^(.)\\1$", "i", "Tt");
    tc.test_match_succeeds("^(.)\\1$", "i", "Uu");
    tc.test_match_succeeds("^(.)\\1$", "i", "Vv");
    tc.test_match_succeeds("^(.)\\1$", "i", "Ww");
    tc.test_match_succeeds("^(.)\\1$", "i", "Xx");
    tc.test_match_succeeds("^(.)\\1$", "i", "Yy");
    tc.test_match_succeeds("^(.)\\1$", "i", "Zz");
    tc.compilef("^(.)\\1$", "i").test_fails("[{");
    tc.compilef("^(.)\\1$", "i").test_fails("\\|");
    tc.compilef("^(.)\\1$", "i").test_fails("]}");
    tc.compilef("^(.)\\1$", "i").test_fails("^~");
    tc.compilef("^(.)\\1$", "i").test_fails("_\u{7f}");
    tc.compilef("^(.)\\1$", "i").test_fails("`@");
    tc.test_match_succeeds("^(.)\\1$", "i", "aA");
    tc.test_match_succeeds("^(.)\\1$", "i", "bB");
    tc.test_match_succeeds("^(.)\\1$", "i", "cC");
    tc.test_match_succeeds("^(.)\\1$", "i", "dD");
    tc.test_match_succeeds("^(.)\\1$", "i", "eE");
    tc.test_match_succeeds("^(.)\\1$", "i", "fF");
    tc.test_match_succeeds("^(.)\\1$", "i", "gG");
    tc.test_match_succeeds("^(.)\\1$", "i", "hH");
    tc.test_match_succeeds("^(.)\\1$", "i", "iI");
    tc.test_match_succeeds("^(.)\\1$", "i", "jJ");
    tc.test_match_succeeds("^(.)\\1$", "i", "kK");
    tc.test_match_succeeds("^(.)\\1$", "i", "lL");
    tc.test_match_succeeds("^(.)\\1$", "i", "mM");
    tc.test_match_succeeds("^(.)\\1$", "i", "nN");
    tc.test_match_succeeds("^(.)\\1$", "i", "oO");
    tc.test_match_succeeds("^(.)\\1$", "i", "pP");
    tc.test_match_succeeds("^(.)\\1$", "i", "qQ");
    tc.test_match_succeeds("^(.)\\1$", "i", "rR");
    tc.test_match_succeeds("^(.)\\1$", "i", "sS");
    tc.test_match_succeeds("^(.)\\1$", "i", "tT");
    tc.test_match_succeeds("^(.)\\1$", "i", "uU");
    tc.test_match_succeeds("^(.)\\1$", "i", "vV");
    tc.test_match_succeeds("^(.)\\1$", "i", "wW");
    tc.test_match_succeeds("^(.)\\1$", "i", "xX");
    tc.test_match_succeeds("^(.)\\1$", "i", "yY");
    tc.test_match_succeeds("^(.)\\1$", "i", "zZ");
    tc.compilef("^(.)\\1$", "i").test_fails("{[");
    tc.compilef("^(.)\\1$", "i").test_fails("|\\");
    tc.compilef("^(.)\\1$", "i").test_fails("}]");
    tc.compilef("^(.)\\1$", "i").test_fails("~^");
    tc.compilef("^(.)\\1$", "i").test_fails("\u{7f}_");
    tc.compilef("f(o)$\\1", "").test_fails("foo");
    tc.compilef("a{111111111111111111111111111111111111111111111}", "").test_fails("b");
    tc.compilef("a{999999999999999999999999999999999999999999999}", "").test_fails("b");
    tc.compilef("a{1,111111111111111111111111111111111111111111111}", "").test_fails("b");
    tc.compilef("a{1,999999999999999999999999999999999999999999999}", "").test_fails("b");
    tc.compilef("a{2147483648}", "").test_fails("b");
    tc.compilef("a{21474836471}", "").test_fails("b");
    tc.compilef("a{1,2147483648}", "").test_fails("b");
    tc.compilef("a{1,21474836471}", "").test_fails("b");
    tc.compilef("a{2147483648,2147483648}", "").test_fails("b");
    tc.compilef("a{21474836471,21474836471}", "").test_fails("b");
    tc.compilef("a{2147483647}", "").test_fails("b");
    tc.compilef("a{1,2147483647}", "").test_fails("b");
    tc.test_match_succeeds("a{1,2147483647}", "", "a");
    tc.compilef("a{2147483647,2147483647}", "").test_fails("a");
    tc.compilef("f", "").test_fails("b");
    tc.compilef("[abc]f", "").test_fails("x");
    tc.compilef("[abc]f", "").test_fails("xa");
    tc.compilef("[abc]<", "").test_fails("x");
    tc.compilef("[abc]<", "").test_fails("xa");
    tc.compilef("f", "i").test_fails("b");
    tc.compilef("[abc]f", "i").test_fails("x");
    tc.compilef("[abc]f", "i").test_fails("xa");
    tc.compilef("[abc]<", "i").test_fails("x");
    tc.compilef("[abc]<", "i").test_fails("xa");
    tc.compilef("f[abc]", "").test_fails("x");
    tc.compilef("f[abc]", "").test_fails("xa");
    tc.compilef("<[abc]", "").test_fails("x");
    tc.compilef("<[abc]", "").test_fails("xa");
    tc.compilef("f[abc]", "i").test_fails("x");
    tc.compilef("f[abc]", "i").test_fails("xa");
    tc.compilef("<[abc]", "i").test_fails("x");
    tc.compilef("<[abc]", "i").test_fails("xa");
    tc.compilef("x([0-7]%%x|[0-6]%%y)", "").test_fails("x7%%y");
    tc.compilef("()x\\1(y([0-7]%%%x|[0-6]%%%y)|dkjasldkas)", "").test_fails("xy7%%%y");
    tc.compilef("()x\\1(y([0-7]%%%x|[0-6]%%%y)|dkjasldkas)", "").test_fails("xy%%%y");
    tc.compilef("()x\\1y([0-7]%%%x|[0-6]%%%y)", "").test_fails("xy7%%%y");
    tc.compilef("()x\\1(y([0-7]%%%x|[0-6]%%%y)|dkjasldkas)", "").test_fails("xy%%%y");
    tc.compilef("()x\\1y([0-7]%%%x|[0-6]%%%y)", "").test_fails("xy7%%%y");
    tc.compilef("xy([0-7]%%%x|[0-6]%%%y)", "").test_fails("xy7%%%y");
    tc.compilef("x([0-7]%%%x|[0-6]%%%y)", "").test_fails("x7%%%y");
    tc.test_match_succeeds("[^\\xfe-\\xff]*", "", "");
    tc.test_match_succeeds("b\\b", "", "b");
    tc.test_match_succeeds("b\\b$", "", "b");
    tc.test_match_succeeds("\\bb", "", "b");
    tc.test_match_succeeds("^\\bb", "", "b");
    tc.compilef(",\\b", "").test_fails(",");
    tc.compilef(",\\b$", "").test_fails(",");
    tc.compilef("\\b,", "").test_fails(",");
    tc.compilef("^\\b,", "").test_fails(",");
    tc.compilef("b\\B", "").test_fails("b");
    tc.compilef("b\\B$", "").test_fails("b");
    tc.compilef("\\Bb", "").test_fails("b");
    tc.compilef("^\\Bb", "").test_fails("b");
    tc.test_match_succeeds(",\\B", "", ",");
    tc.test_match_succeeds(",\\B$", "", ",");
    tc.test_match_succeeds("\\B,", "", ",");
    tc.test_match_succeeds("^\\B,", "", ",");
    tc.test_match_succeeds("b\\b", "", "b,");
    tc.compilef("b\\b", "").test_fails("ba");
    tc.compilef("b\\B", "").test_fails("b,");
    tc.test_match_succeeds("b\\B", "", "ba");
    tc.test_match_succeeds("b\\Bb", "", "bb");
    tc.compilef("b\\bb", "").test_fails("bb");
    tc.compilef("b\\b[,b]", "").test_fails("bb");
    tc.test_match_succeeds("b\\B[,b]", "", "bb");
    tc.test_match_succeeds("b\\b[,b]", "", "b,");
    tc.compilef("b\\B[,b]", "").test_fails("b,");
    tc.compilef("[,b]\\bb", "").test_fails("bb");
    tc.test_match_succeeds("[,b]\\Bb", "", "bb");
    tc.test_match_succeeds("[,b]\\bb", "", ",b");
    tc.compilef("[,b]\\Bb", "").test_fails(",b");
    tc.compilef("[,b]\\b[,b]", "").test_fails("bb");
    tc.test_match_succeeds("[,b]\\B[,b]", "", "bb");
    tc.test_match_succeeds("[,b]\\b[,b]", "", ",b");
    tc.compilef("[,b]\\B[,b]", "").test_fails(",b");
    tc.test_match_succeeds("[,b]\\b[,b]", "", "b,");
    tc.compilef("[,b]\\B[,b]", "").test_fails("b,");
    tc.test_match_succeeds("(?:a|bc)g$", "", "ag");
    tc.test_match_succeeds("(?:a|bc)g$", "", "bcg");
    tc.test_match_succeeds("(?:a|bc)g$", "", "abcg");
    tc.test_match_succeeds("(?:a|bc)g$", "", "zimbag");
    tc.test_match_succeeds("(?:a|bc)g$", "", "zimbcg");
    tc.compilef("(?:a|bc)g$", "").test_fails("");
    tc.compilef("(?:a|bc)g$", "").test_fails("");
    tc.test_match_succeeds("(?:a|bc)g$", "", "ag");
    tc.test_match_succeeds("(?:a|bc)g$", "", "zimbag");
    tc.test_match_succeeds("^(?:a|bc)g$", "", "ag");
    tc.compilef("^(?:a|bc)g$", "").test_fails("zag");
    tc.test_match_succeeds("VeryLongRegExp!{1,1000}$", "", "BahoolaVeryLongRegExp!!!!!!");
    tc.compilef("VeryLongRegExp!{1,1000}$", "").test_fails("VeryLongRegExp");
    tc.compilef("VeryLongRegExp!{1,1000}$", "").test_fails("!");
    tc.test_match_succeeds("(?:a$|bc$)", "", "a");
    tc.test_match_succeeds("(?:a$|bc$)", "", "bc");
    tc.test_match_succeeds("(?:a$|bc$)", "", "abc");
    tc.test_match_succeeds("(?:a$|bc$)", "", "zimzamzumba");
    tc.test_match_succeeds("(?:a$|bc$)", "", "zimzamzumbc");
    tc.compilef("(?:a$|bc$)", "").test_fails("c");
    tc.compilef("(?:a$|bc$)", "").test_fails("");
    tc.test_match_succeeds("(?:a|bc$)", "", "a");
    tc.test_match_succeeds("(?:a|bc$)", "", "bc");
    tc.compilef("(?:a|bc$)", "").test_fails("c");
    tc.compilef("(?:a|bc$)", "").test_fails("");
    tc.test_match_succeeds(".*abc", "", "abc");
    tc.compilef(".*\\d+", "").test_fails("q");
    // Skipping Unicode-unsavvy ^{*$
    // Skipping Unicode-unsavvy ^}*$
    // Skipping Unicode-unsavvy ]
    // Skipping Unicode-unsavvy ^\c%$
    tc.test_match_succeeds("^\\d%$", "", "2%");
    // Skipping Unicode-unsavvy ^\e%$
    tc.test_match_succeeds("^\\ca$", "", "\u{1}");
    tc.test_match_succeeds("^\\cA$", "", "\u{1}");
    // Skipping Unicode-unsavvy ^\c9$
    // Skipping Unicode-unsavvy ^\c$
    // Skipping Unicode-unsavvy ^[\c%]*$
    // Skipping Unicode-unsavvy ^[\c:]*$
    // Skipping Unicode-unsavvy ^[\c0]*$
    // Skipping Unicode-unsavvy ^[\c1]*$
    // Skipping Unicode-unsavvy ^[\c2]*$
    // Skipping Unicode-unsavvy ^[\c3]*$
    // Skipping Unicode-unsavvy ^[\c4]*$
    // Skipping Unicode-unsavvy ^[\c5]*$
    // Skipping Unicode-unsavvy ^[\c6]*$
    // Skipping Unicode-unsavvy ^[\c7]*$
    // Skipping Unicode-unsavvy ^[\c8]*$
    // Skipping Unicode-unsavvy ^[\c9]*$
    // Skipping Unicode-unsavvy ^[\c_]*$
    // Skipping Unicode-unsavvy ^[\c11]*$
    // Skipping Unicode-unsavvy ^[\8]*$
    // Skipping Unicode-unsavvy ^[\7]*$
    // Skipping Unicode-unsavvy ^[\11]*$
    // Skipping Unicode-unsavvy ^[\111]*$
    // Skipping Unicode-unsavvy ^[\222]*$
    // Skipping Unicode-unsavvy ^[\333]*$
    // Skipping Unicode-unsavvy ^[\444]*$
    // Skipping Unicode-unsavvy ^[\d-X]*$
    // Skipping Unicode-unsavvy ^[\d-X-Z]*$
    // Skipping Unicode-unsavvy ^[\d-X-Z]*$
    tc.compilef("\\uDB88|\\uDBEC|aa", "").test_fails("");
}
