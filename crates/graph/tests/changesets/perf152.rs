use super::*;
#[allow(dead_code)]
mod support {
    include!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../test-support/perf152.rs"));
}
use support::*;
fn fixture(shape: &str, count: usize) -> Vec<Change> {
    let id = |n: usize| NodeId::from_bytes(u128::try_from(n + 1).unwrap().to_be_bytes());
    let mut changes = Vec::new();
    for n in 0..count {
        let mut v = node(1);
        v.id = id(n);
        changes.push(Change::AddNode { canvas: canvas(), node: v });
    }
    for (n, (a, b)) in edges(shape, count).into_iter().enumerate() {
        changes.push(Change::Connect {
            canvas: canvas(),
            edge: Edge {
                id: EdgeId::from_bytes(u128::try_from(n + 1).unwrap().to_be_bytes()),
                from: Endpoint { node: id(a), port: "out".into() },
                to: Endpoint { node: id(b), port: "in".into() },
                order: u32::try_from(n).unwrap(),
            },
        });
    }
    changes
}
pub fn collect() {
    support::require_release();
    let config = Config::read();
    let mut records = vec![];
    for shape in SHAPES {
        for count in SIZES {
            let before = project();
            let changes = fixture(shape, count);
            let mut expected = before.clone();
            Changeset::new(changes.clone()).apply(&mut expected).unwrap();
            expected.validate().unwrap();
            let (mut apply, mut undo, mut redo) = (vec![], vec![], vec![]);
            for n in 0..config.warmups + config.samples {
                let mut p = before.clone();
                let set = Changeset::new(changes.clone());
                let start = std::time::Instant::now();
                let inverse = set.apply(&mut p).unwrap();
                let a = ns(start.elapsed());
                assert_eq!(p, expected);
                let start = std::time::Instant::now();
                let replay = inverse.apply(&mut p).unwrap();
                let u = ns(start.elapsed());
                assert_eq!(p, before);
                let start = std::time::Instant::now();
                replay.apply(&mut p).unwrap();
                let d = ns(start.elapsed());
                assert_eq!(p, expected);
                if n >= config.warmups {
                    apply.push(a);
                    undo.push(u);
                    redo.push(d);
                }
            }
            for (op, samples) in [("apply", apply), ("undo", undo), ("redo", redo)] {
                records.push(record(
                    format!("graph/{shape}/{count}/{op}"),
                    "graph_cpu",
                    shape,
                    count,
                    op,
                    config.warmups,
                    samples,
                    serde_json::json!({"boundary":"Changeset::apply; prepared changeset; validation/inverse included; fixture/verification excluded"}),
                ));
            }
        }
    }
    finish(records);
}
#[test]
fn perf152_all_shapes_are_valid_and_round_trip() {
    for shape in SHAPES {
        let mut p = project();
        let before = p.clone();
        let undo = Changeset::new(fixture(shape, 200)).apply(&mut p).unwrap();
        assert_eq!(p.canvases().get(&canvas()).unwrap().nodes().len(), 200);
        p.validate().unwrap();
        undo.apply(&mut p).unwrap();
        assert_eq!(p, before);
    }
}
