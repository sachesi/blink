mod application;
mod backup;
mod config;
mod conflict;
mod editor_view;
mod export;
mod markdown;
mod preferences;
mod window;

use adw::prelude::*;
use gettextrs::{LocaleCategory, bind_textdomain_codeset, bindtextdomain, setlocale, textdomain};
use gtk::{gio, glib};

fn main() -> glib::ExitCode {
    // SAFETY: the first thing the program does; no thread has been started that could be
    // reading the environment or the locale.
    unsafe { setlocale(LocaleCategory::LcAll, "") };
    bindtextdomain(config::GETTEXT_PACKAGE, config::LOCALEDIR).ok();
    // GTK requires UTF-8 strings regardless of the locale's own charset.
    bind_textdomain_codeset(config::GETTEXT_PACKAGE, "UTF-8").ok();
    textdomain(config::GETTEXT_PACKAGE).ok();

    gio::resources_register_include!("blink.gresource").expect("resources bundled at build time");
    glib::set_application_name("Blink");

    application::BlinkApplication::new().run()
}
