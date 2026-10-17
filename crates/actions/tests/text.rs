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
    assert_eq!(replace("aaa", &rep(MatchMode::Literal("a".into()), ""), tiny).unwrap(), "");
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
    let request = TextRequest::Split { input: "é".into(), mode: SplitMode::Chars(1), max_parts: MAX_ITEMS };
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
    assert_eq!(replace(&input, &options, l()).unwrap(), "");
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

#[test]
fn ordinary_large_regex_and_literal_workloads_remain_usable() {
    let input = format!("{}\n", "a".repeat(63)).repeat(4096);
    assert_eq!(input.len(), 256 * 1024);
    assert_eq!(split(&input, &SplitMode::Regex("\n".into()), l()).unwrap().len(), 4097);
    let input = format!("{} ", "a".repeat(49)).repeat(4000);
    assert_eq!(replace(&input, &rep(MatchMode::Regex(r"\s+".into()), " "), l()).unwrap(), input);
    let mut input = format!("{}\n", "a".repeat(63)).repeat(16_383);
    input.push_str(&"a".repeat(62));
    assert_eq!(input.len() + 2, MAX_INPUT_BYTES); // Pattern and replacement share the input cap.
    assert_eq!(split(&input, &SplitMode::Regex("\n".into()), l()).unwrap().len(), MAX_ITEMS);
    assert_eq!(replace(&input, &rep(MatchMode::Regex("\n".into()), " "), l()).unwrap(), input.replace('\n', " "));
    let input = "\n".repeat(MAX_INPUT_BYTES - 1);
    assert_eq!(replace(&input, &rep(MatchMode::Literal("\n".into()), ""), Limits { items: 0, ..l() }).unwrap(), "");
    let input = "a".repeat(16_384);
    assert_eq!(replace(&input, &rep(MatchMode::Regex("a+b|a".into()), ""), l()), Err(TextError::WorkLimit));
}
#[test]
fn whole_word_matches_punctuation_edges_and_preserves_valid_alternatives() {
    for term in ["C#", "C++", "#tag", "e.g.", "é#", "#猫"] {
        let input = format!("{term} ({term}) x{term} {term}x _{term} {term}_ 猫{term} {term}\u{301}");
        let expected = format!("! (!) x{term} {term}x _{term} {term}_ 猫{term} {term}\u{301}");
        for pattern in [MatchMode::Literal(term.into()), MatchMode::Regex(format!("({})", regex::escape(term)))] {
            let mut options = rep(pattern, "!");
            options.whole_word = true;
            assert_eq!(replace(&input, &options, l()).unwrap(), expected);
        }
    }
    let mut options = rep(MatchMode::Regex(r"(C\+\+)".into()), "<$1>");
    options.whole_word = true;
    assert_eq!(replace("C++ xC++ C++x", &options, l()).unwrap(), "<C++> xC++ C++x");
    let mut alternatives = rep(MatchMode::Regex("(foo|foobar)".into()), "<$1>");
    alternatives.whole_word = true;
    assert_eq!(replace("foobar foo xfoobar", &alternatives, l()).unwrap(), "<foobar> <foo> xfoobar");
    let mut options = rep(MatchMode::Literal("ß".into()), "!");
    options.case_sensitive = false;
    assert_eq!(replace("ß SS", &options, l()).unwrap(), "! SS");
}
#[test]
fn replacement_validation_is_eager_and_expansion_work_is_bounded() {
    for input in ["a", "b", ""] {
        for replacement in ["${", "${name", "${}", "${a{b}"] {
            assert_eq!(replace(input, &rep(MatchMode::Regex("a".into()), replacement), l()), Err(TextError::InvalidReplacement));
        }
    }
    assert_eq!(replace("a", &rep(MatchMode::Literal("a".into()), "${"), l()).unwrap(), "${");
    let options = rep(MatchMode::Regex("(a?)".into()), &"$1".repeat(MAX_ITEMS));
    assert_eq!(replace(&"b".repeat(65), &options, l()), Err(TextError::WorkLimit));
}

#[test]
fn documented_schema_wire_fixtures_round_trip_and_execute() {
    #[derive(serde::Deserialize)]
    struct Case {
        name: String,
        request: serde_json::Value,
        canonical: serde_json::Value,
        result: serde_json::Value,
    }
    let cases: Vec<Case> = serde_json::from_str(include_str!("fixtures/text_requests.json")).unwrap();
    for case in cases {
        let request: TextRequest = serde_json::from_value(case.request).unwrap();
        assert_eq!(serde_json::to_value(&request).unwrap(), case.canonical, "{}", case.name);
        let canonical: TextRequest = serde_json::from_value(case.canonical).unwrap();
        assert_eq!(request, canonical, "{}", case.name);
        assert_eq!(serde_json::to_value(request.execute(l()).unwrap()).unwrap(), case.result, "{}", case.name);
    }
}

#[test]
fn persisted_maximum_parts_intersects_host_budget_without_truncating() {
    let request = |input: &str, mode: SplitMode, max_parts| TextRequest::Split { input: input.into(), mode, max_parts };
    let parts = request("a,b", SplitMode::Separator(",".into()), 2);
    assert_eq!(parts.execute(l()).unwrap(), TextResult::Parts(vec!["a".into(), "b".into()]));
    assert_eq!(parts.execute(Limits { items: 1, ..l() }), Err(TextError::ItemLimit));
    assert_eq!(request("a,b,c", SplitMode::Separator(",".into()), 2).execute(l()), Err(TextError::ItemLimit));
    assert_eq!(request("a,b", SplitMode::Separator(",".into()), 1).execute(l()), Err(TextError::ItemLimit));
    assert_eq!(parts.execute(Limits { items: 0, ..l() }), Err(TextError::ItemLimit));
    for mode in [SplitMode::Chars(1), SplitMode::Lines(1), SplitMode::Paragraphs] {
        assert_eq!(request("", mode, 1).execute(Limits { items: 0, ..l() }).unwrap(), TextResult::Parts(vec![]));
    }
    for max_parts in [0, MAX_ITEMS + 1, usize::MAX] {
        assert_eq!(request("", SplitMode::Regex("[".into()), max_parts).execute(l()), Err(TextError::InvalidMaxParts));
    }
    let exact = request(&"x".repeat(MAX_ITEMS), SplitMode::Chars(1), MAX_ITEMS);
    let TextResult::Parts(values) = exact.execute(l()).unwrap() else { panic!("parts") };
    assert_eq!(values.len(), MAX_ITEMS);
    assert_eq!(request(&"x".repeat(MAX_ITEMS + 1), SplitMode::Chars(1), MAX_ITEMS).execute(l()), Err(TextError::ItemLimit));
    assert_eq!(parts.execute(Limits { items: MAX_ITEMS + 1, ..l() }), Err(TextError::InvalidLimits));
}

#[test]
fn schema_required_null_and_unknown_field_policies_match_wire() {
    use serde_json::json;
    let base = json!({"action":"text.split","input":"","mode":{"mode":"chars","value":1},"max_parts":1});
    for value in [json!(null), json!(-1), json!(1.5), json!("1")] {
        let mut invalid = base.clone();
        invalid["max_parts"] = value;
        assert!(serde_json::from_value::<TextRequest>(invalid).is_err());
    }
    for field in ["input", "mode", "action"] {
        let mut invalid = base.clone();
        invalid.as_object_mut().unwrap().remove(field);
        assert!(serde_json::from_value::<TextRequest>(invalid).is_err());
    }
    let mut invalid = base.clone();
    invalid["surprise"] = json!(true);
    assert!(serde_json::from_value::<TextRequest>(invalid).is_err());
    let mut invalid = base.clone();
    invalid["mode"]["surprise"] = json!(true);
    assert!(serde_json::from_value::<TextRequest>(invalid).is_err());
    let replacement = json!({"action":"text.replace","input":"x","options":{"pattern":{"mode":"literal","value":"x"},"replacement":"","case_sensitive":true,"whole_word":false}});
    for field in ["pattern", "replacement", "case_sensitive", "whole_word"] {
        for null in [false, true] {
            let mut invalid = replacement.clone();
            if null {
                invalid["options"][field] = json!(null);
            } else {
                invalid["options"].as_object_mut().unwrap().remove(field);
            }
            assert!(serde_json::from_value::<TextRequest>(invalid).is_err());
        }
    }
    let concat = json!({"action":"text.concat","inputs":[],"options":{"separator":"","prefix":"","suffix":"","trim":false,"skip_empty":false}});
    for field in ["separator", "prefix", "suffix", "trim", "skip_empty"] {
        let mut invalid = concat.clone();
        invalid["options"].as_object_mut().unwrap().remove(field);
        assert!(serde_json::from_value::<TextRequest>(invalid).is_err(), "ConcatOptions::default is not a serde omission default");
    }
    for request in [
        json!({"action":"text.template","template":"","fields":null}),
        json!({"action":"text.template","template":""}),
        json!({"action":"text.template","template":"","fields":{"name":null}}),
        json!({"action":"text.template","template":"","fields":{},"missing_variable":"empty"}),
    ] {
        assert!(serde_json::from_value::<TextRequest>(request).is_err());
    }
}

#[test]
fn ordinary_regex_sequences_accept_near_ceiling_documents() {
    for (pattern, unit) in [(r",\s*", "entry, "), (r"\s*,\s*", "entry , "), (r"v(\d+)", "v123456 "), (r"\n\s*\n", "paragraph\n \n")] {
        let count = 8192;
        let padding = "x".repeat((MAX_INPUT_BYTES - pattern.len() - 1 - unit.len() * count) / count);
        let input = format!("{padding}{unit}").repeat(count);
        assert!(input.len() > 900_000);
        let reference = regex::Regex::new(pattern).unwrap();
        assert_eq!(split(&input, &SplitMode::Regex(pattern.into()), l()).unwrap(), reference.split(&input).collect::<Vec<_>>());
        assert_eq!(replace(&input, &rep(MatchMode::Regex(pattern.into()), "$1"), l()).unwrap(), reference.replace_all(&input, "$1"));
    }
    let input = "a ".repeat(100_000);
    assert_eq!(replace(&input, &rep(MatchMode::Regex(r"\s+".into()), ""), Limits { items: 0, ..l() }).unwrap(), "a".repeat(100_000));
    for pattern in ["a+b|a", "a*a*b|a", "(?:a+)+b|a"] {
        assert_eq!(replace(&"a".repeat(16384), &rep(MatchMode::Regex(pattern.into()), ""), l()), Err(TextError::WorkLimit));
    }
}

#[test]
fn replacement_step_limit_has_an_exact_boundary_without_an_item_limit() {
    let options = rep(MatchMode::Regex("a".into()), "$0$0");
    let exact = "a".repeat(MAX_REPLACEMENT_STEPS / 2);
    assert_eq!(replace(&exact, &options, Limits { items: 0, ..l() }).unwrap().len(), MAX_REPLACEMENT_STEPS);
    assert_eq!(replace(&(exact + "a"), &options, l()), Err(TextError::WorkLimit));
}

#[test]
fn near_ceiling_whole_word_regex_preserves_results() {
    for (pattern, word) in [(r"fo\w*", "foobar"), (r"\d+", "123456"), ("[A-Z][a-z]+", "Foobar"), (r"\w+", "éclair")] {
        let input = format!("{word} ").repeat((MAX_INPUT_BYTES - pattern.len() - 1) / (word.len() + 1));
        assert!(input.len() > MAX_INPUT_BYTES - 64);
        let mut options = rep(MatchMode::Regex(pattern.into()), "!");
        options.whole_word = true;
        assert_eq!(replace(&input, &options, l()).unwrap(), "! ".repeat(input.len() / (word.len() + 1)));
    }
}

#[test]
fn boundary_and_multiline_assertions_accept_large_inputs() {
    for (pattern, word, ending) in
        [(r"\bfo\w*\b", "foobar", " "), (r"(?m)^\w+", "éclair", "\n"), (r"(?m)\w+$", "éclair", "\n"), (r"\b{start-half}\w+\b{end-half}", "éclair", " ")]
    {
        let unit = format!("{word}{ending}");
        let count = (MAX_INPUT_BYTES - pattern.len() - 1) / unit.len();
        let input = unit.repeat(count);
        assert_eq!(replace(&input, &rep(MatchMode::Regex(pattern.into()), "!"), l()).unwrap(), format!("!{ending}").repeat(count));
    }
    for pattern in [r"^\w+$", r"\A\w+\z", r"\w+\b{end-half}$"] {
        let input = "é".repeat((MAX_INPUT_BYTES - pattern.len() - 1) / 2);
        assert_eq!(replace(&input, &rep(MatchMode::Regex(pattern.into()), "!"), l()).unwrap(), "!");
    }
    for input in ["", "é foobar 中", "éfoobar 中", "e\u{301}foobar", "foobar\u{301} 中", "🦀foobar🦀", "foo\nbar\n"] {
        for pattern in [r"\bfo\w*\b", r"(?m)^\w+", r"(?m)\w+$", r"\b", r"\w*\b", r"(?m)[a\n]*^", r"\w+\B", r"\A\w+\z"] {
            let re = regex::Regex::new(pattern).unwrap();
            assert_eq!(replace(input, &rep(MatchMode::Regex(pattern.into()), "!"), l()).unwrap(), re.replace_all(input, "!"));
            assert_eq!(split(input, &SplitMode::Regex(pattern.into()), l()).unwrap(), re.split(input).collect::<Vec<_>>());
        }
    }
    let mut captured = rep(MatchMode::Regex(r"(?P<word>fo\w*)".into()), "${word}!");
    captured.whole_word = true;
    assert_eq!(replace("éfoobar foobar🦀 foobar\u{301}", &captured, l()).unwrap(), "éfoobar foobar!🦀 foobar\u{301}!");
    let input = "a ".repeat(100_000);
    assert_eq!(replace(&input, &rep(MatchMode::Regex(r"^\w+z$".into()), "!"), l()).unwrap(), input);
    assert_eq!(replace(&input, &rep(MatchMode::Regex(r"\b".into()), ""), l()).unwrap(), input);
}
