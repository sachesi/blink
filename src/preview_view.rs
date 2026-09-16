//! The rendered text of a document, which draws the boxes of its blockquotes behind the text.

use adw::subclass::prelude::*;
use gtk::prelude::*;
use gtk::{glib, graphene, gsk};
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
    }

    #[glib::object_subclass]
    impl ObjectSubclass for BlinkPreviewView {
        const NAME: &'static str = "BlinkPreviewView";
        type Type = super::BlinkPreviewView;
        type ParentType = gtk::TextView;
    }

    impl ObjectImpl for BlinkPreviewView {}

    impl WidgetImpl for BlinkPreviewView {}

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
    /// Draw the boxes of `quotes` from now on.
    pub fn set_quotes(&self, quotes: Vec<Quote>) {
        self.imp().quotes.replace(quotes);
        self.queue_draw();
    }
}
