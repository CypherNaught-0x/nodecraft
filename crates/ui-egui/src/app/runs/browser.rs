//! The run host's runtime in the browser (#118): tasks run on the page's queue
//! (`wasm-bindgen-futures`), sleeps are `setTimeout`s and the clock is `performance.now()`.

use eframe::wasm_bindgen::{JsCast as _, JsValue, closure::Closure};
use nodecraft_exec::driver::{Runtime, TaskFuture};
use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};
use wasm_bindgen_futures::JsFuture;

/// The page's runtime.
pub(super) struct Page;

impl Page {
    pub(super) fn new() -> Self {
        Self
    }
}

impl Runtime for Page {
    fn spawn(&self, task: TaskFuture<()>) {
        wasm_bindgen_futures::spawn_local(task);
    }

    fn sleep(&self, ms: u64) -> TaskFuture<()> {
        // `setTimeout` takes an i32; a longer wait ends at its cap and the scheduler asks again.
        let ms = i32::try_from(ms).unwrap_or(i32::MAX);
        let mut timer = None;
        let fired = js_sys::Promise::new(&mut |resolve, _reject| {
            let Some(window) = web_sys::window() else {
                // No window to time with: wake at once rather than never.
                let _ = resolve.call0(&JsValue::NULL);
                return;
            };
            let done = resolve.clone();
            let fire = Closure::once_into_js(move || {
                let _ = done.call0(&JsValue::NULL);
            });
            match window.set_timeout_with_callback_and_timeout_and_arguments_0(fire.unchecked_ref(), ms) {
                Ok(id) => timer = Some(id),
                // Waking early is safe (the scheduler checks the time again); never waking isn't.
                Err(_) => {
                    let _ = resolve.call0(&JsValue::NULL);
                }
            }
        });
        Box::pin(Sleep { fired: JsFuture::from(fired), timer })
    }

    fn now_ms(&self) -> u64 {
        now_ms()
    }
}

/// A `setTimeout`, cleared if the scheduler stops waiting for it first (a job ended, or a
/// command came), as the desktop's tokio sleep is.
struct Sleep {
    fired: JsFuture,
    timer: Option<i32>,
}

impl Future for Sleep {
    type Output = ();

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        match Pin::new(&mut self.fired).poll(cx) {
            Poll::Ready(_) => {
                self.timer = None;
                Poll::Ready(())
            }
            Poll::Pending => Poll::Pending,
        }
    }
}

impl Drop for Sleep {
    fn drop(&mut self) {
        if let (Some(id), Some(window)) = (self.timer.take(), web_sys::window()) {
            window.clear_timeout_with_handle(id);
        }
    }
}

/// Milliseconds since the page loaded, on its monotonic clock.
pub(super) fn now_ms() -> u64 {
    millis(web_sys::window().and_then(|window| window.performance()).map_or_else(js_sys::Date::now, |performance| performance.now()))
}

/// A browser time in milliseconds as a whole number; NaN and negative times are 0.
pub(super) fn millis(ms: f64) -> u64 {
    if ms.is_finite() && ms > 0.0 {
        // Saturates past u64::MAX.
        ms as u64
    } else {
        0
    }
}
