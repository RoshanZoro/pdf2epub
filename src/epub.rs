use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipWriter};

const CSS: &str = "body { margin: 0 4%; line-height: 1.45; }
h1 { font-size: 1.5em; margin: 1em 0 0.8em; }
p { margin: 0 0 0.7em; text-align: justify; }
.page { margin-bottom: 1.2em; }
.figure { text-align: center; margin: 1em 0; }
.figure img { max-width: 100%; height: auto; }
";

struct Chapter {
    file: String,
    title: String,
}

struct Image {
    file: String,
    media_type: &'static str,
}

pub struct EpubWriter {
    zip: ZipWriter<BufWriter<File>>,
    chapters: Vec<Chapter>,
    images: Vec<Image>,
}

impl EpubWriter {
    pub fn create(path: &Path) -> Result<Self, String> {
        let file = File::create(path).map_err(|e| format!("Cannot create {}: {e}", path.display()))?;
        let mut w = Self { zip: ZipWriter::new(BufWriter::new(file)), chapters: Vec::new(), images: Vec::new() };
        let stored = SimpleFileOptions::default().compression_method(CompressionMethod::Stored);
        w.put("mimetype", b"application/epub+zip", stored)?;
        w.put(
            "META-INF/container.xml",
            br#"<?xml version="1.0" encoding="UTF-8"?>
<container version="1.0" xmlns="urn:oasis:names:tc:opendocument:xmlns:container">
  <rootfiles>
    <rootfile full-path="OEBPS/content.opf" media-type="application/oebps-package+xml"/>
  </rootfiles>
</container>
"#,
            deflated(),
        )?;
        Ok(w)
    }

    fn put(&mut self, name: &str, data: &[u8], opts: SimpleFileOptions) -> Result<(), String> {
        self.zip.start_file(name, opts).map_err(|e| e.to_string())?;
        self.zip.write_all(data).map_err(|e| e.to_string())
    }

    /// `name` is relative to OEBPS/images; returns the href to use from a chapter.
    pub fn add_image(&mut self, name: &str, data: &[u8]) -> Result<String, String> {
        let media_type = if name.ends_with(".png") { "image/png" } else { "image/jpeg" };
        let file = format!("images/{name}");
        self.put(&format!("OEBPS/{file}"), data, SimpleFileOptions::default().compression_method(CompressionMethod::Stored))?;
        self.images.push(Image { file: file.clone(), media_type });
        Ok(file)
    }

    pub fn add_chapter(&mut self, title: &str, body_html: &str) -> Result<(), String> {
        let file = format!("chapter{:04}.xhtml", self.chapters.len() + 1);
        let doc = format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE html>\n\
<html xmlns=\"http://www.w3.org/1999/xhtml\" xmlns:epub=\"http://www.idpf.org/2007/ops\">\n\
<head><meta charset=\"UTF-8\"/><title>{}</title><link rel=\"stylesheet\" type=\"text/css\" href=\"style.css\"/></head>\n\
<body>\n{body_html}</body>\n</html>\n",
            escape(title)
        );
        self.put(&format!("OEBPS/{file}"), doc.as_bytes(), deflated())?;
        self.chapters.push(Chapter { file, title: title.to_string() });
        Ok(())
    }

    pub fn finish(mut self, title: &str, author: &str, uid_seed: u64) -> Result<(), String> {
        let uid = uuid_from(uid_seed);
        let title = escape(title);
        self.put("OEBPS/style.css", CSS.as_bytes(), deflated())?;

        let nav_items: String = self
            .chapters
            .iter()
            .map(|c| format!("      <li><a href=\"{}\">{}</a></li>\n", c.file, escape(&c.title)))
            .collect();
        let nav = format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE html>\n\
<html xmlns=\"http://www.w3.org/1999/xhtml\" xmlns:epub=\"http://www.idpf.org/2007/ops\">\n\
<head><meta charset=\"UTF-8\"/><title>{title}</title></head>\n<body>\n\
  <nav epub:type=\"toc\" id=\"toc\">\n    <h1>Contents</h1>\n    <ol>\n{nav_items}    </ol>\n  </nav>\n</body>\n</html>\n"
        );
        self.put("OEBPS/nav.xhtml", nav.as_bytes(), deflated())?;

        let nav_points: String = self
            .chapters
            .iter()
            .enumerate()
            .map(|(i, c)| {
                format!(
                    "    <navPoint id=\"np{n}\" playOrder=\"{n}\"><navLabel><text>{}</text></navLabel><content src=\"{}\"/></navPoint>\n",
                    escape(&c.title),
                    c.file,
                    n = i + 1
                )
            })
            .collect();
        let ncx = format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
<ncx xmlns=\"http://www.daisy.org/z3986/2005/ncx/\" version=\"2005-1\">\n\
  <head><meta name=\"dtb:uid\" content=\"urn:uuid:{uid}\"/><meta name=\"dtb:depth\" content=\"1\"/>\
<meta name=\"dtb:totalPageCount\" content=\"0\"/><meta name=\"dtb:maxPageNumber\" content=\"0\"/></head>\n\
  <docTitle><text>{title}</text></docTitle>\n  <navMap>\n{nav_points}  </navMap>\n</ncx>\n"
        );
        self.put("OEBPS/toc.ncx", ncx.as_bytes(), deflated())?;

        let mut manifest = String::from(
            "    <item id=\"nav\" href=\"nav.xhtml\" media-type=\"application/xhtml+xml\" properties=\"nav\"/>\n\
    <item id=\"ncx\" href=\"toc.ncx\" media-type=\"application/x-dtbncx+xml\"/>\n\
    <item id=\"css\" href=\"style.css\" media-type=\"text/css\"/>\n",
        );
        for (i, c) in self.chapters.iter().enumerate() {
            manifest += &format!("    <item id=\"ch{}\" href=\"{}\" media-type=\"application/xhtml+xml\"/>\n", i + 1, c.file);
        }
        for (i, img) in self.images.iter().enumerate() {
            manifest += &format!("    <item id=\"img{}\" href=\"{}\" media-type=\"{}\"/>\n", i + 1, img.file, img.media_type);
        }
        let spine: String = (1..=self.chapters.len()).map(|i| format!("    <itemref idref=\"ch{i}\"/>\n")).collect();
        let creator = if author.trim().is_empty() {
            String::new()
        } else {
            format!("    <dc:creator>{}</dc:creator>\n", escape(author))
        };
        let opf = format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
<package xmlns=\"http://www.idpf.org/2007/opf\" version=\"3.0\" unique-identifier=\"bookid\">\n\
  <metadata xmlns:dc=\"http://purl.org/dc/elements/1.1/\">\n\
    <dc:identifier id=\"bookid\">urn:uuid:{uid}</dc:identifier>\n\
    <dc:title>{title}</dc:title>\n{creator}\
    <dc:language>en</dc:language>\n\
    <meta property=\"dcterms:modified\">{}</meta>\n\
  </metadata>\n  <manifest>\n{manifest}  </manifest>\n  <spine toc=\"ncx\">\n{spine}  </spine>\n</package>\n",
            utc_timestamp()
        );
        self.put("OEBPS/content.opf", opf.as_bytes(), deflated())?;

        let mut out = self.zip.finish().map_err(|e| e.to_string())?;
        out.flush().map_err(|e| e.to_string())
    }
}

fn deflated() -> SimpleFileOptions {
    SimpleFileOptions::default().compression_method(CompressionMethod::Deflated)
}

pub fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\t' | '\n' | '\r' => out.push(c),
            c if (c as u32) < 0x20 || c == '\u{FFFE}' || c == '\u{FFFF}' => {}
            c => out.push(c),
        }
    }
    out
}

fn uuid_from(seed: u64) -> String {
    let a = seed;
    let b = seed.rotate_left(29) ^ 0x9E37_79B9_7F4A_7C15;
    format!(
        "{:08x}-{:04x}-4{:03x}-{:04x}-{:012x}",
        a >> 32,
        (a >> 16) & 0xFFFF,
        a & 0xFFF,
        0x8000 | (b >> 48) & 0x3FFF,
        b & 0xFFFF_FFFF_FFFF
    )
}

fn utc_timestamp() -> String {
    let secs = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0) as i64;
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    // Howard Hinnant's civil_from_days.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z", rem / 3600, rem % 3600 / 60, rem % 60)
}
