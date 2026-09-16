//! The document window: the editor, its rendered preview, or both side by side.
//!
//! In `window/`: the document's file lifecycle and the queue its operations run through,
//! crash recovery, the preview, and find and replace.

use adw::prelude::*;
use adw::subclass::prelude::*;
use gettextrs::gettext;
use gtk::{gio, glib};
use sourceview5::prelude::*;
use std::cell::{Cell, OnceCell, RefCell};

use crate::config;
use crate::markdown;

mod document;
mod preview;
mod recovery;
mod search;

use document::Command;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ViewMode {
    #[default]
    Edit,
    Preview,
    Split,
}

impl ViewMode {
    /// The name of the view's toggle in the header bar.
    fn name(self) -> &'static str {
        match self {
            Self::Edit => "edit",
            Self::Preview => "preview",
            Self::Split => "split",
        }
    }

    fn from_name(name: &str) -> Option<Self> {
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
    #[template(resource = "/io/github/sachesi/blink/ui/window.ui")]
    #[properties(wrapper_type = super::BlinkWindow)]
    pub struct BlinkWindow {
        #[template_child]
        pub drop_area: TemplateChild<gtk::Overlay>,
        #[template_child]
        pub drop_overlay: TemplateChild<gtk::Box>,
        #[template_child]
        pub toast_overlay: TemplateChild<adw::ToastOverlay>,
        #[template_child]
        pub window_title: TemplateChild<adw::WindowTitle>,
        #[template_child]
        pub view_toggles: TemplateChild<adw::ToggleGroup>,
        #[template_child]
        pub recent_menu: TemplateChild<gio::Menu>,
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
        pub edit_view: TemplateChild<sourceview5::View>,
        #[template_child]
        pub edit_buffer: TemplateChild<sourceview5::Buffer>,
        #[template_child]
        pub preview_scroll: TemplateChild<gtk::ScrolledWindow>,
        #[template_child]
        pub preview_view: TemplateChild<gtk::TextView>,
        #[template_child]
        pub status_label: TemplateChild<gtk::Label>,

        /// Full screen, with the header bar and the status bar hidden.
        #[property(get, set = Self::set_focus_mode)]
        focus_mode: Cell<bool>,

        pub settings: OnceCell<gio::Settings>,
        /// The editor font and zoom, as CSS that changes with the settings.
        pub font_css: OnceCell<gtk::CssProvider>,
        pub style_handlers: RefCell<Vec<glib::SignalHandlerId>>,
        pub autosave_timer: RefCell<Option<glib::SourceId>>,

        pub view_mode: Cell<ViewMode>,
        /// The single-pane view the split view was entered from, which it falls back to.
        pub last_single_mode: Cell<ViewMode>,

        pub preview: preview::State,
        pub search: search::State,
        pub document: RefCell<document::State>,
        pub recovery: RefCell<recovery::State>,
        pub commands: OnceCell<async_channel::Sender<Command>>,
        /// Set once the close has been confirmed, so the next close request goes through.
        pub closing: Cell<bool>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for BlinkWindow {
        const NAME: &'static str = "BlinkWindow";
        type Type = super::BlinkWindow;
        type ParentType = adw::ApplicationWindow;

        fn class_init(klass: &mut Self::Class) {
            sourceview5::View::ensure_type();
            klass.bind_template();
            klass.bind_template_callbacks();

            for (name, command) in [
                ("win.new", Command::New),
                ("win.open", Command::Open),
                ("win.save", Command::Save),
                ("win.save-as", Command::SaveAs),
                ("win.export-html", Command::ExportHtml),
            ] {
                klass.install_action(name, None, move |win, _, _| {
                    win.enqueue(command.clone());
                });
            }
            klass.install_action(
                "win.open-recent",
                Some(glib::VariantTy::STRING),
                |win, _, param| {
                    if let Some(path) = param.and_then(glib::Variant::str) {
                        win.enqueue(Command::OpenFile(gio::File::for_path(path)));
                    }
                },
            );

            klass.install_action(
                "win.copy-code",
                Some(glib::VariantTy::STRING),
                |win, _, param| {
                    if let Some(code) = param.and_then(glib::Variant::str) {
                        win.clipboard().set_text(code);
                        win.toast(&gettext("Code copied"));
                    }
                },
            );

            klass.install_action("win.find", None, |win, _, _| win.toggle_find());
            klass.install_action("win.find-next", None, |win, _, _| win.find_next(true));
            klass.install_action("win.find-previous", None, |win, _, _| win.find_next(false));
            klass.install_action("win.replace", None, |win, _, _| win.show_replace());
            klass.install_action("win.replace-all", None, |win, _, _| win.replace_all());

            klass.install_action("win.format-bold", None, |win, _, _| {
                win.wrap_selection("**", "**");
            });
            klass.install_action("win.format-italic", None, |win, _, _| {
                win.wrap_selection("*", "*");
            });
            klass.install_action("win.format-link", None, |win, _, _| {
                win.wrap_selection("[", "](url)");
            });

            klass.install_action("win.zoom-in", None, |win, _, _| win.zoom(1));
            klass.install_action("win.zoom-out", None, |win, _, _| win.zoom(-1));
            klass.install_action("win.zoom-reset", None, |win, _, _| {
                let _ = win.settings().set_int("zoom", 0);
            });
            klass.install_property_action("win.focus-mode", "focus-mode");
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    #[glib::derived_properties]
    impl ObjectImpl for BlinkWindow {
        fn constructed(&self) {
            self.parent_constructed();
            let obj = self.obj();
            let settings = config::settings();

            let (width, height) = settings.get::<(i32, i32)>("window-size");
            obj.set_default_size(width, height);
            if settings.boolean("window-maximized") {
                obj.maximize();
            }
            self.settings.set(settings).ok();

            obj.setup_editor();
            obj.setup_preview();
            obj.setup_search();
            obj.setup_theme();
            obj.setup_recent_menu();
            obj.setup_drop();
            obj.setup_focus_mode_escape();
            obj.setup_document();
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
            if let (Some(css), Some(display)) = (self.font_css.get(), gtk::gdk::Display::default())
            {
                gtk::style_context_remove_provider_for_display(&display, css);
            }
        }
    }

    impl WidgetImpl for BlinkWindow {}

    impl WindowImpl for BlinkWindow {
        /// Closing goes through the document queue, which asks about unsaved changes and
        /// then closes the window again for real.
        fn close_request(&self) -> glib::Propagation {
            if self.closing.get() {
                return self.parent_close_request();
            }
            self.obj().enqueue(Command::Close);
            glib::Propagation::Stop
        }
    }

    impl ApplicationWindowImpl for BlinkWindow {}
    impl AdwApplicationWindowImpl for BlinkWindow {}

    #[gtk::template_callbacks]
    impl BlinkWindow {
        #[template_callback]
        fn on_view_toggled(&self) {
            if let Some(mode) = self
                .view_toggles
                .active_name()
                .and_then(|name| ViewMode::from_name(&name))
                && mode != self.view_mode.get()
            {
                self.obj().set_view_mode(mode);
            }
        }

        /// The window became too narrow for the split view.
        #[template_callback]
        fn on_narrow(&self) {
            if self.view_mode.get() == ViewMode::Split {
                self.obj().set_view_mode(self.last_single_mode.get());
            }
        }

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

    impl BlinkWindow {
        fn set_focus_mode(&self, focus_mode: bool) {
            if self.focus_mode.replace(focus_mode) == focus_mode {
                return;
            }
            let obj = self.obj();
            if focus_mode {
                obj.fullscreen();
            } else {
                obj.unfullscreen();
            }
        }
    }
}

glib::wrapper! {
    pub struct BlinkWindow(ObjectSubclass<imp::BlinkWindow>)
        @extends adw::ApplicationWindow, gtk::ApplicationWindow, gtk::Window, gtk::Widget,
        @implements gio::ActionGroup, gio::ActionMap, gtk::Accessible, gtk::Buildable,
                    gtk::ConstraintTarget, gtk::Native, gtk::Root, gtk::ShortcutManager;
}

impl BlinkWindow {
    pub fn new(app: &impl IsA<gtk::Application>) -> Self {
        glib::Object::builder().property("application", app).build()
    }

    fn settings(&self) -> &gio::Settings {
        self.imp()
            .settings
            .get()
            .expect("settings set in constructed")
    }

    /// Open `file`, asking first about unsaved changes.
    pub fn open_file(&self, file: gio::File) {
        self.enqueue(Command::OpenFile(file));
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
        self.apply_wrap();
        settings.connect_changed(
            Some("wrap-text"),
            glib::clone!(
                #[weak(rename_to = win)]
                self,
                move |_, _| win.apply_wrap()
            ),
        );

        // A provider of its own carries the font and zoom, so they change without
        // touching the stylesheet.
        let css = gtk::CssProvider::new();
        if let Some(display) = gtk::gdk::Display::default() {
            gtk::style_context_add_provider_for_display(
                &display,
                &css,
                gtk::STYLE_PROVIDER_PRIORITY_APPLICATION + 1,
            );
        }
        imp.font_css.set(css).ok();
        self.apply_font_css();
        for key in ["zoom", "editor-font"] {
            settings.connect_changed(
                Some(key),
                glib::clone!(
                    #[weak(rename_to = win)]
                    self,
                    move |_, _| win.apply_font_css()
                ),
            );
        }
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

    fn apply_font_css(&self) {
        let settings = self.settings();
        let size = (11 + settings.int("zoom")).clamp(6, 32);
        let font = settings.string("editor-font");
        let family = if font.is_empty() {
            String::new()
        } else {
            format!("textview.editor-view {{ font-family: \"{font}\"; }}")
        };
        let css = format!(
            "textview.editor-view {{ font-size: {size}pt; }}\ntextview.transparent-bg {{ font-size: {size}pt; }}\n{family}"
        );
        if let Some(provider) = self.imp().font_css.get() {
            provider.load_from_string(&css);
        }
    }

    fn zoom(&self, delta: i32) {
        let settings = self.settings();
        let _ = settings.set_int("zoom", (settings.int("zoom") + delta).clamp(-5, 21));
    }

    /// Follow the light or dark style and the accent colour: the editor's style scheme
    /// and the preview's text tags cannot use CSS variables.
    fn setup_theme(&self) {
        self.apply_editor_scheme();
        markdown::apply_theme_colors(&self.imp().preview_view.buffer());
        let style_manager = adw::StyleManager::default();
        let retheme = glib::clone!(
            #[weak(rename_to = win)]
            self,
            move |_: &adw::StyleManager| win.retheme()
        );
        let handlers = vec![
            style_manager.connect_dark_notify(retheme.clone()),
            style_manager.connect_accent_color_notify(retheme),
        ];
        self.imp().style_handlers.replace(handlers);
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

    fn setup_recent_menu(&self) {
        self.rebuild_recent_menu();
        self.settings().connect_changed(
            Some("recent-files"),
            glib::clone!(
                #[weak(rename_to = win)]
                self,
                move |_, _| win.rebuild_recent_menu()
            ),
        );
    }

    fn rebuild_recent_menu(&self) {
        let menu = &self.imp().recent_menu;
        menu.remove_all();
        for path in self.settings().strv("recent-files") {
            let label = std::path::Path::new(path.as_str())
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| path.to_string());
            let item = gio::MenuItem::new(Some(&label), None);
            item.set_action_and_target_value(Some("win.open-recent"), Some(&path.to_variant()));
            menu.append_item(&item);
        }
    }

    /// Files dragged onto the window open like files from the Open dialog.
    fn setup_drop(&self) {
        let imp = self.imp();
        // GTK delivers dropped files as a GdkFileList, even a single one.
        let target = gtk::DropTarget::new(
            gtk::gdk::FileList::static_type(),
            gtk::gdk::DragAction::COPY,
        );
        let overlay = imp.drop_overlay.get();
        target.connect_enter(glib::clone!(
            #[weak]
            overlay,
            #[upgrade_or]
            gtk::gdk::DragAction::empty(),
            move |_, _, _| {
                overlay.set_visible(true);
                gtk::gdk::DragAction::COPY
            }
        ));
        target.connect_leave(glib::clone!(
            #[weak]
            overlay,
            move |_| overlay.set_visible(false)
        ));
        target.connect_drop(glib::clone!(
            #[weak(rename_to = win)]
            self,
            #[upgrade_or]
            false,
            move |_, value, _, _| {
                win.imp().drop_overlay.set_visible(false);
                let Some(file) = value
                    .get::<gtk::gdk::FileList>()
                    .ok()
                    .and_then(|list| list.files().into_iter().next())
                else {
                    return false;
                };
                win.open_file(file);
                true
            }
        ));
        imp.drop_area.add_controller(target);
    }

    /// Escape leaves focus mode. In the bubble phase, so an open search entry takes the
    /// key first and closes the search.
    fn setup_focus_mode_escape(&self) {
        let keys = gtk::EventControllerKey::new();
        keys.connect_key_pressed(glib::clone!(
            #[weak(rename_to = win)]
            self,
            #[upgrade_or]
            glib::Propagation::Proceed,
            move |_, key, _, _| {
                if key == gtk::gdk::Key::Escape && win.focus_mode() {
                    win.set_focus_mode(false);
                    return glib::Propagation::Stop;
                }
                glib::Propagation::Proceed
            }
        ));
        self.add_controller(keys);
    }

    fn set_view_mode(&self, mode: ViewMode) {
        let imp = self.imp();
        if imp.view_mode.get() != mode {
            self.close_search();
            if mode != ViewMode::Split {
                imp.last_single_mode.set(mode);
            }
            imp.view_mode.set(mode);
        }
        // Keeps the toggles in step when the mode is set from code; setting the name the
        // group already has does not notify again.
        imp.view_toggles.set_active_name(Some(mode.name()));
        self.apply_view_mode();
    }

    /// Markdown markup around the selection in the editor. Nothing happens in the
    /// preview, where the editor and its selection are out of sight.
    fn wrap_selection(&self, prefix: &str, suffix: &str) {
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

    /// Show a transient, non-blocking notice.
    fn toast(&self, message: &str) {
        self.imp().toast_overlay.add_toast(adw::Toast::new(message));
    }

    /// The document name with a marker while it has unsaved changes, and its folder as
    /// the subtitle.
    fn update_title(&self) {
        let imp = self.imp();
        let file = imp.document.borrow().file.clone();
        let name = file
            .as_ref()
            .map(document::file_title)
            .unwrap_or_else(|| gettext("Untitled Document"));
        let title = if imp.edit_buffer.is_modified() {
            format!("• {name}")
        } else {
            name
        };
        let subtitle = file
            .and_then(|file| file.path())
            .and_then(|path| path.parent().map(|dir| dir.display().to_string()))
            .unwrap_or_default();
        imp.window_title.set_title(&title);
        imp.window_title.set_subtitle(&subtitle);
        self.set_title(Some(&title));
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
