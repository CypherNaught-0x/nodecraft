#![cfg(not(target_arch = "wasm32"))]
use nodecraft_engine::Session;
use nodecraft_format::{AssetStore, MemoryStorage, NativeStorage, ProjectStorage, Snapshot};
use nodecraft_graph::{Change, Position, Project};
use serde_json::json;
use std::{io::Read, time::Instant};
fn manifest_version(p: &Project) -> u64 {
    let bytes = nodecraft_format::encode(&Snapshot { project: p.clone(), assets: AssetStore::default(), epoch: 0 }).unwrap();
    let mut zip = zip::ZipArchive::new(std::io::Cursor::new(bytes)).unwrap();
    let mut manifest = String::new();
    zip.by_name("project.json").unwrap().read_to_string(&mut manifest).unwrap();
    serde_json::from_str::<serde_json::Value>(&manifest).unwrap().get("version").and_then(serde_json::Value::as_u64).unwrap()
}
#[allow(dead_code)]
mod support {
    include!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../test-support/perf152.rs"));
}
#[allow(dead_code)]
mod fixture {
    include!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../test-support/perf152_fixture.rs"));
}
use support::*;
fn project(shape: &str, count: usize) -> Project {
    let mut s = fixture::session(fixture::empty());
    fixture::build(&mut s, fixture::canvas(), shape, count, "perf.many_relay");
    assert_eq!(s.document().canvases().get(&fixture::canvas()).unwrap().edges().len(), edges(shape, count).len());
    s.document().validate().unwrap();
    s.document().clone()
}
fn dispatch(p: &Project, config: &Config) -> Vec<u64> {
    let mut s = fixture::session(p.clone());
    let mut expected = p.clone();
    Change::MoveNode { canvas: fixture::canvas(), id: fixture::id(0), pos: Position { x: 13., y: 17. } }.apply(&mut expected).unwrap();
    let request = json!({"canvas":fixture::canvas(),"id":fixture::id(0),"pos":{"x":13.,"y":17.}});
    let mut raw = vec![];
    for n in 0..config.warmups + config.samples {
        let start = Instant::now();
        let result = s.dispatch("node.move", &request);
        let elapsed = ns(start.elapsed());
        result.unwrap();
        assert_eq!(s.document(), &expected);
        assert!(s.can_undo());
        s.dispatch("edit.undo", &json!({})).unwrap();
        assert_eq!(s.document(), p);
        assert!(!s.can_undo());
        assert!(s.can_redo());
        if n >= config.warmups {
            raw.push(elapsed);
        }
    }
    raw
}
fn storage(p: &Project, config: &Config, native: bool) -> (Vec<u64>, Vec<u64>) {
    let dir = tempfile::tempdir().unwrap();
    let host: Box<dyn ProjectStorage> =
        if native { Box::new(NativeStorage::new(dir.path().join("recovery")).unwrap()) } else { Box::<MemoryStorage>::default() };
    let mut s = Session::with_storage(Snapshot { project: p.clone(), assets: AssetStore::default(), epoch: 0 }, host).unwrap();
    let location = if native { dir.path().join("fixture.ncraft").to_str().unwrap().to_owned() } else { "fixture.ncraft".into() };
    s.dispatch("file.save_as", &json!({"location":location})).unwrap();
    assert!(!s.file_status().dirty);
    let (mut saves, mut loads) = (vec![], vec![]);
    for n in 0..config.warmups + config.samples {
        let start = Instant::now();
        let result = s.dispatch("file.save", &json!({}));
        let save = ns(start.elapsed());
        assert_eq!(result.unwrap().get("saved"), Some(&json!(true)));
        assert!(!s.file_status().dirty);
        assert!(!s.file_status().recovery_required);
        assert_eq!(s.document(), p);
        // Same-owner warm reopen. Includes native freeze/verification/checkpoint, not cold disk.
        let start = Instant::now();
        let result = s.dispatch("file.open", &json!({"location":location,"discard_dirty":true}));
        let load = ns(start.elapsed());
        result.unwrap();
        assert_eq!(s.document(), p);
        assert_eq!(s.assets().len(), 0);
        assert!(!s.can_undo());
        assert!(!s.can_redo());
        assert!(!s.file_status().dirty);
        if n >= config.warmups {
            saves.push(save);
            loads.push(load);
        }
    }
    drop(s);
    drop(dir);
    (saves, loads)
}
#[test]
#[ignore = "bounded release regression workload; invoke exact filter through cargo xtask perf collect"]
fn perf152_hot_paths() {
    support::require_release();
    let config = Config::read();
    let mut records = vec![];
    for shape in SHAPES {
        for count in SIZES {
            let p = project(shape, count);
            records.push(record(
                format!("engine/{shape}/{count}/dispatch"),
                "dispatch_cpu",
                shape,
                count,
                "dispatch",
                config.warmups,
                dispatch(&p, &config),
                json!({"boundary":"registered node.move dispatch; ephemeral Session; undo/reset/verification excluded","assets":0}),
            ));
            for native in [false, true] {
                let category = if native { "native_storage_wall" } else { "memory_storage_wall" };
                let (save, load) = storage(&p, &config, native);
                for (op, raw) in [("save", save), ("load", load)] {
                    records.push(record(format!("{category}/{shape}/{count}/{op}"),category,shape,count,op,config.warmups,raw,json!({"boundary":"synchronous engine file command, including publication/checkpoint; setup/verification excluded","cache":"warm same-owner","assets":0,"family":"classic-small","manifest_version":manifest_version(&p),"durability":if native{"native host fsync/atomic publication policy"}else{"memory byte host; no device persistence"}})));
                }
            }
        }
    }
    finish(records);
}
#[test]
fn perf152_fixture_shapes_dispatch_and_storage_round_trip() {
    let config = Config { warmups: 1, samples: 3 };
    for shape in SHAPES {
        let p = project(shape, 20);
        assert!(manifest_version(&p) >= 1);
        assert_eq!(dispatch(&p, &config).len(), 3);
        for native in [false, true] {
            let (a, b) = storage(&p, &config, native);
            assert_eq!((a.len(), b.len()), (3, 3));
        }
    }
}
