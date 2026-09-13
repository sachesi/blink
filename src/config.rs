//! Build-time constants and the settings.

use gtk::gio;

pub const APP_ID: &str = "io.github.sachesi.blink";
pub const RESOURCE_PATH: &str = "/io/github/sachesi/blink";
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
pub const GETTEXT_PACKAGE: &str = "blink";
/// Where the compiled catalogues are installed; `just build` passes the prefix it will
/// install to.
pub const LOCALEDIR: &str = match option_env!("BLINK_LOCALEDIR") {
    Some(dir) => dir,
    None => "/usr/local/share/locale",
};

/// The application's settings. Aborts when the schema is not installed; `just run` points
/// GSettings at a compiled copy for running from the source tree.
pub fn settings() -> gio::Settings {
    gio::Settings::new(APP_ID)
}
