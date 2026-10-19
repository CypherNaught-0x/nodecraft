use super::*;
use proptest::prelude::*;

fn wire(text: &str) -> Result<Json> {
    parse(text.as_bytes(), Limits::WIRE)
}

fn code(text: &str) -> Code {
    wire(text).unwrap_err().code
}

#[test]
fn the_profile_refuses_what_two_readers_could_read_differently() {
    assert_eq!(code(r#"{"a":1,"a":2}"#), Code::DuplicateKey);
    assert_eq!(code("1.5"), Code::BadNumber);
    assert_eq!(code("1e3"), Code::BadNumber);
    assert_eq!(code("-0"), Code::BadNumber);
    assert_eq!(code("9007199254740992"), Code::BadNumber);
    assert_eq!(wire("9007199254740991").unwrap(), Json::Int(MAX_SAFE_INTEGER));
    assert_eq!(wire("-9007199254740991").unwrap(), Json::Int(-MAX_SAFE_INTEGER));
    assert_eq!(code("01"), Code::Syntax);
    assert_eq!(code("\u{feff}{}"), Code::Syntax);
    assert_eq!(code(r#""\ud800""#), Code::BadString);
    assert_eq!(code(r#""\udc00""#), Code::BadString);
    assert_eq!(code("\"a\u{1}b\""), Code::BadString);
    assert_eq!(code("{} {}"), Code::Syntax);
    assert_eq!(code("[1,]"), Code::Syntax);
    assert_eq!(code(r#"{"a":1,}"#), Code::Syntax);
    assert_eq!(parse(b"\xff", Limits::WIRE).unwrap_err().code, Code::Syntax);
    assert_eq!(wire(r#""😀""#).unwrap(), Json::Str("😀".into()));
}

#[test]
fn depth_entries_and_sizes_are_bounded() {
    let deep = format!("{}{}", "[".repeat(32), "]".repeat(32));
    assert!(wire(&deep).is_ok());
    let deeper = format!("{}{}", "[".repeat(33), "]".repeat(33));
    assert_eq!(code(&deeper), Code::TooDeep);
    // Far deeper than the stack could take, refused at the limit.
    let hostile = "[".repeat(1_000_000);
    assert_eq!(code(&hostile), Code::TooDeep);
    let small = Limits { bytes: 16, depth: 4, string_bytes: 4, entries: 2, floats: false };
    assert_eq!(parse(b"[1,2,3]", small).unwrap_err().code, Code::TooLarge);
    assert_eq!(parse(br#""abcde""#, small).unwrap_err().code, Code::TooLarge);
    assert_eq!(parse(&[b' '; 17], small).unwrap_err().code, Code::TooLarge);
}

#[test]
fn canonical_form_is_fixed() {
    let value = wire("{ \"b\" : [true, null, -3], \"a\":\"x\\/y\\u00e9\\u2028\\u001f\\n\", \"A\": {} }").unwrap();
    assert_eq!(canonical(&value).unwrap(), "{\"A\":{},\"a\":\"x/yé\u{2028}\\u001f\\n\",\"b\":[true,null,-3]}");
    // Keys sort by UTF-8 bytes, not by UTF-16 units or locale.
    let value = wire(r#"{"é":1,"z":2,"😀":3,"ｚ":4}"#).unwrap();
    assert_eq!(canonical(&value).unwrap(), r#"{"z":2,"é":1,"ｚ":4,"😀":3}"#);
    let floats = parse(b"[1.5]", Limits { floats: true, ..Limits::WIRE }).unwrap();
    assert_eq!(canonical(&floats).unwrap_err().code, Code::NotCanonical);
}

#[test]
fn decimals_and_hex_are_canonical() {
    assert_eq!(decimal("0").unwrap(), 0);
    assert_eq!(decimal("18446744073709551615").unwrap(), u64::MAX);
    for bad in ["", "00", "01", "-1", "+1", "1.0", "18446744073709551616", " 1"] {
        assert_eq!(decimal(bad).unwrap_err().code, Code::BadNumber, "{bad}");
    }
    assert_eq!(hex::<2>("0aff").unwrap(), [0x0a, 0xff]);
    assert!(hex::<2>("0AFF").is_err());
    assert!(hex::<2>("0af").is_err());
    assert!(hex::<2>("0afé").is_err());
}

fn arb_json() -> impl Strategy<Value = Json> {
    let leaf = prop_oneof![
        Just(Json::Null),
        any::<bool>().prop_map(Json::Bool),
        (-MAX_SAFE_INTEGER..=MAX_SAFE_INTEGER).prop_map(Json::Int),
        any::<String>().prop_map(Json::Str),
    ];
    leaf.prop_recursive(6, 64, 8, |inner| {
        prop_oneof![
            prop::collection::vec(inner.clone(), 0..8).prop_map(Json::Array),
            prop::collection::btree_map(any::<String>(), inner, 0..8).prop_map(Json::Object),
        ]
    })
}

proptest! {
    #[test]
    fn any_bytes_are_an_error_or_a_value_never_a_panic(bytes in prop::collection::vec(any::<u8>(), 0..512)) {
        let _ = parse(&bytes, Limits::WIRE);
        let _ = parse(&bytes, Limits { floats: true, ..Limits::WIRE });
    }

    #[test]
    fn canonical_text_reads_back_to_itself(value in arb_json()) {
        let text = canonical(&value).unwrap();
        let back = parse(text.as_bytes(), Limits { string_bytes: usize::MAX, ..Limits::WIRE }).unwrap();
        prop_assert_eq!(&back, &value);
        prop_assert_eq!(canonical(&back).unwrap(), text);
    }
}
