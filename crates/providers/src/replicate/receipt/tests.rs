use super::*;
fn receipt(s: &str) -> Option<Receipt> {
    parse(s.as_bytes(), s.len())
}
#[test]
fn complete_grammar_unicode_and_unique_root_identity_are_required() {
    for raw in [r#"{"id":"ok","x":[true,false,null,-12.5e+2,{"s":"é\uD83D\uDE00\n"}]}"#, r#" { "x":{}, "id":"ok" } "#] {
        let r = receipt(raw).unwrap();
        assert_eq!(r.id(), Some("ok"));
        assert!(!r.terminal);
        assert!(parse(raw.as_bytes(), raw.len() - 1).is_none());
    }
    for raw in [
        r#"{"id":"ok","id":"other"}"#,
        r#"{"x":{"id":"nested"}}"#,
        r#"{"id":null}"#,
        r#"[{"id":"ok"}]"#,
        r#"{"id":"ok"}false"#,
        r#"{"id":"ok",}"#,
        r#"{"id":"ok","x":[1,]}"#,
        r#"{"id":"ok","x":"\uD800"}"#,
        r#"{"id":"ok","x":"\uDC00"}"#,
        r#"{"id":"ok","x":"\uD800 \uDC00"}"#,
        r#"{"id":"ok","x":"\x00"}"#,
        r#"{"id":"ok","x":01}"#,
        r#"{"id":"ok","x":1.}"#,
        r#"{"id":"ok","x":1e+}"#,
        r#"{"id":"ok","x":NaN}"#,
        r#"{"id":"ok","x":+1}"#,
        r#"{"id":"ok","x":falsee}"#,
        r#"{"id":"ok","x":nul}"#,
        r#"{"id":"ok","x":"unterminated}"#,
    ] {
        assert!(receipt(raw).is_none(), "{raw}");
    }
    assert!(parse(b"{\"id\":\"ok\",\"x\":\"\xff\"}", 100).is_none());
    for id in ["", "../x", "space id", "é", &"a".repeat(129)] {
        assert!(receipt(&format!(r#"{{"id":"{id}"}}"#)).is_none());
    }
    assert!(receipt(&format!(r#"{{"id":"{}"}}"#, "a".repeat(128))).is_some());
}
#[test]
fn depth_keys_scalars_and_status_confirmation_are_bounded() {
    for (n, ok) in [(14, true), (15, false)] {
        let raw = format!(r#"{{"id":"ok","x":{}0{}}}"#, "[".repeat(n), "]".repeat(n));
        assert_eq!(receipt(&raw).is_some(), ok);
    }
    for (n, ok) in [(256, true), (257, false)] {
        assert_eq!(receipt(&format!(r#"{{"id":"ok","{}":0}}"#, "a".repeat(n))).is_some(), ok);
    }
    for (n, ok) in [(64, true), (65, false)] {
        assert_eq!(receipt(&format!(r#"{{"id":"ok","n":{}}}"#, "1".repeat(n))).is_some(), ok);
    }
    for status in ["succeeded", "failed", "canceled", "aborted"] {
        assert!(receipt(&format!(r#"{{"id":"ok","status":"{status}"}}"#)).unwrap().terminal);
    }
    for raw in [
        r#"{"id":"ok","status":"processing"}"#,
        r#"{"id":"ok","status":"canceled","status":"canceled"}"#,
        r#"{"id":"ok","status":null}"#,
        r#"{"id":"ok","status":"unknown"}"#,
    ] {
        assert!(!receipt(raw).unwrap().terminal);
    }
}

#[test]
fn escaped_ascii_identity_and_semantic_keys_have_fixed_complete_bounds() {
    for raw in [r#"{"id":"a\u0062"}"#, r#"{"\u0069\u0064":"\u0061b"}"#, r#"{"id":"\u0061\u0062"}"#] {
        assert_eq!(receipt(raw).unwrap().id(), Some("ab"));
    }
    let owned = {
        let raw = String::from(r#"{"id":"owned_1-2"}"#);
        receipt(&raw).unwrap()
    };
    assert_eq!(owned.id(), Some("owned_1-2"));
    for (n, ok) in [(128, true), (129, false)] {
        let raw = format!(r#"{{"id":"{}"}}"#, r"\u0061".repeat(n));
        assert_eq!(receipt(&raw).is_some(), ok);
        if ok {
            assert_eq!(receipt(&raw).unwrap().id(), Some("a".repeat(n).as_str()));
        }
    }
    for (suffix, ok) in [("aaaa", true), ("aaaaa", false)] {
        let raw = format!(r#"{{"id":"ok","{}{suffix}":0}}"#, r"\u0061".repeat(42));
        assert_eq!(receipt(&raw).is_some(), ok, "256 raw bytes includes escapes");
    }
    for raw in [
        r#"{"id":"ok","\u0069d":"ok"}"#,
        r#"{"\u0069d":"ok","id":"other"}"#,
        r#"{"\u0069d":"ok","\u0069\u0064":"ok"}"#,
        r#"{"x":{"\u0069d":"nested"}}"#,
        r#"{"id":"\u002f"}"#,
        r#"{"id":"\u005c"}"#,
        r#"{"id":"\u000a"}"#,
        r#"{"id":"\u00e9"}"#,
        r#"{"id":"\ud83d\ude00"}"#,
        r#"{"id":"ok","x":"\uD800"}"#,
        r#"{"id":"\u0061"} false"#,
        r#"{"id":"\u0061""#,
    ] {
        assert!(receipt(raw).is_none(), "{raw}");
    }
}
#[test]
fn escaped_status_is_exact_and_semantic_duplicates_cannot_prove_terminal() {
    for raw in [
        r#"{"id":"ok","\u0073tatus":"\u0063anceled"}"#,
        r#"{"id":"ok","status":"succe\u0065ded"}"#,
        r#"{"id":"ok","status":"\u0066ailed"}"#,
        r#"{"id":"ok","status":"\u0061borted"}"#,
    ] {
        assert!(receipt(raw).unwrap().terminal);
    }
    for raw in [
        r#"{"id":"ok","status":"canceled","\u0073tatus":"canceled"}"#,
        r#"{"id":"ok","\u0073tatus":"failed","status":"aborted"}"#,
        r#"{"id":"ok","status":"succeededX"}"#,
        r#"{"id":"ok","status":"\u0063anceled-extra"}"#,
        r#"{"id":"ok","status":"canc\u00e9led"}"#,
        r#"{"id":"ok","x":{"status":"canceled"}}"#,
    ] {
        assert!(!receipt(raw).unwrap().terminal, "{raw}");
    }
}
