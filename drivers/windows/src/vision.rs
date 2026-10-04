//! Opt-in OCR fallback (`ObservationScope::vision`) for apps whose UIA
//! tree is thin or absent — see `docs/sdd/vision.md`.
//!
//! Mirrors the macOS pass exactly: capture the chosen window's region so
//! normalized OCR boxes map back to real screen points, append the
//! result as inert `source: ocr` elements, and degrade
//! (`collection_errors += 1`) rather than fail when a pass cannot run —
//! whatever UIA content was collected is still valid evidence.

use dexter_core::{Observation, ObservationScope};
use dexter_vision::png_dimensions;

pub fn augment(obs: &mut Observation, scope: &ObservationScope) {
    let Some(provider) = dexter_vision::platform_provider() else {
        obs.collection_errors += 1;
        return;
    };
    let window = scope
        .window
        .and_then(|wid| obs.windows.iter().find(|w| w.id == wid))
        .or_else(|| crate::geometry::pick_capture_window(&obs.windows));
    let Some(window) = window else {
        obs.collection_errors += 1;
        return;
    };
    let path = std::env::temp_dir().join(format!("dexter-vision-{}.png", obs.id.0));
    // The region actually captured anchors the token→screen mapping —
    // identical geometry on both sides.
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
            let first_id = obs.elements.iter().map(|e| e.id.0).max().unwrap_or(0) + 1;
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
