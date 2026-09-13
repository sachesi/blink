//! The application: one window, the app.* actions, the accelerators and the colour scheme.

use adw::prelude::*;
use adw::subclass::prelude::*;
use gettextrs::gettext;
use gtk::{gio, glib};

use crate::config;
use crate::preferences::BlinkPreferencesDialog;
use crate::window::BlinkWindow;

/// Keyboard accelerators, set at startup. The shortcuts dialog lists the same keys; a test
/// keeps the two in step. `app.shortcuts` is missing because libadwaita binds it itself.
pub const ACCELS: &[(&str, &[&str])] = &[
    ("win.new", &["<Control>n"]),
    ("win.open", &["<Control>o"]),
    ("win.save", &["<Control>s"]),
    ("win.save-as", &["<Control><Shift>s"]),
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
        pub settings: std::cell::OnceCell<gio::Settings>,
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
            app.load_style();
            app.setup_actions();
            app.follow_color_scheme();
        }

        fn activate(&self) {
            self.parent_activate();
            self.obj().window().present();
        }

        /// Files from the command line, the desktop entry, or a second launch while this
        /// one runs: the first of them opens in the window, behind the unsaved-changes
        /// question.
        fn open(&self, files: &[gio::File], _hint: &str) {
            let window = self.obj().window();
            if let Some(file) = files.first() {
                window.open_file(file.clone());
            }
            window.present();
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

    /// The window, made on first use.
    fn window(&self) -> BlinkWindow {
        self.windows()
            .into_iter()
            .find_map(|window| window.downcast::<BlinkWindow>().ok())
            .unwrap_or_else(|| BlinkWindow::new(self))
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
                BlinkPreferencesDialog::new().present(app.active_window().as_ref());
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
        let settings = config::settings();
        let apply = |settings: &gio::Settings| {
            let scheme = match settings.string("color-scheme").as_str() {
                "light" => adw::ColorScheme::ForceLight,
                "dark" => adw::ColorScheme::ForceDark,
                _ => adw::ColorScheme::Default,
            };
            adw::StyleManager::default().set_color_scheme(scheme);
        };
        apply(&settings);
        settings.connect_changed(Some("color-scheme"), move |settings, _| apply(settings));
        self.imp().settings.set(settings).ok();
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
