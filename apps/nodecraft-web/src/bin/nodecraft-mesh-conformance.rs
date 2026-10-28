//! Browser execution probe, available only with mesh-conformance; not shipped in release sites.
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]
#[cfg(target_arch = "wasm32")]
mod probe {
    use js_sys::Promise;
    use nodecraft_mesh_preview::{
        Error, Prepared,
        browser::{Browser, Fault},
    };
    use std::{
        cell::{Cell, RefCell},
        rc::Rc,
        sync::Arc,
    };
    use wasm_bindgen::{JsCast, closure::Closure};
    use wasm_bindgen_futures::JsFuture;
    use web_sys::{Event, HtmlElement};
    type ButtonHandler = Closure<dyn FnMut(Event)>;
    thread_local! {static BUTTON:RefCell<Vec<ButtonHandler>>=const {RefCell::new(Vec::new())};}
    fn status(text: &str) {
        if let Some(node) = web_sys::window().and_then(|w| w.document()).and_then(|d| d.get_element_by_id("conformance-status")) {
            node.set_text_content(Some(text));
        }
    }
    async fn frame() -> Result<(), Error> {
        let window = web_sys::window().ok_or(Error::Unavailable)?;
        let promise = Promise::new(&mut |resolve, reject| {
            if window.request_animation_frame(&resolve).is_err() {
                let _ = reject.call0(&js_sys::global());
            }
        });
        JsFuture::from(promise).await.map_err(|_| Error::Unavailable)?;
        Ok(())
    }
    fn time() -> f64 {
        web_sys::window().and_then(|w| w.performance()).map_or(0.0, |p| p.now())
    }
    async fn complete(client: &mut Browser, label: &str) -> Result<(Prepared, f64, u32), Error> {
        let mut maximum = 0.0f64;
        let mut ticks = 0u32;
        loop {
            let start = time();
            let result = client.poll();
            maximum = maximum.max(time() - start);
            if let Some(result) = result {
                return result.map(|model| (model, maximum, ticks));
            }
            ticks = ticks.saturating_add(1);
            status(&format!("{label}: {} · main frames {ticks}", client.phase_for_conformance()));
            frame().await?;
        }
    }
    fn expect_error(client: &mut Browser, expected: Error) -> Result<(), Error> {
        match client.poll() {
            Some(Err(actual)) if actual == expected && !client.busy() => Ok(()),
            _ => Err(Error::Protocol),
        }
    }
    pub fn start() -> Result<(), Error> {
        let document = web_sys::window().and_then(|w| w.document()).ok_or(Error::Unavailable)?;
        let body = document.body().ok_or(Error::Unavailable)?;
        let title = document.create_element("h1").map_err(|_| Error::Unavailable)?;
        title.set_text_content(Some("NodeCraft mesh worker conformance"));
        body.append_child(&title).map_err(|_| Error::Unavailable)?;
        let report = document.create_element("p").map_err(|_| Error::Unavailable)?;
        report.set_id("conformance-status");
        report.set_text_content(Some("Ready to run"));
        body.append_child(&report).map_err(|_| Error::Unavailable)?;
        let button = document.create_element("button").map_err(|_| Error::Unavailable)?.dyn_into::<HtmlElement>().map_err(|_| Error::Unavailable)?;
        button.set_id("conformance-edit");
        button.set_text_content(Some("Edit counter: 0"));
        body.append_child(&button).map_err(|_| Error::Unavailable)?;
        let clicks = Rc::new(Cell::new(0u32));
        let counter = clicks.clone();
        let label = button.clone();
        let callback = Closure::new(move |_: Event| {
            counter.set(counter.get().saturating_add(1));
            label.set_text_content(Some(&format!("Edit counter: {}", counter.get())));
        });
        button.set_onclick(Some(callback.as_ref().unchecked_ref()));
        BUTTON.with(|slot| slot.borrow_mut().push(callback));
        let run_button = document.create_element("button").map_err(|_| Error::Unavailable)?.dyn_into::<HtmlElement>().map_err(|_| Error::Unavailable)?;
        run_button.set_id("conformance-run");
        run_button.set_text_content(Some("Run conformance"));
        body.append_child(&run_button).map_err(|_| Error::Unavailable)?;
        let started = Cell::new(false);
        let callback = Closure::new(move |_: Event| {
            if started.replace(true) {
                return;
            }
            let clicks = clicks.clone();
            wasm_bindgen_futures::spawn_local(async move {
                let result = run(clicks).await;
                if let Err(error) = result {
                    status(&format!("FAIL: {error:?}"));
                }
            });
        });
        run_button.set_onclick(Some(callback.as_ref().unchecked_ref()));
        BUTTON.with(|slot| slot.borrow_mut().push(callback));
        Ok(())
    }
    async fn run(clicks: Rc<Cell<u32>>) -> Result<(), Error> {
        let checks = nodecraft_mesh_preview::browser::protocol_for_conformance()?;
        let bytes: Arc<[u8]> = nodecraft_mesh_preview::fixture::large().into();
        let original = *blake3::hash(&bytes).as_bytes();
        let missing = web_sys::window().and_then(|w| w.location().pathname().ok()).is_some_and(|p| p.contains("/missing/"));
        let mut client = Browser::default();
        client.start(original, bytes.clone(), || {})?;
        if missing {
            match complete(&mut client, "Missing worker").await {
                Err(Error::Unavailable) => {
                    status("PASS missing-worker: host started, worker unavailable, edit counter remains usable");
                    return Ok(());
                }
                _ => return Err(Error::Protocol),
            }
        }
        let (model, maximum, ticks) = complete(&mut client, "Original 9,000,084-byte STL").await?;
        let fingerprint = blake3::Hash::from_bytes(model.fingerprint()).to_hex().to_string();
        if model.layout() != (nodecraft_mesh_preview::Layout { vertices: 540_000, triangles: 180_000, edge_words: 1_080_000 })
            || model.bytes() != 28_080_000
            || model.center() != [0.5859375, 0.5859375, 0.29296875]
        {
            return Err(Error::Protocol);
        }
        let expected = "79dc46f44cc13d0aa3b6932125d6e5013f4d9ef41d5ad4712590de324374088b";
        if fingerprint != expected {
            status(&format!("FAIL fingerprint: {fingerprint}"));
            return Err(Error::Protocol);
        }
        drop(model);
        client.start(original, bytes.clone(), || {})?;
        client.cancel();
        if client.busy() {
            return Err(Error::Protocol);
        }
        client.start(original, bytes.clone(), || {})?;
        client.fault_for_conformance(Fault::Timeout)?;
        expect_error(&mut client, Error::Timeout)?;
        client.start(original, bytes.clone(), || {})?;
        client.fault_for_conformance(Fault::WrongBuild)?;
        expect_error(&mut client, Error::Protocol)?;
        client.start(original, bytes.clone(), || {})?;
        client.fault_for_conformance(Fault::MessageError)?;
        expect_error(&mut client, Error::Unavailable)?;
        for (fault, expected) in [(Fault::StaleReply, Error::Cancelled), (Fault::OversizedReply, Error::Protocol), (Fault::Timeout, Error::Timeout)] {
            client.start(original, bytes.clone(), || {})?;
            while client.phase_for_conformance() != "decoding" {
                if client.poll().is_some() {
                    return Err(Error::Protocol);
                }
                frame().await?;
            }
            let reserved = nodecraft_mesh_preview::reserved_for_conformance();
            client.fault_for_conformance(fault)?;
            // Stale and oversized envelopes are rejected without reserving any geometry bytes.

            expect_error(&mut client, expected)?;
            if nodecraft_mesh_preview::reserved_for_conformance() != reserved {
                return Err(Error::Protocol);
            }
        }
        let corrupt: Arc<[u8]> = b"not a model".to_vec().into();
        client.start(*blake3::hash(&corrupt).as_bytes(), corrupt, || {})?;
        if !matches!(complete(&mut client, "Corrupt original").await, Err(Error::Invalid)) {
            return Err(Error::Protocol);
        }
        let wakes = Rc::new(Cell::new(0u32));
        {
            let mut dropped = Browser::default();
            let observed = wakes.clone();
            dropped.start(original, bytes.clone(), move || observed.set(observed.get().saturating_add(1)))?;
        }
        frame().await?;
        frame().await?;
        if wakes.get() != 0 {
            return Err(Error::Protocol);
        }
        // A real restart after each terminal path must again produce the reference geometry.
        client.start(original, bytes.clone(), || {})?;
        let (restart, restart_max, _) = complete(&mut client, "Restart verification").await?;
        if restart.fingerprint() != *blake3::Hash::from_hex(&fingerprint).map_err(|_| Error::Protocol)?.as_bytes()
            || blake3::hash(&bytes).as_bytes() != &original
        {
            return Err(Error::Protocol);
        }
        status(&format!(
            "PASS: protocol={checks}, lifecycle=10, original=9000084B, vertices=540000, triangles=180000, edge_words=1080000, prepared=28080000B, fingerprint={fingerprint}, main_frames={ticks}, max_poll_ms={:.3}, pointer_edits={}",
            maximum.max(restart_max),
            clicks.get()
        ));
        Ok(())
    }
}
#[cfg(target_arch = "wasm32")]
fn main() {
    let _ = probe::start();
}
#[cfg(not(target_arch = "wasm32"))]
fn main() {}
