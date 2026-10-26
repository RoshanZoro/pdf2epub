use std::collections::HashSet;
use std::io::Cursor;
use std::panic::{self, AssertUnwindSafe};

use image::{DynamicImage, GrayAlphaImage, GrayImage, ImageFormat, RgbImage, RgbaImage};
use pdf_extract::{Dictionary, Document, Object, ObjectId, PlainTextOutput, Stream};

pub struct PdfMeta {
    pub title: String,
    pub author: String,
}

pub struct Bookmark {
    pub title: String,
    pub page: u32,
}

pub struct ExtractedImage {
    pub data: Vec<u8>,
    pub ext: &'static str,
}

pub struct Pdf {
    doc: Document,
}

impl Pdf {
    pub fn load(bytes: &[u8]) -> Result<Self, String> {
        let mut doc = Document::load_mem(bytes).map_err(|e| format!("Cannot read PDF: {e}"))?;
        if doc.is_encrypted() {
            doc.decrypt("")
                .map_err(|_| "PDF is password protected; this tool only opens PDFs without a user password.".to_string())?;
        }
        Ok(Self { doc })
    }

    pub fn page_count(&self) -> u32 {
        self.doc.get_pages().len() as u32
    }

    pub fn meta(&self) -> PdfMeta {
        let info = self
            .doc
            .trailer
            .get(b"Info")
            .ok()
            .and_then(|o| self.doc.dereference(o).ok())
            .and_then(|(_, o)| o.as_dict().ok());
        let field = |key: &[u8]| {
            info.and_then(|d| d.get(key).ok())
                .and_then(|o| self.doc.dereference(o).ok())
                .and_then(|(_, o)| o.as_str().ok())
                .map(decode_text_string)
                .map(|s| s.trim().to_string())
                .unwrap_or_default()
        };
        PdfMeta { title: field(b"Title"), author: field(b"Author") }
    }

    pub fn bookmarks(&self) -> Vec<Bookmark> {
        let toc = match panic::catch_unwind(AssertUnwindSafe(|| self.doc.get_toc())) {
            Ok(Ok(toc)) => toc.toc,
            _ => return Vec::new(),
        };
        let top: Vec<_> = toc.iter().filter(|t| t.level == 1).collect();
        let chosen: Vec<_> = if top.len() >= 2 { top } else { toc.iter().collect() };
        chosen
            .into_iter()
            .map(|t| Bookmark { title: t.title.split_whitespace().collect::<Vec<_>>().join(" "), page: t.page as u32 })
            .collect()
    }

    /// Returns `None` when the text extractor fails on this page.
    pub fn page_text(&self, page: u32) -> Option<String> {
        panic::catch_unwind(AssertUnwindSafe(|| {
            let mut s = String::new();
            let mut out = PlainTextOutput::new(&mut s);
            pdf_extract::output_doc_page(&self.doc, &mut out, page).ok()?;
            Some(s)
        }))
        .ok()
        .flatten()
    }

    pub fn page_image_ids(&self, page: u32) -> Vec<ObjectId> {
        let Some(&page_id) = self.doc.get_pages().get(&page) else {
            return Vec::new();
        };
        let mut found = Vec::new();
        let mut visited = HashSet::new();
        if let Ok((inline, ids)) = self.doc.get_page_resources(page_id) {
            if let Some(res) = inline {
                self.collect_images(res, &mut found, &mut visited);
            }
            for id in ids {
                if let Ok(res) = self.doc.get_dictionary(id) {
                    self.collect_images(res, &mut found, &mut visited);
                }
            }
        }
        found
    }

    fn collect_images(&self, resources: &Dictionary, found: &mut Vec<ObjectId>, visited: &mut HashSet<ObjectId>) {
        let Some(xobjects) = resources
            .get(b"XObject")
            .ok()
            .and_then(|o| self.doc.dereference(o).ok())
            .and_then(|(_, o)| o.as_dict().ok())
        else {
            return;
        };
        for (_, value) in xobjects.iter() {
            let Ok(id) = value.as_reference() else { continue };
            if !visited.insert(id) {
                continue;
            }
            let Ok(stream) = self.doc.get_object(id).and_then(Object::as_stream) else { continue };
            match stream.dict.get(b"Subtype").and_then(Object::as_name) {
                Ok(b"Image") => found.push(id),
                Ok(b"Form") => {
                    if let Some(res) = stream
                        .dict
                        .get(b"Resources")
                        .ok()
                        .and_then(|o| self.doc.dereference(o).ok())
                        .and_then(|(_, o)| o.as_dict().ok())
                    {
                        self.collect_images(res, found, visited);
                    }
                }
                _ => {}
            }
        }
    }

    pub fn extract_image(&self, id: ObjectId) -> Option<ExtractedImage> {
        let stream = self.doc.get_object(id).ok()?.as_stream().ok()?;
        panic::catch_unwind(AssertUnwindSafe(|| self.convert_image(stream))).ok().flatten()
    }

    fn convert_image(&self, stream: &Stream) -> Option<ExtractedImage> {
        let dict = &stream.dict;
        if self.get_bool(dict, b"ImageMask") {
            return None;
        }
        let width = self.get_int(dict, b"Width")? as u32;
        let height = self.get_int(dict, b"Height")? as u32;
        if width < 16 || height < 16 {
            return None;
        }
        let filters: Vec<Vec<u8>> = stream.filters().unwrap_or_default().into_iter().map(<[u8]>::to_vec).collect();
        let cs = dict.get(b"ColorSpace").ok().and_then(|o| self.color_space(o));

        match filters.last().map(Vec::as_slice) {
            Some(b"DCTDecode") if filters.len() == 1 => {
                if matches!(cs, Some(ColorSpace::Cmyk)) {
                    let img = image::load_from_memory_with_format(&stream.content, ImageFormat::Jpeg).ok()?;
                    return encode_png(&DynamicImage::ImageRgb8(img.to_rgb8()));
                }
                Some(ExtractedImage { data: stream.content.clone(), ext: "jpg" })
            }
            Some(b"FlateDecode" | b"LZWDecode" | b"ASCII85Decode") | None => {
                let raw = stream.decompressed_content().ok()?;
                let bpc = self.get_int(dict, b"BitsPerComponent").unwrap_or(8) as u32;
                let rgb = decode_raster(&raw, width, height, bpc, cs.as_ref()?)?;
                let alpha = self.soft_mask(dict, width, height);
                let img = match (rgb, alpha) {
                    (Raster::Gray(g), Some(a)) => DynamicImage::ImageLumaA8(GrayAlphaImage::from_fn(width, height, |x, y| {
                        let i = (y * width + x) as usize;
                        image::LumaA([g[i], a[i]])
                    })),
                    (Raster::Gray(g), None) => DynamicImage::ImageLuma8(GrayImage::from_raw(width, height, g)?),
                    (Raster::Rgb(c), Some(a)) => DynamicImage::ImageRgba8(RgbaImage::from_fn(width, height, |x, y| {
                        let i = (y * width + x) as usize;
                        image::Rgba([c[i * 3], c[i * 3 + 1], c[i * 3 + 2], a[i]])
                    })),
                    (Raster::Rgb(c), None) => DynamicImage::ImageRgb8(RgbImage::from_raw(width, height, c)?),
                };
                encode_png(&img)
            }
            _ => None,
        }
    }

    fn soft_mask(&self, dict: &Dictionary, width: u32, height: u32) -> Option<Vec<u8>> {
        let id = dict.get(b"SMask").ok()?.as_reference().ok()?;
        let mask = self.doc.get_object(id).ok()?.as_stream().ok()?;
        if self.get_int(&mask.dict, b"Width")? as u32 != width
            || self.get_int(&mask.dict, b"Height")? as u32 != height
            || self.get_int(&mask.dict, b"BitsPerComponent").unwrap_or(8) != 8
        {
            return None;
        }
        let data = mask.decompressed_content().ok()?;
        (data.len() >= (width * height) as usize).then(|| data[..(width * height) as usize].to_vec())
    }

    fn get_int(&self, dict: &Dictionary, key: &[u8]) -> Option<i64> {
        let (_, o) = self.doc.dereference(dict.get(key).ok()?).ok()?;
        o.as_i64().ok().or_else(|| o.as_float().ok().map(|f| f as i64))
    }

    fn get_bool(&self, dict: &Dictionary, key: &[u8]) -> bool {
        dict.get(key)
            .ok()
            .and_then(|o| self.doc.dereference(o).ok())
            .and_then(|(_, o)| o.as_bool().ok())
            .unwrap_or(false)
    }

    fn color_space(&self, obj: &Object) -> Option<ColorSpace> {
        let (_, obj) = self.doc.dereference(obj).ok()?;
        match obj {
            Object::Name(n) => match n.as_slice() {
                b"DeviceGray" | b"CalGray" | b"G" => Some(ColorSpace::Gray),
                b"DeviceRGB" | b"CalRGB" | b"RGB" => Some(ColorSpace::Rgb),
                b"DeviceCMYK" | b"CMYK" => Some(ColorSpace::Cmyk),
                _ => None,
            },
            Object::Array(arr) => {
                let family = arr.first()?.as_name().ok()?;
                match family {
                    b"ICCBased" => {
                        let (_, s) = self.doc.dereference(arr.get(1)?).ok()?;
                        let s = s.as_stream().ok()?;
                        match self.get_int(&s.dict, b"N")? {
                            1 => Some(ColorSpace::Gray),
                            3 => Some(ColorSpace::Rgb),
                            4 => Some(ColorSpace::Cmyk),
                            _ => None,
                        }
                    }
                    b"Indexed" | b"I" => {
                        let base = self.color_space(arr.get(1)?)?;
                        let hival = self.doc.dereference(arr.get(2)?).ok()?.1.as_i64().ok()? as usize;
                        let lookup = match self.doc.dereference(arr.get(3)?).ok()?.1 {
                            Object::String(bytes, _) => bytes.clone(),
                            Object::Stream(s) => s.get_plain_content().ok()?,
                            _ => return None,
                        };
                        let comps = base.components();
                        let palette = (0..=hival)
                            .map(|i| {
                                let start = i * comps;
                                let entry = lookup.get(start..start + comps).unwrap_or(&[0, 0, 0, 0][..comps]);
                                base.to_rgb(entry)
                            })
                            .collect();
                        Some(ColorSpace::Indexed(palette))
                    }
                    b"CalRGB" => Some(ColorSpace::Rgb),
                    b"CalGray" => Some(ColorSpace::Gray),
                    _ => None,
                }
            }
            _ => None,
        }
    }
}

enum ColorSpace {
    Gray,
    Rgb,
    Cmyk,
    Indexed(Vec<[u8; 3]>),
}

impl ColorSpace {
    fn components(&self) -> usize {
        match self {
            ColorSpace::Gray | ColorSpace::Indexed(_) => 1,
            ColorSpace::Rgb => 3,
            ColorSpace::Cmyk => 4,
        }
    }

    fn to_rgb(&self, c: &[u8]) -> [u8; 3] {
        match self {
            ColorSpace::Gray => [c[0]; 3],
            ColorSpace::Rgb => [c[0], c[1], c[2]],
            ColorSpace::Cmyk => {
                let k = 255 - c[3] as u32;
                let ch = |v: u8| ((255 - v as u32) * k / 255) as u8;
                [ch(c[0]), ch(c[1]), ch(c[2])]
            }
            ColorSpace::Indexed(p) => p.get(c[0] as usize).copied().unwrap_or([0; 3]),
        }
    }
}

enum Raster {
    Gray(Vec<u8>),
    Rgb(Vec<u8>),
}

fn decode_raster(raw: &[u8], width: u32, height: u32, bpc: u32, cs: &ColorSpace) -> Option<Raster> {
    if !matches!(bpc, 1 | 2 | 4 | 8 | 16) {
        return None;
    }
    let comps = cs.components();
    let row_bytes = (width as usize * comps * bpc as usize).div_ceil(8);
    if raw.len() < row_bytes * height as usize {
        return None;
    }
    let max = (1u32 << bpc.min(8)) - 1;
    let mut samples = Vec::with_capacity(width as usize * height as usize * comps);
    for row in raw.chunks(row_bytes).take(height as usize) {
        for i in 0..width as usize * comps {
            let v = match bpc {
                8 => row[i] as u32,
                16 => row[i * 2] as u32,
                _ => {
                    let bit = i * bpc as usize;
                    (row[bit / 8] as u32 >> (8 - bpc as usize - bit % 8)) & max
                }
            };
            samples.push(v);
        }
    }
    let scale = |v: u32| (v * 255 / max) as u8;
    match cs {
        ColorSpace::Gray => Some(Raster::Gray(samples.into_iter().map(scale).collect())),
        ColorSpace::Indexed(_) => Some(Raster::Rgb(
            samples.into_iter().flat_map(|v| cs.to_rgb(&[v as u8])).collect(),
        )),
        ColorSpace::Rgb | ColorSpace::Cmyk => {
            let scaled: Vec<u8> = samples.into_iter().map(scale).collect();
            Some(Raster::Rgb(scaled.chunks_exact(comps).flat_map(|c| cs.to_rgb(c)).collect()))
        }
    }
}

fn encode_png(img: &DynamicImage) -> Option<ExtractedImage> {
    let mut buf = Cursor::new(Vec::new());
    img.write_to(&mut buf, ImageFormat::Png).ok()?;
    Some(ExtractedImage { data: buf.into_inner(), ext: "png" })
}

fn decode_text_string(bytes: &[u8]) -> String {
    if let Some(rest) = bytes.strip_prefix(&[0xFE, 0xFF]) {
        let units: Vec<u16> = rest.chunks_exact(2).map(|c| u16::from_be_bytes([c[0], c[1]])).collect();
        String::from_utf16_lossy(&units)
    } else if let Some(rest) = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]) {
        String::from_utf8_lossy(rest).into_owned()
    } else {
        bytes.iter().map(|&b| b as char).collect()
    }
}
