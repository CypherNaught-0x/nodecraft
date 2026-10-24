use super::*;

fn fragmented() -> Arc<[u8]> {
    Arc::from(b"\0\0\0\x18ftypisom\0\0\0\0isomiso2".as_slice())
}

#[test]
fn unavailable_tools_and_expired_probes_leave_the_file_importable() {
    let ctx = egui::Context::default();
    let mut videos = Videos::default();
    let mut probe = Probe::new(fragmented()).unwrap();
    assert!(matches!(probe.poll(&mut videos, &ctx), Some(None)));
    let mut probe = Probe::new(fragmented()).unwrap();
    probe.started = Instant::now().checked_sub(WAIT).unwrap();
    assert!(matches!(probe.poll(&mut videos, &ctx), Some(None)));
    assert!(probe.job.is_none());
    assert!(Probe::new(Arc::from(b"Public note".as_slice())).is_none());
}
