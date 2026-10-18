use super::*;
fn receipt(s: &str) -> Option<Receipt<'_>> {
    parse(s.as_bytes(), s.len())
}
#[test]
fn complete_grammar_unicode_and_unique_root_identity_are_required() {
    for raw in [r#"{"id":"ok","x":[true,false,null,-12.5e+2,{"s":"é\uD83D\uDE00\n"}]}"#, r#" { "x":{}, "id":"ok" } "#] {
        let r = receipt(raw).unwrap();
        assert_eq!(r.id, "ok");
        assert!(!r.terminal);
        assert!(r.id.as_ptr() >= raw.as_ptr());
        assert!(r.id.as_ptr() < raw.as_ptr().wrapping_add(raw.len()));
        assert!(parse(raw.as_bytes(), raw.len() - 1).is_none());
    }
    for raw in [
        r#"{"id":"ok","id":"other"}"#,
        r#"{"x":{"id":"nested"}}"#,
        r#"{"id":null}"#,
        r#"{"id":"a\u0062"}"#,
        r#"{"\u0069d":"ok"}"#,
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
