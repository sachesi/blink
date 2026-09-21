//! A document: the editor, its rendered preview, or both side by side, as one tab of a
//! window.
//!
//! In `document/`: the file and the queue its operations run through, crash recovery, the
//! preview, and find and replace. A document keeps no reference to the window it is in,
//! since a tab can be dragged to another; it looks the window up when it needs one.

use adw::prelude::*;
use adw::subclass::prelude::*;
use gettextrs::gettext;
use gtk::{gio, glib};
use sourceview5::prelude::*;
use std::cell::{Cell, OnceCell, RefCell};

use crate::config;
use crate::editor_view::BlinkEditorView;
use crate::markdown;
use crate::preview_view::BlinkPreviewView;
use crate::window::BlinkWindow;

mod file;
mod preview;
mod recovery;
mod search;

pub use file::{Command, file_title, markdown_filters};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ViewMode {
    #[default]
    Edit,
    Preview,
    Split,
}

impl ViewMode {
    /// The name of the view's toggle in the header bar.
    pub fn name(self) -> &'static str {
        match self {
            Self::Edit => "edit",
            Self::Preview => "preview",
            Self::Split => "split",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "edit" => Some(Self::Edit),
            "preview" => Some(Self::Preview),
            "split" => Some(Self::Split),
            _ => None,
        }
    }
}

mod imp {
    use super::*;

    #[derive(Default, gtk::CompositeTemplate, glib::Properties)]
    #[template(resource = "/io/github/sachesi/blink/ui/document.ui")]
    #[properties(wrapper_type = super::BlinkDocument)]
    pub struct BlinkDocument {
        #[template_child]
        pub search_bar: TemplateChild<gtk::Box>,
        #[template_child]
        pub search_entry: TemplateChild<gtk::SearchEntry>,
        #[template_child]
        pub search_status: TemplateChild<gtk::Label>,
        #[template_child]
        pub replace_row: TemplateChild<gtk::Box>,
        #[template_child]
        pub replace_entry: TemplateChild<gtk::Entry>,
        #[template_child]
        pub replace_button: TemplateChild<gtk::Button>,
        #[template_child]
        pub replace_all_button: TemplateChild<gtk::Button>,
        #[template_child]
        pub monitor_banner: TemplateChild<adw::Banner>,
        #[template_child]
        pub edit_scroll: TemplateChild<gtk::ScrolledWindow>,
        #[template_child]
        pub edit_view: TemplateChild<BlinkEditorView>,
        #[template_child]
        pub edit_buffer: TemplateChild<sourceview5::Buffer>,
        #[template_child]
        pub preview_scroll: TemplateChild<gtk::ScrolledWindow>,
        #[template_child]
        pub preview_view: TemplateChild<BlinkPreviewView>,
        #[template_child]
        pub status_label: TemplateChild<gtk::Label>,

        /// The name of the document with a marker while it has unsaved changes.
        #[property(get)]
        title: RefCell<String>,
        /// The folder of the document's file, empty while it has none.
        #[property(get)]
        folder: RefCell<String>,
        /// The path of the document's file, empty while it has none.
        #[property(get)]
        path: RefCell<String>,
        /// The name of the view: "edit", "preview" or "split".
        #[property(get)]
        view_mode_name: RefCell<String>,
        /// The status bar is hidden, as the window's header bar is in focus mode.
        #[property(get, set)]
        focus_mode: Cell<bool>,
        /// The widest the editor and the preview get, in pixels.
        #[property(get, set)]
        content_width: Cell<i32>,

        pub settings: OnceCell<gio::Settings>,
        pub style_handlers: RefCell<Vec<glib::SignalHandlerId>>,
        pub autosave_timer: RefCell<Option<glib::SourceId>>,

        pub view_mode: Cell<ViewMode>,
        /// The single-pane view the split view was entered from, which it falls back to.
        pub last_single_mode: Cell<ViewMode>,

        pub preview: preview::State,
        pub search: search::State,
        pub document: RefCell<file::State>,
        pub recovery: RefCell<recovery::State>,
        pub commands: OnceCell<async_channel::Sender<Command>>,
        /// A file or a backup is on its way into this document, which is no longer blank.
        pub reserved: Cell<bool>,
        /// The file on its way in, which is not to be opened anywhere else meanwhile.
        pub claimed: RefCell<Option<gio::File>>,
        /// The document was made for the file on its way in, and goes if the file cannot
        /// be opened.
        pub made_for_file: Cell<bool>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for BlinkDocument {
        const NAME: &'static str = "BlinkDocument";
        type Type = super::BlinkDocument;
        type ParentType = adw::Bin;

        fn class_init(klass: &mut Self::Class) {
            BlinkEditorView::ensure_type();
            BlinkPreviewView::ensure_type();
            klass.bind_template();
            klass.bind_template_callbacks();
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    #[glib::derived_properties]
    impl ObjectImpl for BlinkDocument {
        fn constructed(&self) {
            self.parent_constructed();
            let obj = self.obj();
            let settings = config::settings();
            self.content_width.set(settings.int("content-width"));
            self.settings.set(settings).ok();

            let hadj = self.preview_scroll.hadjustment();
            markdown::set_content_width(&hadj, obj.content_width());
            obj.connect_content_width_notify(move |document| {
                markdown::set_content_width(&hadj, document.content_width());
            });
            obj.setup_editor();
            obj.setup_preview();
            obj.setup_search();
            obj.setup_theme();
            obj.setup_document();
            obj.update_title();
            obj.set_view_mode(ViewMode::Edit);
        }

        fn dispose(&self) {
            if let Some(id) = self.autosave_timer.take() {
                id.remove();
            }
            if let Some(id) = self.preview.render_timer.take() {
                id.remove();
            }
            if let Some(id) = self.recovery.borrow_mut().timer.take() {
                id.remove();
            }
            if let Some(monitor) = self.document.borrow_mut().monitor.take() {
                monitor.cancel();
            }
            let style_manager = adw::StyleManager::default();
            for handler in self.style_handlers.take() {
                style_manager.disconnect(handler);
            }
        }
    }

    impl WidgetImpl for BlinkDocument {}
    impl BinImpl for BlinkDocument {}

    #[gtk::template_callbacks]
    impl BlinkDocument {
        #[template_callback]
        fn on_buffer_changed(&self) {
            let obj = self.obj();
            obj.schedule_render();
            obj.schedule_backup();
        }

        #[template_callback]
        fn on_modified_changed(&self) {
            self.obj().update_title();
        }

        #[template_callback]
        fn on_search_changed(&self) {
            self.obj().search_changed();
        }

        #[template_callback]
        fn on_search_next(&self) {
            self.obj().find_next(true);
        }

        #[template_callback]
        fn on_search_closed(&self) {
            self.obj().close_search();
        }

        #[template_callback]
        fn on_replace(&self) {
            self.obj().replace_one();
        }
    }

    impl BlinkDocument {
        pub(super) fn set_title(&self, title: String, folder: String, path: String) {
            let obj = self.obj();
            if *self.path.borrow() != path {
                self.path.replace(path);
                obj.notify_path();
            }
            if *self.title.borrow() != title {
                self.title.replace(title);
                obj.notify_title();
            }
            if *self.folder.borrow() != folder {
                self.folder.replace(folder);
                obj.notify_folder();
            }
        }

        pub(super) fn set_view_mode_name(&self, name: &str) {
            if *self.view_mode_name.borrow() != name {
                self.view_mode_name.replace(name.to_owned());
                self.obj().notify_view_mode_name();
            }
        }
    }
}

glib::wrapper! {
    pub struct BlinkDocument(ObjectSubclass<imp::BlinkDocument>)
        @extends adw::Bin, gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl BlinkDocument {
    pub fn new() -> Self {
        glib::Object::new()
    }

    fn settings(&self) -> &gio::Settings {
        self.imp()
            .settings
            .get()
            .expect("settings set in constructed")
    }

    /// The window the document is in, if it is in one at the moment.
    pub fn window(&self) -> Option<BlinkWindow> {
        self.root().and_downcast()
    }

    /// The window to show a file dialog over.
    fn dialog_parent(&self) -> Option<gtk::Window> {
        self.root().and_downcast()
    }

    /// Whether the document is the selected tab of its window.
    fn is_selected(&self) -> bool {
        self.window()
            .and_then(|window| window.selected_document())
            .as_ref()
            == Some(self)
    }

    /// The document became the selected tab: catch up on what waited for that.
    pub fn selected(&self) {
        self.render_missed();
        self.ask_waiting_conflict();
    }

    /// The name of the document's file, as its tab shows it without the marker of unsaved
    /// changes.
    fn display_name(&self) -> String {
        self.imp()
            .document
            .borrow()
            .file
            .as_ref()
            .map(file::file_title)
            .unwrap_or_else(|| gettext("Untitled Document"))
    }

    /// Bring the document to the front, before it asks the user something.
    pub fn present(&self) {
        if let Some(window) = self.window() {
            window.select_document(self);
            window.present();
        }
    }

    /// Whether the document is an untitled one that was never typed in, which a file that
    /// is opened can go into instead of a tab or window of its own.
    pub fn is_blank(&self) -> bool {
        let imp = self.imp();
        !imp.reserved.get()
            && imp.document.borrow().file.is_none()
            && !imp.edit_buffer.is_modified()
            && imp.edit_buffer.char_count() == 0
    }

    /// Whether `file` is open in the document, or on its way into it.
    pub fn holds(&self, file: &gio::File) -> bool {
        let imp = self.imp();
        let holds = |held: &Option<gio::File>| held.as_ref().is_some_and(|held| held.equal(file));
        holds(&imp.claimed.borrow()) || holds(&imp.document.borrow().file)
    }

    /// Whether the document holds `file`, or the file at the resolved path `canonical`.
    pub fn holds_either(
        &self,
        file: Option<&gio::File>,
        canonical: Option<&std::path::Path>,
    ) -> bool {
        file.is_some_and(|file| self.holds(file))
            || canonical.is_some_and(|canonical| {
                self.imp().document.borrow().canonical.as_deref() == Some(canonical)
            })
    }

    /// Open `file` in the document, which should be blank. `made_for_file` when the
    /// document was made to open the file in.
    pub fn load(&self, file: gio::File, made_for_file: bool) {
        let imp = self.imp();
        imp.made_for_file.set(made_for_file);
        imp.reserved.set(true);
        imp.claimed.replace(Some(file.clone()));
        self.enqueue(Command::OpenFile(file));
    }

    /// Put the text of a backup from a previous session into the document, which should be
    /// blank or hold the file the backup is of.
    pub fn restore(&self, record: crate::backup::BackupRecord) {
        let imp = self.imp();
        imp.reserved.set(true);
        imp.claimed
            .replace(record.original_path.as_deref().map(gio::File::for_path));
        self.enqueue(Command::Restore(record));
    }

    fn release_claim(&self) {
        let imp = self.imp();
        imp.reserved.set(false);
        imp.claimed.take();
    }

    fn setup_editor(&self) {
        let imp = self.imp();
        if let Some(language) = sourceview5::LanguageManager::default().language("markdown") {
            imp.edit_buffer.set_language(Some(&language));
        }
        markdown::setup_tags(imp.edit_buffer.upcast_ref::<gtk::TextBuffer>());

        let settings = self.settings();
        settings
            .bind("show-line-numbers", &*imp.edit_view, "show-line-numbers")
            .get_only()
            .build();
        settings
            .bind("tab-width", &*imp.edit_view, "tab-width")
            .get_only()
            .build();
        settings
            .bind(
                "highlight-current-line",
                &*imp.edit_view,
                "highlight-current-line",
            )
            .get_only()
            .build();
        settings
            .bind(
                "shade-alternate-lines",
                &*imp.edit_view,
                "shade-alternate-lines",
            )
            .get_only()
            .build();
        self.apply_wrap();
        settings.connect_changed(
            Some("wrap-text"),
            glib::clone!(
                #[weak(rename_to = document)]
                self,
                move |_, _| document.apply_wrap()
            ),
        );

        let refont = glib::clone!(
            #[weak(rename_to = document)]
            self,
            move || document.apply_fonts()
        );
        for key in ["text-font", "monospace-font"] {
            let refont = refont.clone();
            settings.connect_changed(Some(key), move |_, _| refont());
        }
        let style_manager = adw::StyleManager::default();
        let handlers = [
            style_manager.connect_document_font_name_notify({
                let refont = refont.clone();
                move |_| refont()
            }),
            style_manager.connect_monospace_font_name_notify(move |_| refont()),
        ];
        imp.style_handlers.borrow_mut().extend(handlers);
    }

    /// Follow a change of font: the inline code of the preview, and code in its table
    /// cells, which only a new render changes. The application's stylesheet does the rest.
    fn apply_fonts(&self) {
        let imp = self.imp();
        let buffer = imp.preview_view.buffer();
        markdown::set_monospace_family(&buffer, &config::font_family(self.settings(), true));
        imp.preview
            .rendered
            .borrow_mut()
            .clear(imp.preview_view.upcast_ref());
        self.render_tick();
    }

    /// Word wrap, or a horizontal scrollbar: without wrapping the longest line would
    /// otherwise become the window's minimum width.
    fn apply_wrap(&self) {
        let imp = self.imp();
        let wrap = self.settings().boolean("wrap-text");
        imp.edit_view.set_wrap_mode(if wrap {
            gtk::WrapMode::Word
        } else {
            gtk::WrapMode::None
        });
        imp.edit_scroll.set_hscrollbar_policy(if wrap {
            gtk::PolicyType::Never
        } else {
            gtk::PolicyType::Automatic
        });
    }

    /// Follow the light or dark style and the accent colour: the editor's style scheme
    /// and the preview's text tags cannot use CSS variables.
    fn setup_theme(&self) {
        self.apply_editor_scheme();
        markdown::apply_theme_colors(&self.imp().preview_view.buffer());
        let style_manager = adw::StyleManager::default();
        let retheme = glib::clone!(
            #[weak(rename_to = document)]
            self,
            move |_: &adw::StyleManager| document.retheme()
        );
        let handlers = vec![
            style_manager.connect_dark_notify(retheme.clone()),
            style_manager.connect_accent_color_notify(retheme),
        ];
        self.imp().style_handlers.borrow_mut().extend(handlers);
    }

    fn apply_editor_scheme(&self) {
        if let Some(scheme) = markdown::current_scheme() {
            self.imp().edit_buffer.set_style_scheme(Some(&scheme));
        }
    }

    fn retheme(&self) {
        self.apply_editor_scheme();
        markdown::apply_theme_colors(&self.imp().preview_view.buffer());
        self.restyle_code_blocks();
    }

    pub fn view_mode(&self) -> ViewMode {
        self.imp().view_mode.get()
    }

    pub fn set_view_mode(&self, mode: ViewMode) {
        let imp = self.imp();
        if imp.view_mode.get() != mode {
            self.close_search();
            if mode != ViewMode::Split {
                imp.last_single_mode.set(mode);
            }
            imp.view_mode.set(mode);
        }
        imp.set_view_mode_name(mode.name());
        self.apply_view_mode();
    }

    /// The window became too narrow for the split view: go back to the view it was
    /// entered from.
    pub fn leave_split(&self) {
        let imp = self.imp();
        if imp.view_mode.get() == ViewMode::Split {
            self.set_view_mode(imp.last_single_mode.get());
        }
    }

    /// Markdown markup around the selection in the editor. Nothing happens in the
    /// preview, where the editor and its selection are out of sight.
    pub fn wrap_selection(&self, prefix: &str, suffix: &str) {
        let imp = self.imp();
        if imp.view_mode.get() == ViewMode::Preview {
            return;
        }
        let buffer = &imp.edit_buffer;
        if let Some((mut start, mut end)) = buffer.selection_bounds() {
            let text = buffer.text(&start, &end, false);
            buffer.begin_user_action();
            buffer.delete(&mut start, &mut end);
            buffer.insert(&mut start, &format!("{prefix}{text}{suffix}"));
            buffer.end_user_action();
        }
    }

    /// Show a transient, non-blocking notice in the document's window.
    fn toast(&self, message: &str) {
        if let Some(window) = self.window() {
            window.toast(message);
        }
    }

    /// The document name with a marker while it has unsaved changes, and its folder.
    fn update_title(&self) {
        let imp = self.imp();
        let file = imp.document.borrow().file.clone();
        let name = file
            .as_ref()
            .map(file::file_title)
            .unwrap_or_else(|| gettext("Untitled Document"));
        let title = if imp.edit_buffer.is_modified() {
            format!("• {name}")
        } else {
            name
        };
        let path = file.and_then(|file| file.path());
        let folder = path
            .as_deref()
            .and_then(|path| path.parent().map(|dir| dir.display().to_string()))
            .unwrap_or_default();
        let path = path
            .map(|path| path.display().to_string())
            .unwrap_or_default();
        imp.set_title(title, folder, path);
    }
}

impl Default for BlinkDocument {
    fn default() -> Self {
        Self::new()
    }
}

/// The whole text of a buffer.
fn buffer_text(buffer: &impl IsA<gtk::TextBuffer>) -> String {
    let (start, end) = buffer.bounds();
    buffer.text(&start, &end, false).to_string()
}

fn saturating_u32(n: usize) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}
