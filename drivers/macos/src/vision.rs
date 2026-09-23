//! Opt-in OCR fallback (`ObservationScope::vision`) for apps whose AX tree
//! is thin or absent — see `docs/sdd/vision.md`.
//!
//! The pass is deliberately scoped: we capture exactly the target window's
//! bounds (`capture_window_region`, not xcap's frontmost pick) so the
//! normalized Vision boxes map back to real screen points, and we append the
//! result as inert `source: ocr` elements. A failed pass degrades the
//! observation (`collection_errors += 1`) rather than failing it — whatever
//! AX content was collected is still valid evidence.

use crate::screenshot;
use dexter_core::{Observation, ObservationScope};
use dexter_vision::{platform_provider, png_dimensions, tokens_to_elements};

pub fn augment(obs: &mut Observation, scope: &ObservationScope) {
    let Some(provider) = platform_provider() else {
        obs.collection_errors += 1;
        return;
    };
    let window = match scope
        .window
        .and_then(|id| obs.windows.iter().find(|w| w.id == id))
        .or_else(|| screenshot::pick_capture_window(&obs.windows))
        .cloned()
    {
        Some(w) => w,
        None => {
            obs.collection_errors += 1;
            return;
        }
    };

    let path = std::env::temp_dir().join(format!("dexter-vision-{}.png", obs.id.0));
    let result = (|| -> Result<(Vec<dexter_vision::VisionToken>, u32, u32), String> {
        screenshot::capture_window_region(&window, &path).map_err(|e| e.to_string())?;
        let png = std::fs::read(&path).map_err(|e| e.to_string())?;
        let (w, h) =
            png_dimensions(&png).ok_or_else(|| "capture was not a valid png".to_string())?;
        let tokens = provider.recognize(&png).map_err(|e| e.to_string())?;
        Ok((tokens, w, h))
    })();

    match result {
        Ok((tokens, w, h)) => {
            let first_id = obs.elements.iter().map(|e| e.id.0).max().unwrap_or(0) + 1;
            obs.elements
                .extend(tokens_to_elements(&tokens, w, h, &window.bounds, first_id));
            // Provenance: the image the tokens came from. Only fill it when
            // the caller did not already request a (frontmost) screenshot.
            if obs.screenshot.is_none() {
                obs.screenshot = Some(path.display().to_string());
            }
        }
        Err(_) => obs.collection_errors += 1,
    }
}
