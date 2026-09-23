//! Real Apple Vision OCR against a checked-in fixture.
//! macOS-only: the provider does not exist elsewhere.
#![cfg(target_os = "macos")]

use dexter_core::Rect;
use dexter_vision::{platform_provider, png_dimensions, tokens_to_elements};

const FIXTURE: &[u8] = include_bytes!("fixtures/save-document.png");
/// The fixture was rendered as a 480x140pt view and captured at 2x.
const WINDOW: Rect = Rect {
    x: 100.0,
    y: 200.0,
    w: 480.0,
    h: 140.0,
};

#[test]
fn apple_vision_recognizes_fixture_text() {
    let provider = platform_provider().expect("macOS always has a provider");
    let tokens = provider.recognize(FIXTURE).expect("ocr succeeds");

    let text: String = tokens
        .iter()
        .map(|t| t.text.as_str())
        .collect::<Vec<_>>()
        .join(" ");
    assert!(
        text.contains("Save") && text.contains("document"),
        "expected 'Save document', got {tokens:?}"
    );
    for t in &tokens {
        assert!(t.confidence > 0.5, "low confidence {t:?}");
        assert!(
            (0.0..=1.0).contains(&t.bounds.x)
                && (0.0..=1.0).contains(&t.bounds.y)
                && t.bounds.w > 0.0
                && t.bounds.h > 0.0,
            "bounds not normalized {t:?}"
        );
    }
}

#[test]
fn tokens_map_into_window_points() {
    let provider = platform_provider().expect("macOS always has a provider");
    let tokens = provider.recognize(FIXTURE).expect("ocr succeeds");
    let (w, h) = png_dimensions(FIXTURE).expect("valid png");
    assert_eq!((w, h), (960, 280)); // 2x capture of a 480x140pt view

    let elements = tokens_to_elements(&tokens, w, h, &WINDOW, 0);
    assert_eq!(elements.len(), tokens.len());
    for e in &elements {
        let b = e.bounds.expect("ocr elements always have bounds");
        assert!(
            b.x >= WINDOW.x && b.y >= WINDOW.y && b.x + b.w <= WINDOW.x + WINDOW.w + 1.0,
            "element outside window: {b:?}"
        );
    }
}
