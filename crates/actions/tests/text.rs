use nodecraft_actions::text::*;
use proptest::prelude::*;
use std::collections::BTreeMap;
fn l() -> Limits {
    Limits::default()
}
fn rep(pattern: MatchMode, replacement: &str) -> ReplaceOptions {
    ReplaceOptions { pattern, replacement: replacement.into(), case_sensitive: true, whole_word: false }
}
#[test]
fn split_modes_and_boundaries() {
    let cases = [
        ("a,,b,", SplitMode::Separator(",".into()), vec!["a", "", "b", ""]),
        ("a1b22c", SplitMode::Regex(r"\d+".into()), vec!["a", "b", "c"]),
        ("first\r\nline\r\n \t\r\nsecond\n\n", SplitMode::Paragraphs, vec!["first\r\nline", "second"]),
        ("a\r\nb\nc", SplitMode::Lines(2), vec!["a\r\nb\n", "c"]),
        ("é🦀a", SplitMode::Chars(2), vec!["é🦀", "a"]),
        ("", SplitMode::Chars(3), vec![]),
        ("", SplitMode::Separator(",".into()), vec![""]),
        ("\n \r\n", SplitMode::Paragraphs, vec![]),
    ];
    for (input, mode, want) in cases {
        assert_eq!(split(input, &mode, l()).unwrap(), want);
    }
    assert_eq!(split("ab", &SplitMode::Regex("".into()), l()).unwrap(), vec!["", "a", "b", ""]);
    for mode in [SplitMode::Chars(0), SplitMode::Lines(0)] {
        assert_eq!(split("", &mode, l()), Err(TextError::InvalidCount));
    }
    assert_eq!(split("a", &SplitMode::Separator("".into()), l()), Err(TextError::EmptySeparator));
    assert_eq!(split("a", &SplitMode::Regex("[".into()), l()), Err(TextError::InvalidPattern));
}
#[test]
fn regex_capture_literal_case_and_words() {
    let mut o = rep(MatchMode::Regex(r"(?P<word>\w+)-(\d+)".into()), "${word}:$2/$$/$missing");
    assert_eq!(replace("é-12", &o, l()).unwrap(), "é:12/$/");
    o = rep(MatchMode::Literal("cat".into()), "$1");
    o.case_sensitive = false;
    o.whole_word = true;
    assert_eq!(replace("Cat catapult CAT écAté cat!", &o, l()).unwrap(), "$1 catapult $1 écAté $1!");
    assert_eq!(replace("a.b", &rep(MatchMode::Literal(".".into()), "!"), l()).unwrap(), "a!b");
    assert_eq!(replace("é", &rep(MatchMode::Regex("".into()), "|"), l()).unwrap(), "|é|");
    assert_eq!(replace("a", &rep(MatchMode::Regex("(a)?b".into()), "$1"), l()).unwrap(), "a");
    assert_eq!(replace("b", &rep(MatchMode::Regex("(a)?b".into()), "$1"), l()).unwrap(), "");
}
#[test]
fn concat_and_templates() {
    let options = ConcatOptions { separator: ",".into(), prefix: "[".into(), suffix: "]".into(), trim: true, skip_empty: true };
    assert_eq!(concat(&[" a ".into(), " \n".into(), "é".into()], &options, l()).unwrap(), "[a,é]");
    assert_eq!(concat(&[], &options, l()).unwrap(), "[]");
    let fields = BTreeMap::from([("name".into(), "{{nested}}".into()), ("é".into(), "🦀".into())]);
    assert_eq!(fill_template("{{name}} {{é}} {{{{raw}}}}", &fields, l()).unwrap(), "{{nested}} 🦀 {{raw}}");
    assert_eq!(fill_template("{{missing}}", &fields, l()), Err(TextError::MissingField));
    for template in ["{{", "}}", "{{}}", "{{bad{key}}"] {
        assert_eq!(fill_template(template, &fields, l()), Err(TextError::InvalidTemplate));
    }
    assert_eq!(fill_template("a{b}c", &fields, l()).unwrap(), "a{b}c");
}
#[test]
fn resource_edges_and_amplification() {
    let tiny = Limits { input_bytes: 8, output_bytes: 3, items: 2 };
    assert_eq!(split("abc", &SplitMode::Chars(3), tiny).unwrap(), vec!["abc"]);
    assert_eq!(split("abcd", &SplitMode::Chars(4), tiny), Err(TextError::OutputLimit));
    assert_eq!(split("abc", &SplitMode::Chars(1), tiny), Err(TextError::ItemLimit));
    assert_eq!(split("123456789", &SplitMode::Chars(9), tiny), Err(TextError::InputLimit));
    assert_eq!(replace("aaa", &rep(MatchMode::Literal("a".into()), "bb"), tiny), Err(TextError::OutputLimit));
    assert_eq!(replace("aaa", &rep(MatchMode::Literal("a".into()), ""), tiny), Err(TextError::ItemLimit));
    assert_eq!(replace("abcd", &rep(MatchMode::Regex("(.*)".into()), "$1$1"), Limits { output_bytes: 7, ..l() }), Err(TextError::OutputLimit));
    assert_eq!(fill_template("{{x}}{{x}}", &BTreeMap::from([("x".into(), "ab".into())]), Limits { output_bytes: 3, ..l() }), Err(TextError::OutputLimit));
    assert_eq!(concat(&["a".into(), "b".into()], &ConcatOptions { separator: "xx".into(), ..Default::default() }, tiny), Err(TextError::OutputLimit));
    assert_eq!(split("a", &SplitMode::Regex("a".repeat(MAX_PATTERN_BYTES + 1)), l()), Err(TextError::PatternLimit));
    assert_eq!(split("a", &SplitMode::Regex("a{999999999}".into()), l()), Err(TextError::InvalidPattern));
    assert_eq!(split("a", &SplitMode::Chars(1), Limits { items: MAX_ITEMS + 1, ..l() }), Err(TextError::InvalidLimits));
    let fields = (0..=MAX_FIELDS).map(|i| (i.to_string(), String::new())).collect();
    assert_eq!(fill_template("", &fields, l()), Err(TextError::FieldLimit));
    assert_eq!(fill_template(&"{{x}}".repeat(MAX_FIELDS + 1), &BTreeMap::from([("x".into(), String::new())]), l()), Err(TextError::FieldLimit));
}
#[test]
fn stable_wire_contract_and_safe_errors() {
    let request = TextRequest::Split { input: "é".into(), mode: SplitMode::Chars(1) };
    let value = serde_json::to_value(&request).unwrap();
    assert_eq!(value["action"], "text.split");
    assert_eq!(request.id(), "text.split");
    assert_eq!(serde_json::from_value::<TextRequest>(value).unwrap(), request);
    assert_eq!(request.execute(l()).unwrap(), TextResult::Parts(vec!["é".into()]));
    let private = "PRIVATE_TOKEN_[";
    let e = replace("", &rep(MatchMode::Regex(private.into()), ""), l()).unwrap_err();
    assert!(!e.to_string().contains(private));
}
proptest! {
    #[test]
    fn unicode_char_groups_reconstruct(input in ".{0,200}", count in 1usize..100){
        let parts=split(&input,&SplitMode::Chars(count),l()).unwrap();
        prop_assert_eq!(parts.concat(),input);prop_assert!(parts.iter().all(|s|s.chars().count()<=count));
    }
    #[test]
    fn hostile_patterns_and_budgets_never_exceed_output(input in ".{0,100}", pattern in ".{0,50}", replacement in ".{0,50}", output in 0usize..200, items in 0usize..20){
        let limits=Limits{output_bytes:output,items,..l()};
        if let Ok(result)=replace(&input,&rep(MatchMode::Regex(pattern.clone()),&replacement),limits){prop_assert!(result.len()<=output);}
        if let Ok(parts)=split(&input,&SplitMode::Regex(pattern),limits){prop_assert!(parts.len()<=items);prop_assert!(parts.iter().map(String::len).sum::<usize>()<=output);}
    }
}

#[test]
fn work_caps_prevent_zero_output_and_suffix_rescan_amplification() {
    let input = "a".repeat(1025);
    let options = rep(MatchMode::Regex("a".into()), &"$missing".repeat(2048));
    assert_eq!(replace(&input, &options, l()), Err(TextError::WorkLimit));
    let literal_input = "a".repeat(MAX_ITEMS);
    assert_eq!(replace(&literal_input, &rep(MatchMode::Literal("a".into()), "b"), l()).unwrap(), "b".repeat(MAX_ITEMS));
    let input = "A".repeat(16_384);
    let pattern = ".*[^A-Z]|[A-Z]";
    assert_eq!(replace(&input, &rep(MatchMode::Regex(pattern.into()), ""), l()), Err(TextError::WorkLimit));
    assert_eq!(split(&input, &SplitMode::Regex(pattern.into()), l()), Err(TextError::WorkLimit));
}
#[test]
fn explicit_bounded_search_preserves_regex_iterator_semantics() {
    for input in ["", "éa", "a", "aa", "ba", "a\nb"] {
        for pattern in ["", "a*", "a?", "^|$", r"\b", "a|", "(?m)^", "a+"] {
            let re = regex::Regex::new(pattern).unwrap();
            assert_eq!(split(input, &SplitMode::Regex(pattern.into()), l()).unwrap(), re.split(input).collect::<Vec<_>>());
            assert_eq!(replace(input, &rep(MatchMode::Regex(pattern.into()), "!"), l()).unwrap(), re.replace_all(input, "!"));
        }
    }
}
