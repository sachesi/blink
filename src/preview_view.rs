//! The rendered text of a document, which draws the boxes of its blockquotes behind the text.

use adw::subclass::prelude::*;
use gtk::prelude::*;
use gtk::{gdk, gio, glib, graphene, gsk};
use std::cell::RefCell;

use crate::markdown::{self, Quote};

/// How rounded the corners of a quote's box are, as those of a code block's.
const CORNER_RADIUS: f32 = 12.0;
/// How far a quote's box reaches above its first and below its last line.
const VERTICAL_PADDING: f32 = 6.0;
/// How wide the bar at the left of an alert's box is.
const ALERT_BAR_WIDTH: f32 = 4.0;
/// Opacity of the text colour a quote's box is tinted with.
const TINT_ALPHA: f32 = 0.05;

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct BlinkPreviewView {
        pub quotes: RefCell<Vec<Quote>>,
        /// The menu of a code block or table cell, which cannot show a menu of its own.
        pub menu: RefCell<Option<gtk::PopoverMenu>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for BlinkPreviewView {
        const NAME: &'static str = "BlinkPreviewView";
        type Type = super::BlinkPreviewView;
        type ParentType = gtk::TextView;
    }

    impl ObjectImpl for BlinkPreviewView {
        fn dispose(&self) {
            if let Some(menu) = self.menu.take() {
                menu.unparent();
            }
        }
    }

    impl WidgetImpl for BlinkPreviewView {
        // A popover is placed by the widget it belongs to when that widget is allocated,
        // as a text view places its own menu.
        fn size_allocate(&self, width: i32, height: i32, baseline: i32) {
            self.parent_size_allocate(width, height, baseline);
            if let Some(menu) = self.menu.borrow().as_ref() {
                menu.present();
            }
        }
    }

    impl TextViewImpl for BlinkPreviewView {
        fn snapshot_layer(&self, layer: gtk::TextViewLayer, snapshot: gtk::Snapshot) {
            if layer == gtk::TextViewLayer::BelowText {
                self.draw_quotes(&snapshot);
            }
            self.parent_snapshot_layer(layer, snapshot);
        }
    }

    impl BlinkPreviewView {
        fn draw_quotes(&self, snapshot: &gtk::Snapshot) {
            let view = self.obj();
            let buffer = view.buffer();
            let visible = view.visible_rect();
            let width = view.width();
            let dark = adw::StyleManager::default().is_dark();
            let mut tint = view.color();
            tint.set_alpha(tint.alpha() * TINT_ALPHA);
            for quote in self.quotes.borrow().iter() {
                if quote.end <= quote.start {
                    continue;
                }
                let (top, _) = view.line_yrange(&buffer.iter_at_offset(quote.start));
                let (last, height) = view.line_yrange(&buffer.iter_at_offset(quote.end - 1));
                let bottom = last + height;
                if bottom < visible.y() || top > visible.y() + visible.height() {
                    continue;
                }
                let bounds = graphene::Rect::new(
                    quote.left as f32,
                    top as f32 - VERTICAL_PADDING,
                    (width - quote.left - quote.right).max(0) as f32,
                    (bottom - top) as f32 + 2.0 * VERTICAL_PADDING,
                );
                let rounded = gsk::RoundedRect::from_rect(bounds, CORNER_RADIUS);
                snapshot.push_rounded_clip(&rounded);
                snapshot.append_color(&tint, &bounds);
                if let Some(kind) = quote.alert {
                    snapshot.append_color(
                        &markdown::alert_color(kind, dark),
                        &graphene::Rect::new(
                            bounds.x(),
                            bounds.y(),
                            ALERT_BAR_WIDTH,
                            bounds.height(),
                        ),
                    );
                }
                snapshot.pop();
            }
        }
    }
}

glib::wrapper! {
    pub struct BlinkPreviewView(ObjectSubclass<imp::BlinkPreviewView>)
        @extends gtk::TextView, gtk::Widget,
        @implements gtk::Accessible, gtk::AccessibleText, gtk::Buildable, gtk::ConstraintTarget, gtk::Scrollable;
}

impl BlinkPreviewView {
    /// Show `model` as a menu pointing at `(x, y)` in the view.
    ///
    /// The menu of a widget in the preview's text gets no height and closes at once, while
    /// one of the view itself shows.
    pub fn popup_menu(&self, model: &gio::MenuModel, x: f64, y: f64) {
        let menu = self
            .imp()
            .menu
            .borrow_mut()
            .get_or_insert_with(|| {
                let menu = gtk::PopoverMenu::from_model(None::<&gio::MenuModel>);
                menu.set_parent(self);
                menu.set_position(gtk::PositionType::Bottom);
                menu.set_has_arrow(false);
                menu.set_halign(gtk::Align::Start);
                menu
            })
            .clone();
        menu.set_menu_model(Some(model));
        menu.set_pointing_to(Some(&gdk::Rectangle::new(x as i32, y as i32, 1, 1)));
        menu.popup();
    }

    /// Draw the boxes of `quotes` from now on.
    pub fn set_quotes(&self, quotes: Vec<Quote>) {
        self.imp().quotes.replace(quotes);
        self.queue_draw();
    }
}
