//! Opt-in Linux OCR augmentation for X11 windows.

use dexter_core::{Observation, ObservationScope};
use dexter_vision::png_dimensions;
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_CAPTURE_ID: AtomicU64 = AtomicU64::new(0);

pub fn augment(obs: &mut Observation, scope: &ObservationScope) {
    let Some(provider) = dexter_vision::platform_provider() else {
        obs.collection_errors += 1;
        return;
    };
    let window = scope
        .window
        .and_then(|wid| obs.windows.iter().find(|window| window.id == wid))
        .or_else(|| crate::geometry::pick_capture_window(&obs.windows));
    let Some(window) = window else {
        obs.collection_errors += 1;
        return;
    };
    let nonce = NEXT_CAPTURE_ID.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!(
        "dexter-vision-{}-{}-{nonce}.png",
        std::process::id(),
        obs.id.0
    ));
    let Ok(captured) = crate::capture::capture_window_region(window, &path) else {
        obs.collection_errors += 1;
        return;
    };
    let Ok(png) = std::fs::read(&path) else {
        obs.collection_errors += 1;
        return;
    };
    let Some((img_w, img_h)) = png_dimensions(&png) else {
        obs.collection_errors += 1;
        return;
    };
    match provider.recognize(&png) {
        Ok(tokens) => {
            let first_id = obs
                .elements
                .iter()
                .map(|element| element.id.0)
                .max()
                .unwrap_or(0)
                + 1;
            obs.elements.extend(dexter_vision::tokens_to_elements(
                &tokens, img_w, img_h, &captured, first_id,
            ));
            if obs.screenshot.is_none() {
                obs.screenshot = Some(path.display().to_string());
            }
        }
        Err(_) => obs.collection_errors += 1,
    }
}
