//! On-device OCR via `Windows.Media.Ocr` (WinRT) — the engine built into
//! Windows itself; fully local, no model downloads, works offline.
//!
//! PNG bytes are decoded through `Windows.Graphics.Imaging.BitmapDecoder`,
//! downscaled to the engine's `MaxImageDimension` when needed, and each
//! `OcrWord.BoundingRect` — top-left-origin image pixels — becomes a
//! bottom-left `NormRect` via `ocr_word_rect`. WinRT reports no per-word
//! confidence, so tokens carry `f32::NAN` — the honest "not measured",
//! never an invented score. A box with no recognizer language pack
//! reports `VisionError::Unsupported`.

use crate::{ocr_word_rect, VisionError, VisionProvider, VisionToken};
use dexter_core::Rect;
use windows::Graphics::Imaging::{
    BitmapAlphaMode, BitmapDecoder, BitmapPixelFormat, BitmapTransform, ColorManagementMode,
    ExifOrientationMode, SoftwareBitmap,
};
use windows::Media::Ocr::OcrEngine;
use windows::Storage::Streams::{DataWriter, InMemoryRandomAccessStream};

/// `Windows.Media.Ocr` recognizer. Stateless — the engine is created
/// per call and nothing is held between calls, so the provider is
/// trivially `Send + Sync`.
pub struct WinOcr;

fn failed(e: windows::core::Error) -> VisionError {
    VisionError::Failed(e.message())
}

/// An OCR engine for the current user's profile languages, or
/// `Unsupported` when the box has no recognizer language pack — the one
/// case where no honest analysis exists.
fn engine() -> Result<OcrEngine, VisionError> {
    let languages = OcrEngine::AvailableRecognizerLanguages().map_err(failed)?;
    if languages.Size().map_err(failed)? == 0 {
        return Err(VisionError::Unsupported);
    }
    OcrEngine::TryCreateFromUserProfileLanguages().map_err(|_| VisionError::Unsupported)
}

/// PNG bytes → the `SoftwareBitmap` the recognizer accepts. The decoder
/// honors the image's own pixels; an image wider than the engine's
/// `MaxImageDimension` is scaled down proportionally (normalized token
/// bounds are computed against the decoded size, so they still map onto
/// the caller's full-size capture).
fn software_bitmap(png: &[u8], max_dim: u32) -> Result<SoftwareBitmap, VisionError> {
    let stream = InMemoryRandomAccessStream::new().map_err(failed)?;
    {
        let writer = DataWriter::CreateDataWriter(&stream).map_err(failed)?;
        writer.WriteBytes(png).map_err(failed)?;
        writer
            .StoreAsync()
            .map_err(failed)?
            .join()
            .map_err(failed)?;
        writer.DetachStream().map_err(failed)?;
    }
    stream.Seek(0).map_err(failed)?;
    let decoder = BitmapDecoder::CreateAsync(&stream)
        .map_err(failed)?
        .join()
        .map_err(failed)?;
    let w = decoder.PixelWidth().map_err(failed)?;
    let h = decoder.PixelHeight().map_err(failed)?;
    if w.max(h) <= max_dim {
        decoder
            .GetSoftwareBitmapAsync()
            .map_err(failed)?
            .join()
            .map_err(failed)
    } else {
        let shrink = f64::from(max_dim) / f64::from(w.max(h));
        let transform = BitmapTransform::new().map_err(failed)?;
        transform
            .SetScaledWidth((f64::from(w) * shrink).round() as u32)
            .map_err(failed)?;
        transform
            .SetScaledHeight((f64::from(h) * shrink).round() as u32)
            .map_err(failed)?;
        decoder
            .GetSoftwareBitmapTransformedAsync(
                BitmapPixelFormat::Bgra8,
                BitmapAlphaMode::Premultiplied,
                &transform,
                ExifOrientationMode::IgnoreExifOrientation,
                ColorManagementMode::DoNotColorManage,
            )
            .map_err(failed)?
            .join()
            .map_err(failed)
    }
}

impl VisionProvider for WinOcr {
    fn name(&self) -> &'static str {
        "windows-media-ocr"
    }

    fn recognize(&self, image_png: &[u8]) -> Result<Vec<VisionToken>, VisionError> {
        let engine = engine()?;
        let bitmap = software_bitmap(image_png, OcrEngine::MaxImageDimension().map_err(failed)?)?;
        let img_w = bitmap.PixelWidth().map_err(failed)?.max(0) as u32;
        let img_h = bitmap.PixelHeight().map_err(failed)?.max(0) as u32;
        let result = engine
            .RecognizeAsync(&bitmap)
            .map_err(failed)?
            .join()
            .map_err(failed)?;
        let mut tokens = Vec::new();
        for line in result.Lines().map_err(failed)? {
            for word in line.Words().map_err(failed)? {
                let text = word.Text().map_err(failed)?.to_string();
                if text.is_empty() {
                    continue;
                }
                let r = word.BoundingRect().map_err(failed)?;
                tokens.push(VisionToken {
                    text,
                    bounds: ocr_word_rect(
                        &Rect {
                            x: f64::from(r.X),
                            y: f64::from(r.Y),
                            w: f64::from(r.Width),
                            h: f64::from(r.Height),
                        },
                        img_w,
                        img_h,
                    ),
                    confidence: f32::NAN,
                });
            }
        }
        Ok(tokens)
    }
}
