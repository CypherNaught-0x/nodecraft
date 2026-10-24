// Shared test-only workload/report support. No product crate or public API.
use serde_json::{Value as Json, json};
use std::time::Duration;

pub const SIZES: [usize; 2] = [200, 1000];
pub const SHAPES: [&str; 4] = ["chain", "fan_in", "fan_out", "disconnected"];
pub const MAX_BYTES: usize = 8 * 1024 * 1024;
pub struct Config {
    pub warmups: usize,
    pub samples: usize,
}
impl Config {
    pub fn read() -> Self {
        let number = |name: &str, default, min, max| {
            let n = std::env::var(name).map(|v| v.parse::<usize>().expect("bounded integer configuration")).unwrap_or(default);
            assert!((min..=max).contains(&n), "configuration outside workload bounds");
            n
        };
        Self { warmups: number("NODECRAFT_PERF_WARMUPS", 3, 1, 16), samples: number("NODECRAFT_PERF_SAMPLES", 120, 3, 256) }
    }
}
pub fn ns(d: Duration) -> u64 {
    let n = u64::try_from(d.as_nanos()).expect("duration fits u64");
    assert!(n <= 3_600_000_000_000, "suspended/invalid measurement");
    n
}
pub fn edges(shape: &str, count: usize) -> Vec<(usize, usize)> {
    assert!((2..=1000).contains(&count));
    match shape {
        "chain" => (1..count).map(|n| (n - 1, n)).collect(),
        "fan_in" => (0..count - 1).map(|n| (n, count - 1)).collect(),
        "fan_out" => (1..count).map(|n| (0, n)).collect(),
        "grid" => {
            let cols = (1..=count).find(|c| c * c >= count).unwrap();
            (1..count).filter(|n| n % cols != 0).map(|n| (n - 1, n)).collect()
        }
        "rows" => (1..count).filter(|n| n % 10 != 0).map(|n| (n - 1, n)).collect(),
        "disconnected" => vec![],
        _ => panic!("unknown fixture topology"),
    }
}
#[allow(clippy::too_many_arguments)]
pub fn record(id: String, measurement: &str, shape: &str, nodes: usize, operation: &str, warmups: usize, samples: Vec<u64>, context: Json) -> Json {
    assert!((3..=256).contains(&samples.len()));
    json!({"id":id,"measurement":measurement,"shape":shape,"nodes":nodes,"operation":operation,
        "edges":edges(shape,nodes).len(),
        "warmups":warmups,"samples_ns":samples,"context":context,"perf2":null})
}
pub fn finish(records: Vec<Json>) {
    let bytes = serde_json::to_vec(&json!({"records":records})).unwrap();
    assert!(bytes.len() <= MAX_BYTES);
    if let Ok(path) = std::env::var("NODECRAFT_PERF_OUT") {
        std::fs::write(path, bytes).unwrap();
    } else {
        println!("{}", String::from_utf8(bytes).unwrap());
    }
}

/// Runtime guard: a const assertion would prevent ordinary debug CI from compiling ignored tests.
pub fn require_release() {
    assert!(!std::hint::black_box(cfg!(debug_assertions)), "use --release");
}
