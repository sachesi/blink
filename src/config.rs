//! Build-time constants, the settings and the values they take.

use gtk::gio;
use gtk::prelude::*;

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

/// The font setting of text, or of monospace text.
pub fn font_key(monospace: bool) -> &'static str {
    if monospace {
        "monospace-font"
    } else {
        "text-font"
    }
}

/// The family of the system's document font, or of its monospace font. A family that is
/// not installed, as the Adwaita fonts often are not outside GNOME, gives way to the
/// generic one, which Fontconfig always resolves; text would otherwise fall back to a
/// proportional font, code included.
pub fn system_font_family(monospace: bool) -> String {
    let style_manager = adw::StyleManager::default();
    let name = if monospace {
        style_manager.monospace_font_name()
    } else {
        style_manager.document_font_name()
    };
    gtk::pango::FontDescription::from_string(&name)
        .family()
        .filter(|family| is_installed(family))
        .map_or_else(
            || String::from(if monospace { "Monospace" } else { "Sans" }),
            |family| family.to_string(),
        )
}

/// The font family of text, or of monospace text: the one picked in Preferences, or else
/// the system's.
pub fn font_family(settings: &gio::Settings, monospace: bool) -> String {
    picked_font_family(&settings.string(font_key(monospace)), monospace)
}

/// `picked`, a family stored in the settings, or the system's while none is picked or the
/// one picked is no longer installed.
pub fn picked_font_family(picked: &str, monospace: bool) -> String {
    if picked.is_empty() || !is_installed(picked) {
        system_font_family(monospace)
    } else {
        picked.to_owned()
    }
}

fn is_installed(family: &str) -> bool {
    pangocairo::FontMap::default()
        .list_families()
        .iter()
        .any(|installed| installed.name().eq_ignore_ascii_case(family))
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
