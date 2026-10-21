use super::*;

fn id(n: u128) -> NodeId {
    format!("00000000-0000-0000-0000-{n:012x}").parse().unwrap()
}

#[test]
fn a_token_round_trips_and_keeps_out_what_it_cant_hold() {
    let token = token("Story (draft) [v2]", id(1));
    assert_eq!(token, format!("@[Story draft v2]({})", id(1)));
    let text = format!("Paint {token}, twice: {token}.");
    let found = tokens(&text);
    assert_eq!(found.len(), 2);
    assert_eq!(found[0].title, "Story draft v2");
    assert_eq!(found[0].id, id(1));
    assert_eq!(text.get(found[0].start..found[0].end), Some(token.as_str()));
    assert_eq!(ids(&text), [id(1)]);
    assert_eq!(display(&text), "Paint @Story draft v2, twice: @Story draft v2.");
    assert_eq!(super::token("  ", id(2)), format!("@[card]({})", id(2)));
    let long: String = "é".repeat(200);
    assert_eq!(super::token(&long, id(3)).chars().filter(|c| *c == 'é').count(), MAX_TITLE_CHARS);
}

#[test]
fn text_that_only_looks_like_a_token_stays_text() {
    for text in [
        "mail me @[me](not-an-id)",
        "@[unclosed",
        "@[a](00000000-0000-0000-0000-000000000001",
        "@[two\nlines](00000000-0000-0000-0000-000000000001)",
        "@[a] (00000000-0000-0000-0000-000000000001)",
        "@[é",
        "@[",
        "@",
    ] {
        assert!(tokens(text).is_empty(), "{text}");
        assert_eq!(display(text), text);
        assert_eq!(substitute(text, &BTreeMap::new()).unwrap(), text);
    }
    // A broken token before a good one doesn't hide it.
    let text = format!("@[x](nope) and {}", token("A", id(1)));
    assert_eq!(ids(&text), [id(1)]);
}

#[test]
fn substitution_follows_first_mention_order() {
    let (a, b) = (token("A", id(1)), token("B", id(2)));
    let text = format!("{b} meets {a}, then {b} again");
    assert_eq!(ids(&text), [id(2), id(1)]);
    let values = BTreeMap::from([(id(1), "ay"), (id(2), "bee")]);
    assert_eq!(substitute(&text, &values).unwrap(), "bee meets ay, then bee again");
    let error = substitute(&text, &BTreeMap::from([(id(2), "bee")])).unwrap_err();
    assert_eq!(error.kind, ErrorKind::InvalidInput);
    assert!(error.message.contains("@A"), "{}", error.message);
}

#[test]
fn mentions_are_bounded() {
    let text: String = (0..40u128).map(|n| token("x", id(n))).collect::<Vec<_>>().join(" ");
    let ids = ids(&text);
    assert_eq!(ids.len(), MAX_MENTIONS);
    let texts: Vec<String> = (0..MAX_MENTIONS).map(|n| format!("<{n}>")).collect();
    let values: BTreeMap<NodeId, &str> = ids.iter().copied().zip(texts.iter().map(String::as_str)).collect();
    let out = substitute(&text, &values).unwrap();
    assert!(out.starts_with("<0> <1>"));
    // Past the limit a token stays as written.
    assert!(out.ends_with(&token("x", id(39))));
    let many = "@[".repeat(100_000);
    assert!(tokens(&many).is_empty());
    assert!(too_many(&text), "40 cards");
    assert!(!too_many(&ids.iter().map(|id| token("x", *id)).collect::<String>()), "16 cards");
    let repeated = token("x", id(1)).repeat(MAX_TOKENS);
    assert!(!too_many(&repeated));
    assert!(too_many(&format!("{repeated}{}", token("x", id(1)))), "one token past the scan limit");
}

#[test]
fn substitution_is_capped() {
    let text = token("Big", id(1)).repeat(20);
    let big = "x".repeat(MAX_PROMPT_BYTES / 10);
    let error = substitute(&text, &BTreeMap::from([(id(1), big.as_str())])).unwrap_err();
    assert_eq!(error.kind, ErrorKind::InvalidInput);
    assert!(error.message.contains("longer than 1 MiB"), "{}", error.message);
    let fits = substitute(&token("Big", id(1)).repeat(9), &BTreeMap::from([(id(1), big.as_str())])).unwrap();
    assert_eq!(fits.len(), big.len() * 9);
}

#[test]
fn an_edited_token_is_counted_as_broken() {
    let whole = token("Story", id(1));
    assert_eq!(broken(&format!("{whole} and {whole}")), 0);
    let edited = whole.replacen("](", "] (", 1);
    assert_eq!(broken(&format!("{whole} and {edited}")), 1);
    assert_eq!(broken("plain text"), 0);
}

#[test]
fn the_start_of_a_display_matches_the_whole_display() {
    let text = format!("é {} and @[broken {} end", token("Story", id(1)), token("Ünï", id(2)));
    let whole = display(&text);
    for chars in 0..whole.chars().count() + 3 {
        let start = display_start(&text, chars);
        assert_eq!(start, whole.chars().take(chars).collect::<String>(), "{chars} chars");
    }
    let long = format!("{}{}", "x".repeat(MAX_PROMPT_BYTES), token("Late", id(3)));
    assert_eq!(display_start(&long, 4), "xxxx");
}
