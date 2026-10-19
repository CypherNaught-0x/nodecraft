use super::*;
use std::{cell::Cell, rc::Rc};
struct Counted {
    data: Cursor<Vec<u8>>,
    calls: Rc<Cell<usize>>,
    largest: Rc<Cell<usize>>,
}
impl Read for Counted {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        self.calls.set(self.calls.get() + 1);
        self.largest.set(self.largest.get().max(out.len()));
        self.data.read(out)
    }
}
fn counted(bytes: Vec<u8>) -> (Counted, Rc<Cell<usize>>, Rc<Cell<usize>>) {
    let calls = Rc::new(Cell::new(0));
    let largest = Rc::new(Cell::new(0));
    (Counted { data: Cursor::new(bytes), calls: calls.clone(), largest: largest.clone() }, calls, largest)
}
#[test]
fn materialization_checks_cap_before_reads_and_uses_bounded_complete_reads() {
    let (reader, calls, _) = counted(vec![]);
    assert!(materialize(reader, 256 * 1024 * 1024 + 1, Limits::default().max_input_bytes).is_err());
    assert_eq!(calls.get(), 0);
    let bytes = vec![7; CHUNK + 13];
    let (reader, _, largest) = counted(bytes.clone());
    assert_eq!(materialize(reader, bytes.len() as u64, bytes.len() as u64).unwrap(), bytes);
    assert_eq!(largest.get(), CHUNK);
    let (reader, _, _) = counted(vec![1]);
    assert!(materialize(reader, 2, 2).is_err());
    let (reader, _, _) = counted(vec![1, 2]);
    assert!(materialize(reader, 1, 1).is_err(), "exact EOF, not a valid prefix");
    assert!(materialize(Cursor::new(Vec::<u8>::new()), 0, 0).unwrap().is_empty());
}
#[test]
fn materialization_rejects_invalid_extents_and_hides_foreign_io_details() {
    struct Bad(bool);
    impl Read for Bad {
        fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
            if self.0 { Ok(out.len() + 1) } else { Err(io::Error::other("private foreign path")) }
        }
    }
    assert!(materialize(Bad(true), 1, 1).is_err());
    let error = materialize(Bad(false), 1, 1).unwrap_err();
    assert_eq!(error.kind, ErrorKind::Conflict);
    assert!(!error.message.contains("private"));
}
#[test]
fn interruption_budget_is_total_including_eof_probe() {
    struct Interrupted {
        before: usize,
        after: usize,
        sent: bool,
    }
    impl Read for Interrupted {
        fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
            let left = if self.sent { &mut self.after } else { &mut self.before };
            if *left > 0 {
                *left -= 1;
                return Err(io::ErrorKind::Interrupted.into());
            }
            if self.sent {
                return Ok(0);
            }
            self.sent = true;
            out[0] = 9;
            Ok(1)
        }
    }
    assert_eq!(materialize(Interrupted { before: 524, after: 500, sent: false }, 1, 1).unwrap(), [9]);
    assert!(materialize(Interrupted { before: 524, after: 501, sent: false }, 1, 1).is_err());
    assert!(materialize(Interrupted { before: 1025, after: 0, sent: false }, 1, 1).is_err());
}
fn session() -> Session {
    Session::new(nodecraft_graph::Project::new(nodecraft_types::ProjectId::from_bytes([4; 16]), "Tile ownership").unwrap()).unwrap()
}
#[test]
fn sixty_four_equal_tiles_have_one_shared_encoded_body() {
    let s = session();
    let mut planned = Tiles::new(&s, 64).unwrap();
    for _ in 0..64 {
        planned.admit(&s, vec![1; 4096], 1, 1).unwrap();
    }
    assert_eq!((planned.tiles.len(), planned.unique.len(), planned.bytes), (64, 1, 4096));
    assert!(planned.tiles.iter().all(|tile| tile.source == 0));
    let bytes = &planned.unique[0].bytes;
    assert_eq!(Arc::strong_count(bytes), 1);
    let memory = MemorySource::new(Arc::clone(bytes));
    assert_eq!(Arc::strong_count(bytes), 2);
    drop(memory);
    assert_eq!(Arc::strong_count(bytes), 1);
}
#[test]
fn encoded_table_refuses_beyond_exact_unique_and_per_tile_caps_without_partial_entry() {
    let s = session();
    let mut planned = Tiles::new(&s, 4).unwrap();
    for fill in [1, 2, 3] {
        planned.admit(&s, vec![fill; MAX_ASSET_BYTES], 1, 1).unwrap();
    }
    assert_eq!(planned.bytes, MAX_TOTAL_ASSET_BYTES as u64);
    assert!(planned.admit(&s, vec![4], 1, 1).is_err());
    assert_eq!((planned.tiles.len(), planned.unique.len()), (3, 3));
    assert!(planned.admit(&s, vec![5; MAX_ASSET_BYTES + 1], 1, 1).is_err());
    assert_eq!(planned.bytes, MAX_TOTAL_ASSET_BYTES as u64);
}
