//! Native 3D captures share one serial capturer; browser GPU handles stay on their owning thread.
use nodecraft_gpu::mesh::capture::MeshCapture;

type Slot = Option<MeshCapture>;

#[cfg(not(target_arch = "wasm32"))]
static CAPTURE_GPU: std::sync::Mutex<Slot> = std::sync::Mutex::new(None);

#[cfg(target_arch = "wasm32")]
std::thread_local! {
    static LOCAL_CAPTURE_GPU: std::cell::RefCell<Slot> = const { std::cell::RefCell::new(None) };
}

#[cfg(not(target_arch = "wasm32"))]
pub(super) fn with<T>(work: impl FnOnce(&mut Slot) -> Result<T, String>) -> Result<T, String> {
    let mut slot = CAPTURE_GPU.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    work(&mut slot)
}

#[cfg(target_arch = "wasm32")]
pub(super) fn with<T>(work: impl FnOnce(&mut Slot) -> Result<T, String>) -> Result<T, String> {
    LOCAL_CAPTURE_GPU
        .try_with(|slot| {
            let mut slot = slot.try_borrow_mut().map_err(|_| "the 3D capturer is already in use on this thread".to_owned())?;
            work(&mut slot)
        })
        .map_err(|_| "the 3D capturer is unavailable on this thread".to_owned())?
}
