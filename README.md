# pdf2epub

A small Windows desktop app that converts PDF files into EPUB e-books. It uses native Windows controls, runs fully offline, and is a single executable with no runtime dependencies.

## Features

- **Native Windows interface**: standard file dialogs, checkboxes and progress bar.
- **Drag and drop**: drop a PDF onto the window to load it.
- **Automatic metadata**: title and author are read from the PDF and can be edited before converting.
- **Optional images**: embedded images are extracted and placed in the book. Turn this off for a smaller, text-only EPUB.
- **Chapters from bookmarks**: if the PDF has bookmarks (an outline), each top-level bookmark becomes a chapter in the table of contents. Without bookmarks, a chapter is created every 10 pages.
- **Readable text**: lines are rejoined into paragraphs, hyphenated words are repaired, bullet and numbered lists are kept apart, and standalone page numbers are removed.
- **Safe output**: the EPUB is written to a temporary file first, so a failed conversion never leaves a broken book behind.
- **Command-line mode** for scripts and batch jobs.

## Requirements

- Windows 10 or later
- To build from source: [Rust](https://rustup.rs/) (stable, 2024 edition) with the MSVC toolchain

## Building

```powershell
git clone https://github.com/RoshanZoro/pdf2epub.git
cd pdf2epub
cargo build --release
```

The program is created at `target\release\pdf2epub.exe`. You can copy this file anywhere; nothing else is needed to run it.

## Usage

### Desktop app

1. Start `pdf2epub.exe`.
2. Click **Browse...** next to *PDF file*, or drag a PDF onto the window.
3. Check the suggested *EPUB file* location, *Title* and *Author*, and change them if you like.
4. Choose your options:
   - **Include images**: copy pictures from the PDF into the book.
   - **Split chapters at PDF bookmarks**: use the PDF's outline for chapters.
5. Click **Convert**. The progress bar shows each page as it is processed, and a message appears when the book is ready.

### Command line

```powershell
pdf2epub.exe <input.pdf> <output.epub> [--no-images] [--no-bookmarks]
```

| Option           | Effect                                            |
| ---------------- | ------------------------------------------------- |
| `--no-images`    | Leave images out of the EPUB                      |
| `--no-bookmarks` | Ignore PDF bookmarks and split every 10 pages     |

The exit code is `0` on success and `1` on failure. Because the program is a Windows GUI application, it prints no console output.

Example: convert every PDF in a folder:

```powershell
Get-ChildItem *.pdf | ForEach-Object {
    .\pdf2epub.exe $_.FullName ($_.FullName -replace '\.pdf$', '.epub')
}
```

## Output format

The generated file is a standard **EPUB 3** book that also includes an EPUB 2 table of contents (`toc.ncx`), so it opens in both modern and older readers such as Calibre, Apple Books, Thorium, Kobo and most e-reader apps.

Images are stored as JPEG or PNG. Supported PDF image types include grayscale, RGB, CMYK, indexed (palette) and images with transparency masks.

## Limitations

PDF is a page-layout format, while EPUB is designed for reflowable text. Some things cannot be converted perfectly:

- **Scanned PDFs**: pages that are only images contain no text, and no OCR (text recognition) is performed.
- **Layout**: multi-column layouts, tables and complex formatting become simple paragraphs. Images appear after the text of the page they belong to, not at their exact original position.
- **Headers and footers**: repeated page headers and footers stay in the text.
- **Password-protected PDFs**: files that require a password to open are not supported.
- **Some image types**: JPEG 2000, CCITT fax and JBIG2 images are skipped.
- **Language**: the book language is always set to English.

If text on a particular page cannot be read, that page is skipped and the rest of the book is still converted; the final message reports how many pages were affected.

## Project structure

| File             | Purpose                                                        |
| ---------------- | -------------------------------------------------------------- |
| `src/main.rs`    | Window, controls, background conversion and command-line mode  |
| `src/convert.rs` | Chapter planning and paragraph reconstruction                  |
| `src/pdf.rs`     | Reading PDF text, metadata, bookmarks and images               |
| `src/epub.rs`    | Writing the EPUB package                                       |

## Built with

- [native-windows-gui](https://crates.io/crates/native-windows-gui): native Win32 controls
- [pdf-extract](https://crates.io/crates/pdf-extract) / [lopdf](https://crates.io/crates/lopdf): PDF parsing and text extraction
- [image](https://crates.io/crates/image): image conversion to PNG
- [zip](https://crates.io/crates/zip): EPUB packaging

## Running tests

```powershell
cargo test
```

## License

Released under the [MIT License](LICENSE).
