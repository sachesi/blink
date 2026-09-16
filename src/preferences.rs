//! Preferences, bound straight to the settings: a change applies at once.

use adw::prelude::*;
use adw::subclass::prelude::*;
use gtk::{gio, glib, pango};
use std::cell::OnceCell;

use crate::config;
use crate::window::BlinkWindow;

/// The `color-scheme` nicks, in the order of the style row's list.
const SCHEMES: [&str; 3] = ["system", "light", "dark"];

mod imp {
    use super::*;

    #[derive(Default, gtk::CompositeTemplate)]
    #[template(resource = "/io/github/sachesi/blink/ui/preferences_dialog.ui")]
    pub struct BlinkPreferencesDialog {
        #[template_child]
        pub style_row: TemplateChild<adw::ComboRow>,
        #[template_child]
        pub width_row: TemplateChild<adw::ComboRow>,
        #[template_child]
        pub text_font_button: TemplateChild<gtk::FontDialogButton>,
        #[template_child]
        pub text_font_reset: TemplateChild<gtk::Button>,
        #[template_child]
        pub monospace_font_button: TemplateChild<gtk::FontDialogButton>,
        #[template_child]
        pub monospace_font_reset: TemplateChild<gtk::Button>,
        #[template_child]
        pub tabs_row: TemplateChild<adw::SwitchRow>,
        #[template_child]
        pub wrap_row: TemplateChild<adw::SwitchRow>,
        #[template_child]
        pub line_numbers_row: TemplateChild<adw::SwitchRow>,
        #[template_child]
        pub current_line_row: TemplateChild<adw::SwitchRow>,
        #[template_child]
        pub alternate_lines_row: TemplateChild<adw::SwitchRow>,
        #[template_child]
        pub tab_width_row: TemplateChild<adw::SpinRow>,

        /// The window the dialog is for, whose width it shows and sets.
        pub window: glib::WeakRef<BlinkWindow>,
        pub width_handler: OnceCell<glib::SignalHandlerId>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for BlinkPreferencesDialog {
        const NAME: &'static str = "BlinkPreferencesDialog";
        type Type = super::BlinkPreferencesDialog;
        type ParentType = adw::PreferencesDialog;

        fn class_init(klass: &mut Self::Class) {
            klass.bind_template();
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    impl ObjectImpl for BlinkPreferencesDialog {
        fn constructed(&self) {
            self.parent_constructed();
            let settings = config::settings();
            settings
                .bind("color-scheme", &*self.style_row, "selected")
                .mapping(|value, _| {
                    let nick = value.str()?;
                    let index = SCHEMES.iter().position(|scheme| *scheme == nick)?;
                    Some((index as u32).to_value())
                })
                .set_mapping(|value, _| {
                    let index = value.get::<u32>().ok()?;
                    SCHEMES.get(index as usize).map(|nick| nick.to_variant())
                })
                .build();
            let widths: Vec<String> = config::CONTENT_WIDTHS
                .into_iter()
                .map(config::content_width_label)
                .collect();
            let widths: Vec<&str> = widths.iter().map(String::as_str).collect();
            self.width_row
                .set_model(Some(&gtk::StringList::new(&widths)));
            self.obj().show_content_width(settings.int("content-width"));
            // Picking a width sets it for new windows and for the window the dialog is on.
            // A settings binding would miss the second whenever the window had its own
            // width and the setting was picked again, which changes nothing there.
            let handler = self.width_row.connect_selected_notify(glib::clone!(
                #[weak(rename_to = imp)]
                self,
                #[strong]
                settings,
                move |row| {
                    let Some(width) = config::CONTENT_WIDTHS.get(row.selected() as usize) else {
                        return;
                    };
                    let _ = settings.set_int("content-width", *width);
                    if let Some(window) = imp.window.upgrade() {
                        window.set_content_width(*width);
                    }
                }
            ));
            self.width_handler.set(handler).ok();
            bind_font(
                &settings,
                false,
                &self.text_font_button,
                &self.text_font_reset,
            );
            bind_font(
                &settings,
                true,
                &self.monospace_font_button,
                &self.monospace_font_reset,
            );
            if let Some(dialog) = self.monospace_font_button.dialog() {
                dialog.set_filter(Some(&gtk::CustomFilter::new(|item| {
                    let family = item
                        .downcast_ref::<pango::FontFamily>()
                        .cloned()
                        .or_else(|| {
                            item.downcast_ref::<pango::FontFace>()
                                .map(pango::FontFace::family)
                        });
                    family.is_none_or(|family| family.is_monospace())
                })));
            }
            settings
                .bind("open-in-tabs", &*self.tabs_row, "active")
                .build();
            settings
                .bind("wrap-text", &*self.wrap_row, "active")
                .build();
            settings
                .bind("show-line-numbers", &*self.line_numbers_row, "active")
                .build();
            settings
                .bind("highlight-current-line", &*self.current_line_row, "active")
                .build();
            settings
                .bind(
                    "shade-alternate-lines",
                    &*self.alternate_lines_row,
                    "active",
                )
                .build();
            settings
                .bind("tab-width", &*self.tab_width_row, "value")
                .build();
        }
    }

    impl WidgetImpl for BlinkPreferencesDialog {}
    impl AdwDialogImpl for BlinkPreferencesDialog {}
    impl PreferencesDialogImpl for BlinkPreferencesDialog {}
}

/// Show the font family of text, or of monospace text, on `button`, the system's while
/// none is picked, and store the family picked there. `reset` goes back to the system's.
fn bind_font(
    settings: &gio::Settings,
    monospace: bool,
    button: &gtk::FontDialogButton,
    reset: &gtk::Button,
) {
    let key = config::font_key(monospace);
    settings
        .bind(key, button, "font-desc")
        .mapping(move |value, _| {
            let mut family = value.str()?.to_owned();
            if family.is_empty() {
                family = config::system_font_family(monospace);
            }
            let mut description = pango::FontDescription::new();
            description.set_family(&family);
            Some(description.to_value())
        })
        .set_mapping(|value, _| {
            let description = value.get::<pango::FontDescription>().ok()?;
            Some(description.family()?.to_variant())
        })
        .build();
    settings
        .bind(key, reset, "visible")
        .mapping(|value, _| {
            Some(
                value
                    .str()
                    .is_some_and(|family| !family.is_empty())
                    .to_value(),
            )
        })
        .get_only()
        .build();
    reset.connect_clicked(glib::clone!(
        #[strong]
        settings,
        move |_| settings.reset(key)
    ));
}

glib::wrapper! {
    pub struct BlinkPreferencesDialog(ObjectSubclass<imp::BlinkPreferencesDialog>)
        @extends adw::PreferencesDialog, adw::Dialog, gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget, gtk::ShortcutManager;
}

impl BlinkPreferencesDialog {
    /// Preferences for `window`, showing its content width.
    pub fn new(window: Option<&BlinkWindow>) -> Self {
        let dialog: Self = glib::Object::new();
        let imp = dialog.imp();
        if let Some(window) = window {
            imp.window.set(Some(window));
            if let Some(handler) = imp.width_handler.get() {
                imp.width_row.block_signal(handler);
                dialog.show_content_width(window.content_width());
                imp.width_row.unblock_signal(handler);
            }
        }
        dialog
    }

    fn show_content_width(&self, width: i32) {
        if let Some(index) = config::CONTENT_WIDTHS.iter().position(|w| *w == width) {
            self.imp().width_row.set_selected(index as u32);
        }
    }
}
