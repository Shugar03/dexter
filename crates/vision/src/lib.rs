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
#[cfg(windows)]
mod win;

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
/// macOS returns the Apple Vision provider, Windows the built-in
/// `Windows.Media.Ocr` engine; everything else returns `None`.
pub fn platform_provider() -> Option<Box<dyn VisionProvider>> {
    #[cfg(target_os = "macos")]
    {
        Some(Box::new(apple::AppleVision))
    }
    #[cfg(windows)]
    {
        Some(Box::new(win::WinOcr))
    }
    #[cfg(not(any(target_os = "macos", windows)))]
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
pub fn token_rect(token: &NormRect, img_w_px: u32, img_h_px: u32, window: &Rect) -> Option<Rect> {
    // A degenerate window extent can't anchor a mapping — division by
    // zero would emit NaN bounds that look like real coordinates.
    if window.w <= 0.0 || window.h <= 0.0 {
        return None;
    }
    let px_per_pt_x = f64::from(img_w_px) / window.w;
    let px_per_pt_y = f64::from(img_h_px) / window.h;
    // Flip the vertical axis: Vision y grows up from the image bottom.
    // Bounds are rounded — sub-point precision is noise for targeting.
    let top_norm = 1.0 - token.y - token.h;
    Some(Rect {
        x: (window.x + token.x * f64::from(img_w_px) / px_per_pt_x).round(),
        y: (window.y + top_norm * f64::from(img_h_px) / px_per_pt_y).round(),
        w: (token.w * f64::from(img_w_px) / px_per_pt_x).round(),
        h: (token.h * f64::from(img_h_px) / px_per_pt_y).round(),
    })
}

/// Map a top-left-origin pixel rect — the shape `OcrWord.BoundingRect`
/// reports on Windows — to the normalized bottom-left `NormRect` every
/// `VisionToken` uses. `img_*_px` is the bitmap the recognizer actually
/// saw (already downscaled when applicable), so normalized tokens stay
/// valid for the caller's own full-size capture. A degenerate image
/// returns an all-zero box — honest zero coverage, not invented bounds.
pub fn ocr_word_rect(px: &Rect, img_w_px: u32, img_h_px: u32) -> NormRect {
    if img_w_px == 0 || img_h_px == 0 {
        return NormRect {
            x: 0.0,
            y: 0.0,
            w: 0.0,
            h: 0.0,
        };
    }
    let w = f64::from(img_w_px);
    let h = f64::from(img_h_px);
    NormRect {
        x: px.x / w,
        y: 1.0 - (px.y + px.h) / h,
        w: px.w / w,
        h: px.h / h,
    }
}

/// One display in global screen points (the CGWindowList space) and its
/// pixels-per-point scale.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MonitorGeometry {
    pub bounds: Rect,
    pub scale: f64,
}

/// A crop rectangle in a monitor capture's physical pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PixelCrop {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
}

/// Sub-point slack for float noise in display/window bounds.
const CONTAIN_EPS_PT: f64 = 0.5;

fn contains(outer: &Rect, inner: &Rect) -> bool {
    inner.x >= outer.x - CONTAIN_EPS_PT
        && inner.y >= outer.y - CONTAIN_EPS_PT
        && inner.x + inner.w <= outer.x + outer.w + CONTAIN_EPS_PT
        && inner.y + inner.h <= outer.y + outer.h + CONTAIN_EPS_PT
}

/// Index of the monitor whose capture fully covers `region`.
///
/// `None` when the region is degenerate, off every display, or straddles
/// displays — no single image covers it, and a clipped crop would skew
/// the token→point mapping.
pub fn capture_monitor(monitors: &[MonitorGeometry], region: &Rect) -> Option<usize> {
    if region.w <= 0.0 || region.h <= 0.0 {
        return None;
    }
    monitors
        .iter()
        .position(|m| m.scale > 0.0 && contains(&m.bounds, region))
}

/// Pixel crop of `region` inside a `img_w_px`×`img_h_px` capture of
/// `monitor`, offset from the monitor's own origin. `None` unless the
/// image covers the whole region (≤1px rounding slack is clamped).
pub fn monitor_pixel_crop(
    monitor: &MonitorGeometry,
    region: &Rect,
    img_w_px: u32,
    img_h_px: u32,
) -> Option<PixelCrop> {
    if region.w <= 0.0 || region.h <= 0.0 || !contains(&monitor.bounds, region) {
        return None;
    }
    let edge = |pt: f64, origin: f64, limit: u32| -> Option<u32> {
        let px = ((pt - origin) * monitor.scale).round().max(0.0);
        (px <= f64::from(limit) + 1.0).then(|| (px as u32).min(limit))
    };
    let left = edge(region.x, monitor.bounds.x, img_w_px)?;
    let top = edge(region.y, monitor.bounds.y, img_h_px)?;
    let right = edge(region.x + region.w, monitor.bounds.x, img_w_px)?;
    let bottom = edge(region.y + region.h, monitor.bounds.y, img_h_px)?;
    (right > left && bottom > top).then_some(PixelCrop {
        x: left,
        y: top,
        w: right - left,
        h: bottom - top,
    })
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
            bounds: token_rect(&t.bounds, img_w_px, img_h_px, window),
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
        )
        .expect("mapped");
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
        assert_eq!(r, Some(WIN));
    }

    #[test]
    fn token_rect_rejects_degenerate_window() {
        // Zero-size window: no point-space to map into — None, never
        // NaN bounds masquerading as coordinates.
        let zero = Rect { w: 0.0, ..WIN };
        let norm = NormRect {
            x: 0.0,
            y: 0.0,
            w: 1.0,
            h: 1.0,
        };
        assert_eq!(token_rect(&norm, 800, 600, &zero), None);
    }

    const PRIMARY: MonitorGeometry = MonitorGeometry {
        bounds: Rect {
            x: 0.0,
            y: 0.0,
            w: 1440.0,
            h: 900.0,
        },
        scale: 2.0,
    };
    // External display to the right of the primary, at 1x.
    const RIGHT: MonitorGeometry = MonitorGeometry {
        bounds: Rect {
            x: 1440.0,
            y: -180.0,
            w: 1920.0,
            h: 1080.0,
        },
        scale: 1.0,
    };

    #[test]
    fn ocr_word_rect_maps_top_left_pixels_to_bottom_left_norm() {
        // 960x240 image: a box at (240,30) 480x60 px maps to
        // x=0.25, w=0.5, y = 1-(30+60)/240 = 0.625, h=0.25.
        assert_eq!(
            ocr_word_rect(
                &Rect {
                    x: 240.0,
                    y: 30.0,
                    w: 480.0,
                    h: 60.0,
                },
                960,
                240,
            ),
            NormRect {
                x: 0.25,
                y: 0.625,
                w: 0.5,
                h: 0.25,
            }
        );
    }

    #[test]
    fn ocr_word_rect_full_image_box_and_degenerate_image() {
        assert_eq!(
            ocr_word_rect(
                &Rect {
                    x: 0.0,
                    y: 0.0,
                    w: 960.0,
                    h: 240.0,
                },
                960,
                240,
            ),
            NormRect {
                x: 0.0,
                y: 0.0,
                w: 1.0,
                h: 1.0,
            }
        );
        // No image dimensions: nothing to normalize against — a zero
        // box, never a NaN or a guessed coverage.
        assert_eq!(
            ocr_word_rect(
                &Rect {
                    x: 10.0,
                    y: 10.0,
                    w: 10.0,
                    h: 10.0,
                },
                0,
                0,
            ),
            NormRect {
                x: 0.0,
                y: 0.0,
                w: 0.0,
                h: 0.0,
            }
        );
    }

    #[test]
    fn capture_monitor_picks_the_display_containing_the_region() {
        let mons = [PRIMARY, RIGHT];
        assert_eq!(capture_monitor(&mons, &WIN), Some(0));
        let on_right = Rect {
            x: 1600.0,
            y: -100.0,
            w: 800.0,
            h: 600.0,
        };
        assert_eq!(capture_monitor(&mons, &on_right), Some(1));
    }

    #[test]
    fn capture_monitor_fails_closed_on_spanning_offscreen_or_degenerate_regions() {
        let mons = [PRIMARY, RIGHT];
        // Straddles both displays: no single image covers it.
        let spanning = Rect {
            x: 1200.0,
            y: 100.0,
            w: 800.0,
            h: 600.0,
        };
        assert_eq!(capture_monitor(&mons, &spanning), None);
        let offscreen = Rect { x: -900.0, ..WIN };
        assert_eq!(capture_monitor(&mons, &offscreen), None);
        assert_eq!(capture_monitor(&mons, &Rect { w: 0.0, ..WIN }), None);
        assert_eq!(capture_monitor(&[], &WIN), None);
    }

    #[test]
    fn monitor_pixel_crop_is_relative_to_the_monitor_origin() {
        // Primary at 2x: points double, origin is (0,0).
        assert_eq!(
            monitor_pixel_crop(&PRIMARY, &WIN, 2880, 1800),
            Some(PixelCrop {
                x: 200,
                y: 400,
                w: 1600,
                h: 1200,
            })
        );
        // Secondary at 1x with a negative-y origin: offsets subtract the
        // monitor origin instead of indexing the primary's image.
        let on_right = Rect {
            x: 1600.0,
            y: -100.0,
            w: 800.0,
            h: 600.0,
        };
        assert_eq!(
            monitor_pixel_crop(&RIGHT, &on_right, 1920, 1080),
            Some(PixelCrop {
                x: 160,
                y: 80,
                w: 800,
                h: 600,
            })
        );
    }

    #[test]
    fn monitor_pixel_crop_rejects_a_region_the_image_does_not_cover() {
        // Image smaller than the geometry promised (mode change mid-capture):
        // a clipped crop would silently skew the token mapping.
        assert_eq!(monitor_pixel_crop(&PRIMARY, &WIN, 1440, 900), None);
        let outside = Rect { x: 1600.0, ..WIN };
        assert_eq!(monitor_pixel_crop(&PRIMARY, &outside, 2880, 1800), None);
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
