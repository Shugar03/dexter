use dexter_core::Rect;

use crate::{ocr_word_rect, VisionToken};

#[cfg(target_os = "linux")]
use crate::{VisionError, VisionProvider};
#[cfg(target_os = "linux")]
use std::fs::{self, OpenOptions};
#[cfg(target_os = "linux")]
use std::io::Write;
#[cfg(target_os = "linux")]
use std::path::PathBuf;
#[cfg(target_os = "linux")]
use std::process::{Command, Stdio};
#[cfg(target_os = "linux")]
use std::sync::atomic::{AtomicU64, Ordering};
#[cfg(target_os = "linux")]
use std::time::{SystemTime, UNIX_EPOCH};

#[cfg(target_os = "linux")]
static NEXT_TEMP_ID: AtomicU64 = AtomicU64::new(0);

pub fn parse_tsv(tsv: &str, img_w: u32, img_h: u32) -> Vec<VisionToken> {
    if img_w == 0 || img_h == 0 {
        return Vec::new();
    }

    tsv.lines()
        .filter_map(|line| {
            let columns: Vec<_> = line.splitn(12, '\t').collect();
            if columns.len() != 12 || columns[0].parse::<u32>().ok()? != 5 {
                return None;
            }
            if columns[1..6]
                .iter()
                .any(|column| column.parse::<u32>().is_err())
            {
                return None;
            }

            let left = columns[6].parse::<f64>().ok()?;
            let top = columns[7].parse::<f64>().ok()?;
            let width = columns[8].parse::<f64>().ok()?;
            let height = columns[9].parse::<f64>().ok()?;
            let confidence = columns[10].parse::<f32>().ok()?;
            let text = columns[11].trim();
            if ![left, top, width, height].into_iter().all(f64::is_finite)
                || width <= 0.0
                || height <= 0.0
                || !confidence.is_finite()
                || confidence < 0.0
                || text.is_empty()
            {
                return None;
            }

            Some(VisionToken {
                text: text.to_owned(),
                bounds: ocr_word_rect(
                    &Rect {
                        x: left,
                        y: top,
                        w: width,
                        h: height,
                    },
                    img_w,
                    img_h,
                ),
                confidence: (confidence / 100.0).clamp(0.0, 1.0),
            })
        })
        .collect()
}

#[cfg(target_os = "linux")]
#[derive(Debug, Clone, Copy, Default)]
pub struct Tesseract;

#[cfg(target_os = "linux")]
struct TempPng(PathBuf);

#[cfg(target_os = "linux")]
impl TempPng {
    fn create(bytes: &[u8]) -> Result<Self, VisionError> {
        for _ in 0..16 {
            let timestamp = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos();
            let unique = NEXT_TEMP_ID.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "dexter-tesseract-{}-{timestamp}-{unique}.png",
                std::process::id()
            ));
            match OpenOptions::new().write(true).create_new(true).open(&path) {
                Ok(mut file) => {
                    let temp = Self(path);
                    file.write_all(bytes).map_err(|error| {
                        VisionError::Failed(format!("write temporary PNG: {error}"))
                    })?;
                    return Ok(temp);
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(error) => {
                    return Err(VisionError::Failed(format!(
                        "create temporary PNG: {error}"
                    )));
                }
            }
        }
        Err(VisionError::Failed(
            "could not allocate a unique temporary PNG".into(),
        ))
    }
}

#[cfg(target_os = "linux")]
impl Drop for TempPng {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

#[cfg(target_os = "linux")]
impl VisionProvider for Tesseract {
    fn name(&self) -> &'static str {
        "tesseract"
    }

    fn recognize(&self, image_png: &[u8]) -> Result<Vec<VisionToken>, VisionError> {
        let (img_w, img_h) = crate::png_dimensions(image_png)
            .ok_or_else(|| VisionError::Failed("input is not a valid PNG".into()))?;
        let png = TempPng::create(image_png)?;
        let mut command = Command::new("tesseract");
        command
            .arg(&png.0)
            .arg("stdout")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if let Some(language) = std::env::var_os("DEXTER_TESSERACT_LANG") {
            command.arg("-l").arg(language);
        }
        let output = command
            .arg("--psm")
            .arg("11")
            .arg("tsv")
            .output()
            .map_err(|error| VisionError::Failed(format!("spawn tesseract: {error}")))?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(VisionError::Failed(format!(
                "tesseract exited with {}: {}",
                output.status,
                stderr.trim()
            )));
        }
        let tsv = String::from_utf8(output.stdout).map_err(|error| {
            VisionError::Failed(format!("Tesseract TSV was not UTF-8: {error}"))
        })?;
        Ok(parse_tsv(&tsv, img_w, img_h))
    }
}

#[cfg(target_os = "linux")]
pub(crate) fn available() -> bool {
    Command::new("tesseract")
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

#[cfg(test)]
mod tests {
    use super::parse_tsv;
    use crate::NormRect;

    #[test]
    fn parses_word_rows_and_skips_invalid_or_non_word_rows() {
        let tsv = concat!(
            "level\tpage_num\tblock_num\tpar_num\tline_num\tword_num\tleft\ttop\twidth\theight\tconf\ttext\n",
            "1\t1\t0\t0\t0\t0\t0\t0\t200\t100\t-1\tpage\n",
            "2\t1\t1\t0\t0\t0\t0\t0\t200\t100\t-1\tblock\n",
            "3\t1\t1\t1\t0\t0\t0\t0\t200\t100\t-1\tparagraph\n",
            "4\t1\t1\t1\t1\t0\t10\t10\t100\t20\t-1\tline\n",
            "5\t1\t1\t1\t1\t1\t20\t10\t40\t20\t96.5\t DEXTER \n",
            "5\t1\t1\t1\t1\t2\t20\t40\t30\t12\t-1\tlow confidence\n",
            "5\t1\t1\t1\t1\t3\t20\t60\t30\t12\t80\t   \n",
            "5\t1\t1\t1\t1\t4\t20\t70\t0\t12\t80\tzero width\n",
            "5\t1\t1\t1\t1\t5\t20\t80\t30\t0\t80\tzero height\n",
            "5\t1\t1\t1\t1\t6\t100\t20\t20\t10\t120\tPROBE\n",
            "malformed row\n",
        );
        let tokens = parse_tsv(tsv, 200, 100);

        assert_eq!(tokens.len(), 2);
        assert_eq!(tokens[0].text, "DEXTER");
        assert!((tokens[0].confidence - 0.965).abs() < f32::EPSILON);
        assert_eq!(
            tokens[0].bounds,
            NormRect {
                x: 0.1,
                y: 0.7,
                w: 0.2,
                h: 0.2,
            }
        );
        assert_eq!(tokens[1].text, "PROBE");
        assert_eq!(tokens[1].confidence, 1.0);
    }

    #[test]
    fn malformed_and_degenerate_dimensions_do_not_panic() {
        let tsv = concat!(
            "level\tpage_num\tblock_num\tpar_num\tline_num\tword_num\tleft\ttop\twidth\theight\tconf\ttext\n",
            "5\t1\t1\t1\t1\t1\t0\t0\t10\t10\t90\tTEXT\n",
        );
        assert!(parse_tsv(tsv, 0, 100).is_empty());
        assert!(parse_tsv(tsv, 100, 0).is_empty());
        assert!(parse_tsv("not TSV", 100, 100).is_empty());
    }
}
