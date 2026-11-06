//! Windows imaging: shell thumbnails (the same provider and on-disk cache
//! Explorer uses) and PDF page rendering with the PDF engine built into
//! Windows 10+. All functions block; call them from worker threads that have
//! called [`init_thread`].

use std::path::Path;

/// A decoded image, RGBA with premultiplied alpha.
#[derive(Clone, Debug)]
pub struct Pixels {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

#[cfg(windows)]
mod imp {
    use super::Pixels;
    use std::path::Path;

    use windows::Data::Pdf::{PdfDocument, PdfPageRenderOptions};
    use windows::Graphics::Imaging::BitmapEncoder;
    use windows::Storage::StorageFile;
    use windows::Storage::Streams::{Buffer, DataReader, InMemoryRandomAccessStream, InputStreamOptions};
    use windows::Win32::Foundation::SIZE;
    use windows::Win32::Graphics::Gdi::{
        BI_RGB, BITMAP, BITMAPINFO, BITMAPINFOHEADER, CreateCompatibleDC, DIB_RGB_COLORS, DeleteDC, DeleteObject,
        GetDIBits, GetObjectW, HBITMAP, HGDIOBJ,
    };
    use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, COINIT_MULTITHREADED, CoInitializeEx};
    use windows::Win32::UI::Shell::{
        IShellItemImageFactory, SHCreateItemFromParsingName, SIIGBF_BIGGERSIZEOK, SIIGBF_ICONONLY, SIIGBF_RESIZETOFIT,
        SIIGBF_THUMBNAILONLY,
    };
    use windows::core::{HSTRING, Interface};

    pub fn init_thread(sta: bool) {
        // SAFETY: called once at the start of a worker thread.
        unsafe {
            let _ = CoInitializeEx(None, if sta { COINIT_APARTMENTTHREADED } else { COINIT_MULTITHREADED });
        }
    }

    pub fn thumbnail(path: &Path, size: u32) -> Option<Pixels> {
        image(path, size, SIIGBF_THUMBNAILONLY | SIIGBF_BIGGERSIZEOK | SIIGBF_RESIZETOFIT)
    }

    /// One frame from a video, decoded by Media Foundation: used when the
    /// shell has no thumbnail for it (a player that took over the file
    /// type, or a thumbnailer that gives up on big files).
    pub fn video_frame(path: &Path, size: u32) -> Option<Pixels> {
        use std::sync::Once;
        use windows::Win32::Media::MediaFoundation::*;
        use windows::Win32::System::Com::StructuredStorage::{PROPVARIANT, PropVariantToInt64};
        use windows::Win32::System::Variant::VT_I8;
        static STARTUP: Once = Once::new();
        // SAFETY: plain Media Foundation calls; the buffer is unlocked below.
        unsafe {
            STARTUP.call_once(|| {
                let _ = MFStartup(MF_VERSION, MFSTARTUP_LITE);
            });
            let mut attrs: Option<IMFAttributes> = None;
            MFCreateAttributes(&mut attrs, 1).ok()?;
            let attrs = attrs?;
            attrs.SetUINT32(&MF_SOURCE_READER_ENABLE_VIDEO_PROCESSING, 1).ok()?;
            let reader = MFCreateSourceReaderFromURL(&HSTRING::from(path.as_os_str()), &attrs).ok()?;
            let stream = MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32;
            let _ = reader.SetStreamSelection(MF_SOURCE_READER_ALL_STREAMS.0 as u32, false);
            reader.SetStreamSelection(stream, true).ok()?;
            let want = MFCreateMediaType().ok()?;
            want.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video).ok()?;
            want.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_RGB32).ok()?;
            reader.SetCurrentMediaType(stream, None, &want).ok()?;
            let got = reader.GetCurrentMediaType(stream).ok()?;
            let packed = got.GetUINT64(&MF_MT_FRAME_SIZE).ok()?;
            let (w, h) = ((packed >> 32) as usize, (packed & 0xFFFF_FFFF) as usize);
            if w == 0 || h == 0 || w > 8192 || h > 8192 {
                return None;
            }
            // Skip intros and black first frames: 10% in, between 1 s and 60 s.
            let duration = reader
                .GetPresentationAttribute(MF_SOURCE_READER_MEDIASOURCE.0 as u32, &MF_PD_DURATION)
                .ok()
                .and_then(|v| PropVariantToInt64(&v).ok())
                .unwrap_or(0);
            let at = (duration / 10).clamp(10_000_000, 600_000_000).min(duration / 2);
            if at > 0 {
                let mut pv = PROPVARIANT::default();
                let inner = &mut *pv.Anonymous.Anonymous;
                inner.vt = VT_I8;
                inner.Anonymous.hVal = at;
                let _ = reader.SetCurrentPosition(&windows::core::GUID::zeroed(), &pv);
            }
            let mut sample = None;
            for _ in 0..60 {
                let mut flags = 0u32;
                let mut s: Option<IMFSample> = None;
                reader.ReadSample(stream, 0, None, Some(&mut flags), None, Some(&mut s)).ok()?;
                if s.is_some() {
                    sample = s;
                    break;
                }
                if flags & MF_SOURCE_READERF_ENDOFSTREAM.0 as u32 != 0 {
                    break;
                }
            }
            let buffer = sample?.ConvertToContiguousBuffer().ok()?;
            let mut ptr = std::ptr::null_mut();
            let mut len = 0u32;
            buffer.Lock(&mut ptr, None, Some(&mut len)).ok()?;
            let data = std::slice::from_raw_parts(ptr, len as usize);
            let stride = data.len() / h;
            let mut rgba = None;
            if stride >= w * 4 {
                let mut out = vec![0u8; w * h * 4];
                for y in 0..h {
                    let row = &data[y * stride..y * stride + w * 4];
                    for x in 0..w {
                        let (s, d) = (&row[x * 4..x * 4 + 4], &mut out[(y * w + x) * 4..][..4]);
                        d[0] = s[2];
                        d[1] = s[1];
                        d[2] = s[0];
                        d[3] = 255;
                    }
                }
                rgba = Some(out);
            }
            let _ = buffer.Unlock();
            let px = Pixels { width: w as u32, height: h as u32, rgba: rgba? };
            Some(super::fit(&px, size, size))
        }
    }

    /// The file's icon as Explorer shows it (app icon for .exe, registered
    /// icon for its type).
    pub fn icon(path: &Path, size: u32) -> Option<Pixels> {
        // Big tiles: an app that only ships a small icon would be blown up
        // into blocks. Then the app's own (sharp) icon is used instead.
        if size > 64 && low_res_icon(path) {
            return None;
        }
        image(path, size, SIIGBF_ICONONLY | SIIGBF_BIGGERSIZEOK)
    }

    /// True when the file's icon has no large (256 px) version. Windows'
    /// jumbo icon list then draws the small icon in the corner of an
    /// otherwise empty 256×256 canvas, which is what this looks for.
    pub fn low_res_icon(path: &Path) -> bool {
        use windows::Win32::Storage::FileSystem::FILE_FLAGS_AND_ATTRIBUTES;
        use windows::Win32::UI::Controls::{IImageList, ILD_TRANSPARENT};
        use windows::Win32::UI::Shell::{SHFILEINFOW, SHGFI_SYSICONINDEX, SHGetFileInfoW, SHGetImageList, SHIL_JUMBO};
        use windows::Win32::UI::WindowsAndMessaging::{DestroyIcon, GetIconInfo, ICONINFO};
        let wide = HSTRING::from(path.as_os_str());
        // SAFETY: COM is initialised on this thread; handles are freed below.
        unsafe {
            let mut sfi = SHFILEINFOW::default();
            let ok = SHGetFileInfoW(
                &wide,
                FILE_FLAGS_AND_ATTRIBUTES(0),
                Some(&mut sfi),
                size_of::<SHFILEINFOW>() as u32,
                SHGFI_SYSICONINDEX,
            );
            if ok == 0 {
                return false;
            }
            let Ok(list) = SHGetImageList::<IImageList>(SHIL_JUMBO as i32) else { return false };
            let Ok(hicon) = list.GetIcon(sfi.iIcon, ILD_TRANSPARENT.0) else { return false };
            let mut info = ICONINFO::default();
            let got = GetIconInfo(hicon, &mut info).is_ok();
            let _ = DestroyIcon(hicon);
            if !got {
                return false;
            }
            let px = if info.hbmColor.is_invalid() { None } else { hbitmap_pixels(info.hbmColor) };
            let _ = DeleteObject(HGDIOBJ(info.hbmColor.0));
            let _ = DeleteObject(HGDIOBJ(info.hbmMask.0));
            let Some(px) = px else { return false };
            if px.width < 128 {
                return false;
            }
            // Bounding box of the visible pixels.
            let (mut max_x, mut max_y) = (0u32, 0u32);
            let mut any = false;
            for y in 0..px.height {
                for x in 0..px.width {
                    if px.rgba[((y * px.width + x) * 4 + 3) as usize] > 8 {
                        any = true;
                        max_x = max_x.max(x);
                        max_y = max_y.max(y);
                    }
                }
            }
            any && max_x < 64 && max_y < 64
        }
    }

    fn image(path: &Path, size: u32, flags: windows::Win32::UI::Shell::SIIGBF) -> Option<Pixels> {
        let wide = HSTRING::from(path.as_os_str());
        // SAFETY: COM is initialised on this thread; the HBITMAP is freed below.
        unsafe {
            let factory: IShellItemImageFactory = SHCreateItemFromParsingName(&wide, None).ok()?;
            let size = SIZE { cx: size as i32, cy: size as i32 };
            let hbmp = factory.GetImage(size, flags).ok()?;
            let px = hbitmap_pixels(hbmp);
            let _ = DeleteObject(HGDIOBJ(hbmp.0));
            px
        }
    }

    unsafe fn hbitmap_pixels(hbmp: HBITMAP) -> Option<Pixels> {
        unsafe {
            let mut bm = BITMAP::default();
            if GetObjectW(HGDIOBJ(hbmp.0), size_of::<BITMAP>() as i32, Some(&mut bm as *mut _ as *mut _)) == 0 {
                return None;
            }
            let (w, h) = (bm.bmWidth.max(1) as u32, bm.bmHeight.unsigned_abs().max(1));
            let mut info = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: w as i32,
                    biHeight: -(h as i32), // top-down
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: BI_RGB.0,
                    ..Default::default()
                },
                ..Default::default()
            };
            let mut buf = vec![0u8; (w * h * 4) as usize];
            let dc = CreateCompatibleDC(None);
            let lines = GetDIBits(dc, hbmp, 0, h, Some(buf.as_mut_ptr().cast()), &mut info, DIB_RGB_COLORS);
            let _ = DeleteDC(dc);
            if lines == 0 {
                return None;
            }
            Some(bgra_to_rgba(w, h, buf))
        }
    }

    /// BGRA -> RGBA. Thumbnails of opaque images often come with alpha = 0
    /// everywhere; treat those as fully opaque.
    fn bgra_to_rgba(width: u32, height: u32, mut buf: Vec<u8>) -> Pixels {
        let opaque = buf.as_chunks::<4>().0.iter().all(|p| p[3] == 0);
        for p in buf.as_chunks_mut::<4>().0 {
            p.swap(0, 2);
            if opaque {
                p[3] = 255;
            }
        }
        super::premultiply_if_needed(&mut buf);
        Pixels { width, height, rgba: buf }
    }

    /// Renders one PDF page `width` pixels wide. Returns the page and the page count.
    pub fn pdf_page(path: &Path, index: u32, width: u32) -> windows::core::Result<(Pixels, u32)> {
        let file = StorageFile::GetFileFromPathAsync(&HSTRING::from(path.as_os_str()))?.join()?;
        let doc = PdfDocument::LoadFromFileAsync(&file)?.join()?;
        let count = doc.PageCount()?;
        let page = doc.GetPage(index.min(count.saturating_sub(1)))?;
        let opts = PdfPageRenderOptions::new()?;
        opts.SetDestinationWidth(width)?;
        opts.SetBitmapEncoderId(BitmapEncoder::BmpEncoderId()?)?;
        let stream = InMemoryRandomAccessStream::new()?;
        page.RenderWithOptionsToStreamAsync(&stream, &opts)?.join()?;
        let len = stream.Size()? as u32;
        let buffer = Buffer::Create(len)?;
        let input = stream.GetInputStreamAt(0)?;
        let filled = input.ReadAsync(&buffer, len, InputStreamOptions::None)?.join()?;
        let reader = DataReader::FromBuffer(&filled)?;
        let mut bytes = vec![0u8; filled.Length()? as usize];
        reader.ReadBytes(&mut bytes)?;
        let _ = input.cast::<windows::Foundation::IClosable>().map(|c| c.Close());
        decode_bmp(&bytes).map(|px| (px, count)).ok_or_else(|| windows::core::Error::from_hresult(windows::core::HRESULT(-1)))
    }

    /// Minimal BMP reader for the 24/32-bit images the PDF renderer writes.
    fn decode_bmp(b: &[u8]) -> Option<Pixels> {
        let u32_at = |o: usize| b.get(o..o + 4).map(|s| u32::from_le_bytes(s.try_into().unwrap()));
        let i32_at = |o: usize| u32_at(o).map(|v| v as i32);
        if b.get(0..2)? != b"BM" {
            return None;
        }
        let off = u32_at(10)? as usize;
        let w = i32_at(18)?;
        let h = i32_at(22)?;
        let bpp = u16::from_le_bytes(b.get(28..30)?.try_into().ok()?) as usize;
        if w <= 0 || h == 0 || (bpp != 32 && bpp != 24) {
            return None;
        }
        let (w, top_down, h) = (w as usize, h < 0, h.unsigned_abs() as usize);
        let stride = (w * bpp / 8).div_ceil(4) * 4;
        let mut out = vec![0u8; w * h * 4];
        for y in 0..h {
            let src_y = if top_down { y } else { h - 1 - y };
            let row = b.get(off + src_y * stride..off + src_y * stride + w * bpp / 8)?;
            for x in 0..w {
                let s = &row[x * bpp / 8..];
                let d = &mut out[(y * w + x) * 4..][..4];
                d[0] = s[2];
                d[1] = s[1];
                d[2] = s[0];
                d[3] = if bpp == 32 { s[3] } else { 255 };
            }
        }
        // White page background where the renderer left transparency.
        if out.as_chunks::<4>().0.iter().all(|p| p[3] == 0) {
            out.as_chunks_mut::<4>().0.iter_mut().for_each(|p| p[3] = 255);
        }
        super::premultiply_if_needed(&mut out);
        Some(Pixels { width: w as u32, height: h as u32, rgba: out })
    }
}

#[cfg(windows)]
pub use imp::init_thread;

#[cfg(not(windows))]
pub fn init_thread(_sta: bool) {}

/// Shell thumbnail of at most `size`×`size` pixels, or `None` if Windows has
/// no thumbnail for this file type (the caller then shows a type icon).
pub fn thumbnail(path: &Path, size: u32) -> Option<Pixels> {
    #[cfg(windows)]
    return imp::thumbnail(path, size).or_else(|| {
        // No shell thumbnail for a video: grab a frame ourselves.
        (super::preview::classify_path(path) == super::preview::Kind::Video).then(|| imp::video_frame(path, size))?
    });
    #[cfg(not(windows))]
    {
        let _ = (path, size);
        None
    }
}

/// The UI expects premultiplied alpha (colour <= alpha). Windows hands back
/// both kinds, so straight-alpha images are converted here. Without this,
/// semi-transparent icon edges overflow when blended (odd bright pixels).
pub fn premultiply_if_needed(rgba: &mut [u8]) {
    let px = rgba.as_chunks_mut::<4>().0;
    if !px.iter().any(|p| p[0] > p[3] || p[1] > p[3] || p[2] > p[3]) {
        return;
    }
    for p in px {
        let a = p[3] as u16;
        for c in &mut p[..3] {
            *c = ((*c as u16 * a + 127) / 255) as u8;
        }
    }
}

/// The file's icon only exists in small sizes (it would look blocky big).
pub fn low_res_icon(path: &Path) -> bool {
    #[cfg(windows)]
    return imp::low_res_icon(path);
    #[cfg(not(windows))]
    {
        let _ = path;
        false
    }
}

/// The file's Windows icon at about `size` pixels.
pub fn icon(path: &Path, size: u32) -> Option<Pixels> {
    #[cfg(windows)]
    return imp::icon(path, size);
    #[cfg(not(windows))]
    {
        let _ = (path, size);
        None
    }
}

/// Shrinks `src` to fit within `max_w` × `max_h`, averaging every source pixel
/// that falls into a destination pixel (box filter). The UI renderer scales
/// images without smoothing, so previews are handed over at their exact on-
/// screen size to stay sharp. Images that already fit are returned as is.
pub fn fit(src: &Pixels, max_w: u32, max_h: u32) -> Pixels {
    let (sw, sh) = (src.width.max(1), src.height.max(1));
    let scale = (max_w.max(1) as f64 / sw as f64).min(max_h.max(1) as f64 / sh as f64);
    if scale >= 1.0 {
        return src.clone();
    }
    let dw = ((sw as f64 * scale).round() as u32).max(1);
    let dh = ((sh as f64 * scale).round() as u32).max(1);
    // Precompute source spans per destination column/row.
    let span = |d: u32, dn: u32, sn: u32| -> (usize, usize) {
        let a = (d as u64 * sn as u64 / dn as u64) as usize;
        let b = (((d + 1) as u64 * sn as u64).div_ceil(dn as u64) as usize).max(a + 1);
        (a, b.min(sn as usize))
    };
    let cols: Vec<(usize, usize)> = (0..dw).map(|x| span(x, dw, sw)).collect();
    let mut out = vec![0u8; (dw * dh * 4) as usize];
    let stride = sw as usize * 4;
    for y in 0..dh {
        let (y0, y1) = span(y, dh, sh);
        for (x, &(x0, x1)) in cols.iter().enumerate() {
            let mut acc = [0u32; 4];
            for row in y0..y1 {
                let line = &src.rgba[row * stride..];
                for col in x0..x1 {
                    let p = &line[col * 4..col * 4 + 4];
                    acc[0] += p[0] as u32;
                    acc[1] += p[1] as u32;
                    acc[2] += p[2] as u32;
                    acc[3] += p[3] as u32;
                }
            }
            let n = ((y1 - y0) * (x1 - x0)) as u32;
            let o = &mut out[(y as usize * dw as usize + x) * 4..][..4];
            for c in 0..4 {
                o[c] = ((acc[c] + n / 2) / n) as u8;
            }
        }
    }
    Pixels { width: dw, height: dh, rgba: out }
}

/// Renders PDF page `index` (0-based), `width` pixels wide. Returns the page
/// image and the total page count.
pub fn pdf_page(path: &Path, index: u32, width: u32) -> Result<(Pixels, u32), String> {
    #[cfg(windows)]
    return imp::pdf_page(path, index, width).map_err(|e| e.message().to_string());
    #[cfg(not(windows))]
    {
        let _ = (path, index, width);
        Err("PDF preview is only available on Windows".into())
    }
}

#[cfg(test)]
mod tests {
    /// Prints which of the given files have only small icons:
    /// FINDER_ICON_FILES="a.srt;b.mp4" cargo test low_res -- --ignored --nocapture
    #[test]
    #[ignore]
    fn low_res_survey() {
        super::init_thread(true);
        for p in std::env::var("FINDER_ICON_FILES").unwrap_or_default().split(';').filter(|s| !s.is_empty()) {
            println!("{p}: low-res={}", super::low_res_icon(std::path::Path::new(p)));
        }
    }
}

#[cfg(test)]
mod thumb_tests {
    /// FINDER_THUMB_FILES="a.mp4;b.mp4" cargo test thumb_survey -- --ignored --nocapture
    #[test]
    #[ignore]
    fn thumb_survey() {
        super::init_thread(true);
        for p in std::env::var("FINDER_THUMB_FILES").unwrap_or_default().split(';').filter(|s| !s.is_empty()) {
            for size in [128u32, 256, 1024] {
                let t = std::time::Instant::now();
                let r = super::thumbnail(std::path::Path::new(p), size);
                println!("{p} @{size}: {:?} in {:?}", r.as_ref().map(|x| (x.width, x.height)), t.elapsed());
                if let (Some(x), Ok(dir)) = (&r, std::env::var("FINDER_THUMB_DUMP")) {
                    let name = format!("{dir}/{}_{size}_{}x{}.rgba", p.replace(['\\', ':', '/', ' '], "_"), x.width, x.height);
                    let _ = std::fs::write(name, &x.rgba);
                }
            }
            let t = std::time::Instant::now();
            let r = super::imp::video_frame(std::path::Path::new(p), 256);
            println!("{p} frame: {:?} in {:?}", r.map(|x| (x.width, x.height, x.rgba[..8].to_vec())), t.elapsed());
        }
    }
}
