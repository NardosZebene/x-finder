#![allow(unsafe_code)]
// X Finder: glyphs rasterised by DirectWrite, Windows' own text engine (the
// same one Skia uses on Windows), for light text on dark backgrounds. The
// letters come out shaped and hinted exactly like Windows text; everything
// else (layout, blending) stays in the software renderer.

use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec::Vec;
use core::num::NonZeroU16;
use std::collections::HashMap;

use windows::Win32::Graphics::DirectWrite::*;
use windows::core::{BOOL, HSTRING};

#[derive(Clone)]
pub struct DwGlyph {
    /// Left edge relative to the pen position.
    pub left: i32,
    /// Top edge relative to the baseline (negative: above it).
    pub top: i32,
    pub width: u16,
    pub height: u16,
    pub alpha: Rc<[u8]>,
}

/// Tuning (fixed in releases; X_FINDER_DW="mode,gamma,contrast,bins" overrides
/// while comparing against Skia).
#[derive(Clone, Copy)]
pub struct Tune {
    /// 0: ClearType texture averaged to gray, 1: DirectWrite grayscale.
    pub mode: u8,
    pub gamma: f32,
    /// Extra contrast around mid coverage (0 = none).
    pub contrast: f32,
    /// Horizontal positions per pixel (1 = whole pixels).
    pub bins: u8,
}

pub fn tune() -> Tune {
    static T: std::sync::OnceLock<Tune> = std::sync::OnceLock::new();
    *T.get_or_init(|| {
        // Measured closest to Skia: grayscale, c^0.6, a little contrast, 1/4-pixel placement.
        let mut t = Tune { mode: 1, gamma: 0.6, contrast: 0.2, bins: 4 };
        if let Ok(v) = std::env::var("X_FINDER_DW") {
            let p: Vec<f32> = v.split(',').filter_map(|x| x.trim().parse().ok()).collect();
            if p.len() == 4 {
                t = Tune { mode: p[0] as u8, gamma: p[1], contrast: p[2], bins: (p[3] as u8).max(1) };
            }
        }
        t
    })
}

i_slint_core::thread_local! {
    static FACTORY: Option<IDWriteFactory2> = unsafe { DWriteCreateFactory::<IDWriteFactory2>(DWRITE_FACTORY_TYPE_SHARED).ok() };
    /// Font file (blob id, index) -> its DirectWrite face, if Windows has it.
    static FACES: core::cell::RefCell<HashMap<(u64, u32), Option<IDWriteFontFace>>> = Default::default();
    static GLYPHS: core::cell::RefCell<HashMap<(u64, u32, i16, u16, u8), Option<DwGlyph>>> = Default::default();
}

/// The installed font that matches this font file (by family name, weight,
/// width and slant), as a DirectWrite face.
fn face_for(font: &swash::FontRef<'_>, key: (u64, u32)) -> Option<IDWriteFontFace> {
    if let Some(f) = FACES.with(|m| m.borrow().get(&key).cloned()) {
        return f;
    }
    let found = (|| unsafe {
        let factory = FACTORY.with(|f| f.clone())?;
        let strings = font.localized_strings();
        let family = strings
            .find_by_id(swash::StringId::TypographicFamily, None)
            .or_else(|| strings.find_by_id(swash::StringId::Family, None))?
            .chars()
            .collect::<String>();
        let mut coll: Option<IDWriteFontCollection> = None;
        factory.GetSystemFontCollection(&mut coll, false).ok()?;
        let coll = coll?;
        let (mut index, mut exists) = (0u32, BOOL(0));
        coll.FindFamilyName(&HSTRING::from(family.as_str()), &mut index, &mut exists).ok()?;
        if !exists.as_bool() {
            return None;
        }
        let fam = coll.GetFontFamily(index).ok()?;
        let attrs = font.attributes();
        let style = match attrs.style() {
            swash::Style::Normal => DWRITE_FONT_STYLE_NORMAL,
            swash::Style::Italic => DWRITE_FONT_STYLE_ITALIC,
            swash::Style::Oblique(_) => DWRITE_FONT_STYLE_OBLIQUE,
        };
        let stretch = DWRITE_FONT_STRETCH(((attrs.stretch().to_percentage() - 50.0) / 12.5 + 1.0).round().clamp(1.0, 9.0) as i32);
        let f = fam.GetFirstMatchingFont(DWRITE_FONT_WEIGHT(attrs.weight().0 as i32), stretch, style).ok()?;
        f.CreateFontFace().ok()
    })();
    FACES.with(|m| m.borrow_mut().insert(key, found.clone()));
    found
}

/// Renders one glyph with DirectWrite at `pixel_size`, offset by `bin` /
/// bins of a pixel, as an 8-bit coverage map. `None` when the font isn't
/// installed (then the caller falls back).
pub fn render(font: &swash::FontRef<'_>, key: (u64, u32), pixel_size: i16, glyph: NonZeroU16, bin: u8) -> Option<DwGlyph> {
    let gkey = (key.0, key.1, pixel_size, glyph.get(), bin);
    if let Some(g) = GLYPHS.with(|m| m.borrow().get(&gkey).cloned()) {
        return g;
    }
    let t = tune();
    let made = (|| unsafe {
        let factory = FACTORY.with(|f| f.clone())?;
        let face = face_for(font, key)?;
        let index = glyph.get();
        let advance = 0f32;
        let offset = DWRITE_GLYPH_OFFSET::default();
        let run = DWRITE_GLYPH_RUN {
            fontFace: core::mem::ManuallyDrop::new(Some(face)),
            fontEmSize: pixel_size as f32,
            glyphCount: 1,
            glyphIndices: &index,
            glyphAdvances: &advance,
            glyphOffsets: &offset,
            isSideways: BOOL(0),
            bidiLevel: 0,
        };
        let x = bin as f32 / t.bins as f32;
        let gray = t.mode == 1;
        let analysis = factory
            .CreateGlyphRunAnalysis(
                &run,
                None,
                DWRITE_RENDERING_MODE_NATURAL_SYMMETRIC,
                DWRITE_MEASURING_MODE_NATURAL,
                DWRITE_GRID_FIT_MODE_ENABLED,
                if gray { DWRITE_TEXT_ANTIALIAS_MODE_GRAYSCALE } else { DWRITE_TEXT_ANTIALIAS_MODE_CLEARTYPE },
                x,
                0.0,
            )
            .ok();
        drop(core::mem::ManuallyDrop::into_inner(run.fontFace));
        let analysis = analysis?;
        let tex = if gray { DWRITE_TEXTURE_ALIASED_1x1 } else { DWRITE_TEXTURE_CLEARTYPE_3x1 };
        let b = analysis.GetAlphaTextureBounds(tex).ok()?;
        let (w, h) = ((b.right - b.left).max(0) as usize, (b.bottom - b.top).max(0) as usize);
        if w == 0 || h == 0 {
            return None;
        }
        let per = if gray { 1 } else { 3 };
        let mut raw = alloc::vec![0u8; w * h * per];
        analysis.CreateAlphaTexture(tex, &b, &mut raw).ok()?;
        let curve = |v: f32| {
            let mut v = v.powf(t.gamma);
            if t.contrast != 0.0 {
                // S-curve around the middle: faint edges fainter, cores fuller.
                v = ((v - 0.5) * (1.0 + t.contrast) + 0.5).clamp(0.0, 1.0);
            }
            (v * 255.0 + 0.5) as u8
        };
        let alpha: Vec<u8> = raw
            .chunks_exact(per)
            .map(|c| curve(c.iter().map(|&v| v as f32).sum::<f32>() / (per as f32 * 255.0)))
            .collect();
        Some(DwGlyph { left: b.left, top: b.top, width: w as u16, height: h as u16, alpha: alpha.into() })
    })();
    GLYPHS.with(|m| {
        let mut m = m.borrow_mut();
        if m.len() > 8000 {
            m.clear();
        }
        m.insert(gkey, made.clone());
    });
    made
}
