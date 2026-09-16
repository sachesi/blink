//! Math written in LaTeX, typeset as KaTeX typesets it, into outlines the preview and the PDF
//! draw with cairo and the HTML export writes as SVG.

use ab_glyph::{Font, FontRef, OutlineCurve};
use gtk::{cairo, pango};
use ratex_font::FontId;
use ratex_types::{Color, DisplayItem, MathStyle, PathCommand};
use std::cell::RefCell;
use std::collections::HashMap;
use std::fmt::Write;
use std::rc::Rc;

/// How much larger math is set than the text around it, as KaTeX sets it.
const MATH_SCALE: f64 = 1.21;

/// A piece of a path, in ems of the text around it, with y growing downwards from the
/// baseline.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Segment {
    Move(f64, f64),
    Line(f64, f64),
    Quad(f64, f64, f64, f64),
    Cubic(f64, f64, f64, f64, f64, f64),
    Close,
}

/// A filled or stroked path, in the colour the formula gives it, or else in the colour of
/// the text.
#[derive(Debug)]
struct Shape {
    path: Vec<Segment>,
    /// The width of the line of a stroked path, in ems.
    stroke: Option<f64>,
    color: Option<(f64, f64, f64)>,
}

/// A typeset formula: how wide it is and how far it reaches above and below the baseline, in
/// ems, and its shapes.
#[derive(Debug)]
pub struct Formula {
    pub width: f64,
    pub ascent: f64,
    pub descent: f64,
    shapes: Vec<Shape>,
}

/// Formulas typeset so far, by their source and whether they are displayed, or `None` for
/// those that do not typeset.
type Typeset = HashMap<(String, bool), Option<Rc<Formula>>>;

/// `latex` typeset in display style, as a block of its own, or in text style, in a line.
/// `None` if it does not parse, or holds a character no font has, so that it is shown as its
/// source instead. Formulas are kept once typeset.
pub fn typeset(latex: &str, display: bool) -> Option<Rc<Formula>> {
    thread_local! {
        static TYPESET: RefCell<Typeset> = RefCell::new(HashMap::new());
    }
    TYPESET.with_borrow_mut(|typeset| {
        // Each version of a formula being typed is kept, so the store is emptied now and then.
        if typeset.len() > 1000 {
            typeset.clear();
        }
        typeset
            .entry((latex.to_owned(), display))
            .or_insert_with(|| {
                // The layout of formulas it was not tested on can panic. That is no reason to
                // stop the application, and the formula is shown as its source.
                std::panic::catch_unwind(|| layout(latex, display))
                    .ok()
                    .flatten()
                    .map(Rc::new)
            })
            .clone()
    })
}

fn layout(latex: &str, display: bool) -> Option<Formula> {
    let kerned = kern_missing_characters(latex);
    let nodes = ratex_parser::parse(&kerned)
        .or_else(|_| ratex_parser::parse(latex))
        .ok()?;
    let options = ratex_layout::LayoutOptions {
        style: if display {
            MathStyle::Display
        } else {
            MathStyle::Text
        },
        ..Default::default()
    };
    let list = ratex_layout::to_display_list(&ratex_layout::layout(&nodes, &options));
    // The display list's y is measured from its top, where the baseline is at its height.
    let top = list.height;
    let mut shapes = Vec::new();
    for item in &list.items {
        let shape = match item {
            DisplayItem::GlyphPath {
                x,
                y,
                scale,
                font,
                char_code,
                color,
            } => Shape {
                path: glyph(*x, y - top, *scale, font, *char_code)
                    .or_else(|| system_glyph(*x, y - top, *scale, font, *char_code))?,
                stroke: None,
                color: own_color(color),
            },
            DisplayItem::Line {
                x,
                y,
                width,
                thickness,
                color,
                ..
            } => Shape {
                path: rectangle(*x, y - top - thickness / 2.0, *width, *thickness),
                stroke: None,
                color: own_color(color),
            },
            DisplayItem::Rect {
                x,
                y,
                width,
                height,
                color,
            } => Shape {
                path: rectangle(*x, y - top, *width, *height),
                stroke: None,
                color: own_color(color),
            },
            DisplayItem::Path {
                x,
                y,
                commands,
                fill,
                color,
            } => Shape {
                path: commands
                    .iter()
                    .map(|command| path_segment(*command, *x, y - top))
                    .collect(),
                stroke: (!fill).then_some(0.04),
                color: own_color(color),
            },
        };
        shapes.push(shape);
    }
    for shape in &mut shapes {
        for segment in &mut shape.path {
            *segment = scaled(*segment);
        }
        shape.stroke = shape.stroke.map(|width| width * MATH_SCALE);
    }
    Some(Formula {
        width: list.width * MATH_SCALE,
        ascent: list.height * MATH_SCALE,
        descent: list.depth * MATH_SCALE,
        shapes,
    })
}

fn scaled(segment: Segment) -> Segment {
    let s = MATH_SCALE;
    match segment {
        Segment::Move(x, y) => Segment::Move(x * s, y * s),
        Segment::Line(x, y) => Segment::Line(x * s, y * s),
        Segment::Quad(cx, cy, x, y) => Segment::Quad(cx * s, cy * s, x * s, y * s),
        Segment::Cubic(ax, ay, bx, by, x, y) => {
            Segment::Cubic(ax * s, ay * s, bx * s, by * s, x * s, y * s)
        }
        Segment::Close => Segment::Close,
    }
}

/// A colour the formula sets, rather than the black it is laid out in by default.
fn own_color(color: &Color) -> Option<(f64, f64, f64)> {
    (*color != Color::BLACK).then(|| (f64::from(color.r), f64::from(color.g), f64::from(color.b)))
}

fn rectangle(x: f64, y: f64, width: f64, height: f64) -> Vec<Segment> {
    vec![
        Segment::Move(x, y),
        Segment::Line(x + width, y),
        Segment::Line(x + width, y + height),
        Segment::Line(x, y + height),
        Segment::Close,
    ]
}

fn path_segment(command: PathCommand, x: f64, y: f64) -> Segment {
    match command {
        PathCommand::MoveTo { x: px, y: py } => Segment::Move(x + px, y + py),
        PathCommand::LineTo { x: px, y: py } => Segment::Line(x + px, y + py),
        PathCommand::QuadTo {
            x1,
            y1,
            x: px,
            y: py,
        } => Segment::Quad(x + x1, y + y1, x + px, y + py),
        PathCommand::CubicTo {
            x1,
            y1,
            x2,
            y2,
            x: px,
            y: py,
        } => Segment::Cubic(x + x1, y + y1, x + x2, y + y2, x + px, y + py),
        PathCommand::Close => Segment::Close,
    }
}

/// The outline of the character `char_code` of the KaTeX font `font`, `scale` ems tall, with
/// its origin at `(x, y)`. `None` if the font does not have it.
fn glyph(x: f64, y: f64, scale: f64, font: &str, char_code: u32) -> Option<Vec<Segment>> {
    let id = FontId::parse(font)?;
    let bytes = ratex_katex_fonts::ttf_bytes(&format!("KaTeX_{}.ttf", id.as_str()))?;
    let face = FontRef::try_from_slice(&bytes).ok()?;
    let glyph_id = face.glyph_id(ratex_font::katex_ttf_glyph_char(id, char_code));
    if glyph_id.0 == 0 {
        return None;
    }
    let units = f64::from(face.units_per_em()?);
    // Font units grow upwards.
    let point = |p: ab_glyph::Point| {
        (
            x + f64::from(p.x) * scale / units,
            y - f64::from(p.y) * scale / units,
        )
    };
    let mut path = Vec::new();
    let mut last: Option<(f64, f64)> = None;
    // A glyph without an outline, such as a space, draws nothing.
    let Some(outline) = face.outline(glyph_id) else {
        return Some(path);
    };
    for curve in &outline.curves {
        let (start, end) = match curve {
            OutlineCurve::Line(from, to) => (point(*from), point(*to)),
            OutlineCurve::Quad(from, _, to) => (point(*from), point(*to)),
            OutlineCurve::Cubic(from, _, _, to) => (point(*from), point(*to)),
        };
        if last.is_none_or(|last| distance(last, start) > 1e-9) {
            if last.is_some() {
                path.push(Segment::Close);
            }
            path.push(Segment::Move(start.0, start.1));
        }
        path.push(match curve {
            OutlineCurve::Line(..) => Segment::Line(end.0, end.1),
            OutlineCurve::Quad(_, control, _) => {
                let control = point(*control);
                Segment::Quad(control.0, control.1, end.0, end.1)
            }
            OutlineCurve::Cubic(_, first, second, _) => {
                let (first, second) = (point(*first), point(*second));
                Segment::Cubic(first.0, first.1, second.0, second.1, end.0, end.1)
            }
        });
        last = Some(end);
    }
    if last.is_some() {
        path.push(Segment::Close);
    }
    Some(path)
}

/// The size the outlines of characters of the system's fonts are made at, in units of the
/// surface.
const SYSTEM_GLYPH_SIZE: f64 = 1000.0;

/// `latex` with a kern after each character that KaTeX's fonts do not have, as a Cyrillic
/// letter in `\text{}`. KaTeX lays such a character out in a width of its guessing, and the
/// kern makes up the width of the character in the font of the system it is drawn in.
fn kern_missing_characters(latex: &str) -> String {
    thread_local! {
        static KERNS: RefCell<HashMap<char, Option<f64>>> = RefCell::new(HashMap::new());
    }
    let kern = |character: char| {
        KERNS.with_borrow_mut(|kerns| {
            *kerns.entry(character).or_insert_with(|| {
                if glyph(
                    0.0,
                    0.0,
                    1.0,
                    FontId::MainRegular.as_str(),
                    u32::from(character),
                )
                .is_some()
                {
                    return None;
                }
                let (layout, _) = system_layout(character, "Serif", false, false)?;
                let natural = f64::from(layout.extents().1.width())
                    / f64::from(pango::SCALE)
                    / SYSTEM_GLYPH_SIZE;
                let text = format!("\\text{{{character}}}");
                let nodes = ratex_parser::parse(&text).ok()?;
                let options = ratex_layout::LayoutOptions::default();
                let guessed = ratex_layout::layout(&nodes, &options).width;
                Some(natural - guessed).filter(|kern| kern.abs() > 0.005)
            })
        })
    };
    let mut kerned = String::with_capacity(latex.len());
    for character in latex.chars() {
        kerned.push(character);
        if !character.is_ascii()
            && let Some(kern) = kern(character)
        {
            let _ = write!(kerned, "\\kern{{{kern:.3}em}}");
        }
    }
    kerned
}

/// A layout of `character` alone in `family` at [`SYSTEM_GLYPH_SIZE`], bold or italic, drawn
/// on a context of its own, or `None` if no font has the character.
fn system_layout(
    character: char,
    family: &str,
    bold: bool,
    italic: bool,
) -> Option<(pango::Layout, cairo::Context)> {
    let surface = cairo::ImageSurface::create(cairo::Format::A8, 1, 1).ok()?;
    let cr = cairo::Context::new(&surface).ok()?;
    let layout = pangocairo::functions::create_layout(&cr);
    let mut description = pango::FontDescription::new();
    description.set_family(family);
    description.set_absolute_size(SYSTEM_GLYPH_SIZE * f64::from(pango::SCALE));
    if bold {
        description.set_weight(pango::Weight::Bold);
    }
    if italic {
        description.set_style(pango::Style::Italic);
    }
    layout.set_font_description(Some(&description));
    layout.set_text(character.encode_utf8(&mut [0; 4]));
    (layout.unknown_glyphs_count() == 0).then_some((layout, cr))
}

/// The outline of the character `char_code` in a font of the system alike to the KaTeX font
/// `font`, for a character none of KaTeX's fonts has, `scale` ems tall, with its origin at
/// `(x, y)`. `None` if no font has it.
fn system_glyph(x: f64, y: f64, scale: f64, font: &str, char_code: u32) -> Option<Vec<Segment>> {
    let character = char::from_u32(char_code)?;
    let id = FontId::parse(font)?;
    let name = id.as_str();
    let family = if name.starts_with("SansSerif") {
        "Sans"
    } else if name.starts_with("Typewriter") {
        "Monospace"
    } else {
        "Serif"
    };
    let (layout, cr) = system_layout(
        character,
        family,
        name.contains("Bold"),
        name.contains("Italic"),
    )?;
    let baseline = f64::from(layout.baseline()) / f64::from(pango::SCALE);
    pangocairo::functions::layout_path(&cr, &layout);
    let factor = scale / SYSTEM_GLYPH_SIZE;
    let at = |(px, py): (f64, f64)| (x + px * factor, y + (py - baseline) * factor);
    let path = cr.copy_path().ok()?;
    Some(
        path.iter()
            .map(|segment| match segment {
                cairo::PathSegment::MoveTo(point) => {
                    let (px, py) = at(point);
                    Segment::Move(px, py)
                }
                cairo::PathSegment::LineTo(point) => {
                    let (px, py) = at(point);
                    Segment::Line(px, py)
                }
                cairo::PathSegment::CurveTo(first, second, end) => {
                    let ((ax, ay), (bx, by), (px, py)) = (at(first), at(second), at(end));
                    Segment::Cubic(ax, ay, bx, by, px, py)
                }
                cairo::PathSegment::ClosePath => Segment::Close,
            })
            .collect(),
    )
}

fn distance(a: (f64, f64), b: (f64, f64)) -> f64 {
    (a.0 - b.0).hypot(a.1 - b.1)
}

impl Formula {
    /// Draw the formula on `cr` with the left end of its baseline at `(x, y)`, in text of
    /// `size` units and of `color`.
    pub fn draw(&self, cr: &cairo::Context, x: f64, y: f64, size: f64, color: (f64, f64, f64)) {
        for shape in &self.shapes {
            let current = cr.current_point().ok();
            cr.new_path();
            let at = |px: f64, py: f64| (x + px * size, y + py * size);
            let mut pen = (0.0, 0.0);
            for segment in &shape.path {
                match *segment {
                    Segment::Move(px, py) => {
                        pen = at(px, py);
                        cr.move_to(pen.0, pen.1);
                    }
                    Segment::Line(px, py) => {
                        pen = at(px, py);
                        cr.line_to(pen.0, pen.1);
                    }
                    Segment::Quad(cx, cy, px, py) => {
                        // Cairo draws cubic curves, which a quadratic one is a case of.
                        let control = at(cx, cy);
                        let end = at(px, py);
                        cr.curve_to(
                            pen.0 + 2.0 / 3.0 * (control.0 - pen.0),
                            pen.1 + 2.0 / 3.0 * (control.1 - pen.1),
                            end.0 + 2.0 / 3.0 * (control.0 - end.0),
                            end.1 + 2.0 / 3.0 * (control.1 - end.1),
                            end.0,
                            end.1,
                        );
                        pen = end;
                    }
                    Segment::Cubic(ax, ay, bx, by, px, py) => {
                        let (a, b) = (at(ax, ay), at(bx, by));
                        pen = at(px, py);
                        cr.curve_to(a.0, a.1, b.0, b.1, pen.0, pen.1);
                    }
                    Segment::Close => cr.close_path(),
                }
            }
            let (red, green, blue) = shape.color.unwrap_or(color);
            cr.set_source_rgb(red, green, blue);
            let _ = match shape.stroke {
                Some(width) => {
                    cr.set_line_width(width * size);
                    cr.stroke()
                }
                None => {
                    cr.set_fill_rule(cairo::FillRule::Winding);
                    cr.fill()
                }
            };
            if let Some((px, py)) = current {
                cr.move_to(px, py);
            }
        }
    }

    /// The formula as an inline SVG element in ems, drawn in the colour of the text around it,
    /// and read out as its source, `latex`.
    pub fn svg(&self, latex: &str) -> String {
        let height = self.ascent + self.descent;
        let mut svg = format!(
            "<svg xmlns=\"http://www.w3.org/2000/svg\" class=\"math\" role=\"img\" \
             aria-label=\"{}\" viewBox=\"0 {} {} {}\" style=\"width: {}em; height: {}em; \
             vertical-align: {}em\">",
            escape_attribute(latex),
            number(-self.ascent),
            number(self.width),
            number(height),
            number(self.width),
            number(height),
            number(-self.descent),
        );
        for shape in &self.shapes {
            let mut data = String::new();
            for segment in &shape.path {
                let _ = match *segment {
                    Segment::Move(x, y) => write!(data, "M{} {}", number(x), number(y)),
                    Segment::Line(x, y) => write!(data, "L{} {}", number(x), number(y)),
                    Segment::Quad(cx, cy, x, y) => write!(
                        data,
                        "Q{} {} {} {}",
                        number(cx),
                        number(cy),
                        number(x),
                        number(y)
                    ),
                    Segment::Cubic(ax, ay, bx, by, x, y) => write!(
                        data,
                        "C{} {} {} {} {} {}",
                        number(ax),
                        number(ay),
                        number(bx),
                        number(by),
                        number(x),
                        number(y)
                    ),
                    Segment::Close => write!(data, "Z"),
                };
            }
            let paint = shape.color.map_or_else(
                || String::from("currentColor"),
                |(red, green, blue)| {
                    let channel = |value: f64| (value.clamp(0.0, 1.0) * 255.0).round() as u8;
                    format!(
                        "rgb({}, {}, {})",
                        channel(red),
                        channel(green),
                        channel(blue)
                    )
                },
            );
            let _ = match shape.stroke {
                Some(width) => write!(
                    svg,
                    "<path d=\"{data}\" fill=\"none\" stroke=\"{paint}\" stroke-width=\"{}\"/>",
                    number(width)
                ),
                None => write!(svg, "<path d=\"{data}\" fill=\"{paint}\"/>"),
            };
        }
        svg.push_str("</svg>");
        svg
    }
}

/// `value` with no more decimals than drawing at any size needs.
fn number(value: f64) -> String {
    let text = format!("{value:.4}");
    let text = text.trim_end_matches('0').trim_end_matches('.');
    if text == "-0" || text.is_empty() {
        String::from("0")
    } else {
        text.to_owned()
    }
}

fn escape_attribute(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('"', "&quot;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

#[cfg(test)]
mod tests {
    use super::{number, typeset};

    #[test]
    fn formulas_are_typeset_or_left_as_their_source() {
        let fraction = typeset(r"\frac{a}{b}", true).expect("a fraction");
        assert!(fraction.width > 0.0 && fraction.ascent > 0.5 && fraction.descent > 0.3);
        let inline = typeset(r"\frac{a}{b}", false).expect("a fraction");
        assert!(inline.ascent < fraction.ascent);
        assert!(typeset(r"\frac{a}{", false).is_none());
        assert!(typeset(r"\notacommand", false).is_none());
        // A letter none of KaTeX's fonts has is drawn in a font of the system.
        let cyrillic = typeset(r"\text{Привіт}", true).expect("Cyrillic text");
        let latin = typeset(r"\text{Pryvit}", true).expect("Latin text");
        assert!((cyrillic.width - latin.width).abs() < 0.3);
        assert!(cyrillic.shapes.iter().all(|shape| !shape.path.is_empty()));
    }

    #[test]
    fn svg_draws_in_the_colour_of_the_text_and_reads_as_the_source() {
        let svg = typeset("x^2", false).expect("a formula").svg("x^2 < \"y\"");
        assert!(svg.starts_with("<svg xmlns=\"http://www.w3.org/2000/svg\" class=\"math\""));
        assert!(svg.contains("aria-label=\"x^2 &lt; &quot;y&quot;\""));
        assert!(svg.contains("fill=\"currentColor\""));
        assert_eq!(number(-0.00001), "0");
        assert_eq!(number(1.5), "1.5");
    }
}
