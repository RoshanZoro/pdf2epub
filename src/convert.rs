use std::collections::HashMap;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::path::PathBuf;
use std::time::SystemTime;

use crate::epub::{EpubWriter, escape};
use crate::pdf::Pdf;

const PAGES_PER_CHAPTER: u32 = 10;

pub struct Options {
    pub input: PathBuf,
    pub output: PathBuf,
    pub title: String,
    pub author: String,
    pub images: bool,
    pub bookmarks: bool,
}

pub struct Summary {
    pub pages: u32,
    pub chapters: usize,
    pub images: usize,
    pub failed_pages: u32,
}

struct ChapterPlan {
    title: String,
    first: u32,
    last: u32,
    heading: bool,
}

pub fn convert(opts: &Options, progress: &mut dyn FnMut(u32, u32)) -> Result<Summary, String> {
    let bytes = std::fs::read(&opts.input).map_err(|e| format!("Cannot read {}: {e}", opts.input.display()))?;
    let pdf = Pdf::load(&bytes)?;
    let pages = pdf.page_count();
    if pages == 0 {
        return Err("The PDF has no pages.".into());
    }
    let plan = plan_chapters(&pdf, pages, opts.bookmarks);

    let mut seed = DefaultHasher::new();
    bytes.len().hash(&mut seed);
    bytes[..bytes.len().min(1 << 16)].hash(&mut seed);
    SystemTime::now().hash(&mut seed);
    drop(bytes);

    let mut part = opts.output.clone().into_os_string();
    part.push(".part");
    let part = PathBuf::from(part);
    let result = write_epub(&pdf, &plan, opts, &part, seed.finish(), progress);
    match result {
        Ok(summary) => {
            std::fs::rename(&part, &opts.output).map_err(|e| {
                let _ = std::fs::remove_file(&part);
                format!("Cannot write {}: {e}", opts.output.display())
            })?;
            Ok(Summary { pages, ..summary })
        }
        Err(e) => {
            let _ = std::fs::remove_file(&part);
            Err(e)
        }
    }
}

fn write_epub(
    pdf: &Pdf,
    plan: &[ChapterPlan],
    opts: &Options,
    path: &std::path::Path,
    uid_seed: u64,
    progress: &mut dyn FnMut(u32, u32),
) -> Result<Summary, String> {
    let total = plan.last().map_or(0, |c| c.last);
    let mut epub = EpubWriter::create(path)?;
    let mut image_hrefs: HashMap<_, Option<String>> = HashMap::new();
    let mut failed_pages = 0;
    let mut done = 0;

    for chapter in plan {
        let mut body = String::new();
        if chapter.heading {
            body += &format!("<h1>{}</h1>\n", escape(&chapter.title));
        }
        for page in chapter.first..=chapter.last {
            body += &format!("<div class=\"page\" id=\"page{page}\">\n");
            match pdf.page_text(page) {
                Some(text) => {
                    for para in paragraphs(&text) {
                        body += &format!("<p>{}</p>\n", escape(&para));
                    }
                }
                None => failed_pages += 1,
            }
            if opts.images {
                for id in pdf.page_image_ids(page) {
                    let href = match image_hrefs.get(&id) {
                        Some(h) => h.clone(),
                        None => {
                            let href = match pdf.extract_image(id) {
                                Some(img) => {
                                    let name = format!("img{:04}.{}", image_hrefs.len() + 1, img.ext);
                                    Some(epub.add_image(&name, &img.data)?)
                                }
                                None => None,
                            };
                            image_hrefs.insert(id, href.clone());
                            href
                        }
                    };
                    if let Some(href) = href {
                        body += &format!("<div class=\"figure\"><img src=\"{href}\" alt=\"\"/></div>\n");
                    }
                }
            }
            body += "</div>\n";
            done += 1;
            progress(done, total);
        }
        epub.add_chapter(&chapter.title, &body)?;
    }

    let images = image_hrefs.values().filter(|h| h.is_some()).count();
    epub.finish(&opts.title, &opts.author, uid_seed)?;
    Ok(Summary { pages: 0, chapters: plan.len(), images, failed_pages })
}

fn plan_chapters(pdf: &Pdf, pages: u32, use_bookmarks: bool) -> Vec<ChapterPlan> {
    let mut marks = if use_bookmarks { pdf.bookmarks() } else { Vec::new() };
    marks.retain(|b| (1..=pages).contains(&b.page));
    marks.sort_by_key(|b| b.page);
    marks.dedup_by_key(|b| b.page);

    if marks.is_empty() {
        return (1..=pages)
            .step_by(PAGES_PER_CHAPTER as usize)
            .map(|first| {
                let last = (first + PAGES_PER_CHAPTER - 1).min(pages);
                let title = if first == last { format!("Page {first}") } else { format!("Pages {first}\u{2013}{last}") };
                ChapterPlan { title, first, last, heading: false }
            })
            .collect();
    }

    let mut plan = Vec::new();
    if marks[0].page > 1 {
        plan.push(ChapterPlan { title: "Start".into(), first: 1, last: marks[0].page - 1, heading: false });
    }
    for (i, mark) in marks.iter().enumerate() {
        let last = marks.get(i + 1).map_or(pages, |next| next.page - 1);
        let title = if mark.title.is_empty() { format!("Page {}", mark.page) } else { mark.title.clone() };
        plan.push(ChapterPlan { title, first: mark.page, last, heading: true });
    }
    plan
}

fn paragraphs(text: &str) -> Vec<String> {
    let lines: Vec<String> = text
        .lines()
        .map(|l| l.split_whitespace().collect::<Vec<_>>().join(" "))
        .filter(|l| !(!l.is_empty() && l.len() <= 4 && l.chars().all(|c| c.is_ascii_digit())))
        .collect();
    let widest = lines.iter().map(|l| l.chars().count()).max().unwrap_or(0);

    let mut out = Vec::new();
    let mut current = String::new();
    let mut prev_len = 0;
    for line in &lines {
        if line.is_empty() {
            if !current.is_empty() {
                out.push(std::mem::take(&mut current));
            }
            continue;
        }
        if !current.is_empty() && starts_new_paragraph(&current, prev_len, widest, line) {
            out.push(std::mem::take(&mut current));
        }
        let joins_hyphenated = current.ends_with('-')
            && !current.ends_with("--")
            && line.chars().next().is_some_and(char::is_lowercase);
        if joins_hyphenated {
            current.pop();
        } else if !current.is_empty() {
            current.push(' ');
        }
        current.push_str(line);
        prev_len = line.chars().count();
    }
    if !current.is_empty() {
        out.push(current);
    }
    out
}

fn starts_new_paragraph(current: &str, prev_len: usize, widest: usize, line: &str) -> bool {
    if is_list_item(line) {
        return true;
    }
    let short_prev = prev_len * 10 < widest * 7;
    let ends_sentence = current.ends_with(['.', '!', '?', ':', '"', '\u{201D}']);
    let next_upper = line.chars().find(|c| c.is_alphanumeric()).is_some_and(|c| c.is_uppercase() || c.is_ascii_digit());
    short_prev && next_upper && (ends_sentence || prev_len * 2 < widest)
}

fn is_list_item(line: &str) -> bool {
    let mut chars = line.chars();
    let Some(first) = chars.next() else { return false };
    let bullet = matches!(
        first,
        '\u{2022}' | '\u{25CF}' | '\u{25CB}' | '\u{25AA}' | '\u{25A0}' | '\u{00B7}' | '\u{2013}' | '\u{F0B7}' | '-' | '*'
    );
    if bullet && chars.next().is_some_and(char::is_whitespace) {
        return true;
    }
    let digits = line.chars().take_while(char::is_ascii_digit).count();
    (1..=3).contains(&digits) && line[digits..].starts_with([')', '.']) && line[digits + 1..].starts_with(' ')
}

#[cfg(test)]
mod tests {
    use super::paragraphs;

    #[test]
    fn joins_lines_and_hyphens() {
        let text = "The quick brown fox jum-\nped over the\nlazy dog.\n\n12\n\nNext   paragraph.";
        assert_eq!(paragraphs(text), vec!["The quick brown fox jumped over the lazy dog.", "Next paragraph."]);
    }

    #[test]
    fn splits_bullets_and_short_lines() {
        let text = "Each lesson has about the same structure, as described here in full:\n\
                    \u{2022} First item that wraps onto the following line of the page\n\
                    continued here.\n\
                    \u{2022} Second item.\n\
                    Heading\n\
                    Body text that is long enough to be a normal paragraph line on the page.";
        assert_eq!(
            paragraphs(text),
            vec![
                "Each lesson has about the same structure, as described here in full:",
                "\u{2022} First item that wraps onto the following line of the page continued here.",
                "\u{2022} Second item.",
                "Heading",
                "Body text that is long enough to be a normal paragraph line on the page.",
            ]
        );
    }
}
