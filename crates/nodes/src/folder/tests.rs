use super::*;
use nodecraft_types::{AssetRef, Blake3, MediaMeta, NodeId};

fn image(n: u8) -> Value {
    Value::Media(AssetRef { hash: Blake3::from_bytes([n; 32]), mime: "image/png".parse().unwrap(), meta: MediaMeta::default() })
}

fn folder(names: &[&str], params: &[(&str, Value)]) -> Node {
    let files = names.iter().enumerate().map(|(n, name)| ((*name).to_owned(), image(u8::try_from(n).unwrap()))).collect();
    let mut node = kind().instantiate_instance(NodeId::from_bytes([1; 16]), config(Modality::Image, "shots", files).unwrap(), &Default::default()).unwrap();
    for (name, value) in params {
        node.params.insert((*name).into(), value.clone());
    }
    kind().validate_instance(&node, true).unwrap();
    node
}

fn text(value: &str) -> Value {
    Value::Text(value.into())
}

fn number(n: u64) -> Value {
    Value::Unsigned(Unsigned(n))
}

fn names(node: &Node) -> Vec<String> {
    selected(node).unwrap().iter().map(|entry| entry.name.to_owned()).collect()
}

#[test]
fn ports_loop_and_list_with_the_folder_modality() {
    let node = folder(&["a.png"], &[]);
    let ports: Vec<_> = node.outputs.iter().map(|p| (p.name.as_str(), p.port_type)).collect();
    let (pictures, words) = (Modality::Image, Modality::Text);
    assert_eq!(
        ports,
        [
            (FILE, PortType { modality: pictures, many: false }),
            (NAME, PortType { modality: words, many: false }),
            (ALL_FILES, PortType { modality: pictures, many: true }),
            (ALL_NAMES, PortType { modality: words, many: true }),
        ]
    );
    assert!(loops_over(&node, FILE) && loops_over(&node, NAME) && !loops_over(&node, ALL_FILES) && !loops_over(&node, ALL_NAMES));
    assert_eq!(folder_name(&node), "shots");
    let words = kind()
        .instantiate_instance(NodeId::from_bytes([2; 16]), config(Modality::Text, "notes", vec![("a.txt".into(), text("hello"))]).unwrap(), &Default::default())
        .unwrap();
    assert_eq!(words.outputs.first().map(|p| p.port_type.modality), Some(Modality::Text));
    assert_eq!(values(&words, FILE).unwrap(), [text("hello")]);
    // An empty folder from the palette holds images.
    let empty = kind().instantiate_instance(NodeId::from_bytes([3; 16]), Default::default(), &Default::default()).unwrap();
    assert_eq!(modality(&empty).unwrap(), Modality::Image);
    assert!(stored(&empty).unwrap().is_empty());
}

#[test]
fn natural_order_is_the_default_and_other_orders_apply() {
    let files = ["img10.png", "img2.png", "Img1.png", "b/img3.png"];
    assert_eq!(names(&folder(&files, &[])), ["b/img3.png", "Img1.png", "img2.png", "img10.png"]);
    assert_eq!(names(&folder(&files, &[("sort", text("name"))])), ["b/img3.png", "Img1.png", "img10.png", "img2.png"]);
    assert_eq!(names(&folder(&files, &[("sort", text("imported"))])), files);
    assert_eq!(names(&folder(&files, &[("reverse", Value::Bool(true))])), ["img10.png", "img2.png", "Img1.png", "b/img3.png"]);
    assert_eq!(natural("a01", "a1"), Ordering::Equal);
    assert_eq!(natural("a9", "a10"), Ordering::Less);
    assert_eq!(natural("a", "a1"), Ordering::Less);
    assert_eq!(natural("99999999999999999999999999", "100000000000000000000000000"), Ordering::Less);
}

#[test]
fn shuffle_is_stable_per_seed_and_independent_of_import_order() {
    let files: Vec<String> = (0..20).map(|n| format!("{n}.png")).collect();
    let refs: Vec<&str> = files.iter().map(String::as_str).collect();
    let reversed: Vec<&str> = refs.iter().rev().copied().collect();
    let shuffled = |files: &[&str], seed| names(&folder(files, &[("sort", text("shuffle")), ("seed", number(seed))]));
    let once = shuffled(&refs, 7);
    assert_eq!(once, shuffled(&refs, 7));
    assert_eq!(once, shuffled(&reversed, 7));
    assert_ne!(once, shuffled(&refs, 8));
    assert_ne!(once, names(&folder(&refs, &[])));
    let mut sorted = once.clone();
    sorted.sort_by(|a, b| natural(a, b));
    assert_eq!(sorted, names(&folder(&refs, &[])), "a shuffle keeps every file once");
}

#[test]
fn filters_and_ranges_choose_files() {
    let files = ["a.png", "b.JPG", "c.png", "d.webp", "raw/e.png", "f.png"];
    assert_eq!(names(&folder(&files, &[("include", text("*.png"))])), ["a.png", "c.png", "f.png", "raw/e.png"]);
    assert_eq!(names(&folder(&files, &[("include", text(" *.jpg ; *.webp ;"))])), ["b.JPG", "d.webp"]);
    assert_eq!(names(&folder(&files, &[("include", text("raw/*"))])), ["raw/e.png"]);
    assert_eq!(names(&folder(&files, &[("exclude", text("?.png"))])), ["b.JPG", "d.webp"]);
    assert_eq!(names(&folder(&files, &[("skip", number(1)), ("every", number(2)), ("limit", number(2))])), ["b.JPG", "d.webp"]);
    assert_eq!(names(&folder(&files, &[("limit", number(1)), ("reverse", Value::Bool(true))])), ["raw/e.png"]);
    let none = folder(&files, &[("include", text("*.gif"))]);
    assert!(values(&none, FILE).unwrap().is_empty());
    assert!(items(&none, FILE).unwrap_err().message.contains("matches its filters"));
    assert!(kind().plan(&PlanCtx { targets: &nodecraft_types::EMPTY_TARGET_SELECTION, node: &none, inputs: &[] }).is_err());
    let empty = kind().instantiate_instance(NodeId::from_bytes([3; 16]), Default::default(), &Default::default()).unwrap();
    assert!(items(&empty, NAME).unwrap_err().message.contains("no files yet"));
}

#[test]
fn wildcards_match_without_case_and_without_blowing_up() {
    assert!(wildcard("*.PNG", "shot.png"));
    assert!(wildcard("s?ot*", "Shot 1.png"));
    assert!(wildcard("*", ""));
    assert!(!wildcard("?", ""));
    assert!(wildcard("a*b*c", "aXbYbZc"));
    assert!(!wildcard("a*b*c", "aXbYbZ"));
    let long = "a".repeat(MAX_NAME_BYTES);
    let pattern = "*a".repeat(MAX_PATTERN_BYTES / 2);
    assert!(wildcard(&pattern, &long));
    assert!(!wildcard(&(pattern + "b"), &long));
}

#[test]
fn names_follow_their_format_and_plan_gives_first_and_all() {
    let files = ["sub/x.final.png", ".hidden", "y.png"];
    let node = folder(&files, &[]);
    assert_eq!(values(&node, ALL_NAMES).unwrap(), [text(".hidden"), text("x.final.png"), text("y.png")]);
    let stems = folder(&files, &[("name_format", text("stem"))]);
    assert_eq!(values(&stems, NAME).unwrap(), [text(".hidden"), text("x.final"), text("y")]);
    let paths = folder(&files, &[("name_format", text("path"))]);
    assert_eq!(values(&paths, NAME).unwrap(), [text(".hidden"), text("sub/x.final.png"), text("y.png")]);
    let jobs = kind().plan(&PlanCtx { targets: &nodecraft_types::EMPTY_TARGET_SELECTION, node: &node, inputs: &[] }).unwrap();
    let on = |port: &str| jobs.iter().filter(|job| matches!(job, JobSpec::Source { output, .. } if output == port)).count();
    assert_eq!((on(FILE), on(NAME), on(ALL_FILES), on(ALL_NAMES)), (1, 1, 3, 3));
    assert_eq!(pairing(&node).unwrap(), crate::batch::Pairing::Cross);
    assert_eq!(pairing(&folder(&files, &[("pairing", text("zip"))])).unwrap(), crate::batch::Pairing::Zip);
}

#[test]
fn bad_stored_files_are_refused() {
    assert!(config(Modality::Image, "x", vec![("a".into(), text("not an image"))]).is_err());
    assert!(config(Modality::Text, "x", vec![("a".into(), image(1))]).is_err());
    assert!(config(Modality::Image, "x", vec![("a".into(), image(1)), ("a".into(), image(2))]).is_err(), "names are unique");
    assert!(config(Modality::Image, "x", vec![("".into(), image(1))]).is_err());
    assert!(config(Modality::Image, "x", vec![("a\nb".into(), image(1))]).is_err());
    assert!(config(Modality::Image, "x", vec![("a".repeat(MAX_NAME_BYTES + 1), image(1))]).is_err());
    let many = (0..=MAX_FILES).map(|n| (format!("{n}"), image(0))).collect();
    assert!(config(Modality::Image, "x", many).is_err());
    assert!(config(Modality::Style, "x", Vec::new()).is_err());
    let mut node = folder(&["a.png", "b.png"], &[]);
    node.config.insert(NAMES.into(), Value::List(vec![text("a.png")]));
    assert!(stored(&node).is_err());
    node.config.insert(NAMES.into(), text("a.png"));
    assert!(stored(&node).is_err());
    let mut bad = folder(&["a.png"], &[]);
    bad.params.insert("sort".into(), text("size"));
    assert!(kind().validate_instance(&bad, true).is_err());
    assert!(selected(&bad).is_err());
    bad.params.insert("sort".into(), number(1));
    assert!(selected(&bad).is_err());
}
