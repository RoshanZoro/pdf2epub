#![windows_subsystem = "windows"]

mod convert;
mod epub;
mod pdf;

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::thread;

use native_windows_derive::NwgUi;
use native_windows_gui as nwg;
use nwg::NativeUi;

use convert::{Options, Summary};

#[derive(Default)]
struct Shared {
    done: u32,
    total: u32,
    result: Option<Result<Summary, String>>,
}

#[derive(Default, NwgUi)]
pub struct App {
    #[nwg_control(size: (620, 300), title: "PDF to EPUB", flags: "WINDOW|VISIBLE", center: true, accept_files: true)]
    #[nwg_events(OnWindowClose: [App::on_close(SELF, EVT_DATA)], OnFileDrop: [App::on_drop(SELF, EVT_DATA)])]
    window: nwg::Window,

    #[nwg_layout(parent: window, spacing: 4, margin: [12, 12, 12, 12], max_row: Some(7), max_column: Some(6))]
    grid: nwg::GridLayout,

    #[nwg_resource(title: "Open PDF", action: nwg::FileDialogAction::Open, filters: "PDF files(*.pdf)|All files(*.*)")]
    open_dialog: nwg::FileDialog,

    #[nwg_resource(title: "Save EPUB as", action: nwg::FileDialogAction::Save, filters: "EPUB books(*.epub)")]
    save_dialog: nwg::FileDialog,

    #[nwg_control(text: "PDF file:", v_align: nwg::VTextAlign::Center)]
    #[nwg_layout_item(layout: grid, row: 0, col: 0)]
    input_label: nwg::Label,

    #[nwg_control]
    #[nwg_layout_item(layout: grid, row: 0, col: 1, col_span: 4)]
    input: nwg::TextInput,

    #[nwg_control(text: "Browse...")]
    #[nwg_layout_item(layout: grid, row: 0, col: 5)]
    #[nwg_events(OnButtonClick: [App::browse_input])]
    input_button: nwg::Button,

    #[nwg_control(text: "EPUB file:", v_align: nwg::VTextAlign::Center)]
    #[nwg_layout_item(layout: grid, row: 1, col: 0)]
    output_label: nwg::Label,

    #[nwg_control]
    #[nwg_layout_item(layout: grid, row: 1, col: 1, col_span: 4)]
    output: nwg::TextInput,

    #[nwg_control(text: "Browse...")]
    #[nwg_layout_item(layout: grid, row: 1, col: 5)]
    #[nwg_events(OnButtonClick: [App::browse_output])]
    output_button: nwg::Button,

    #[nwg_control(text: "Title:", v_align: nwg::VTextAlign::Center)]
    #[nwg_layout_item(layout: grid, row: 2, col: 0)]
    title_label: nwg::Label,

    #[nwg_control]
    #[nwg_layout_item(layout: grid, row: 2, col: 1, col_span: 5)]
    title: nwg::TextInput,

    #[nwg_control(text: "Author:", v_align: nwg::VTextAlign::Center)]
    #[nwg_layout_item(layout: grid, row: 3, col: 0)]
    author_label: nwg::Label,

    #[nwg_control]
    #[nwg_layout_item(layout: grid, row: 3, col: 1, col_span: 5)]
    author: nwg::TextInput,

    #[nwg_control(text: "Include images", check_state: nwg::CheckBoxState::Checked)]
    #[nwg_layout_item(layout: grid, row: 4, col: 1, col_span: 2)]
    images: nwg::CheckBox,

    #[nwg_control(text: "Split chapters at PDF bookmarks", check_state: nwg::CheckBoxState::Checked)]
    #[nwg_layout_item(layout: grid, row: 4, col: 3, col_span: 3)]
    bookmarks: nwg::CheckBox,

    #[nwg_control(range: 0..1)]
    #[nwg_layout_item(layout: grid, row: 5, col: 0, col_span: 6)]
    progress: nwg::ProgressBar,

    #[nwg_control(text: "Choose or drop a PDF file.", v_align: nwg::VTextAlign::Center)]
    #[nwg_layout_item(layout: grid, row: 6, col: 0, col_span: 5)]
    status: nwg::Label,

    #[nwg_control(text: "Convert")]
    #[nwg_layout_item(layout: grid, row: 6, col: 5)]
    #[nwg_events(OnButtonClick: [App::start])]
    convert_button: nwg::Button,

    #[nwg_control]
    #[nwg_events(OnNotice: [App::on_notice])]
    notice: nwg::Notice,

    shared: RefCell<Option<Arc<Mutex<Shared>>>>,
}

impl App {
    fn busy(&self) -> bool {
        self.shared.borrow().is_some()
    }

    fn on_close(&self, data: &nwg::EventData) {
        if self.busy() {
            let choice = nwg::modal_message(
                &self.window,
                &nwg::MessageParams {
                    title: "PDF to EPUB",
                    content: "A conversion is still running. Quit anyway?",
                    buttons: nwg::MessageButtons::YesNo,
                    icons: nwg::MessageIcons::Question,
                },
            );
            if choice != nwg::MessageChoice::Yes {
                if let nwg::EventData::OnWindowClose(close) = data {
                    close.close(false);
                }
                return;
            }
        }
        nwg::stop_thread_dispatch();
    }

    fn on_drop(&self, data: &nwg::EventData) {
        if self.busy() {
            return;
        }
        if let Some(file) = data.on_file_drop().files().into_iter().next() {
            self.set_input(PathBuf::from(file));
        }
    }

    fn browse_input(&self) {
        if let Some(dir) = parent_dir(&self.input.text()) {
            let _ = self.open_dialog.set_default_folder(&dir);
        }
        if self.open_dialog.run(Some(&self.window))
            && let Ok(path) = self.open_dialog.get_selected_item()
        {
            self.set_input(PathBuf::from(path));
        }
    }

    fn set_input(&self, path: PathBuf) {
        self.input.set_text(&path.to_string_lossy());
        self.output.set_text(&path.with_extension("epub").to_string_lossy());
        let stem = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();

        let meta = std::fs::read(&path).map_err(|e| e.to_string()).and_then(|b| pdf::Pdf::load(&b));
        match meta {
            Ok(pdf) => {
                let meta = pdf.meta();
                self.title.set_text(if meta.title.is_empty() { &stem } else { &meta.title });
                self.author.set_text(&meta.author);
                self.status.set_text(&format!("{} pages. Ready to convert.", pdf.page_count()));
            }
            Err(e) => {
                self.title.set_text(&stem);
                self.author.set_text("");
                self.status.set_text(&e);
            }
        }
        self.progress.set_pos(0);
    }

    fn browse_output(&self) {
        if let Some(dir) = parent_dir(&self.output.text()).or_else(|| parent_dir(&self.input.text())) {
            let _ = self.save_dialog.set_default_folder(&dir);
        }
        if self.save_dialog.run(Some(&self.window))
            && let Ok(path) = self.save_dialog.get_selected_item()
        {
            let mut path = PathBuf::from(path);
            if !path.extension().is_some_and(|e| e.eq_ignore_ascii_case("epub")) {
                path.as_mut_os_string().push(".epub");
            }
            self.output.set_text(&path.to_string_lossy());
        }
    }

    fn start(&self) {
        if self.busy() {
            return;
        }
        let input = PathBuf::from(self.input.text().trim());
        if !input.is_file() {
            nwg::modal_error_message(&self.window, "PDF to EPUB", "Choose an existing PDF file first.");
            return;
        }
        let output_text = self.output.text();
        if output_text.trim().is_empty() {
            nwg::modal_error_message(&self.window, "PDF to EPUB", "Choose where to save the EPUB file.");
            return;
        }
        let output = PathBuf::from(output_text.trim());
        if output.exists() {
            let choice = nwg::modal_message(
                &self.window,
                &nwg::MessageParams {
                    title: "PDF to EPUB",
                    content: &format!("{} already exists. Replace it?", output.display()),
                    buttons: nwg::MessageButtons::YesNo,
                    icons: nwg::MessageIcons::Warning,
                },
            );
            if choice != nwg::MessageChoice::Yes {
                return;
            }
        }
        let mut title = self.title.text().trim().to_string();
        if title.is_empty() {
            title = input.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "Untitled".into());
        }
        let opts = Options {
            input,
            output,
            title,
            author: self.author.text().trim().to_string(),
            images: self.images.check_state() == nwg::CheckBoxState::Checked,
            bookmarks: self.bookmarks.check_state() == nwg::CheckBoxState::Checked,
        };

        let shared = Arc::new(Mutex::new(Shared::default()));
        *self.shared.borrow_mut() = Some(shared.clone());
        self.set_controls_enabled(false);
        self.progress.set_pos(0);
        self.status.set_text("Reading PDF...");

        let sender = self.notice.sender();
        thread::spawn(move || {
            let result = convert::convert(&opts, &mut |done, total| {
                let mut s = shared.lock().unwrap();
                s.done = done;
                s.total = total;
                drop(s);
                sender.notice();
            });
            shared.lock().unwrap().result = Some(result);
            sender.notice();
        });
    }

    fn on_notice(&self) {
        let Some(shared) = self.shared.borrow().clone() else { return };
        let mut s = shared.lock().unwrap();
        if s.total > 0 {
            self.progress.set_range(0..s.total);
            self.progress.set_pos(s.done);
        }
        let Some(result) = s.result.take() else {
            self.status.set_text(&format!("Converting page {} of {}...", s.done, s.total));
            return;
        };
        drop(s);
        *self.shared.borrow_mut() = None;
        self.set_controls_enabled(true);

        match result {
            Ok(summary) => {
                let mut msg = format!(
                    "Done: {} pages, {} chapters, {} images.",
                    summary.pages, summary.chapters, summary.images
                );
                if summary.failed_pages > 0 {
                    msg += &format!(" Text could not be read on {} pages.", summary.failed_pages);
                }
                self.status.set_text(&msg);
                nwg::modal_info_message(&self.window, "PDF to EPUB", &format!("{msg}\n\nSaved to {}", self.output.text()));
            }
            Err(e) => {
                self.progress.set_pos(0);
                self.status.set_text("Conversion failed.");
                nwg::modal_error_message(&self.window, "PDF to EPUB", &e);
            }
        }
    }

    fn set_controls_enabled(&self, enabled: bool) {
        self.input.set_enabled(enabled);
        self.input_button.set_enabled(enabled);
        self.output.set_enabled(enabled);
        self.output_button.set_enabled(enabled);
        self.title.set_enabled(enabled);
        self.author.set_enabled(enabled);
        self.images.set_enabled(enabled);
        self.bookmarks.set_enabled(enabled);
        self.convert_button.set_enabled(enabled);
    }
}

fn parent_dir(path: &str) -> Option<String> {
    let parent = Path::new(path.trim()).parent()?;
    parent.is_dir().then(|| parent.to_string_lossy().into_owned())
}

/// Headless use: `pdf2epub <input.pdf> <output.epub> [--no-images]`; reports via exit code.
fn run_cli(args: &[String]) -> i32 {
    let input = PathBuf::from(&args[0]);
    let pdf_meta = std::fs::read(&input).ok().and_then(|b| pdf::Pdf::load(&b).ok()).map(|p| p.meta());
    let (title, author) = pdf_meta.map(|m| (m.title, m.author)).unwrap_or_default();
    let title = if title.is_empty() {
        input.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default()
    } else {
        title
    };
    let opts = Options {
        output: PathBuf::from(&args[1]),
        input,
        title,
        author,
        images: !args.iter().any(|a| a == "--no-images"),
        bookmarks: !args.iter().any(|a| a == "--no-bookmarks"),
    };
    match convert::convert(&opts, &mut |_, _| {}) {
        Ok(_) => 0,
        Err(_) => 1,
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.len() >= 2 {
        std::process::exit(run_cli(&args));
    }
    nwg::init().expect("Failed to initialize Native Windows GUI");
    let _ = nwg::Font::set_global_family("Segoe UI");
    let _app = App::build_ui(Default::default()).expect("Failed to build UI");
    nwg::dispatch_thread_events();
}
