//! A window: the header bar, and the documents it holds as tabs.
//!
//! The header bar and the `win.*` actions act on the selected document. Documents move
//! between windows with their tabs, and a tab dropped outside every window opens in a new
//! one. A window can show a folder, whose Markdown files the sidebar lists.

use adw::prelude::*;
use adw::subclass::prelude::*;
use gettextrs::gettext;
use gettextrs::ngettext;
use gtk::{gio, glib};
use std::cell::{Cell, OnceCell, RefCell};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use crate::application::BlinkApplication;
use crate::config;
use crate::document::{BlinkDocument, Command, ViewMode};
use crate::folder;

/// The actions that act on the selected document, off while the window has none.
const DOCUMENT_ACTIONS: &[&str] = &[
    "win.save",
    "win.save-as",
    "win.export-html",
    "win.export-pdf",
    "win.close-document",
    "win.find",
    "win.find-next",
    "win.find-previous",
    "win.replace",
    "win.replace-all",
    "win.format-bold",
    "win.format-italic",
    "win.format-link",
];

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
        #[template_child]
        pub content_stack: TemplateChild<gtk::Stack>,
        #[template_child]
        pub view_bar: TemplateChild<gtk::ActionBar>,
        #[template_child]
        pub split_view: TemplateChild<adw::OverlaySplitView>,
        #[template_child]
        pub sidebar_button: TemplateChild<gtk::ToggleButton>,
        #[template_child]
        pub folder_title: TemplateChild<adw::WindowTitle>,
        #[template_child]
        pub folder_stack: TemplateChild<gtk::Stack>,
        #[template_child]
        pub folder_list: TemplateChild<gtk::ListView>,
        #[template_child]
        pub folder_truncated: TemplateChild<gtk::Label>,

        /// Full screen, with the header bar and the status bar hidden.
        #[property(get, set = Self::set_focus_mode)]
        focus_mode: Cell<bool>,
        /// The widest the editor and the preview of its documents get, in pixels.
        #[property(get, set)]
        content_width: Cell<i32>,
        /// The sidebar with the files of the folder is shown.
        #[property(get, set)]
        show_sidebar: Cell<bool>,

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

        /// The folder the sidebar lists.
        pub folder: RefCell<Option<gio::File>>,
        /// What the sidebar lists, `None` until the folder was first scanned.
        pub folder_listing: RefCell<Option<folder::Listing>>,
        /// The entries at the top of the folder, which the sidebar's tree grows from.
        pub folder_root: OnceCell<gio::ListStore>,
        /// A scan of the folder is running.
        pub folder_scanning: Cell<bool>,
        /// Another scan was asked for while one ran, and follows it.
        pub folder_rescan: Cell<bool>,
        /// The sidebar was shown when focus mode hid it.
        pub sidebar_before_focus: Cell<bool>,
        /// The file whose folders the sidebar last opened to show it. They are opened once,
        /// so a folder closed by hand stays closed while its file is edited.
        pub folder_revealed: RefCell<Option<PathBuf>>,
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
            klass.install_action_async("win.open-folder", None, |win, _, _| async move {
                win.open_folder().await;
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
            klass.install_property_action("win.show-sidebar", "show-sidebar");
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
            obj.setup_folder_list();
            obj.action_set_enabled("win.show-sidebar", false);
            obj.sync_content();
            // Losing focus is when a crash elsewhere, a logout or a power cut is likeliest
            // to catch unsaved work.
            obj.connect_is_active_notify(|win| {
                if !win.is_active() {
                    for document in win.documents() {
                        document.enqueue(Command::Backup);
                    }
                } else {
                    // Coming back from another program, which may have added or removed
                    // files.
                    win.scan_folder();
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
            let obj = self.obj();
            match obj.selected_document() {
                Some(document) => document.leave_split(),
                // The sidebar collapsing hides it, and with no document it is all there
                // is to pick from.
                None if obj.folder().is_some() => obj.set_show_sidebar(true),
                None => {}
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

        /// "New Tab" in the tab overview: a new document, in the source.
        #[template_callback]
        fn on_create_tab(&self) -> adw::TabPage {
            let document = BlinkDocument::new();
            let page = self.tab_view.append(&document);
            // Selected before the view is set, which selecting would replace with the
            // window's.
            self.tab_view.set_selected_page(&page);
            document.set_view_mode(ViewMode::Edit);
            page
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
            let obj = self.obj();
            obj.attach(page);
            obj.sync_content();
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
            // A tab left with a file of the same name as the one that went needs its
            // folder no more.
            obj.update_tab_titles();
            obj.sync_content();
            // The last document was closed or went to another window. A window with a
            // folder stays for the next file from it, unless it is closing.
            if self.tab_view.n_pages() == 0 && (obj.folder().is_none() || self.closing_all.get()) {
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
            // A window's only document is already in a window of its own, unless the window
            // stays for its folder.
            let obj = self.obj();
            obj.action_set_enabled(
                "win.tab-to-new-window",
                self.tab_view.n_pages() > 1 || obj.folder().is_some(),
            );
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

        /// Enter on a row of the sidebar.
        #[template_callback]
        fn on_folder_row_activated(&self, position: u32) {
            let obj = self.obj();
            if let Some(row) = obj.folder_tree().and_then(|tree| tree.row(position)) {
                obj.activate_folder_row(&row);
            }
        }
    }

    impl BlinkWindow {
        fn set_focus_mode(&self, focus_mode: bool) {
            if self.focus_mode.replace(focus_mode) == focus_mode {
                return;
            }
            let obj = self.obj();
            if focus_mode {
                self.sidebar_before_focus.set(obj.show_sidebar());
                obj.set_show_sidebar(false);
                obj.fullscreen();
            } else {
                if self.sidebar_before_focus.get() {
                    obj.set_show_sidebar(true);
                }
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

    /// Close `document`, a blank one made for a file that failed to open, and with it its
    /// window when it has no other tab. The last window of the application stays.
    pub fn discard_blank(&self, document: &BlinkDocument) {
        let tab_view = &self.imp().tab_view;
        if tab_view.n_pages() > 1 || self.folder().is_some() || self.app().windows().len() > 1 {
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

    async fn open_folder(&self) {
        let dialog = gtk::FileDialog::new();
        if let Ok(folder) = dialog.select_folder_future(Some(self)).await {
            self.app().open_folder(folder, Some(self));
        }
    }

    pub fn folder(&self) -> Option<gio::File> {
        self.imp().folder.borrow().clone()
    }

    /// List `folder` in the sidebar and show it. An untitled document never typed in, the
    /// window's only one, gives way to the page that asks for a file from it.
    pub fn set_folder(&self, folder: gio::File) {
        let imp = self.imp();
        let name = crate::document::file_title(&folder);
        imp.folder_title.set_title(&name);
        imp.folder_title.set_tooltip_text(
            folder
                .path()
                .map(|path| path.display().to_string())
                .as_deref(),
        );
        imp.folder.replace(Some(folder));
        imp.folder_listing.replace(None);
        if let Some(root) = imp.folder_root.get() {
            root.remove_all();
        }
        imp.sidebar_button.set_visible(true);
        self.action_set_enabled("win.show-sidebar", true);
        self.set_show_sidebar(true);
        if let [document] = self.documents().as_slice()
            && document.is_blank()
        {
            imp.tab_view.close_page(&imp.tab_view.page(document));
        }
        self.scan_folder();
        self.sync_header();
    }

    /// The tree of the sidebar: the entries of the folder, each folder with its own below
    /// it once expanded.
    fn setup_folder_list(&self) {
        let imp = self.imp();
        let root = gio::ListStore::new::<glib::BoxedAnyObject>();
        let tree = gtk::TreeListModel::new(root.clone(), false, false, |item| {
            let entry = item.downcast_ref::<glib::BoxedAnyObject>()?;
            let children = entry.borrow::<folder::Entry>().children.clone()?;
            Some(entry_store(&children).upcast())
        });
        imp.folder_root.set(root).ok();
        let selection = gtk::SingleSelection::builder()
            .model(&tree)
            .autoselect(false)
            .can_unselect(true)
            .build();

        let factory = gtk::SignalListItemFactory::new();
        factory.connect_setup(glib::clone!(
            #[weak(rename_to = win)]
            self,
            move |_, item| {
                let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
                    return;
                };
                let icon = gtk::Image::new();
                let label = gtk::Label::builder()
                    .xalign(0.0)
                    .ellipsize(gtk::pango::EllipsizeMode::Middle)
                    .build();
                let content = gtk::Box::new(gtk::Orientation::Horizontal, 6);
                content.append(&icon);
                content.append(&label);
                let expander = gtk::TreeExpander::builder()
                    .indent_for_icon(true)
                    .child(&content)
                    .build();
                // A click opens the file or opens and closes the folder at once. Taking
                // the click from the list keeps the selection on the selected tab's file,
                // and a double click from opening and closing a folder again.
                let click = gtk::GestureClick::new();
                click.connect_pressed(glib::clone!(
                    #[weak]
                    win,
                    #[weak]
                    expander,
                    move |gesture, presses, _, _| {
                        gesture.set_state(gtk::EventSequenceState::Claimed);
                        if presses == 1
                            && let Some(row) = expander.list_row()
                        {
                            win.activate_folder_row(&row);
                        }
                    }
                ));
                expander.add_controller(click);
                item.set_child(Some(&expander));
            }
        ));
        factory.connect_bind(|_, item| {
            let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
                return;
            };
            let Some(row) = item.item().and_downcast::<gtk::TreeListRow>() else {
                return;
            };
            let Some(expander) = item.child().and_downcast::<gtk::TreeExpander>() else {
                return;
            };
            expander.set_list_row(Some(&row));
            let Some(entry) = row.item().and_downcast::<glib::BoxedAnyObject>() else {
                return;
            };
            let entry = entry.borrow::<folder::Entry>();
            let Some(content) = expander.child() else {
                return;
            };
            if let Some(icon) = content.first_child().and_downcast::<gtk::Image>() {
                icon.set_icon_name(Some(if entry.children.is_some() {
                    "folder-symbolic"
                } else {
                    "text-x-generic-symbolic"
                }));
            }
            if let Some(label) = content.last_child().and_downcast::<gtk::Label>() {
                label.set_label(&entry.name);
            }
        });
        factory.connect_unbind(|_, item| {
            if let Some(expander) = item
                .downcast_ref::<gtk::ListItem>()
                .and_then(|item| item.child())
                .and_downcast::<gtk::TreeExpander>()
            {
                expander.set_list_row(None);
            }
        });
        imp.folder_list.set_factory(Some(&factory));
        imp.folder_list.set_model(Some(&selection));
    }

    fn folder_selection(&self) -> Option<gtk::SingleSelection> {
        self.imp().folder_list.model().and_downcast()
    }

    fn folder_tree(&self) -> Option<gtk::TreeListModel> {
        self.folder_selection()?.model().and_downcast()
    }

    /// Open the file of `row` as a tab, or open or close its folder.
    fn activate_folder_row(&self, row: &gtk::TreeListRow) {
        let Some(entry) = row.item().and_downcast::<glib::BoxedAnyObject>() else {
            return;
        };
        let (path, is_folder) = {
            let entry = entry.borrow::<folder::Entry>();
            (entry.path.clone(), entry.children.is_some())
        };
        if is_folder {
            row.set_expanded(!row.is_expanded());
            return;
        }
        self.app()
            .open_file_from_folder(gio::File::for_path(path), self);
        // Over a narrow window the sidebar hides the document just opened.
        if self.imp().split_view.is_collapsed() {
            self.set_show_sidebar(false);
        }
    }

    /// Scan the folder again, off the main thread, and list what it holds now. One scan
    /// runs at a time: a large folder takes long enough to walk that switching windows
    /// would otherwise start scans faster than they end.
    fn scan_folder(&self) {
        let imp = self.imp();
        let Some(root) = self.folder().and_then(|folder| folder.path()) else {
            return;
        };
        if imp.folder_scanning.replace(true) {
            imp.folder_rescan.set(true);
            return;
        }
        glib::spawn_future_local(glib::clone!(
            #[weak(rename_to = win)]
            self,
            async move {
                let listing =
                    gio::spawn_blocking(move || folder::scan(&root, folder::MAX_FILES)).await;
                let imp = win.imp();
                imp.folder_scanning.set(false);
                if let Ok(listing) = listing {
                    win.show_listing(listing);
                }
                if imp.folder_rescan.take() {
                    win.scan_folder();
                }
            }
        ));
    }

    /// Put `listing` in the sidebar, with the folders that were open still open. The tree
    /// is left alone when nothing changed, so its scrolling stays as well.
    fn show_listing(&self, listing: folder::Listing) {
        let imp = self.imp();
        if imp.folder_listing.borrow().as_ref() == Some(&listing) {
            return;
        }
        let (Some(root), Some(tree)) = (imp.folder_root.get(), self.folder_tree()) else {
            return;
        };
        let expanded: HashSet<PathBuf> = (0..tree.n_items())
            .filter_map(|position| tree.row(position))
            .filter(gtk::TreeListRow::is_expanded)
            .filter_map(|row| entry_path(&row))
            .collect();
        let entries: Vec<glib::BoxedAnyObject> = listing
            .entries
            .iter()
            .cloned()
            .map(glib::BoxedAnyObject::new)
            .collect();
        root.splice(0, root.n_items(), &entries);
        let mut position = 0;
        while position < tree.n_items() {
            if let Some(row) = tree.row(position)
                && entry_path(&row).is_some_and(|path| expanded.contains(&path))
            {
                row.set_expanded(true);
            }
            position += 1;
        }
        imp.folder_stack
            .set_visible_child_name(if listing.entries.is_empty() {
                "empty"
            } else {
                "files"
            });
        imp.folder_truncated.set_visible(listing.truncated);
        if listing.truncated {
            let max = u32::try_from(folder::MAX_FILES).unwrap_or(u32::MAX);
            imp.folder_truncated.set_label(
                &ngettext(
                    "Only the first {} file is listed",
                    "Only the first {} files are listed",
                    max,
                )
                .replacen("{}", &max.to_string(), 1),
            );
        }
        imp.folder_listing.replace(Some(listing));
        self.select_folder_row();
    }

    /// Select the file of the selected document in the sidebar, opening the folders it is
    /// in when it comes to be selected, or nothing when it is not in the folder.
    fn select_folder_row(&self) {
        let (Some(selection), Some(tree)) = (self.folder_selection(), self.folder_tree()) else {
            return;
        };
        let path = self
            .selected_document()
            .map(|document| PathBuf::from(document.path()))
            .filter(|path| !path.as_os_str().is_empty());
        let reveal = *self.imp().folder_revealed.borrow() != path;
        let mut found = gtk::INVALID_LIST_POSITION;
        if let Some(path) = &path {
            let mut position = 0;
            while position < tree.n_items() {
                if let Some(row) = tree.row(position)
                    && let Some(entry_path) = entry_path(&row)
                {
                    if entry_path == *path {
                        found = position;
                        break;
                    }
                    if reveal && row.is_expandable() && path.starts_with(&entry_path) {
                        row.set_expanded(true);
                    }
                }
                position += 1;
            }
        }
        // A file not found yet, as none is before the first scan ends, is still to show.
        if reveal {
            self.imp()
                .folder_revealed
                .replace(path.filter(|_| found != gtk::INVALID_LIST_POSITION));
        }
        if selection.selected() != found {
            selection.set_selected(found);
            if found != gtk::INVALID_LIST_POSITION {
                self.imp()
                    .folder_list
                    .scroll_to(found, gtk::ListScrollFlags::NONE, None);
            }
        }
    }

    /// Show the documents, or the page that asks for a file once there is none.
    fn sync_content(&self) {
        let imp = self.imp();
        imp.content_stack
            .set_visible_child_name(if imp.tab_view.n_pages() == 0 {
                "no-document"
            } else {
                "documents"
            });
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
            // The tooltip is markup, and a path can hold markup characters.
            document
                .bind_property("path", page, "tooltip")
                .transform_to(|_, path: String| Some(glib::markup_escape_text(&path)))
                .sync_create()
                .build(),
        ];
        let sync = glib::clone!(
            #[weak(rename_to = win)]
            self,
            move |document: &BlinkDocument| {
                win.update_tab_titles();
                if win.selected_document().as_ref() == Some(document) {
                    win.sync_header();
                }
            }
        );
        let handlers = vec![
            // A file saved under a new name in the folder is listed at once.
            document.connect_path_notify(glib::clone!(
                #[weak(rename_to = win)]
                self,
                move |_| win.scan_folder()
            )),
            document.connect_title_notify(sync.clone()),
            document.connect_folder_notify(sync.clone()),
            document.connect_view_mode_name_notify(sync),
        ];
        self.imp()
            .attachments
            .borrow_mut()
            .insert(page.clone(), Attachment { bindings, handlers });
        self.update_tab_titles();
    }

    /// Give each tab the title of its document, and the name of the folder of its file as
    /// well while another tab of the window holds a file of the same name.
    fn update_tab_titles(&self) {
        let tab_view = &self.imp().tab_view;
        let pages: Vec<(adw::TabPage, BlinkDocument)> = (0..tab_view.n_pages())
            .map(|i| tab_view.nth_page(i))
            .filter_map(|page| {
                page.child()
                    .downcast()
                    .ok()
                    .map(|document| (page, document))
            })
            .collect();
        let file_name = |document: &BlinkDocument| {
            Path::new(&document.path())
                .file_name()
                .map(|name| name.to_owned())
        };
        for (page, document) in &pages {
            let name = file_name(document);
            let shared = name.is_some()
                && pages
                    .iter()
                    .filter(|(_, other)| file_name(other) == name)
                    .count()
                    > 1;
            let title = if shared {
                let folder = document.folder();
                let folder_name = Path::new(&folder)
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or(folder);
                // Translators: the title of a tab, then the name of the folder its file is in,
                // shown when two tabs have files of the same name.
                gettext("{} — {}")
                    .replacen("{}", &document.title(), 1)
                    .replacen("{}", &folder_name, 1)
            } else {
                document.title()
            };
            page.set_title(&title);
        }
    }

    /// Show the selected document's title, folder and view in the header bar, and its
    /// file in the sidebar. Without a document, the window is named after its folder.
    fn sync_header(&self) {
        let imp = self.imp();
        let document = self.selected_document();
        for action in DOCUMENT_ACTIONS {
            self.action_set_enabled(action, document.is_some());
        }
        imp.view_toggles.set_sensitive(document.is_some());
        imp.view_bar.set_sensitive(document.is_some());
        imp.width_button.set_sensitive(document.is_some());
        self.select_folder_row();
        let Some(document) = document else {
            let name = self
                .folder()
                .map(|folder| crate::document::file_title(&folder));
            imp.window_title.set_title("");
            imp.window_title.set_subtitle("");
            self.set_title(name.as_deref());
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
        // Before the editor and the preview, whose text views take a drop of files as well
        // and do nothing with it in the preview. A drag of text within the editor offers no
        // file list, so it still reaches the editor.
        target.set_propagation_phase(gtk::PropagationPhase::Capture);
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

fn entry_store(entries: &[folder::Entry]) -> gio::ListStore {
    let store = gio::ListStore::new::<glib::BoxedAnyObject>();
    for entry in entries {
        store.append(&glib::BoxedAnyObject::new(entry.clone()));
    }
    store
}

fn entry_path(row: &gtk::TreeListRow) -> Option<PathBuf> {
    let entry = row.item().and_downcast::<glib::BoxedAnyObject>()?;
    Some(entry.borrow::<folder::Entry>().path.clone())
}
