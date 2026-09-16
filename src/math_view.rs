//! A typeset formula in the preview, drawn at the size and in the colour of the text around
//! it, as a widget of its own or in the text of a label.

use adw::subclass::prelude::*;
use gtk::prelude::*;
use gtk::{glib, graphene, pango};
use std::cell::{Cell, OnceCell, RefCell};
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
            let size = font_size(&*self.obj());
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
            let size = font_size(&*widget);
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
}

/// The size of the text of `widget`, in pixels.
fn font_size(widget: &impl IsA<gtk::Widget>) -> f64 {
    let context = widget.pango_context();
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

mod label_imp {
    use super::*;

    #[derive(Default)]
    pub struct BlinkLabelMath {
        pub label: glib::WeakRef<gtk::Label>,
        pub formulas: RefCell<Vec<Rc<Formula>>>,
        /// The size of the text of the label the formulas were measured for, in pixels.
        pub size: Cell<f64>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for BlinkLabelMath {
        const NAME: &'static str = "BlinkLabelMath";
        type Type = super::BlinkLabelMath;
        type ParentType = gtk::Widget;
    }

    impl ObjectImpl for BlinkLabelMath {}

    impl WidgetImpl for BlinkLabelMath {
        fn snapshot(&self, snapshot: &gtk::Snapshot) {
            let widget = self.obj();
            let Some(label) = self.label.upgrade() else {
                return;
            };
            // The text changed size, as with the zoom, since the formulas were measured.
            if font_size(&label) != self.size.get() {
                let formulas = self.formulas.borrow().clone();
                glib::idle_add_local_once(move || set_label_formulas(&label, formulas));
                return;
            }
            let layout = label.layout();
            let (offset_x, offset_y) = label.layout_offsets();
            let Some(origin) = label.compute_point(
                &*widget,
                &graphene::Point::new(offset_x as f32, offset_y as f32),
            ) else {
                return;
            };
            let bounds =
                graphene::Rect::new(0.0, 0.0, widget.width() as f32, widget.height() as f32);
            let cr = snapshot.append_cairo(&bounds);
            let color = label.color();
            let color = (
                f64::from(color.red()),
                f64::from(color.green()),
                f64::from(color.blue()),
            );
            let size = self.size.get();
            let scale = f64::from(pango::SCALE);
            let formulas = self.formulas.borrow();
            let mut formulas = formulas.iter();
            let text = layout.text();
            let mut iter = layout.iter();
            loop {
                let index = usize::try_from(iter.index()).unwrap_or(usize::MAX);
                if text[index.min(text.len())..].starts_with('\u{FFFC}') {
                    let Some(formula) = formulas.next() else {
                        break;
                    };
                    let x = f64::from(iter.char_extents().x()) / scale;
                    let baseline = f64::from(iter.baseline()) / scale;
                    formula.draw(
                        &cr,
                        f64::from(origin.x()) + x,
                        f64::from(origin.y()) + baseline,
                        size,
                        color,
                    );
                }
                if !iter.next_char() {
                    break;
                }
            }
        }
    }
}

glib::wrapper! {
    /// The formulas in the text of a label, drawn over it in the places the label keeps for
    /// them, as a label cannot draw them itself.
    pub struct BlinkLabelMath(ObjectSubclass<label_imp::BlinkLabelMath>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

/// `label` with `formulas` shown in its text, in order in the places of its object
/// replacement characters, at the size of its text: the label itself, or an overlay of it.
pub fn label_with_formulas(label: &gtk::Label, formulas: Vec<Rc<Formula>>) -> gtk::Widget {
    if formulas.is_empty() {
        return label.clone().upcast();
    }
    let math: BlinkLabelMath = glib::Object::builder()
        .property("can-target", false)
        .build();
    math.imp().label.set(Some(label));
    let overlay = gtk::Overlay::builder().child(label).build();
    overlay.add_overlay(&math);
    set_label_formulas(label, formulas);
    overlay.upcast()
}

/// Keep room for `formulas` in the text of `label`, at the size of its text, and draw them
/// there. A label keeps the attributes of its markup with these.
fn set_label_formulas(label: &gtk::Label, formulas: Vec<Rc<Formula>>) {
    let Some(math) = label
        .parent()
        .and_then(|overlay| overlay.last_child())
        .and_downcast::<BlinkLabelMath>()
    else {
        return;
    };
    let size = font_size(label);
    let units = |pixels: f64| (pixels * f64::from(pango::SCALE)).round() as i32;
    let attributes = pango::AttrList::new();
    let text = label.text();
    for ((index, _), formula) in text.match_indices('\u{FFFC}').zip(&formulas) {
        let rectangle = pango::Rectangle::new(
            0,
            units(-formula.ascent * size),
            units(formula.width * size),
            units((formula.ascent + formula.descent) * size),
        );
        let mut shape = pango::AttrShape::new(&rectangle, &rectangle);
        shape.set_start_index(u32::try_from(index).unwrap_or(u32::MAX));
        shape.set_end_index(u32::try_from(index + '\u{FFFC}'.len_utf8()).unwrap_or(u32::MAX));
        attributes.insert(shape);
    }
    math.imp().size.set(size);
    math.imp().formulas.replace(formulas);
    label.set_attributes(Some(&attributes));
    math.queue_draw();
}
