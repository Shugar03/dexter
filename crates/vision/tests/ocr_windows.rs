//! Real Windows.Media.Ocr against the shared OCR fixture.
//! Windows-only; when the box has no recognizer language pack the
//! provider must report `Unsupported` — never fabricated tokens.
#![cfg(windows)]

use dexter_vision::{platform_provider, VisionError};

const FIXTURE: &[u8] = include_bytes!("fixtures/save-document.png");

#[test]
fn windows_ocr_recognizes_fixture_text_or_reports_unsupported() {
    let provider = platform_provider().expect("windows has a provider");
    match provider.recognize(FIXTURE) {
        Ok(tokens) => {
            assert!(!tokens.is_empty(), "empty OCR result on a text fixture");
            let text: String = tokens
                .iter()
                .map(|t| t.text.as_str())
                .collect::<Vec<_>>()
                .join(" ");
            assert!(
                text.to_lowercase().contains("save"),
                "expected 'Save ...' text, got {tokens:?}"
            );
            for t in &tokens {
                assert!(
                    (0.0..=1.0).contains(&t.bounds.x)
                        && (0.0..=1.0).contains(&t.bounds.y)
                        && t.bounds.w > 0.0
                        && t.bounds.h > 0.0,
                    "bounds not normalized {t:?}"
                );
            }
        }
        // No recognizer language pack on this box is an honest
        // `Unsupported` — never fabricate tokens.
        Err(VisionError::Unsupported) => {}
        Err(e) => panic!("ocr failed: {e}"),
    }
}
