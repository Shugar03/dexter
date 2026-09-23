//! Vision fallback seam: local OCR for apps whose accessibility tree is
//! poor or absent (Electron, custom toolkits, canvas UIs).
//!
//! Contract:
//! - OCR is evidence, not agency. [`tokens_to_elements`] produces elements
//!   with `source: ElementSource::Ocr`, a `text` role and **no actions** —
//!   interacting with them resolves to coordinate (physical) input, which
//!   stays policy-gated.
//! - Everything runs on-device. Providers that cannot run must return
//!   [`VisionError::Unsupported`], never fabricated elements.
//! - Vision reports normalized bounding boxes with a **bottom-left**
//!   origin; the mapping to Dexter's top-left screen coordinates lives in
//!   [`token_rect`] so it is unit-testable without a screen.

#[cfg(target_os = "macos")]
mod apple;

use dexter_core::{Element, ElementId, ElementSource, Rect};

/// A rectangle in normalized image coordinates (0..1, bottom-left origin),
/// as returned by Apple Vision observations.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct NormRect {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

/// One recognized text run.
#[derive(Debug, Clone, PartialEq)]
pub struct VisionToken {
    pub text: String,
    pub bounds: NormRect,
    /// Recognizer confidence in `0.0..=1.0`.
    pub confidence: f32,
}

#[derive(Debug, thiserror::Error)]
pub enum VisionError {
    /// No on-device vision provider exists on this platform/build.
    #[error("vision unsupported on this platform")]
    Unsupported,
    /// The provider exists but the analysis failed.
    #[error("vision analysis failed: {0}")]
    Failed(String),
}

/// Pluggable local text recognizer. Implementations must be synchronous —
/// callers run them off the observation path — and fully on-device.
pub trait VisionProvider: Send + Sync {
    fn name(&self) -> &'static str;
    /// Recognize text in a PNG-encoded image.
    fn recognize(&self, image_png: &[u8]) -> Result<Vec<VisionToken>, VisionError>;
}

/// The on-device provider for this platform, when one exists.
/// macOS returns the Apple Vision provider; everything else returns `None`.
pub fn platform_provider() -> Option<Box<dyn VisionProvider>> {
    #[cfg(target_os = "macos")]
    {
        Some(Box::new(apple::AppleVision))
    }
    #[cfg(not(target_os = "macos"))]
    {
        None
    }
}

/// `(width, height)` in pixels of a PNG image, read from the IHDR chunk
/// without decoding the pixel data.
pub fn png_dimensions(png: &[u8]) -> Option<(u32, u32)> {
    const SIG: &[u8; 8] = b"\x89PNG\r\n\x1a\n";
    if png.len() < 24 || &png[..8] != SIG || &png[12..16] != b"IHDR" {
        return None;
    }
    let w = u32::from_be_bytes(png[16..20].try_into().ok()?);
    let h = u32::from_be_bytes(png[20..24].try_into().ok()?);
    (w > 0 && h > 0).then_some((w, h))
}

/// Map one normalized (bottom-left origin) token box to Dexter screen
/// coordinates (points, top-left origin).
///
/// `img_*_px` is the captured image size in physical pixels and `window` is
/// the captured region's screen bounds in points — on a Retina display the
/// image is typically `2x` the point size, which is why both are needed.
pub fn token_rect(token: &NormRect, img_w_px: u32, img_h_px: u32, window: &Rect) -> Rect {
    let px_per_pt_x = f64::from(img_w_px) / window.w;
    let px_per_pt_y = f64::from(img_h_px) / window.h;
    // Flip the vertical axis: Vision y grows up from the image bottom.
    // Bounds are rounded — sub-point precision is noise for targeting.
    let top_norm = 1.0 - token.y - token.h;
    Rect {
        x: (window.x + token.x * f64::from(img_w_px) / px_per_pt_x).round(),
        y: (window.y + top_norm * f64::from(img_h_px) / px_per_pt_y).round(),
        w: (token.w * f64::from(img_w_px) / px_per_pt_x).round(),
        h: (token.h * f64::from(img_h_px) / px_per_pt_y).round(),
    }
}

/// Turn OCR tokens into Dexter elements scoped to `window`.
///
/// The elements are deliberately inert: `text` role, recognized text as the
/// name, no actions — they are evidence for the agent's own targeting, and
/// acting on them resolves to coordinate input that policy still gates.
/// `first_id` is the next free [`ElementId`] in the observation.
pub fn tokens_to_elements(
    tokens: &[VisionToken],
    img_w_px: u32,
    img_h_px: u32,
    window: &Rect,
    first_id: u64,
) -> Vec<Element> {
    tokens
        .iter()
        .enumerate()
        .filter(|(_, t)| !t.text.is_empty())
        .map(|(i, t)| Element {
            id: ElementId(first_id + i as u64),
            role: Some("text".into()),
            raw_role: Some("ocr".into()),
            name: Some(t.text.clone()),
            bounds: Some(token_rect(&t.bounds, img_w_px, img_h_px, window)),
            source: ElementSource::Ocr,
            ..Element::default()
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const WIN: Rect = Rect {
        x: 100.0,
        y: 200.0,
        w: 800.0,
        h: 600.0,
    };

    #[test]
    fn token_rect_maps_bottom_left_normalized_to_top_left_points() {
        // 1x capture: image pixels == window points.
        let r = token_rect(
            &NormRect {
                x: 0.25,
                y: 0.5,
                w: 0.5,
                h: 0.25,
            },
            800,
            600,
            &WIN,
        );
        // x: 100 + 0.25*800 = 300
        assert_eq!(r.x, 300.0);
        // y: 200 + (1 - 0.5 - 0.25)*600 = 350
        assert_eq!(r.y, 350.0);
        assert_eq!(r.w, 400.0);
        assert_eq!(r.h, 150.0);
    }

    #[test]
    fn token_rect_handles_exact_2x_retina_scale() {
        // 1600x1200 image over an 800x600pt window: the same normalized box
        // must land on the same points as the 1x case.
        let norm = NormRect {
            x: 0.25,
            y: 0.5,
            w: 0.5,
            h: 0.25,
        };
        let r = token_rect(&norm, 1600, 1200, &WIN);
        assert_eq!(r, token_rect(&norm, 800, 600, &WIN));
    }

    #[test]
    fn token_rect_maps_full_image_box_to_window() {
        let r = token_rect(
            &NormRect {
                x: 0.0,
                y: 0.0,
                w: 1.0,
                h: 1.0,
            },
            1600,
            1200,
            &WIN,
        );
        assert_eq!(r, WIN);
    }

    #[test]
    fn png_dimensions_reads_ihdr() {
        let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
        png.extend_from_slice(&13u32.to_be_bytes());
        png.extend_from_slice(b"IHDR");
        png.extend_from_slice(&1920u32.to_be_bytes());
        png.extend_from_slice(&1080u32.to_be_bytes());
        png.extend_from_slice(&[8, 6, 0, 0, 0]);
        assert_eq!(png_dimensions(&png), Some((1920, 1080)));
        assert_eq!(png_dimensions(b"not a png"), None);
        assert_eq!(png_dimensions(&png[..20]), None);
    }

    #[test]
    fn ocr_elements_are_inert_and_sourced() {
        let tokens = vec![VisionToken {
            text: "Save".into(),
            bounds: NormRect {
                x: 0.1,
                y: 0.8,
                w: 0.2,
                h: 0.05,
            },
            confidence: 0.9,
        }];
        let els = tokens_to_elements(&tokens, 1600, 1200, &WIN, 7);
        assert_eq!(els.len(), 1);
        let e = &els[0];
        assert_eq!(e.id, ElementId(7));
        assert_eq!(e.source, ElementSource::Ocr);
        assert_eq!(e.role.as_deref(), Some("text"));
        assert_eq!(e.name.as_deref(), Some("Save"));
        assert!(e.actions.is_empty());
        assert!(e.bounds.is_some());
    }
}
