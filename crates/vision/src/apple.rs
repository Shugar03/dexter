//! On-device OCR via Apple's Vision framework (`VNRecognizeTextRequest`).
//! Fully local: no network, no model downloads, works offline.

use crate::{NormRect, VisionError, VisionProvider, VisionToken};
use objc2::rc::{autoreleasepool, Retained};
use objc2::AllocAnyThread;
use objc2_core_foundation::CGRect;
use objc2_foundation::{NSArray, NSData, NSDictionary, NSError};
use objc2_vision::{
    VNImageOption, VNImageRequestHandler, VNRecognizeTextRequest, VNRequest,
    VNRequestTextRecognitionLevel,
};

/// Apple Vision text recognizer. Stateless — safe to share.
pub struct AppleVision;

impl VisionProvider for AppleVision {
    fn name(&self) -> &'static str {
        "apple-vision"
    }

    fn recognize(&self, image_png: &[u8]) -> Result<Vec<VisionToken>, VisionError> {
        autoreleasepool(|_| recognize_impl(image_png))
    }
}

fn recognize_impl(image_png: &[u8]) -> Result<Vec<VisionToken>, VisionError> {
    let data = NSData::with_bytes(image_png);
    let options = NSDictionary::<VNImageOption, objc2::runtime::AnyObject>::new();
    let handler = VNImageRequestHandler::initWithData_options(
        VNImageRequestHandler::alloc(),
        &data,
        &options,
    );

    let request = VNRecognizeTextRequest::new();
    request.setRecognitionLevel(VNRequestTextRecognitionLevel::Accurate);
    request.setAutomaticallyDetectsLanguage(true);

    // SAFETY: `VNRecognizeTextRequest` is a `VNRequest` subclass.
    let requests = NSArray::from_retained_slice(&[unsafe {
        Retained::cast_unchecked::<VNRequest>(request.clone())
    }]);
    handler
        .performRequests_error(&requests)
        .map_err(|e: Retained<NSError>| {
            VisionError::Failed(e.localizedDescription().to_string())
        })?;

    let results = request
        .results()
        .ok_or_else(|| VisionError::Failed("request produced no observations".into()))?;

    let mut tokens = Vec::with_capacity(results.len());
    for obs in results.to_vec() {
        let Some(candidate) = obs.topCandidates(1).firstObject() else {
            continue;
        };
        let CGRect { origin, size } = unsafe { obs.boundingBox() };
        tokens.push(VisionToken {
            text: candidate.string().to_string(),
            confidence: candidate.confidence(),
            bounds: NormRect {
                x: origin.x,
                y: origin.y,
                w: size.width,
                h: size.height,
            },
        });
    }
    Ok(tokens)
}
