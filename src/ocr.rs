//! Offline OCR via Windows.Media.Ocr (built into Windows 10/11, ~20-50 ms
//! for a typical window). Input is a packed BGRA frame (DXGI capture).

use windows::Globalization::Language;
use windows::Graphics::Imaging::{BitmapPixelFormat, SoftwareBitmap};
use windows::Media::Ocr::OcrEngine;
use windows::Security::Cryptography::CryptographicBuffer;

#[derive(Debug, Clone)]
pub struct OcrLineInfo {
    pub text: String,
    /// Image-local rect (x, y, w, h).
    pub rect: (i32, i32, i32, i32),
}

#[derive(Debug)]
pub struct OcrOutput {
    pub text: String,
    pub lines: Vec<OcrLineInfo>,
    pub language: String,
}

#[derive(Default)]
pub struct OcrCtx {
    engine: Option<(OcrEngine, String)>,
    init_error: Option<String>,
}

impl OcrCtx {
    pub fn new() -> OcrCtx {
        OcrCtx {
            engine: None,
            init_error: None,
        }
    }

    /// Lazy engine creation: user profile languages → en-US → ru-RU → first
    /// available. The error is cached so repeated calls fail fast with a
    /// helpful message.
    fn ensure_engine(&mut self, lang: Option<&str>) -> Result<(OcrEngine, String), String> {
        if let Some(lang_req) = lang {
            let e = create_for_lang(lang_req)?;
            return Ok(e);
        }
        if let Some(e) = &self.engine {
            return Ok(e.clone());
        }
        if let Some(err) = &self.init_error {
            return Err(err.clone());
        }
        match create_default() {
            Ok(e) => {
                self.engine = Some(e.clone());
                Ok(e)
            }
            Err(e) => {
                let msg = format!(
                    "OCR unavailable: {e}. Install a Windows language pack (Settings → Time & language → Language → Add a language) and retry."
                );
                self.init_error = Some(msg.clone());
                Err(msg)
            }
        }
    }

    /// Recognize text in a packed BGRA image. Returns full text plus per-line
    /// boxes (image-local coordinates).
    pub fn recognize(
        &mut self,
        bgra: &[u8],
        w: u32,
        h: u32,
        lang: Option<&str>,
    ) -> Result<OcrOutput, String> {
        if w == 0 || h == 0 {
            return Err("empty region".into());
        }
        let (engine, lang_tag) = self.ensure_engine(lang)?;
        let bitmap = make_bitmap(bgra, w, h)?;
        let result = engine
            .RecognizeAsync(&bitmap)
            .map_err(|e| format!("RecognizeAsync failed: {e}"))?
            .get()
            .map_err(|e| format!("OCR get failed: {e}"))?;
        let mut lines_out = Vec::new();
        let lines = result
            .Lines()
            .map_err(|e| format!("OcrResult.Lines failed: {e}"))?;
        let n = lines.Size().unwrap_or(0);
        for i in 0..n {
            let line = match lines.GetAt(i) {
                Ok(l) => l,
                Err(_) => continue,
            };
            let text = line
                .Text()
                .map(|t| t.to_string_lossy().to_string())
                .unwrap_or_default();
            let mut rects: Vec<(f32, f32, f32, f32)> = Vec::new();
            if let Ok(words) = line.Words() {
                let wn = words.Size().unwrap_or(0);
                for wi in 0..wn {
                    if let Ok(word) = words.GetAt(wi) {
                        if let Ok(r) = word.BoundingRect() {
                            rects.push((r.X, r.Y, r.Width, r.Height));
                        }
                    }
                }
            }
            lines_out.push(OcrLineInfo {
                text,
                rect: union_rects(&rects),
            });
        }
        let full = result
            .Text()
            .map(|t| t.to_string_lossy().to_string())
            .unwrap_or_default();
        Ok(OcrOutput {
            text: full,
            lines: lines_out,
            language: lang_tag,
        })
    }
}

fn union_rects(rs: &[(f32, f32, f32, f32)]) -> (i32, i32, i32, i32) {
    if rs.is_empty() {
        return (0, 0, 0, 0);
    }
    let x0 = rs.iter().map(|r| r.0).fold(f32::MAX, f32::min);
    let y0 = rs.iter().map(|r| r.1).fold(f32::MAX, f32::min);
    let x1 = rs.iter().map(|r| r.0 + r.2).fold(f32::MIN, f32::max);
    let y1 = rs.iter().map(|r| r.1 + r.3).fold(f32::MIN, f32::max);
    (
        x0.round() as i32,
        y0.round() as i32,
        (x1 - x0).round().max(0.0) as i32,
        (y1 - y0).round().max(0.0) as i32,
    )
}

fn make_bitmap(bgra: &[u8], w: u32, h: u32) -> Result<SoftwareBitmap, String> {
    let buffer = CryptographicBuffer::CreateFromByteArray(bgra)
        .map_err(|e| format!("CryptographicBuffer failed: {e}"))?;
    SoftwareBitmap::CreateCopyFromBuffer(&buffer, BitmapPixelFormat::Bgra8, w as i32, h as i32)
        .map_err(|e| format!("SoftwareBitmap::CreateCopyFromBuffer failed: {e}"))
}

fn lang_tag(l: &Language) -> String {
    l.LanguageTag().map(|t| t.to_string_lossy().to_string()).unwrap_or_default()
}

fn create_for_lang(tag: &str) -> Result<(OcrEngine, String), String> {
    let h = windows::core::HSTRING::from(tag);
    let language = Language::CreateLanguage(&h)
        .map_err(|e| format!("Language::CreateLanguage('{tag}') failed: {e}"))?;
    let engine = OcrEngine::TryCreateFromLanguage(&language)
        .map_err(|_| format!("no OCR recognizer for language '{tag}'"))?;
    Ok((engine, tag.to_string()))
}

fn create_default() -> Result<(OcrEngine, String), String> {
    if let Ok(e) = OcrEngine::TryCreateFromUserProfileLanguages() {
        let tag = e
            .RecognizerLanguage()
            .map(|l| lang_tag(&l))
            .unwrap_or_default();
        return Ok((e, tag));
    }
    for fallback in ["en-US", "ru-RU"] {
        if let Ok(pair) = create_for_lang(fallback) {
            return Ok(pair);
        }
    }
    // Last resort: first available recognizer language.
    if let Ok(langs) = OcrEngine::AvailableRecognizerLanguages() {
        if let Ok(first) = langs.First() {
            if first.MoveNext().unwrap_or(false) {
                if let Ok(l) = first.Current() {
                    let tag = lang_tag(&l);
                    if let Ok(pair) = create_for_lang(&tag) {
                        return Ok(pair);
                    }
                }
            }
        }
    }
    Err("no OCR recognizer language found".into())
}

/// Diagnostics: list recognizer languages usable right now.
pub fn available_languages() -> Vec<String> {
    match OcrEngine::AvailableRecognizerLanguages() {
        Ok(langs) => {
            let mut out = Vec::new();
            if let Ok(it) = langs.First() {
                while it.MoveNext().unwrap_or(false) {
                    if let Ok(l) = it.Current() {
                        out.push(lang_tag(&l));
                    }
                }
            }
            out
        }
        Err(_) => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn union_rects_math() {
        // y spans: 20..30 and 25..30 -> union y 20..30 (h=10); x 10..50.
        let r = union_rects(&[(10.0, 20.0, 30.0, 10.0), (20.0, 25.0, 40.0, 5.0)]);
        assert_eq!(r, (10, 20, 50, 10));
        assert_eq!(union_rects(&[]), (0, 0, 0, 0));
    }

    #[test]
    fn make_bitmap_rejects_empty() {
        assert!(make_bitmap(&[0u8; 0], 0, 0).is_err());
    }
}
