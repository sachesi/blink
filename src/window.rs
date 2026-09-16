//! A window: the header bar, and the documents it holds as tabs.
//!
//! The header bar and the `win.*` actions act on the selected document. Documents move
//! between windows with their tabs, and a tab dropped outside every window opens in a new
//! one.

use adw::prelude::*;
use adw::subclass::prelude::*;
use gettextrs::gettext;
use gtk::{gio, glib};
use std::cell::{Cell, OnceCell, RefCell};
use std::collections::HashMap;

use crate::application::BlinkApplication;
use crate::config;
use crate::document::{BlinkDocument, Command, ViewMode};

/// What ties a document to the window it is in, undone when it leaves.
struct Attachment {
    bindings: Vec<glib::Binding>,
    handlers: Vec<glib::SignalHandlerId>,
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
        pub width_button: TemplateChild<gtk::MenuButton>,
        #[template_child]
        pub recent_menu: TemplateChild<gio::Menu>,
        #[template_child]
        pub tab_view: TemplateChild<adw::TabView>,

        /// Full screen, with the header bar and the status bar hidden.
        #[property(get, set = Self::set_focus_mode)]
        focus_mode: Cell<bool>,
        /// The widest the editor and the preview of its documents get, in pixels.
        #[property(get, set)]
        content_width: Cell<i32>,

        pub settings: OnceCell<gio::Settings>,
        /// The window is too narrow for the split view.
        pub narrow: Cell<bool>,
        /// Every document has been closed, so the next close request goes through.
        pub closing: Cell<bool>,
        /// The window is closing its documents one after the other.
        pub closing_all: Cell<bool>,
        /// The tab whose context menu is open.
        pub menu_page: RefCell<Option<adw::TabPage>>,
        pub(super) attachments: RefCell<HashMap<adw::TabPage, Attachment>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for BlinkWindow {
        const NAME: &'static str = "BlinkWindow";
        type Type = super::BlinkWindow;
        type ParentType = adw::ApplicationWindow;

        fn class_init(klass: &mut Self::Class) {
            BlinkDocument::ensure_type();
            klass.bind_template();
            klass.bind_template_callbacks();

            klass.install_action("win.new", None, |win, _, _| {
                // A new document starts in the source, whatever view the window was
                // showing. Not so a tab opened for a file, which would change the view
                // of the window even when the file fails to open.
                win.app()
                    .new_document(Some(win))
                    .set_view_mode(ViewMode::Edit);
            });
            klass.install_action_async("win.open", None, |win, _, _| async move {
                win.open().await;
            });
            klass.install_action(
                "win.open-recent",
                Some(glib::VariantTy::STRING),
                |win, _, param| {
                    if let Some(path) = param.and_then(glib::Variant::str) {
                        win.app().open_file(gio::File::for_path(path), Some(win));
                    }
                },
            );
            for (name, command) in [
                ("win.save", Command::Save),
                ("win.save-as", Command::SaveAs),
                ("win.export-html", Command::ExportHtml),
                ("win.export-pdf", Command::ExportPdf),
            ] {
                klass.install_action(name, None, move |win, _, _| {
                    if let Some(document) = win.selected_document() {
                        document.enqueue(command.clone());
                    }
                });
            }
            klass.install_action("win.close-document", None, |win, _, _| {
                if let Some(page) = win.imp().tab_view.selected_page() {
                    win.imp().tab_view.close_page(&page);
                }
            });
            klass.install_action("win.tab-to-new-window", None, |win, _, _| {
                if let Some(page) = win.imp().menu_page.take() {
                    let window = super::BlinkWindow::for_tab(&win.app(), win);
                    win.imp()
                        .tab_view
                        .transfer_page(&page, &window.imp().tab_view, 0);
                    window.present();
                }
            });
            klass.install_action("win.tab-close", None, |win, _, _| {
                if let Some(page) = win.imp().menu_page.take() {
                    win.imp().tab_view.close_page(&page);
                }
            });

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

            klass.install_action("win.find", None, |win, _, _| {
                win.with_document(BlinkDocument::toggle_find);
            });
            klass.install_action("win.find-next", None, |win, _, _| {
                win.with_document(|document| document.find_next(true));
            });
            klass.install_action("win.find-previous", None, |win, _, _| {
                win.with_document(|document| document.find_next(false));
            });
            klass.install_action("win.replace", None, |win, _, _| {
                win.with_document(BlinkDocument::show_replace);
            });
            klass.install_action("win.replace-all", None, |win, _, _| {
                win.with_document(BlinkDocument::replace_all);
            });

            klass.install_action("win.format-bold", None, |win, _, _| {
                win.with_document(|document| document.wrap_selection("**", "**"));
            });
            klass.install_action("win.format-italic", None, |win, _, _| {
                win.with_document(|document| document.wrap_selection("*", "*"));
            });
            klass.install_action("win.format-link", None, |win, _, _| {
                win.with_document(|document| document.wrap_selection("[", "](url)"));
            });

            klass.install_action("win.zoom-in", None, |win, _, _| win.zoom(1));
            klass.install_action("win.zoom-out", None, |win, _, _| win.zoom(-1));
            klass.install_action("win.zoom-reset", None, |win, _, _| {
                let _ = win.settings().set_int("zoom", 0);
            });
            klass.install_property_action("win.focus-mode", "focus-mode");
            klass.install_property_action("win.content-width", "content-width");
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
            self.settings.set(config::settings()).ok();

            obj.setup_content_width();
            obj.setup_recent_menu();
            obj.setup_drop();
            obj.setup_focus_mode_escape();
            // Losing focus is when a crash elsewhere, a logout or a power cut is likeliest
            // to catch unsaved work.
            obj.connect_is_active_notify(|win| {
                if !win.is_active() {
                    for document in win.documents() {
                        document.enqueue(Command::Backup);
                    }
                }
            });
            obj.sync_header();
        }
    }

    impl WidgetImpl for BlinkWindow {}

    impl WindowImpl for BlinkWindow {
        /// Closing closes the documents one by one, each asking about its unsaved changes,
        /// and closes the window again once none is left. Cancelling one question stops
        /// there, with the documents not yet closed still open.
        fn close_request(&self) -> glib::Propagation {
            let obj = self.obj();
            if self.closing.get() || self.tab_view.n_pages() == 0 {
                obj.save_window_state();
                return self.parent_close_request();
            }
            if !self.closing_all.replace(true) {
                obj.close_next_document();
            }
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
                && let Some(document) = self.obj().selected_document()
                && mode != document.view_mode()
            {
                document.set_view_mode(mode);
            }
        }

        /// The window became too narrow for the split view.
        #[template_callback]
        fn on_narrow(&self) {
            self.narrow.set(true);
            if let Some(document) = self.obj().selected_document() {
                document.leave_split();
            }
        }

        #[template_callback]
        fn on_wide(&self) {
            self.narrow.set(false);
        }

        /// A tab is closing: its document asks about unsaved changes first.
        #[template_callback]
        fn on_close_page(&self, page: &adw::TabPage) -> bool {
            let obj = self.obj();
            let Ok(document) = page.child().downcast::<BlinkDocument>() else {
                return false;
            };
            glib::spawn_future_local(glib::clone!(
                #[weak]
                obj,
                #[weak]
                page,
                async move {
                    let close = document.request_close().await;
                    obj.imp().tab_view.close_page_finish(&page, close);
                    if !close {
                        obj.imp().closing_all.set(false);
                    } else if obj.imp().closing_all.get() {
                        obj.close_next_document();
                    }
                }
            ));
            // Stopped here; the answer above finishes the close.
            true
        }

        /// A tab was dropped outside every window.
        #[template_callback]
        fn on_create_window(&self) -> Option<adw::TabView> {
            let obj = self.obj();
            let window = super::BlinkWindow::for_tab(&obj.app(), &obj);
            window.present();
            Some(window.imp().tab_view.get())
        }

        #[template_callback]
        fn on_page_attached(&self, page: &adw::TabPage) {
            self.obj().attach(page);
        }

        #[template_callback]
        fn on_page_detached(&self, page: &adw::TabPage) {
            let obj = self.obj();
            if let Some(attachment) = self.attachments.borrow_mut().remove(page) {
                let document = page.child();
                for binding in attachment.bindings {
                    binding.unbind();
                }
                for handler in attachment.handlers {
                    document.disconnect(handler);
                }
            }
            // The last document was closed or went to another window.
            if self.tab_view.n_pages() == 0 {
                self.closing.set(true);
                glib::idle_add_local_once(glib::clone!(
                    #[weak]
                    obj,
                    move || obj.close()
                ));
            }
        }

        #[template_callback]
        fn on_setup_menu(&self, page: Option<&adw::TabPage>) {
            self.menu_page.replace(page.cloned());
            // A window's only document is already in a window of its own.
            self.obj()
                .action_set_enabled("win.tab-to-new-window", self.tab_view.n_pages() > 1);
        }

        #[template_callback]
        /// The view belongs to the window: the document switched to takes the one the
        /// window shows. A window's only document brings its own, as when a tab arrives
        /// in a new window.
        fn on_selected_page(&self) {
            let obj = self.obj();
            if let Some(document) = obj.selected_document() {
                if self.tab_view.n_pages() > 1
                    && let Some(mode) = self
                        .view_toggles
                        .active_name()
                        .and_then(|name| ViewMode::from_name(&name))
                    && mode != document.view_mode()
                {
                    document.set_view_mode(mode);
                }
                if self.narrow.get() {
                    document.leave_split();
                }
                document.selected();
            }
            obj.sync_header();
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
    /// A window without documents, at the size the last one closed with;
    /// [`BlinkWindow::add_document`] gives it one.
    pub fn new(app: &impl IsA<gtk::Application>) -> Self {
        let window: Self = glib::Object::builder().property("application", app).build();
        let settings = window.settings();
        let (width, height) = settings.get::<(i32, i32)>("window-size");
        window.set_default_size(width, height);
        if settings.boolean("window-maximized") {
            window.maximize();
        }
        window
    }

    /// A window for a tab moved out of `source`, as large as `source` is when not
    /// maximized.
    pub fn for_tab(app: &impl IsA<gtk::Application>, source: &BlinkWindow) -> Self {
        let window: Self = glib::Object::builder().property("application", app).build();
        let (width, height) = source.default_size();
        window.set_default_size(width, height);
        window
    }

    fn app(&self) -> BlinkApplication {
        self.application()
            .and_downcast()
            .expect("windows belong to the application")
    }

    fn settings(&self) -> &gio::Settings {
        self.imp()
            .settings
            .get()
            .expect("settings set in constructed")
    }

    /// Add `document` as a tab after the others, and select it.
    pub fn add_document(&self, document: &BlinkDocument) {
        let tab_view = &self.imp().tab_view;
        let page = tab_view.append(document);
        tab_view.set_selected_page(&page);
    }

    pub fn select_document(&self, document: &BlinkDocument) {
        let tab_view = &self.imp().tab_view;
        tab_view.set_selected_page(&tab_view.page(document));
    }

    /// Mark the tab of `document` as needing attention, or clear the mark.
    pub fn set_needs_attention(&self, document: &BlinkDocument, needs_attention: bool) {
        self.imp()
            .tab_view
            .page(document)
            .set_needs_attention(needs_attention);
    }

    pub fn selected_document(&self) -> Option<BlinkDocument> {
        self.imp()
            .tab_view
            .selected_page()
            .and_then(|page| page.child().downcast().ok())
    }

    pub fn documents(&self) -> Vec<BlinkDocument> {
        let tab_view = &self.imp().tab_view;
        (0..tab_view.n_pages())
            .filter_map(|i| tab_view.nth_page(i).child().downcast().ok())
            .collect()
    }

    /// Close `document`, a blank one a file failed to open in, unless nothing else is
    /// left in the window.
    pub fn discard_blank(&self, document: &BlinkDocument) {
        let tab_view = &self.imp().tab_view;
        if tab_view.n_pages() > 1 {
            tab_view.close_page(&tab_view.page(document));
        }
    }

    fn with_document(&self, action: impl FnOnce(&BlinkDocument)) {
        if let Some(document) = self.selected_document() {
            action(&document);
        }
    }

    /// Show a transient, non-blocking notice.
    pub fn toast(&self, message: &str) {
        self.imp().toast_overlay.add_toast(adw::Toast::new(message));
    }

    fn close_next_document(&self) {
        let tab_view = &self.imp().tab_view;
        if tab_view.n_pages() > 0 {
            tab_view.close_page(&tab_view.nth_page(0));
        }
    }

    /// The default size is the size the window has when it is not maximized, which is the
    /// one to open with next time.
    fn save_window_state(&self) {
        let (width, height) = self.default_size();
        let settings = self.settings();
        let _ = settings.set("window-size", (width, height));
        let _ = settings.set_boolean("window-maximized", self.is_maximized());
    }

    async fn open(&self) {
        let dialog = gtk::FileDialog::new();
        let (filters, markdown) = crate::document::markdown_filters();
        dialog.set_filters(Some(&filters));
        dialog.set_default_filter(Some(&markdown));
        let Ok(files) = dialog.open_multiple_future(Some(self)).await else {
            return;
        };
        let app = self.app();
        for file in files.iter::<gio::File>().flatten() {
            app.open_file(file, Some(self));
        }
    }

    /// Tie a document that arrived to the window: its width and focus mode follow the
    /// window's, and its title shows on its tab and, while selected, in the header bar.
    fn attach(&self, page: &adw::TabPage) {
        let Ok(document) = page.child().downcast::<BlinkDocument>() else {
            return;
        };
        let bindings = vec![
            self.bind_property("content-width", &document, "content-width")
                .sync_create()
                .build(),
            self.bind_property("focus-mode", &document, "focus-mode")
                .sync_create()
                .build(),
            document
                .bind_property("title", page, "title")
                .sync_create()
                .build(),
            // The tooltip is markup, and a folder name can hold markup characters.
            document
                .bind_property("folder", page, "tooltip")
                .transform_to(|_, folder: String| Some(glib::markup_escape_text(&folder)))
                .sync_create()
                .build(),
        ];
        let sync = glib::clone!(
            #[weak(rename_to = win)]
            self,
            move |document: &BlinkDocument| {
                if win.selected_document().as_ref() == Some(document) {
                    win.sync_header();
                }
            }
        );
        let handlers = vec![
            document.connect_title_notify(sync.clone()),
            document.connect_folder_notify(sync.clone()),
            document.connect_view_mode_name_notify(sync),
        ];
        self.imp()
            .attachments
            .borrow_mut()
            .insert(page.clone(), Attachment { bindings, handlers });
    }

    /// Show the selected document's title, folder and view in the header bar.
    fn sync_header(&self) {
        let imp = self.imp();
        let Some(document) = self.selected_document() else {
            return;
        };
        let title = document.title();
        imp.window_title.set_title(&title);
        imp.window_title.set_subtitle(&document.folder());
        self.set_title(Some(&title));
        // Setting the name the group already has does not notify again.
        imp.view_toggles
            .set_active_name(Some(&document.view_mode_name()));
    }

    /// The window starts at the width in the settings and follows a change to it; the
    /// header bar menu changes this window's alone.
    fn setup_content_width(&self) {
        self.settings()
            .bind("content-width", self, "content-width")
            .get_only()
            .build();
        let menu = gio::Menu::new();
        for width in config::CONTENT_WIDTHS {
            let item = gio::MenuItem::new(Some(&config::content_width_label(width)), None);
            item.set_action_and_target_value(Some("win.content-width"), Some(&width.to_variant()));
            menu.append_item(&item);
        }
        self.imp().width_button.set_menu_model(Some(&menu));
    }

    fn zoom(&self, delta: i32) {
        let settings = self.settings();
        let _ = settings.set_int("zoom", (settings.int("zoom") + delta).clamp(-5, 21));
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
                let Ok(files) = value.get::<gtk::gdk::FileList>() else {
                    return false;
                };
                let app = win.app();
                for file in files.files() {
                    app.open_file(file, Some(&win));
                }
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
}
