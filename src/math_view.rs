//! A typeset formula in the preview, drawn at the size and in the colour of the text around
//! it.

use adw::subclass::prelude::*;
use gtk::prelude::*;
use gtk::{glib, graphene, pango};
use std::cell::{Cell, OnceCell};
use std::rc::Rc;

use crate::math::Formula;

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct BlinkMathView {
        pub formula: OnceCell<Rc<Formula>>,
        /// Whether the formula is displayed on a line of its own, with no text to line up with.
        pub display: Cell<bool>,
        /// The size of the text the view was last measured for, in pixels.
        measured: Cell<f64>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for BlinkMathView {
        const NAME: &'static str = "BlinkMathView";
        type Type = super::BlinkMathView;
        type ParentType = gtk::Widget;
    }

    impl ObjectImpl for BlinkMathView {}

    impl WidgetImpl for BlinkMathView {
        fn measure(&self, orientation: gtk::Orientation, _for_size: i32) -> (i32, i32, i32, i32) {
            let Some(formula) = self.formula.get() else {
                return (0, 0, -1, -1);
            };
            let size = self.font_size();
            self.measured.set(size);
            let pixels = |ems: f64| (ems * size).ceil() as i32;
            match orientation {
                gtk::Orientation::Horizontal => {
                    let width = pixels(formula.width);
                    (width, width, -1, -1)
                }
                // The text view sets the bottom of a child on the baseline, so a formula in a
                // line is as tall as it is above the baseline, and the rest is drawn below it.
                _ => {
                    let height = if self.display.get() {
                        pixels(formula.ascent + formula.descent)
                    } else {
                        pixels(formula.ascent)
                    };
                    (height, height, -1, -1)
                }
            }
        }

        fn snapshot(&self, snapshot: &gtk::Snapshot) {
            let Some(formula) = self.formula.get() else {
                return;
            };
            let widget = self.obj();
            let size = self.font_size();
            let bounds = graphene::Rect::new(
                0.0,
                0.0,
                widget.width() as f32,
                ((formula.ascent + formula.descent) * size).ceil() as f32,
            );
            let cr = snapshot.append_cairo(&bounds);
            let color = widget.color();
            // The text changed size, as with the zoom, since the view was measured.
            if size != self.measured.get() {
                widget.queue_resize();
            }
            formula.draw(
                &cr,
                0.0,
                (formula.ascent * size).ceil(),
                size,
                (
                    f64::from(color.red()),
                    f64::from(color.green()),
                    f64::from(color.blue()),
                ),
            );
        }
    }

    impl BlinkMathView {
        /// The size of the text around the formula, in pixels.
        fn font_size(&self) -> f64 {
            let context = self.obj().pango_context();
            let Some(description) = context.font_description() else {
                return 16.0;
            };
            let size = f64::from(description.size()) / f64::from(pango::SCALE);
            if description.is_size_absolute() {
                size
            } else {
                let resolution = pangocairo::functions::context_get_resolution(&context);
                size * if resolution > 0.0 { resolution } else { 96.0 } / 72.0
            }
        }
    }
}

glib::wrapper! {
    pub struct BlinkMathView(ObjectSubclass<imp::BlinkMathView>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl BlinkMathView {
    /// A view of `formula`, displayed or in a line, read out as its source, `latex`.
    pub fn new(formula: Rc<Formula>, display: bool, latex: &str) -> Self {
        let view: Self = glib::Object::builder()
            .property("accessible-role", gtk::AccessibleRole::Img)
            .build();
        view.update_property(&[gtk::accessible::Property::Label(latex.trim())]);
        let _ = view.imp().formula.set(formula);
        view.imp().display.set(display);
        // Clicks go to the text around it, which selects and follows links.
        view.set_can_target(false);
        view
    }
}
