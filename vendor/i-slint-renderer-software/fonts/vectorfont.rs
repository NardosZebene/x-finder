// Copyright © SixtyFPS GmbH <info@slint.dev>
// SPDX-License-Identifier: GPL-3.0-only OR LicenseRef-Slint-Royalty-free-2.0 OR LicenseRef-Slint-Software-3.0

use core::num::NonZeroU16;

use alloc::rc::Rc;
use alloc::vec::Vec;
use skrifa::MetadataProvider;

use crate::PhysicalLength;
use crate::fixed::Fixed;
use i_slint_common::sharedfontique::fontique;
use i_slint_core::lengths::PhysicalPx;
use i_slint_core::textlayout::{Glyph, TextShaper};

use super::RenderableVectorGlyph;

// A length in font design space.
struct FontUnit;
type FontLength = euclid::Length<i32, FontUnit>;
type FontScaleFactor = euclid::Scale<f32, FontUnit, PhysicalPx>;

/// Number of horizontal sub-pixel positions a glyph can be placed at. The
/// shaper produces sub-pixel accurate pen positions, but glyph bitmaps live on
/// the integer pixel grid; rendering each glyph at the nearest 1/N pixel bin
/// (instead of snapping the pen to a whole pixel) keeps inter-glyph spacing
/// even. 4 bins (quarter-pixel) is enough to remove the visible unevenness at
/// UI text sizes while keeping the glyph cache small.
pub(crate) const SUBPIXEL_BIN_COUNT: i32 = 4;

/// Cache key includes blob id, font index, pixel size, glyph id, a hash of normalized
/// variation coordinates (so different variable font instances produce distinct cache
/// entries) and the horizontal sub-pixel bin.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct GlyphCacheKey {
    /// Font blob id.
    font_blob_id: u64,
    /// Font index within the blob.
    font_index: u32,
    /// Rendered pixel size.
    pixel_size: PhysicalLength,
    /// Glyph id.
    glyph_id: core::num::NonZeroU16,
    /// Hash of the normalized variation coordinates.
    coords_hash: u64,
    /// Horizontal sub-pixel bin.
    subpixel_bin: u8,
}

struct RenderableGlyphWeightScale;

impl clru::WeightScale<GlyphCacheKey, RenderableVectorGlyph> for RenderableGlyphWeightScale {
    fn weight(&self, _: &GlyphCacheKey, value: &RenderableVectorGlyph) -> usize {
        value.alpha_map.len()
    }
}

type GlyphCache = clru::CLruCache<
    GlyphCacheKey,
    RenderableVectorGlyph,
    std::collections::hash_map::RandomState,
    RenderableGlyphWeightScale,
>;

i_slint_core::thread_local!(static GLYPH_CACHE: core::cell::RefCell<GlyphCache>  =
    core::cell::RefCell::new(
        clru::CLruCache::with_config(
            clru::CLruCacheConfig::new(core::num::NonZeroUsize::new(1024 * 1024).unwrap())
                .with_scale(RenderableGlyphWeightScale)
        )
    )
);

pub struct VectorFont {
    font_index: u32,
    font_blob: fontique::Blob<u8>,
    swash_key: swash::CacheKey,
    swash_offset: u32,
    ascender: PhysicalLength,
    descender: PhysicalLength,
    height: PhysicalLength,
    pixel_size: PhysicalLength,
    x_height: PhysicalLength,
    cap_height: PhysicalLength,
    /// Normalized variation coordinates (F2Dot14, fvar axis order) for variable font rendering.
    normalized_coords: Vec<i16>,
    /// Hash of normalized_coords for use in the glyph cache key.
    coords_hash: u64,
}

fn hash_coords(coords: &[i16]) -> u64 {
    use core::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    coords.hash(&mut hasher);
    hasher.finish()
}

impl VectorFont {
    fn swash_font_ref(&self) -> swash::FontRef<'_> {
        swash::FontRef {
            data: self.font_blob.data(),
            offset: self.swash_offset,
            key: self.swash_key,
        }
    }

    pub fn new(
        font: fontique::QueryFont,
        swash_key: swash::CacheKey,
        swash_offset: u32,
        pixel_size: PhysicalLength,
    ) -> Self {
        Self::new_from_blob_and_index(font.blob, font.index, swash_key, swash_offset, pixel_size)
    }

    pub fn new_from_blob_and_index(
        font_blob: fontique::Blob<u8>,
        font_index: u32,
        swash_key: swash::CacheKey,
        swash_offset: u32,
        pixel_size: PhysicalLength,
    ) -> Self {
        Self::new_from_blob_and_index_with_coords(
            font_blob,
            font_index,
            swash_key,
            swash_offset,
            pixel_size,
            &[],
        )
    }

    pub fn new_from_blob_and_index_with_coords(
        font_blob: fontique::Blob<u8>,
        font_index: u32,
        swash_key: swash::CacheKey,
        swash_offset: u32,
        pixel_size: PhysicalLength,
        normalized_coords: &[i16],
    ) -> Self {
        let face = skrifa::FontRef::from_index(font_blob.data(), font_index).unwrap();

        let skrifa_coords: Vec<skrifa::instance::NormalizedCoord> = normalized_coords
            .iter()
            .map(|&c| skrifa::instance::NormalizedCoord::from_bits(c))
            .collect();
        let location = skrifa::instance::LocationRef::new(&skrifa_coords);

        let metrics = face.metrics(skrifa::instance::Size::unscaled(), location);

        let ascender = FontLength::new(metrics.ascent as _);
        let descender = FontLength::new(metrics.descent as _);
        let height = FontLength::new((metrics.ascent - metrics.descent) as _);
        let x_height = FontLength::new(metrics.x_height.unwrap_or_default() as _);
        let cap_height = FontLength::new(metrics.cap_height.unwrap_or_default() as _);
        let units_per_em = metrics.units_per_em;
        let scale = FontScaleFactor::new(pixel_size.get() as f32 / units_per_em as f32);
        let coords_hash = hash_coords(normalized_coords);
        Self {
            font_index,
            font_blob,
            swash_key,
            swash_offset,
            ascender: (ascender.cast() * scale).cast(),
            descender: (descender.cast() * scale).cast(),
            height: (height.cast() * scale).cast(),
            pixel_size,
            x_height: (x_height.cast() * scale).cast(),
            cap_height: (cap_height.cast() * scale).cast(),
            normalized_coords: normalized_coords.to_vec(),
            coords_hash,
        }
    }

    pub fn render_vector_glyph(
        &self,
        glyph_id: core::num::NonZeroU16,
        subpixel_bin: u8,
        slint_context: &i_slint_core::SlintContext,
    ) -> Option<RenderableVectorGlyph> {
        GLYPH_CACHE.with(|cache| {
            let mut cache = cache.borrow_mut();

            let cache_key = GlyphCacheKey {
                font_blob_id: self.font_blob.id(),
                font_index: self.font_index,
                pixel_size: self.pixel_size,
                glyph_id,
                coords_hash: self.coords_hash,
                subpixel_bin,
            };

            if let Some(entry) = cache.get(&cache_key) {
                return Some(entry.clone());
            }

            // X Finder: glyphs sit on whole pixels; the bin only says
            // whether the text is light (contrast boost) or dark.
            let boost = subpixel_bin as i32 >= SUBPIXEL_BIN_COUNT;
            let subpixel_offset_x = if boost { 0.0 } else { subpixel_bin as f32 / SUBPIXEL_BIN_COUNT as f32 };

            let glyph = {
                let font_ref = self.swash_font_ref();
                let mut ctx = slint_context.swash_scale_context().borrow_mut();
                let mut scaler = ctx
                    .builder(font_ref)
                    .size(self.pixel_size.get() as f32)
                    // X Finder: hinted outlines snap stems to whole pixels
                    // (like Windows' own text), so small text stays sharp.
                    .hint(boost)
                    .normalized_coords(&self.normalized_coords)
                    .build();
                let image = swash::scale::Render::new(&[swash::scale::Source::Outline])
                    .format(swash::zeno::Format::Alpha)
                    .offset(swash::zeno::Vector::new(subpixel_offset_x, 0.0))
                    .render(&mut scaler, glyph_id.get())?;

                let placement = image.placement;
                // X Finder: a little more contrast in the edge pixels, as
                // ClearType does; plain coverage looks thin and grey.
                let alpha_map: Rc<[u8]> = if boost {
                    image.data.iter().map(|&a| TEXT_CONTRAST[a as usize]).collect::<Vec<u8>>().into()
                } else {
                    image.data.into()
                };

                Some(RenderableVectorGlyph {
                    x: Fixed::from_integer(placement.left),
                    y: Fixed::from_integer(placement.top - placement.height as i32),
                    width: PhysicalLength::new(placement.width.try_into().unwrap()),
                    height: PhysicalLength::new(placement.height.try_into().unwrap()),
                    alpha_map,
                    pixel_stride: placement.width.try_into().unwrap(),
                    glyph_origin_x: placement.left as f32,
                })
            };

            if let Some(ref glyph) = glyph {
                cache.put_with_weight(cache_key, glyph.clone()).ok();
            }
            glyph
        })
    }
}

impl TextShaper for VectorFont {
    type LengthPrimitive = i16;
    type Length = PhysicalLength;
    fn shape_text<GlyphStorage: core::iter::Extend<Glyph<PhysicalLength>>>(
        &self,
        text: &str,
        glyphs: &mut GlyphStorage,
    ) {
        let font_ref = self.swash_font_ref();
        let charmap = font_ref.charmap();
        let gm = font_ref.glyph_metrics(&[]);
        let metrics = font_ref.metrics(&[]);
        let scale = self.pixel_size.get() as f32 / metrics.units_per_em as f32;

        glyphs.extend(text.char_indices().map(|(byte_offset, char)| {
            let glyph_id = NonZeroU16::try_from(charmap.map(char)).ok();
            let x_advance = glyph_id.map_or_else(
                || self.pixel_size.get(),
                |id| (gm.advance_width(id.get()) * scale) as _,
            );

            Glyph {
                glyph_id,
                advance: PhysicalLength::new(x_advance),
                text_byte_offset: byte_offset,
                ..Default::default()
            }
        }));
    }

    fn glyph_for_char(&self, ch: char) -> Option<Glyph<PhysicalLength>> {
        let font_ref = self.swash_font_ref();
        let charmap = font_ref.charmap();
        let gm = font_ref.glyph_metrics(&[]);
        let metrics = font_ref.metrics(&[]);
        let scale = self.pixel_size.get() as f32 / metrics.units_per_em as f32;

        NonZeroU16::try_from(charmap.map(ch)).ok().map(|glyph_id| Glyph {
            glyph_id: Some(glyph_id),
            advance: PhysicalLength::new((gm.advance_width(glyph_id.get()) * scale) as _),
            ..Default::default()
        })
    }
}

impl i_slint_core::textlayout::FontMetrics<PhysicalLength> for VectorFont {
    fn ascent(&self) -> PhysicalLength {
        self.ascender
    }

    fn height(&self) -> PhysicalLength {
        self.height
    }

    fn descent(&self) -> PhysicalLength {
        self.descender
    }

    fn x_height(&self) -> PhysicalLength {
        self.x_height
    }

    fn cap_height(&self) -> PhysicalLength {
        self.cap_height
    }
}

impl super::GlyphRenderer for VectorFont {
    fn render_glyph(
        &self,
        glyph_id: core::num::NonZeroU16,
        slint_context: &i_slint_core::SlintContext,
    ) -> Option<super::RenderableGlyph> {
        self.render_vector_glyph(glyph_id, 0, slint_context).map(|glyph| super::RenderableGlyph {
            x: glyph.x,
            y: glyph.y,
            width: glyph.width,
            height: glyph.height,
            alpha_map: glyph.alpha_map.into(),
            pixel_stride: glyph.pixel_stride,
            sdf: false,
        })
    }

    fn scale_delta(&self) -> super::Fixed<u16, 8> {
        super::Fixed::from_integer(1)
    }
}

/// X Finder: coverage -> alpha with a gentle contrast curve (gamma 1/1.3),
/// so anti-aliased edges read darker and text looks crisper.
pub(crate) static TEXT_CONTRAST: [u8; 256] = {
    let mut t = [0u8; 256];
    let mut i = 0;
    while i < 256 {
        // Integer approximation of 255 * (i/255)^(1/1.3) via a blend of
        // the linear value and a square-root-ish curve.
        let lin = i as u32;
        let mut r = 0u32;
        while (r + 1) * (r + 1) <= lin * 255 {
            r += 1;
        }
        // r = sqrt(i*255) ~ 255*(i/255)^0.5; mix 70% linear, 30% sqrt:
        // a gentle boost, so light text stays thin and clean.
        t[i] = ((lin * 55 + r * 45) / 100) as u8;
        i += 1;
    }
    t
};


impl VectorFont {
    /// X Finder: the glyph as Windows (DirectWrite) draws it, if the font is
    /// installed on this PC.
    #[cfg(windows)]
    pub fn render_dwrite_glyph(&self, glyph_id: NonZeroU16, bin: u8) -> Option<super::dwrite::DwGlyph> {
        super::dwrite::render(&self.swash_font_ref(), (self.font_blob.id(), self.font_index), self.pixel_size.get(), glyph_id, bin)
    }
}

/// X Finder: a glyph drawn ClearType-style (three samples per pixel, one per
/// red/green/blue stripe of the screen), already coloured, as premultiplied
/// RGBA. Only used for light text on dark backgrounds, where blending the
/// colour fringes with a single alpha is all but exact.
#[derive(Clone)]
pub struct SubpixelGlyph {
    pub x: i32,
    pub top: i32,
    pub width: u32,
    pub height: u32,
    pub image: i_slint_core::graphics::SharedImageBuffer,
}

i_slint_core::thread_local!(static SUBPIXEL_CACHE: core::cell::RefCell<std::collections::HashMap<(u64, u32, i16, u16, u64, u32), Option<SubpixelGlyph>>> = Default::default());

/// FreeType's default LCD filter: spreads each sample over its neighbours so
/// the colour fringes stay faint.
const LCD_FILTER: [u32; 5] = [8, 77, 86, 77, 8];

impl VectorFont {
    pub fn render_subpixel_glyph(&self, glyph_id: NonZeroU16, color: i_slint_core::Color, slint_context: &i_slint_core::SlintContext) -> Option<SubpixelGlyph> {
        let rgb = ((color.red() as u32) << 16) | ((color.green() as u32) << 8) | color.blue() as u32;
        let key = (self.font_blob.id(), self.font_index, self.pixel_size.get(), glyph_id.get(), self.coords_hash, rgb);
        if let Some(hit) = SUBPIXEL_CACHE.with(|c| c.borrow().get(&key).cloned()) {
            return hit;
        }
        let made = (|| {
            let font_ref = self.swash_font_ref();
            let mut ctx = slint_context.swash_scale_context().borrow_mut();
            let mut scaler = ctx.builder(font_ref).size(self.pixel_size.get() as f32).hint(true).normalized_coords(&self.normalized_coords).build();
            let image = swash::scale::Render::new(&[swash::scale::Source::Outline])
                .format(swash::zeno::Format::Subpixel)
                .render(&mut scaler, glyph_id.get())?;
            let p = image.placement;
            let (w, h) = (p.width as usize, p.height as usize);
            if w == 0 || h == 0 {
                return None;
            }
            // One extra pixel each side for the filter's spread.
            let ow = w + 2;
            let mut out = alloc::vec![0u8; ow * h * 4];
            let (cr, cg, cb) = (color.red() as u32, color.green() as u32, color.blue() as u32);
            let mut sub = alloc::vec![0u32; (w + 2) * 3];
            for y in 0..h {
                // Subpixel samples of this row, padded by one pixel each side.
                for v in sub.iter_mut() {
                    *v = 0;
                }
                for x in 0..w {
                    let s = (y * w + x) * 4;
                    sub[(x + 1) * 3] = image.data[s] as u32;
                    sub[(x + 1) * 3 + 1] = image.data[s + 1] as u32;
                    sub[(x + 1) * 3 + 2] = image.data[s + 2] as u32;
                }
                for x in 0..ow {
                    let mut c = [0u32; 3];
                    for (ch, cv) in c.iter_mut().enumerate() {
                        let center = (x * 3 + ch) as i64;
                        let mut acc = 0u32;
                        for (k, wt) in LCD_FILTER.iter().enumerate() {
                            let i = center + k as i64 - 2;
                            if i >= 0 && (i as usize) < sub.len() {
                                acc += sub[i as usize] * wt;
                            }
                        }
                        *cv = (acc / 256).min(255);
                        // Gentle contrast, as for the grayscale glyphs.
                        *cv = super::vectorfont::TEXT_CONTRAST[*cv as usize] as u32;
                    }
                    let a = c[0].max(c[1]).max(c[2]);
                    let o = (y * ow + x) * 4;
                    out[o] = (cr * c[0] / 255) as u8;
                    out[o + 1] = (cg * c[1] / 255) as u8;
                    out[o + 2] = (cb * c[2] / 255) as u8;
                    out[o + 3] = a as u8;
                }
            }
            let buf = i_slint_core::graphics::SharedPixelBuffer::<i_slint_core::graphics::Rgba8Pixel>::clone_from_slice(&out, ow as u32, h as u32);
            Some(SubpixelGlyph {
                x: p.left - 1,
                top: p.top,
                width: ow as u32,
                height: h as u32,
                image: i_slint_core::graphics::SharedImageBuffer::RGBA8Premultiplied(buf),
            })
        })();
        SUBPIXEL_CACHE.with(|c| {
            let mut c = c.borrow_mut();
            if c.len() > 8000 {
                c.clear();
            }
            c.insert(key, made.clone());
        });
        made
    }
}
