//! The source editor: a GtkSourceView that can shade every other line.

use adw::subclass::prelude::*;
use gtk::prelude::*;
use gtk::{glib, graphene};
use sourceview5::prelude::*;
use sourceview5::subclass::prelude::*;
use std::cell::Cell;

/// Opacity of the text colour over the shaded lines.
const SHADE_ALPHA: f32 = 0.035;
/// Opacity of the text colour added over the current line, whose highlight in the dark
/// style scheme is about as faint as the shading.
const CURRENT_LINE_ALPHA: f32 = 0.04;

mod imp {
    use super::*;

    #[derive(Default, glib::Properties)]
    #[properties(wrapper_type = super::BlinkEditorView)]
    pub struct BlinkEditorView {
        /// Shade every other line of the source, the way line numbers count them.
        #[property(get, set = Self::set_shade_alternate_lines)]
        shade_alternate_lines: Cell<bool>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for BlinkEditorView {
        const NAME: &'static str = "BlinkEditorView";
        type Type = super::BlinkEditorView;
        type ParentType = sourceview5::View;
    }

    #[glib::derived_properties]
    impl ObjectImpl for BlinkEditorView {}

    impl WidgetImpl for BlinkEditorView {}

    impl TextViewImpl for BlinkEditorView {
        fn snapshot_layer(&self, layer: gtk::TextViewLayer, snapshot: gtk::Snapshot) {
            let below = layer == gtk::TextViewLayer::BelowText;
            if below && self.shade_alternate_lines.get() {
                self.shade_lines(&snapshot);
            }
            // After the shading, so the current line's highlight is drawn over it.
            self.parent_snapshot_layer(layer, snapshot.clone());
            let view = self.obj();
            if below && self.shade_alternate_lines.get() && view.is_highlight_current_line() {
                let buffer = view.buffer();
                let cursor = buffer.iter_at_mark(&buffer.get_insert());
                self.tint_line(&snapshot, &cursor, CURRENT_LINE_ALPHA);
            }
        }
    }

    impl ViewImpl for BlinkEditorView {}

    impl BlinkEditorView {
        fn set_shade_alternate_lines(&self, shade: bool) {
            if self.shade_alternate_lines.replace(shade) != shade {
                self.obj().queue_draw();
            }
        }

        /// Shade the even-numbered lines that are in view, each across its wrapped rows.
        fn shade_lines(&self, snapshot: &gtk::Snapshot) {
            let view = self.obj();
            let visible = view.visible_rect();
            let bottom = visible.y() + visible.height();
            let (mut line, _) = view.line_at_y(visible.y());
            loop {
                if view.line_yrange(&line).0 >= bottom {
                    break;
                }
                if line.line() % 2 == 1 {
                    self.tint_line(snapshot, &line, SHADE_ALPHA);
                }
                if !line.forward_line() {
                    break;
                }
            }
        }

        /// Tint the line at `iter` across the view with the text colour at `alpha`.
        fn tint_line(&self, snapshot: &gtk::Snapshot, iter: &gtk::TextIter, alpha: f32) {
            let view = self.obj();
            let visible = view.visible_rect();
            let (y, height) = view.line_yrange(iter);
            let mut color = view.color();
            color.set_alpha(color.alpha() * alpha);
            snapshot.append_color(
                &color,
                &graphene::Rect::new(
                    visible.x() as f32,
                    y as f32,
                    visible.width() as f32,
                    height as f32,
                ),
            );
        }
    }
}

glib::wrapper! {
    pub struct BlinkEditorView(ObjectSubclass<imp::BlinkEditorView>)
        @extends sourceview5::View, gtk::TextView, gtk::Widget,
        @implements gtk::Accessible, gtk::AccessibleText, gtk::Buildable, gtk::ConstraintTarget, gtk::Scrollable;
}
