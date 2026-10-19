//! Native shaders share one serial runner; browser GPU handles stay on their owning thread.
use nodecraft_gpu::shader::ShaderRunner;

type Slot = Option<ShaderRunner>;

#[cfg(not(target_arch = "wasm32"))]
static SHADER_GPU: std::sync::Mutex<Slot> = std::sync::Mutex::new(None);

#[cfg(any(target_arch = "wasm32", test))]
std::thread_local! {
    static LOCAL_SHADER_GPU: std::cell::RefCell<Slot> = const { std::cell::RefCell::new(None) };
}

#[cfg(not(target_arch = "wasm32"))]
pub(super) fn with<T>(work: impl FnOnce(&mut Slot) -> Result<T, String>) -> Result<T, String> {
    let mut slot = SHADER_GPU.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    work(&mut slot)
}

#[cfg(target_arch = "wasm32")]
pub(super) fn with<T>(work: impl FnOnce(&mut Slot) -> Result<T, String>) -> Result<T, String> {
    with_local(work)
}

#[cfg(any(target_arch = "wasm32", test))]
fn with_local<T>(work: impl FnOnce(&mut Slot) -> Result<T, String>) -> Result<T, String> {
    LOCAL_SHADER_GPU
        .try_with(|slot| {
            let mut slot = slot.try_borrow_mut().map_err(|_| "the custom shader runner is already in use on this thread".to_owned())?;
            work(&mut slot)
        })
        .map_err(|_| "the custom shader runner is unavailable on this thread".to_owned())?
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    #[test]
    fn local_reentrance_refuses_before_work_and_releases_the_borrow() {
        let calls = Cell::new(0);
        with_local(|slot| {
            assert!(slot.is_none());
            let nested = with_local(|_| {
                calls.set(calls.get() + 1);
                Ok(())
            });
            assert_eq!(nested.unwrap_err(), "the custom shader runner is already in use on this thread");
            Ok(())
        })
        .unwrap();
        assert_eq!(calls.get(), 0);
        assert!(with_local(|slot| Ok(slot.is_none())).unwrap());
    }

    #[test]
    fn local_work_failure_does_not_keep_the_slot_borrowed() {
        let failed: Result<(), String> = with_local(|_| Err("fixture failure".into()));
        assert_eq!(failed.unwrap_err(), "fixture failure");
        assert!(with_local(|slot| Ok(slot.is_none())).unwrap());
    }
}
