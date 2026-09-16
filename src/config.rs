//! Build-time constants, the settings and the values they take.

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

/// The widths the editor and the preview can be limited to, in pixels.
pub const CONTENT_WIDTHS: [i32; 4] = [700, 800, 900, 1000];

/// A content width as it is shown in menus.
pub fn content_width_label(width: i32) -> String {
    // Translators: a width in pixels
    gettextrs::gettext("{} px").replacen("{}", &width.to_string(), 1)
}

/// The application's settings. Aborts when the schema is not installed; `just run` points
/// GSettings at a compiled copy for running from the source tree.
pub fn settings() -> gio::Settings {
    gio::Settings::new(APP_ID)
}

#[cfg(test)]
mod tests {
    use super::{APP_ID, CONTENT_WIDTHS};
    use gtk::gio;

    /// The widths offered are the range the schema accepts, and its default is one of them.
    #[test]
    fn content_widths_match_the_schema() {
        let source = gio::SettingsSchemaSource::from_directory(
            concat!(env!("OUT_DIR"), "/schemas"),
            None,
            false,
        )
        .expect("schema compiled by build.rs");
        let key = source
            .lookup(APP_ID, false)
            .expect("the application's schema")
            .key("content-width");
        let range = key.range();
        let (kind, bounds) = (range.child_value(0), range.child_value(1).child_value(0));
        assert_eq!(kind.str(), Some("range"));
        assert_eq!(
            (
                bounds.child_value(0).get::<i32>(),
                bounds.child_value(1).get::<i32>()
            ),
            (
                CONTENT_WIDTHS.first().copied(),
                CONTENT_WIDTHS.last().copied()
            )
        );
        let default = key
            .default_value()
            .get::<i32>()
            .expect("an integer default");
        assert!(CONTENT_WIDTHS.contains(&default));
    }
}
