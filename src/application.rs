//! The application: its windows, where new and opened documents go, the app.* actions, the
//! accelerators, the colour scheme and the fonts, and the backups a previous session left.

use adw::prelude::*;
use adw::subclass::prelude::*;
use gettextrs::gettext;
use gtk::{gio, glib};
use std::cell::{Cell, OnceCell};

use crate::backup::{self, BackupRecord};
use crate::config;
use crate::document::BlinkDocument;
use crate::preferences::BlinkPreferencesDialog;
use crate::window::BlinkWindow;

/// Keyboard accelerators, set at startup. The shortcuts dialog lists the same keys; a test
/// keeps the two in step. `app.shortcuts` is missing because libadwaita binds it itself.
pub const ACCELS: &[(&str, &[&str])] = &[
    ("win.new", &["<Control>n"]),
    ("win.open", &["<Control>o"]),
    ("win.save", &["<Control>s"]),
    ("win.save-as", &["<Control><Shift>s"]),
    ("win.close-document", &["<Control>w"]),
    ("win.find", &["<Control>f"]),
    ("win.find-next", &["<Control>g"]),
    ("win.find-previous", &["<Control><Shift>g"]),
    ("win.replace", &["<Control>h"]),
    ("win.replace-all", &["<Control><Shift>h"]),
    ("win.format-bold", &["<Control>b"]),
    ("win.format-italic", &["<Control>i"]),
    ("win.format-link", &["<Control>k"]),
    ("win.zoom-in", &["<Control>plus", "<Control>equal"]),
    ("win.zoom-out", &["<Control>minus"]),
    ("win.zoom-reset", &["<Control>0"]),
    ("win.focus-mode", &["F11"]),
    ("app.preferences", &["<Control>comma"]),
    ("app.quit", &["<Control>q"]),
];

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct BlinkApplication {
        pub settings: OnceCell<gio::Settings>,
        /// The fonts and the zoom, as CSS that changes with the settings.
        pub font_css: OnceCell<gtk::CssProvider>,
        /// Backups left by a previous session were looked for.
        pub recovery_checked: Cell<bool>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for BlinkApplication {
        const NAME: &'static str = "BlinkApplication";
        type Type = super::BlinkApplication;
        type ParentType = adw::Application;
    }

    impl ObjectImpl for BlinkApplication {}

    impl ApplicationImpl for BlinkApplication {
        fn startup(&self) {
            self.parent_startup();
            sourceview5::init();
            let app = self.obj();
            self.settings.set(config::settings()).ok();
            app.load_style();
            app.setup_actions();
            app.follow_color_scheme();
            app.follow_fonts();
            // The lock marks this run as alive, so its backups are not offered to another.
            let _ = backup::create_lock(&backup::locks_dir(), std::process::id());
        }

        fn shutdown(&self) {
            backup::remove_lock(&backup::locks_dir(), std::process::id());
            self.parent_shutdown();
        }

        fn activate(&self) {
            self.parent_activate();
            let app = self.obj();
            match app.active_window() {
                Some(window) => window.present(),
                None => {
                    app.new_document(None);
                }
            }
            app.check_recovery();
        }

        /// Files from the command line, the desktop entry, or a second launch while this
        /// one runs.
        fn open(&self, files: &[gio::File], _hint: &str) {
            let app = self.obj();
            let mut window = app.active_window().and_downcast::<BlinkWindow>();
            for file in files {
                let document = app.open_file(file.clone(), window.as_ref());
                // The files after the first go where it went, not each to a window of its
                // own.
                window = window.or_else(|| document.window());
            }
            app.check_recovery();
        }
    }

    impl GtkApplicationImpl for BlinkApplication {}
    impl AdwApplicationImpl for BlinkApplication {}
}

glib::wrapper! {
    pub struct BlinkApplication(ObjectSubclass<imp::BlinkApplication>)
        @extends adw::Application, gtk::Application, gio::Application,
        @implements gio::ActionGroup, gio::ActionMap;
}

impl BlinkApplication {
    pub fn new() -> Self {
        glib::Object::builder()
            .property("application-id", config::APP_ID)
            .property("flags", gio::ApplicationFlags::HANDLES_OPEN)
            .property("resource-base-path", config::RESOURCE_PATH)
            .build()
    }

    fn settings(&self) -> &gio::Settings {
        self.imp().settings.get().expect("settings set at startup")
    }

    pub fn documents(&self) -> impl Iterator<Item = BlinkDocument> {
        self.windows()
            .into_iter()
            .filter_map(|window| window.downcast::<BlinkWindow>().ok())
            .flat_map(|window| window.documents())
    }

    /// A new, untitled document: a tab of `window` when documents open in tabs, otherwise
    /// in a window of its own.
    pub fn new_document(&self, window: Option<&BlinkWindow>) -> BlinkDocument {
        let document = BlinkDocument::new();
        let window = match window {
            Some(window) if self.settings().boolean("open-in-tabs") => window.clone(),
            _ => BlinkWindow::new(self),
        };
        window.add_document(&document);
        window.present();
        document
    }

    /// Open `file` from `window`: where it is open already, otherwise in the selected
    /// document of the window if that is blank, otherwise as a new document. Returns the
    /// document it is in.
    pub fn open_file(&self, file: gio::File, window: Option<&BlinkWindow>) -> BlinkDocument {
        // Two documents saving to one file would each take the other's writes for a change
        // made by another program.
        if let Some(document) = self.documents().find(|document| document.holds(&file)) {
            document.present();
            return document;
        }
        let (document, made) = self.blank_document(window);
        document.load(file, made);
        document.present();
        document
    }

    /// The selected document of `window` when it is blank, or else a new document, and
    /// whether it is new.
    fn blank_document(&self, window: Option<&BlinkWindow>) -> (BlinkDocument, bool) {
        match window
            .and_then(BlinkWindow::selected_document)
            .filter(BlinkDocument::is_blank)
        {
            Some(document) => (document, false),
            None => (self.new_document(window), true),
        }
    }

    fn load_style(&self) {
        let css = gtk::CssProvider::new();
        css.load_from_resource(&format!("{}/style.css", config::RESOURCE_PATH));
        if let Some(display) = gtk::gdk::Display::default() {
            gtk::style_context_add_provider_for_display(
                &display,
                &css,
                gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
            );
        }
    }

    fn setup_actions(&self) {
        let quit = gio::ActionEntry::builder("quit")
            // Through each window's close request, so unsaved changes are asked about.
            .activate(|app: &Self, _, _| {
                for window in app.windows() {
                    window.close();
                }
            })
            .build();
        let about = gio::ActionEntry::builder("about")
            .activate(|app: &Self, _, _| app.show_about())
            .build();
        let preferences = gio::ActionEntry::builder("preferences")
            .activate(|app: &Self, _, _| {
                let window = app.active_window().and_downcast::<BlinkWindow>();
                BlinkPreferencesDialog::new(window.as_ref()).present(window.as_ref());
            })
            .build();
        self.add_action_entries([quit, about, preferences]);
        for (action, accels) in ACCELS {
            self.set_accels_for_action(action, accels);
        }
    }

    /// Apply the style chosen in Preferences now and whenever it changes. "System" follows
    /// the desktop; the other two override it.
    fn follow_color_scheme(&self) {
        let apply = |settings: &gio::Settings| {
            let scheme = match settings.string("color-scheme").as_str() {
                "light" => adw::ColorScheme::ForceLight,
                "dark" => adw::ColorScheme::ForceDark,
                _ => adw::ColorScheme::Default,
            };
            adw::StyleManager::default().set_color_scheme(scheme);
        };
        let settings = self.settings();
        apply(settings);
        settings.connect_changed(Some("color-scheme"), move |settings, _| apply(settings));
    }

    /// Give the editors and the previews the fonts and the text size in the settings, and
    /// the system's fonts while none is picked. A provider of its own carries them, so they
    /// change without touching the stylesheet.
    fn follow_fonts(&self) {
        let css = gtk::CssProvider::new();
        if let Some(display) = gtk::gdk::Display::default() {
            gtk::style_context_add_provider_for_display(
                &display,
                &css,
                gtk::STYLE_PROVIDER_PRIORITY_APPLICATION + 1,
            );
        }
        self.imp().font_css.set(css).ok();
        self.apply_font_css();
        let settings = self.settings();
        for key in ["zoom", "text-font", "monospace-font"] {
            settings.connect_changed(
                Some(key),
                glib::clone!(
                    #[weak(rename_to = app)]
                    self,
                    move |_, _| app.apply_font_css()
                ),
            );
        }
        let style_manager = adw::StyleManager::default();
        style_manager.connect_document_font_name_notify(glib::clone!(
            #[weak(rename_to = app)]
            self,
            move |_| app.apply_font_css()
        ));
        style_manager.connect_monospace_font_name_notify(glib::clone!(
            #[weak(rename_to = app)]
            self,
            move |_| app.apply_font_css()
        ));
    }

    fn apply_font_css(&self) {
        let settings = self.settings();
        let size = (11 + settings.int("zoom")).clamp(6, 32);
        let text = css_string(&config::font_family(settings, false));
        let monospace = css_string(&config::font_family(settings, true));
        let css = format!(
            "textview.editor-view {{ font-size: {size}pt; }}\n\
             textview.transparent-bg {{ font-size: {size}pt; }}\n\
             textview.preview-view {{ font-family: {text}; }}\n\
             textview.editor-view, textview.code-view {{ font-family: {monospace}; }}"
        );
        if let Some(provider) = self.imp().font_css.get() {
            provider.load_from_string(&css);
        }
    }

    /// Offer back, once per run, the unsaved changes a session that ended without saving
    /// them left in backups.
    fn check_recovery(&self) {
        if self.imp().recovery_checked.replace(true) {
            return;
        }
        glib::spawn_future_local(glib::clone!(
            #[weak(rename_to = app)]
            self,
            async move {
                let pid = std::process::id();
                let orphans = gio::spawn_blocking(move || {
                    let locks_dir = backup::locks_dir();
                    backup::list_records(&backup::backups_dir()).map(|records| {
                        records
                            .into_iter()
                            .filter(|record| {
                                let lock_present =
                                    backup::lock_present(&locks_dir, record.owner_pid);
                                let alive = backup::is_pid_alive(record.owner_pid);
                                backup::classify(record.owner_pid, pid, lock_present, alive)
                                    == backup::OrphanClass::Orphan
                            })
                            .collect::<Vec<_>>()
                    })
                })
                .await;
                for record in orphans.ok().and_then(Result::ok).unwrap_or_default() {
                    app.offer_recovery(record).await;
                }
            }
        ));
    }

    async fn offer_recovery(&self, record: BackupRecord) {
        let window = self.active_window().and_downcast::<BlinkWindow>();
        let body = gettext("Unsaved changes to \"{}\" were found from a previous session.")
            .replacen("{}", &record.display_name, 1);
        let alert = adw::AlertDialog::builder()
            .heading(gettext("Recover Unsaved Document?"))
            .body(body)
            .build();
        alert.add_response("discard", &gettext("Discard"));
        alert.add_response("restore", &gettext("Restore"));
        alert.set_response_appearance("discard", adw::ResponseAppearance::Destructive);
        alert.set_response_appearance("restore", adw::ResponseAppearance::Suggested);
        alert.set_default_response(Some("restore"));
        // Dismissing the dialog keeps the backup for next time rather than deleting it.
        alert.set_close_response("keep");
        match alert.choose_future(window.as_ref()).await.as_str() {
            "restore" => {
                // Into the document of the file when it is open already, so the file is not
                // open twice.
                let path = record.original_path.clone();
                let canonical = gio::spawn_blocking(move || {
                    path.and_then(|path| std::fs::canonicalize(path).ok())
                })
                .await
                .ok()
                .flatten();
                let file = record.original_path.as_deref().map(gio::File::for_path);
                let open = self
                    .documents()
                    .find(|document| document.holds_either(file.as_ref(), canonical.as_deref()));
                let document = open.unwrap_or_else(|| self.blank_document(window.as_ref()).0);
                document.restore(record);
                document.present();
            }
            "discard" => {
                let id = record.backup_id;
                gio::spawn_blocking(move || backup::delete_backup(&backup::backups_dir(), &id))
                    .await
                    .ok();
            }
            _ => {}
        }
    }

    fn show_about(&self) {
        let about = adw::AboutDialog::builder()
            .application_name("Blink")
            .application_icon(config::APP_ID)
            .developer_name("sachesi")
            .version(config::VERSION)
            .comments(gettext("Edit Markdown with a live preview"))
            .website("https://github.com/sachesi/blink")
            .issue_url("https://github.com/sachesi/blink/issues")
            .license_type(gtk::License::Gpl30)
            // Translators: put your name here, one per line, optionally with an email address.
            .translator_credits(gettext("translator-credits"))
            .build();
        about.present(self.active_window().as_ref());
    }
}

/// `text` as a quoted CSS string.
fn css_string(text: &str) -> String {
    format!("\"{}\"", text.replace('\\', "\\\\").replace('"', "\\\""))
}

#[cfg(test)]
mod tests {
    use super::ACCELS;

    /// Every accelerator the application sets is listed in the shortcuts dialog.
    #[test]
    fn shortcuts_dialog_lists_every_accelerator() {
        let ui = std::fs::read_to_string(concat!(
            env!("OUT_DIR"),
            "/resources/ui/shortcuts_dialog.ui"
        ))
        .expect("shortcuts dialog compiled by build.rs");
        let listed: Vec<String> = ui
            .split("<property name=\"accelerator\">")
            .skip(1)
            .filter_map(|rest| rest.split('<').next())
            .flat_map(|value| {
                value
                    .replace("&lt;", "<")
                    .replace("&gt;", ">")
                    .split_whitespace()
                    .map(str::to_owned)
                    .collect::<Vec<_>>()
            })
            .collect();
        for (action, accels) in ACCELS {
            for accel in *accels {
                assert!(
                    listed.iter().any(|l| l == accel),
                    "{action}: {accel} missing from the shortcuts dialog"
                );
            }
        }
    }
}
