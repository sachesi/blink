//! PDF export. The document is set on A4 pages with Pango and written by cairo, so the text
//! stays text, web links can be followed and the headings make the outline. Like the
//! preview, it shows only images of the document's folder and follows only web and mail
//! links.

use gtk::gdk::prelude::GdkCairoContextExt;
use gtk::{cairo, gdk_pixbuf, pango};
use pango::prelude::*;
use pulldown_cmark::{Alignment, BlockQuoteKind, CodeBlockKind, Event, Tag, TagEnd};
use std::cell::RefCell;
use std::collections::HashSet;
use std::ops::Range;
use std::path::PathBuf;
use std::rc::Rc;

use crate::export::Options;
use crate::markdown::{self, CodeRun, HtmlAlign, HtmlStyle};
use crate::math;

/// A4, in points.
const PAGE_WIDTH: f64 = 595.276;
const PAGE_HEIGHT: f64 = 841.89;
const MARGIN: f64 = 64.0;
const COLUMN_WIDTH: f64 = PAGE_WIDTH - 2.0 * MARGIN;

const BODY_SIZE: f64 = 11.0;
const CODE_SIZE: f64 = 9.5;
const FOOTER_SIZE: f64 = 9.0;
/// Sizes of the headings of levels 1 to 4 and below, relative to the body, as in the preview.
const HEADING_SCALES: [f64; 4] = [2.0, 1.75, 1.5, 1.2];
const LINE_SPACING: f32 = 1.3;
const CODE_LINE_SPACING: f32 = 1.15;

/// Space after a paragraph, and after a list item.
const PARAGRAPH_GAP: f64 = 8.0;
const ITEM_GAP: f64 = 3.0;
const LIST_INDENT: f64 = 16.0;
const QUOTE_INDENT: f64 = 16.0;
const QUOTE_BAR_WIDTH: f64 = 2.5;
const CODE_PADDING: f64 = 10.0;
/// Padding of a table cell, as in the preview.
const CELL_PADDING_X: f64 = 9.0;
const CELL_PADDING_Y: f64 = 7.5;
const CORNER_RADIUS: f64 = 6.0;
/// Characters after which the text of a table cell wraps, as in the preview.
const CELL_WRAP_CHARS: f64 = 40.0;
/// The smallest size the text of a table too wide for the page is set at.
const MIN_TABLE_SIZE: f64 = 8.0;
/// Pixels are shown at 96 per inch, as on screen.
const POINTS_PER_PIXEL: f64 = 0.75;
/// Images are decoded at up to 192 pixels per inch, for print.
const DECODED_PIXELS_PER_POINT: f64 = 192.0 / 72.0;
/// The cairo tag of a link.
const LINK_TAG: &str = "Link";

type Rgb = (f64, f64, f64);
const TEXT_COLOR: Rgb = (0.13, 0.13, 0.14);
const DIM_COLOR: Rgb = (0.4, 0.4, 0.42);
const LINK_COLOR: Rgb = (0.11, 0.44, 0.85);
const LINE_COLOR: Rgb = (0.82, 0.82, 0.84);
const TINT_COLOR: Rgb = (0.955, 0.955, 0.96);

/// The blockquotes a block is in: how many, and for each of them, from the outermost, which
/// quote of the document it is and whether it is an alert of some kind.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Quote {
    depth: usize,
    numbers: [usize; 8],
    alerts: [Option<BlockQuoteKind>; 8],
}

impl Quote {
    /// Enter the quote numbered `number`, an alert of `kind` or not.
    fn enter(&mut self, number: usize, kind: Option<BlockQuoteKind>) {
        if let (Some(slot), Some(alert)) = (
            self.numbers.get_mut(self.depth),
            self.alerts.get_mut(self.depth),
        ) {
            *slot = number;
            *alert = kind;
        }
        self.depth += 1;
    }

    fn leave(&mut self) {
        self.depth = self.depth.saturating_sub(1);
        if let Some(alert) = self.alerts.get_mut(self.depth) {
            *alert = None;
        }
    }
}

/// How a stretch of text looks.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Inline {
    bold: bool,
    italic: bool,
    strikethrough: bool,
    underline: bool,
    code: bool,
    superscript: bool,
    subscript: bool,
    mark: bool,
    link: bool,
    footnote: bool,
    /// The kind of the alert whose title the text is.
    alert: Option<BlockQuoteKind>,
}

/// Text with its styled stretches and links, as byte ranges of the text, and what is drawn
/// in it, at the byte offset of the character it takes the place of.
#[derive(Debug, Default)]
struct Paragraph {
    text: String,
    styles: Vec<(Range<usize>, Inline)>,
    links: Vec<(Range<usize>, String)>,
    objects: Vec<(usize, Object)>,
    /// The destinations of links in the paragraph, by name.
    anchors: Vec<(usize, String)>,
    /// How the block of raw HTML the paragraph is in aligns it.
    align: Option<HtmlAlign>,
}

impl Paragraph {
    fn push(&mut self, text: &str, style: Inline) {
        let start = self.text.len();
        self.text.push_str(text);
        if style != Inline::default() && !text.is_empty() {
            self.styles.push((start..self.text.len(), style));
        }
    }

    /// Hold the place of `object` with an object replacement character, which it is drawn
    /// over.
    fn push_object(&mut self, object: Object) {
        self.objects.push((self.text.len(), object));
        self.text.push('\u{FFFC}');
    }
}

/// Something drawn in a paragraph rather than set in a font.
#[derive(Debug)]
enum Object {
    /// The box of a task list item, ticked or not. Drawn, as the box glyphs of the fonts
    /// that have them differ in size from each other.
    Task(bool),
    /// A formula, set on a line of its own if it is displayed.
    Math {
        formula: Rc<math::Formula>,
        display: bool,
    },
}

impl Object {
    /// How wide the object is, and how far it reaches above and below the baseline, in text
    /// of `size` points.
    fn extents(&self, size: f64) -> (f64, f64, f64) {
        match self {
            // About as tall as a capital letter, and centred on the lower-case letters.
            Self::Task(_) => {
                let side = 0.8 * size;
                let middle = 0.28 * size;
                (side, middle + side / 2.0, side / 2.0 - middle)
            }
            Self::Math { formula, .. } => (
                formula.width * size,
                formula.ascent * size,
                formula.descent * size,
            ),
        }
    }

    /// Draw the object with its left end at `x` on the baseline at `y`, in text of `size`
    /// points.
    fn draw(&self, cr: &cairo::Context, x: f64, y: f64, size: f64) {
        let (width, ascent, descent) = self.extents(size);
        match self {
            Self::Task(checked) => {
                let top = y - ascent;
                let side = ascent + descent;
                if *checked {
                    rounded_path(cr, x, top, width, side, 0.2 * side);
                    let (red, green, blue) = LINK_COLOR;
                    cr.set_source_rgb(red, green, blue);
                    let _ = cr.fill();
                    cr.set_source_rgb(1.0, 1.0, 1.0);
                    cr.set_line_width(0.13 * side);
                    cr.set_line_cap(cairo::LineCap::Round);
                    cr.set_line_join(cairo::LineJoin::Round);
                    cr.move_to(x + 0.24 * width, top + 0.52 * side);
                    cr.line_to(x + 0.42 * width, top + 0.7 * side);
                    cr.line_to(x + 0.77 * width, top + 0.3 * side);
                    let _ = cr.stroke();
                } else {
                    let (red, green, blue) = DIM_COLOR;
                    cr.set_source_rgb(red, green, blue);
                    cr.set_line_width(0.08 * side);
                    // Inside the box, as the ticked box is filled to its edge.
                    let inset = 0.04 * side;
                    rounded_path(
                        cr,
                        x + inset,
                        top + inset,
                        width - 2.0 * inset,
                        side - 2.0 * inset,
                        0.2 * side - inset,
                    );
                    let _ = cr.stroke();
                }
            }
            Self::Math { formula, .. } => formula.draw(cr, x, y, size, TEXT_COLOR),
        }
    }
}

#[derive(Debug)]
enum Block {
    Text {
        paragraph: Paragraph,
        /// The heading level, from 1.
        heading: Option<usize>,
        /// The identifier of the heading, which links to it point at.
        id: Option<String>,
        indent: f64,
        /// The length of the list marker that opens the text, which wrapped lines hang
        /// after.
        marker: usize,
        quote: Quote,
        gap: f64,
    },
    Code {
        code: String,
        runs: Vec<CodeRun>,
        indent: f64,
        quote: Quote,
    },
    Table {
        rows: Vec<Vec<Paragraph>>,
        alignments: Vec<Alignment>,
        indent: f64,
        quote: Quote,
    },
    Image {
        path: PathBuf,
        alt: String,
        /// The width in pixels an `<img>` tag gave the image.
        width: Option<i32>,
        /// How the block of raw HTML the image is in aligns it, if not centred.
        align: Option<HtmlAlign>,
        indent: f64,
        quote: Quote,
    },
    Rule,
}

type Row = Vec<Paragraph>;

/// The blocks of a document, in the order they are set, read from its Markdown events the
/// way the preview reads them.
struct Reader<'a> {
    options: &'a Options,
    blocks: Vec<Block>,
    paragraph: Paragraph,
    /// The length of what opens the paragraph, a list marker or a footnote label, which does
    /// not make it worth setting on its own.
    prefix: usize,
    /// Whether the prefix is a list marker, which wrapped lines hang after.
    hanging: bool,
    heading: Option<usize>,
    heading_id: Option<String>,
    style: Inline,
    /// The styles raw HTML opened, which it may never close, and the blocks it opened, with
    /// how each aligns its content.
    html_styles: Vec<HtmlStyle>,
    html_blocks: Vec<Option<HtmlAlign>>,
    lists: Vec<Option<u64>>,
    /// How many definitions of definition lists are open.
    definitions: usize,
    quote: Quote,
    /// How many blockquotes have started.
    quotes: usize,
    links: Vec<(String, usize)>,
    /// Whether each open image is shown, which hides its alternative text.
    images: Vec<bool>,
    /// The labels of the open footnotes.
    footnotes: Vec<String>,
    /// The labels of the footnotes referred to so far.
    referenced: HashSet<String>,
    code: Option<(String, String)>,
    code_blocks: usize,
    /// The open table: its alignments, its rows and the row being read.
    table: Option<(Vec<Alignment>, Vec<Row>, Row)>,
}

impl<'a> Reader<'a> {
    fn new(options: &'a Options) -> Self {
        Self {
            options,
            blocks: Vec::new(),
            paragraph: Paragraph::default(),
            prefix: 0,
            hanging: false,
            heading: None,
            heading_id: None,
            style: Inline::default(),
            html_styles: Vec::new(),
            html_blocks: Vec::new(),
            lists: Vec::new(),
            definitions: 0,
            quote: Quote::default(),
            quotes: 0,
            links: Vec::new(),
            images: Vec::new(),
            footnotes: Vec::new(),
            referenced: HashSet::new(),
            code: None,
            code_blocks: 0,
            table: None,
        }
    }

    fn indent(&self) -> f64 {
        (self.lists.len() + self.definitions) as f64 * LIST_INDENT
            + self.quote.depth as f64 * QUOTE_INDENT
    }

    fn has_text(&self) -> bool {
        !self.paragraph.text[self.prefix..].trim().is_empty()
    }

    /// Set the paragraph read so far, if it has more than its prefix, followed by `gap`.
    fn flush(&mut self, gap: f64) {
        let mut paragraph = std::mem::take(&mut self.paragraph);
        // Line breaks at the end, as raw HTML leaves, would set empty lines.
        let end = paragraph.text.trim_end_matches('\n').len();
        if paragraph.styles.iter().all(|(range, _)| range.end <= end)
            && paragraph.links.iter().all(|(range, _)| range.end <= end)
            && paragraph.objects.iter().all(|(index, _)| *index < end)
            && paragraph.anchors.iter().all(|(index, _)| *index <= end)
        {
            paragraph.text.truncate(end);
        }
        paragraph.align = self.html_align();
        let marker = if self.hanging { self.prefix } else { 0 };
        let has_text = !paragraph.text[self.prefix..].trim().is_empty();
        self.prefix = 0;
        self.hanging = false;
        if !has_text {
            return;
        }
        // An item's marker sits one step out from the text of the list.
        let indent = if marker > 0 {
            self.indent() - LIST_INDENT
        } else {
            self.indent()
        };
        self.blocks.push(Block::Text {
            paragraph,
            heading: self.heading,
            id: self.heading.and(self.heading_id.clone()),
            indent,
            marker,
            quote: self.quote,
            gap,
        });
    }

    /// How the innermost block of raw HTML that aligns its content aligns it.
    fn html_align(&self) -> Option<HtmlAlign> {
        self.html_blocks.iter().rev().flatten().next().copied()
    }

    /// Leave at least `gap` after the last block.
    fn widen_gap(&mut self, gap: f64) {
        if let Some(Block::Text { gap: last, .. }) = self.blocks.last_mut() {
            *last = last.max(gap);
        }
    }

    /// The style of text read now: that of the Markdown around it, and of the raw HTML.
    fn text_style(&self) -> Inline {
        let mut style = self.style;
        for html_style in &self.html_styles {
            match html_style {
                HtmlStyle::Bold => style.bold = true,
                HtmlStyle::Italic => style.italic = true,
                HtmlStyle::Strikethrough => style.strikethrough = true,
                HtmlStyle::Underline => style.underline = true,
                HtmlStyle::Code | HtmlStyle::Keyboard => style.code = true,
                HtmlStyle::Superscript => style.superscript = true,
                HtmlStyle::Subscript => style.subscript = true,
                HtmlStyle::Mark => style.mark = true,
            }
        }
        style
    }

    fn push_text(&mut self, text: &str) {
        if let Some(shown) = self.images.last() {
            // Kept for an image that turns out not to decode.
            if *shown {
                if let Some(Block::Image { alt, .. }) = self.blocks.last_mut() {
                    alt.push_str(text);
                }
                return;
            }
            let style = Inline {
                italic: true,
                ..self.text_style()
            };
            self.paragraph.push(text, style);
            return;
        }
        self.paragraph.push(text, self.text_style());
    }

    fn read(mut self, text: &str) -> Vec<Block> {
        for (event, _) in markdown::events_with_emoji(text, markdown::font_has_emoji) {
            self.event(event);
        }
        self.flush(PARAGRAPH_GAP);
        self.blocks
    }

    fn event(&mut self, event: Event) {
        if let Some((_, code)) = self.code.as_mut() {
            match event {
                Event::Text(text) => code.push_str(&text),
                Event::End(TagEnd::CodeBlock) => {
                    let (_, code) = self.code.take().unwrap_or_default();
                    self.blocks.push(Block::Code {
                        code: markdown::shown_code(&code).to_owned(),
                        runs: self
                            .options
                            .light
                            .get(self.code_blocks)
                            .cloned()
                            .unwrap_or_default(),
                        indent: self.indent(),
                        quote: self.quote,
                    });
                    self.code_blocks += 1;
                }
                _ => {}
            }
            return;
        }

        match event {
            Event::Start(tag) => self.start(tag),
            Event::End(tag) => self.end(tag),
            Event::Text(text) => self.push_text(&text),
            Event::Code(code) => {
                let style = self.style;
                self.style.code = true;
                self.push_text(&code);
                self.style = style;
            }
            // A `<details>` element is set open, its summary in bold on a line of its own.
            Event::Html(ref html) | Event::InlineHtml(ref html) => {
                for part in markdown::html_parts_with_emoji(html, markdown::font_has_emoji) {
                    match part {
                        markdown::HtmlPart::Text(text) => {
                            // A line break is not set at the start of a line, nor twice.
                            let mut text = text.as_str();
                            if !self.has_text() || self.paragraph.text.ends_with('\n') {
                                text = text.trim_start_matches('\n');
                            }
                            let mut collapsed = String::with_capacity(text.len());
                            for c in text.chars() {
                                if !(c == '\n' && collapsed.ends_with('\n')) {
                                    collapsed.push(c);
                                }
                            }
                            self.push_text(&collapsed);
                        }
                        markdown::HtmlPart::SummaryStart => {
                            self.flush(PARAGRAPH_GAP);
                            self.style.bold = true;
                        }
                        markdown::HtmlPart::SummaryEnd => {
                            self.flush(ITEM_GAP);
                            self.style.bold = false;
                        }
                        markdown::HtmlPart::DetailsStart { .. }
                        | markdown::HtmlPart::DetailsEnd => {
                            self.flush(PARAGRAPH_GAP);
                        }
                        // A block is a paragraph of its own; blocks inside text are not laid
                        // out.
                        markdown::HtmlPart::BlockStart(align)
                            if matches!(event, Event::Html(_)) =>
                        {
                            self.flush(PARAGRAPH_GAP);
                            self.html_blocks.push(align);
                        }
                        markdown::HtmlPart::BlockEnd if matches!(event, Event::Html(_)) => {
                            self.flush(PARAGRAPH_GAP);
                            self.html_blocks.pop();
                        }
                        markdown::HtmlPart::BlockStart(_) | markdown::HtmlPart::BlockEnd => {}
                        markdown::HtmlPart::StyleStart(style) => self.html_styles.push(style),
                        markdown::HtmlPart::StyleEnd(style) => {
                            if let Some(index) = self.html_styles.iter().rposition(|s| *s == style)
                            {
                                self.html_styles.remove(index);
                            }
                        }
                    }
                }
                // As the end of a paragraph closes them in a browser.
                if matches!(event, Event::Html(_)) {
                    self.html_styles.clear();
                }
            }
            Event::InlineMath(ref latex) | Event::DisplayMath(ref latex) => {
                let display = matches!(event, Event::DisplayMath(_));
                // Math that does not typeset has been made code already.
                if let Some(formula) = math::typeset(latex, display) {
                    // A displayed formula is a centred paragraph of its own, as in the preview.
                    if display && self.has_text() {
                        self.flush(PARAGRAPH_GAP);
                    }
                    self.paragraph
                        .push_object(Object::Math { formula, display });
                    if display {
                        self.flush(PARAGRAPH_GAP);
                    }
                }
            }
            // Not at the start of the paragraph after a displayed formula.
            Event::SoftBreak if !self.has_text() && self.table.is_none() => {}
            Event::SoftBreak => self.push_text(" "),
            Event::HardBreak => self.push_text("\n"),
            // A reference links to its note, and the first one is where the note links back to.
            Event::FootnoteReference(name) => {
                let style = Inline {
                    footnote: true,
                    ..self.style
                };
                let start = self.paragraph.text.len();
                if self.referenced.insert(name.to_string()) {
                    self.paragraph
                        .anchors
                        .push((start, markdown::footnote_reference_id(&name)));
                }
                self.paragraph.push(&format!("[{name}]"), style);
                let end = self.paragraph.text.len();
                self.paragraph
                    .links
                    .push((start..end, format!("#{}", markdown::footnote_id(&name))));
            }
            Event::TaskListMarker(checked) => {
                // The box takes the place of a bullet, as in the preview.
                if self.hanging
                    && self.lists.last() == Some(&None)
                    && self.paragraph.text.len() == self.prefix
                {
                    self.paragraph = Paragraph::default();
                }
                self.paragraph.push_object(Object::Task(checked));
                self.paragraph.push(" ", self.style);
                self.prefix = self.paragraph.text.len();
            }
            Event::Rule => {
                self.flush(PARAGRAPH_GAP);
                self.blocks.push(Block::Rule);
            }
        }
    }

    fn start(&mut self, tag: Tag) {
        match tag {
            Tag::Paragraph => {
                if self.has_text() {
                    self.flush(PARAGRAPH_GAP);
                }
            }
            Tag::Heading { level, id, .. } => {
                self.flush(PARAGRAPH_GAP);
                self.heading = Some(level as usize);
                self.heading_id = id.map(|id| id.to_string());
            }
            Tag::DefinitionListTitle => {
                self.flush(PARAGRAPH_GAP);
                self.style.bold = true;
            }
            Tag::DefinitionListDefinition => {
                self.flush(ITEM_GAP);
                self.definitions += 1;
            }
            Tag::BlockQuote(kind) => {
                self.flush(PARAGRAPH_GAP);
                self.quotes += 1;
                self.quote.enter(self.quotes, kind);
                // The title of an alert is in its colour.
                self.style.alert = kind;
            }
            Tag::CodeBlock(kind) => {
                self.flush(PARAGRAPH_GAP);
                let info = match kind {
                    CodeBlockKind::Fenced(info) => info.to_string(),
                    CodeBlockKind::Indented => String::new(),
                };
                self.code = Some((info, String::new()));
            }
            Tag::List(first) => {
                if self.has_text() {
                    self.flush(ITEM_GAP);
                }
                self.lists.push(first);
            }
            Tag::Item => {
                if self.has_text() {
                    self.flush(ITEM_GAP);
                }
                let marker = markdown::list_marker(&mut self.lists);
                let style = Inline {
                    bold: true,
                    ..Inline::default()
                };
                self.paragraph.push(&marker, style);
                self.prefix = self.paragraph.text.len();
                self.hanging = true;
            }
            Tag::FootnoteDefinition(label) => {
                self.flush(PARAGRAPH_GAP);
                self.paragraph
                    .anchors
                    .push((0, markdown::footnote_id(&label)));
                self.footnotes.push(label.to_string());
                let style = Inline {
                    bold: true,
                    ..Inline::default()
                };
                self.paragraph.push(&format!("[{label}]: "), style);
                self.prefix = self.paragraph.text.len();
            }
            Tag::Table(alignments) => {
                self.flush(PARAGRAPH_GAP);
                self.table = Some((alignments, Vec::new(), Vec::new()));
            }
            Tag::TableHead | Tag::TableRow | Tag::TableCell => {
                self.paragraph = Paragraph::default();
            }
            Tag::Emphasis => self.style.italic = true,
            Tag::Strong => self.style.bold = true,
            Tag::Strikethrough => self.style.strikethrough = true,
            Tag::Link { dest_url, .. } => {
                self.links
                    .push((dest_url.to_string(), self.paragraph.text.len()));
                self.style.link = true;
            }
            Tag::Image {
                link_type,
                dest_url,
                id,
                ..
            } => {
                let path = markdown::local_image_path(&dest_url, self.options.base_dir.as_deref());
                self.images.push(path.is_some());
                if let Some(path) = path {
                    self.flush(PARAGRAPH_GAP);
                    self.blocks.push(Block::Image {
                        path,
                        alt: String::new(),
                        width: markdown::image_width(link_type, &id),
                        align: self.html_align(),
                        indent: self.indent(),
                        quote: self.quote,
                    });
                }
            }
            _ => {}
        }
    }

    fn end(&mut self, tag: TagEnd) {
        match tag {
            // The title of an alert sits right over its text, as in the preview.
            TagEnd::Paragraph if self.style.alert.is_some() => {
                self.style.alert = None;
                self.html_styles.clear();
                self.flush(ITEM_GAP);
            }
            TagEnd::Paragraph => {
                self.html_styles.clear();
                self.flush(PARAGRAPH_GAP);
            }
            TagEnd::Heading(_) => {
                self.html_styles.clear();
                self.flush(PARAGRAPH_GAP);
                self.heading = None;
                self.heading_id = None;
            }
            TagEnd::DefinitionListTitle => {
                self.flush(ITEM_GAP);
                self.style.bold = false;
            }
            TagEnd::DefinitionListDefinition => {
                self.flush(ITEM_GAP);
                self.definitions = self.definitions.saturating_sub(1);
            }
            TagEnd::DefinitionList => self.widen_gap(PARAGRAPH_GAP),
            TagEnd::BlockQuote(_) => {
                self.flush(PARAGRAPH_GAP);
                self.quote.leave();
                self.widen_gap(PARAGRAPH_GAP);
            }
            TagEnd::List(_) => {
                self.flush(ITEM_GAP);
                self.lists.pop();
                if self.lists.is_empty() {
                    self.widen_gap(PARAGRAPH_GAP);
                }
            }
            TagEnd::Item => self.flush(ITEM_GAP),
            TagEnd::FootnoteDefinition => {
                self.flush(PARAGRAPH_GAP);
                // After the last word of the note, whose paragraph is set by now.
                let label = self.footnotes.pop().unwrap_or_default();
                if let Some(Block::Text { paragraph, .. }) = self.blocks.last_mut() {
                    let start = paragraph.text.len() + '\u{a0}'.len_utf8();
                    paragraph.push(markdown::FOOTNOTE_BACKLINK, Inline::default());
                    let end = paragraph.text.len();
                    let url = format!("#{}", markdown::footnote_reference_id(&label));
                    paragraph.styles.push((
                        start..end,
                        Inline {
                            link: true,
                            ..Inline::default()
                        },
                    ));
                    paragraph.links.push((start..end, url));
                }
            }
            // The blocks of raw HTML it leaves open end with it.
            TagEnd::HtmlBlock if !self.html_blocks.is_empty() => {
                self.flush(PARAGRAPH_GAP);
                self.html_blocks.clear();
            }
            TagEnd::TableCell => {
                self.html_styles.clear();
                let cell = std::mem::take(&mut self.paragraph);
                if let Some((_, _, row)) = self.table.as_mut() {
                    row.push(cell);
                }
            }
            TagEnd::TableHead | TagEnd::TableRow => {
                if let Some((_, rows, row)) = self.table.as_mut() {
                    rows.push(std::mem::take(row));
                }
            }
            TagEnd::Table => {
                if let Some((alignments, rows, _)) = self.table.take() {
                    self.blocks.push(Block::Table {
                        rows,
                        alignments,
                        indent: self.indent(),
                        quote: self.quote,
                    });
                }
            }
            TagEnd::Emphasis => self.style.italic = false,
            TagEnd::Strong => self.style.bold = false,
            TagEnd::Strikethrough => self.style.strikethrough = false,
            TagEnd::Link => {
                if let Some((url, start)) = self.links.pop()
                    && (markdown::is_safe_link(&url) || url.starts_with('#'))
                {
                    let end = self.paragraph.text.len();
                    self.paragraph.links.push((start..end, url));
                }
                self.style.link = !self.links.is_empty();
            }
            TagEnd::Image => {
                self.images.pop();
            }
            _ => {}
        }
    }
}

/// A line of a layout as it is set: its offsets in the layout, in points.
struct Line {
    line: pango::LayoutLine,
    /// The byte range of the text it shows.
    range: Range<usize>,
    x: f64,
    top: f64,
    baseline: f64,
    height: f64,
}

fn lines(layout: &pango::Layout) -> Vec<Line> {
    let scale = f64::from(pango::SCALE);
    let text_length = layout.text().len();
    let mut lines = Vec::new();
    let mut iter = layout.iter();
    loop {
        let (top, bottom) = iter.line_yrange();
        let (_, logical) = iter.line_extents();
        let start = usize::try_from(iter.index()).unwrap_or(0);
        if let Some(line) = iter.line_readonly() {
            lines.push(Line {
                line,
                range: start..text_length,
                x: f64::from(logical.x()) / scale,
                top: f64::from(top) / scale,
                baseline: f64::from(iter.baseline()) / scale,
                height: f64::from(bottom - top) / scale,
            });
        }
        if !iter.next_line() {
            break;
        }
    }
    for index in 1..lines.len() {
        let next = lines[index].range.start;
        lines[index - 1].range.end = next;
    }
    lines
}

/// Sets blocks on pages.
struct Typesetter<'a> {
    options: &'a Options,
    surface: cairo::PdfSurface,
    cr: cairo::Context,
    context: pango::Context,
    y: f64,
    page: u32,
    /// The open headings, by level, as outline entries.
    outline: Vec<(usize, i32)>,
    /// The identifiers of the headings of the document.
    headings: HashSet<String>,
    /// Where the bar of each level of blockquote last ended: the quote's number, the page and
    /// the bottom.
    bars: RefCell<Vec<(usize, u32, f64)>>,
}

impl<'a> Typesetter<'a> {
    fn new(options: &'a Options) -> Result<Self, cairo::Error> {
        let surface = cairo::PdfSurface::for_stream(PAGE_WIDTH, PAGE_HEIGHT, Vec::<u8>::new())?;
        surface.set_metadata(cairo::PdfMetadata::Title, &options.title)?;
        surface.set_metadata(cairo::PdfMetadata::Creator, "Blink")?;
        let cr = cairo::Context::new(&surface)?;
        let context = pangocairo::functions::create_context(&cr);
        // Sizes are in points, which are the units of the page.
        pangocairo::functions::context_set_resolution(&context, 72.0);
        let mut font_options = cairo::FontOptions::new()?;
        font_options.set_hint_metrics(cairo::HintMetrics::Off);
        font_options.set_hint_style(cairo::HintStyle::None);
        pangocairo::functions::context_set_font_options(&context, Some(&font_options));
        Ok(Self {
            options,
            surface,
            cr,
            context,
            y: MARGIN,
            page: 1,
            outline: Vec::new(),
            headings: HashSet::new(),
            bars: RefCell::new(Vec::new()),
        })
    }

    fn bottom() -> f64 {
        PAGE_HEIGHT - MARGIN
    }

    fn at_top(&self) -> bool {
        self.y <= MARGIN
    }

    fn layout(&self, family: &str, size: f64) -> pango::Layout {
        let layout = pango::Layout::new(&self.context);
        let mut description = pango::FontDescription::new();
        description.set_family(family);
        description.set_absolute_size(size * f64::from(pango::SCALE));
        layout.set_font_description(Some(&description));
        layout
    }

    fn set_color(&self, (red, green, blue): Rgb) {
        self.cr.set_source_rgb(red, green, blue);
    }

    fn footer(&self) -> Result<(), cairo::Error> {
        let layout = self.layout(&self.options.text_font, FOOTER_SIZE);
        layout.set_text(&self.page.to_string());
        let (_, logical) = layout.pixel_extents();
        self.set_color(DIM_COLOR);
        self.cr.move_to(
            (PAGE_WIDTH - f64::from(logical.width())) / 2.0,
            PAGE_HEIGHT - MARGIN / 2.0 - f64::from(logical.height()) / 2.0,
        );
        pangocairo::functions::show_layout(&self.cr, &layout);
        self.cr.status()
    }

    fn new_page(&mut self) -> Result<(), cairo::Error> {
        self.footer()?;
        self.cr.show_page()?;
        self.page += 1;
        self.y = MARGIN;
        Ok(())
    }

    /// Make room for `height` points, on a new page if this one has too little left.
    fn reserve(&mut self, height: f64) -> Result<(), cairo::Error> {
        if self.y + height > Self::bottom() && !self.at_top() {
            self.new_page()?;
        }
        Ok(())
    }

    fn finish(self) -> Result<Vec<u8>, cairo::Error> {
        self.footer()?;
        drop(self.cr);
        let stream = self
            .surface
            .finish_output_stream()
            .map_err(|_| cairo::Error::WriteError)?;
        stream
            .downcast::<Vec<u8>>()
            .map(|bytes| *bytes)
            .map_err(|_| cairo::Error::WriteError)
    }

    /// The Pango attributes of `paragraph`'s styles.
    fn attributes(&self, paragraph: &Paragraph, size: f64) -> pango::AttrList {
        let list = no_hyphens();
        let units = |points: f64| (points * f64::from(pango::SCALE)).round() as i32;
        for (index, object) in &paragraph.objects {
            let (width, ascent, descent) = object.extents(size);
            let rectangle =
                pango::Rectangle::new(0, units(-ascent), units(width), units(ascent + descent));
            let mut shape: pango::Attribute =
                pango::AttrShape::new(&rectangle, &rectangle).upcast();
            shape.set_start_index(saturating_u32(*index));
            shape.set_end_index(saturating_u32(index + '\u{FFFC}'.len_utf8()));
            list.insert(shape);
        }
        let channel = |value: f64| (value * 65535.0).round() as u16;
        for (range, style) in &paragraph.styles {
            let mut attributes: Vec<pango::Attribute> = Vec::new();
            if style.bold || style.footnote {
                attributes.push(pango::AttrInt::new_weight(pango::Weight::Bold).upcast());
            }
            if style.italic {
                attributes.push(pango::AttrInt::new_style(pango::Style::Italic).upcast());
            }
            if style.strikethrough {
                attributes.push(pango::AttrInt::new_strikethrough(true).upcast());
            }
            if style.underline {
                attributes.push(pango::AttrInt::new_underline(pango::Underline::Single).upcast());
            }
            if style.superscript || style.subscript {
                let rise = if style.superscript { 0.35 } else { -0.18 };
                attributes.push(pango::AttrInt::new_rise(units(rise * size)).upcast());
                attributes.push(pango::AttrFloat::new_scale(0.8).upcast());
            }
            if style.mark {
                attributes.push(pango::AttrColor::new_background(0xffff, 0xc8c8, 0).upcast());
                attributes.push(pango::AttrInt::new_background_alpha(channel(0.3)).upcast());
            }
            if style.code {
                attributes
                    .push(pango::AttrString::new_family(&self.options.monospace_font).upcast());
                attributes.push(pango::AttrFloat::new_scale(0.9).upcast());
                attributes.push(pango::AttrColor::new_background(0x8000, 0x8000, 0x8000).upcast());
                attributes.push(pango::AttrInt::new_background_alpha(channel(0.15)).upcast());
            }
            if style.link {
                let (red, green, blue) = LINK_COLOR;
                attributes.push(
                    pango::AttrColor::new_foreground(channel(red), channel(green), channel(blue))
                        .upcast(),
                );
                attributes.push(pango::AttrInt::new_underline(pango::Underline::Single).upcast());
            }
            if let Some(kind) = style.alert {
                let (red, green, blue) = alert_color(kind);
                attributes.push(
                    pango::AttrColor::new_foreground(channel(red), channel(green), channel(blue))
                        .upcast(),
                );
            }
            if style.footnote {
                attributes.push(pango::AttrInt::new_rise(4 * pango::SCALE).upcast());
                attributes.push(pango::AttrFloat::new_scale(0.8).upcast());
            }
            for mut attribute in attributes {
                attribute.set_start_index(saturating_u32(range.start));
                attribute.set_end_index(saturating_u32(range.end));
                list.insert(attribute);
            }
        }
        list
    }

    /// Draw `lines` of a layout whose left edge is at `x`, turning the page before a line
    /// that does not fit. `padding` is kept above the first line and below the last, and
    /// `decorate` paints under each page's part, given its top and bottom.
    fn set_lines(
        &mut self,
        lines: &[Line],
        x: f64,
        padding: f64,
        paragraph: &Paragraph,
        size: f64,
        decorate: &dyn Fn(&Self, f64, f64),
    ) -> Result<(), cairo::Error> {
        let mut start = 0;
        while start < lines.len() {
            // The lines that fit on this page, at least one.
            let offset = lines[start].top;
            let room = Self::bottom() - self.y - 2.0 * padding;
            let mut end = start + 1;
            while end < lines.len() && lines[end].top + lines[end].height - offset <= room {
                end += 1;
            }
            // A part of a paragraph or a code block is not one line alone at the end or the start
            // of a page, where it reads as a line of something else.
            if end < lines.len() && lines.len() - end == 1 && end - start > 2 {
                end -= 1;
            }
            let first_fits = lines[start].height <= room;
            let orphan = end < lines.len() && end - start < 2;
            if (!first_fits || orphan) && !self.at_top() {
                self.new_page()?;
                continue;
            }
            let height = lines[end - 1].top + lines[end - 1].height - offset;
            let top = self.y;
            decorate(self, top, top + height + 2.0 * padding);
            for line in &lines[start..end] {
                let line_top = top + padding + line.top - offset;
                self.set_color(TEXT_COLOR);
                self.cr
                    .move_to(x + line.x, line_top + line.baseline - line.top);
                pangocairo::functions::show_layout_line(&self.cr, &line.line);
                self.draw_objects(line, x, line_top, paragraph, size);
                self.anchor_line(line, x, line_top, paragraph);
                self.link_line(line, x, line_top, &paragraph.links);
            }
            self.y = top + height + 2.0 * padding;
            start = end;
            if start < lines.len() {
                self.new_page()?;
            }
        }
        self.cr.status()
    }

    /// Draw the objects of `paragraph` on `line`, set at `x` and `top` in text of `size` points.
    fn draw_objects(&self, line: &Line, x: f64, top: f64, paragraph: &Paragraph, size: f64) {
        let scale = f64::from(pango::SCALE);
        for (index, object) in &paragraph.objects {
            if !line.range.contains(index) {
                continue;
            }
            let left = line.line.index_to_x(saturating_i32(*index), false);
            object.draw(
                &self.cr,
                x + line.x + f64::from(left) / scale,
                top + line.baseline - line.top,
                size,
            );
        }
    }

    /// Make the destinations of `paragraph` on `line`, set at `x` and `top`.
    fn anchor_line(&self, line: &Line, x: f64, top: f64, paragraph: &Paragraph) {
        let scale = f64::from(pango::SCALE);
        for (index, name) in &paragraph.anchors {
            if !line.range.contains(index) {
                continue;
            }
            let left =
                x + line.x + f64::from(line.line.index_to_x(saturating_i32(*index), false)) / scale;
            self.cr.tag_begin(
                cairo::CAIRO_TAG_DEST,
                &format!("name='{}' x={left:.2} y={top:.2}", tag_string(name)),
            );
            self.cr.tag_end(cairo::CAIRO_TAG_DEST);
        }
    }

    /// Make the parts of `links` on `line`, set at `x` and `top`, follow their address.
    fn link_line(&self, line: &Line, x: f64, top: f64, links: &[(Range<usize>, String)]) {
        let scale = f64::from(pango::SCALE);
        for (range, url) in links {
            let start = range.start.max(line.range.start);
            let end = range.end.min(line.range.end);
            if start >= end {
                continue;
            }
            let bounds = line
                .line
                .x_ranges(saturating_i32(start), saturating_i32(end));
            for [from, to] in bounds.as_chunks::<2>().0 {
                let left = x + f64::from(*from) / scale;
                let width = f64::from(to - from) / scale;
                let Some(target) = link_attribute(url, &self.headings) else {
                    continue;
                };
                self.cr.tag_begin(
                    LINK_TAG,
                    &format!(
                        "rect=[{left:.2} {top:.2} {width:.2} {:.2}] {target}",
                        line.height
                    ),
                );
                self.cr.tag_end(LINK_TAG);
            }
        }
    }

    /// Paint the bars of `quote` levels of blockquote from `top` to `bottom`.
    /// The bar of an alert is in its colour. A bar goes on from the last one of its level on
    /// the page, so that the bar of a quote runs past the gaps between its blocks.
    fn quote_bars(&self, quote: Quote, top: f64, bottom: f64) {
        let mut bars = self.bars.borrow_mut();
        bars.truncate(quote.depth);
        for level in 0..quote.depth {
            let x = MARGIN + level as f64 * QUOTE_INDENT + (QUOTE_INDENT - QUOTE_BAR_WIDTH) / 2.0;
            let number = quote.numbers.get(level).copied().unwrap_or_default();
            let from = match bars.get(level) {
                Some((last, page, end)) if *last == number && *page == self.page && *end <= top => {
                    *end
                }
                _ => top,
            };
            self.set_color(
                quote
                    .alerts
                    .get(level)
                    .copied()
                    .flatten()
                    .map_or(LINE_COLOR, alert_color),
            );
            self.cr.rectangle(x, from, QUOTE_BAR_WIDTH, bottom - from);
            let _ = self.cr.fill();
            if let Some(bar) = bars.get_mut(level) {
                *bar = (number, self.page, bottom);
            } else {
                bars.push((number, self.page, bottom));
            }
        }
    }

    fn rounded_rectangle(&self, x: f64, y: f64, width: f64, height: f64) {
        rounded_path(&self.cr, x, y, width, height, CORNER_RADIUS);
    }

    fn set(&mut self, blocks: &[Block]) -> Result<(), cairo::Error> {
        self.headings = blocks
            .iter()
            .filter_map(|block| match block {
                Block::Text { id, paragraph, .. } => Some(
                    id.iter()
                        .cloned()
                        .chain(paragraph.anchors.iter().map(|(_, name)| name.clone()))
                        .collect::<Vec<_>>(),
                ),
                _ => None,
            })
            .flatten()
            .collect();
        for block in blocks {
            match block {
                Block::Text {
                    paragraph,
                    heading,
                    id,
                    indent,
                    marker,
                    quote,
                    gap,
                } => {
                    let heading = heading.map(|level| (level, id.as_deref()));
                    self.text(paragraph, heading, *indent, *marker, *quote, *gap)?;
                }
                Block::Code {
                    code,
                    runs,
                    indent,
                    quote,
                } => self.code(code, runs, *indent, *quote)?,
                Block::Table {
                    rows,
                    alignments,
                    indent,
                    quote,
                } => self.table(rows, alignments, *indent, *quote)?,
                Block::Image {
                    path,
                    alt,
                    width,
                    align,
                    indent,
                    quote,
                } => self.image(path, alt, (*width, *align), *indent, *quote)?,
                Block::Rule => self.rule()?,
            }
        }
        Ok(())
    }

    fn text(
        &mut self,
        paragraph: &Paragraph,
        heading: Option<(usize, Option<&str>)>,
        indent: f64,
        marker: usize,
        quote: Quote,
        gap: f64,
    ) -> Result<(), cairo::Error> {
        let (heading, id) = heading.unzip();
        let scale = heading.map_or(1.0, |level| {
            HEADING_SCALES[level.clamp(1, HEADING_SCALES.len()) - 1]
        });
        let layout = self.layout(&self.options.text_font, BODY_SIZE * scale);
        layout.set_width(saturating_i32_points(COLUMN_WIDTH - indent));
        layout.set_wrap(pango::WrapMode::WordChar);
        layout.set_line_spacing(LINE_SPACING);
        layout.set_text(&paragraph.text);
        // A displayed formula alone in its paragraph is centred, as in the preview.
        if paragraph.text == "\u{FFFC}"
            && matches!(
                paragraph.objects.first(),
                Some((_, Object::Math { display: true, .. }))
            )
        {
            layout.set_alignment(pango::Alignment::Center);
        }
        match paragraph.align {
            Some(HtmlAlign::Center) => layout.set_alignment(pango::Alignment::Center),
            Some(HtmlAlign::Right) => layout.set_alignment(pango::Alignment::Right),
            None => {}
        }
        let attributes = self.attributes(paragraph, BODY_SIZE * scale);
        if heading.is_some() {
            let mut bold: pango::Attribute =
                pango::AttrInt::new_weight(pango::Weight::Bold).upcast();
            bold.set_start_index(0);
            bold.set_end_index(saturating_u32(paragraph.text.len()));
            attributes.insert(bold);
        }
        if quote.depth > 0 {
            // Dim and italic as in the preview; a link or a style inside keeps its own.
            let channel = |value: f64| (value * 65535.0).round() as u16;
            let (red, green, blue) = DIM_COLOR;
            let quoted: [pango::Attribute; 2] = [
                pango::AttrInt::new_style(pango::Style::Italic).upcast(),
                pango::AttrColor::new_foreground(channel(red), channel(green), channel(blue))
                    .upcast(),
            ];
            for mut attribute in quoted {
                attribute.set_start_index(0);
                attribute.set_end_index(saturating_u32(paragraph.text.len()));
                attributes.insert_before(attribute);
            }
        }
        layout.set_attributes(Some(&attributes));
        if marker > 0 {
            // Wrapped lines of an item line up with its text rather than its marker.
            let position = layout.index_to_pos(saturating_i32(marker));
            layout.set_indent(-position.x());
        }
        let lines = lines(&layout);

        if let Some(level) = heading {
            let before = [18.0, 14.0, 12.0, 10.0][level.clamp(1, 4) - 1];
            if !self.at_top() {
                self.y += before;
            }
            // A heading does not end a page: it keeps a few lines of what follows with it, as
            // many as the first part of a code block is at least.
            let height = lines.iter().map(|line| line.height).sum::<f64>();
            self.reserve(height + 4.0 * BODY_SIZE * f64::from(LINE_SPACING))?;
            self.add_outline(level, &paragraph.text)?;
            if let Some(id) = id.flatten() {
                self.cr.tag_begin(
                    cairo::CAIRO_TAG_DEST,
                    &format!("name='{}' x={MARGIN:.2} y={:.2}", tag_string(id), self.y),
                );
                self.cr.tag_end(cairo::CAIRO_TAG_DEST);
            }
        }

        self.set_lines(
            &lines,
            MARGIN + indent,
            0.0,
            paragraph,
            BODY_SIZE * scale,
            &|setter, top, bottom| setter.quote_bars(quote, top, bottom),
        )?;
        self.y += gap;
        Ok(())
    }

    fn add_outline(&mut self, level: usize, title: &str) -> Result<(), cairo::Error> {
        while self.outline.last().is_some_and(|(open, _)| *open >= level) {
            self.outline.pop();
        }
        let parent = self
            .outline
            .last()
            .map_or(cairo::PDF_OUTLINE_ROOT, |(_, id)| *id);
        let id = self.surface.add_outline(
            parent,
            title.trim(),
            &format!("page={} pos=[{:.2} {:.2}]", self.page, MARGIN, self.y),
            cairo::PdfOutline::OPEN,
        )?;
        self.outline.push((level, id));
        Ok(())
    }

    fn code(
        &mut self,
        code: &str,
        runs: &[CodeRun],
        indent: f64,
        quote: Quote,
    ) -> Result<(), cairo::Error> {
        let width = COLUMN_WIDTH - indent;
        let layout = self.layout(&self.options.monospace_font, CODE_SIZE);
        layout.set_width(saturating_i32_points(width - 2.0 * CODE_PADDING));
        // Code cannot scroll on paper, so a long line wraps wherever it has to.
        layout.set_wrap(pango::WrapMode::Char);
        // The rest of a wrapped line is indented, so it does not read as a line of its own.
        layout.set_indent(-saturating_i32_points(2.0 * CODE_SIZE * 0.6));
        layout.set_line_spacing(CODE_LINE_SPACING);
        layout.set_text(code);
        let attributes = no_hyphens();
        let channel = |value: u8| u16::from(value) * 257;
        for run in runs {
            let mut run_attributes: Vec<pango::Attribute> = Vec::new();
            if let Some((red, green, blue)) = run.style.color {
                run_attributes.push(
                    pango::AttrColor::new_foreground(channel(red), channel(green), channel(blue))
                        .upcast(),
                );
            }
            if run.style.bold {
                run_attributes.push(pango::AttrInt::new_weight(pango::Weight::Bold).upcast());
            }
            if run.style.italic {
                run_attributes.push(pango::AttrInt::new_style(pango::Style::Italic).upcast());
            }
            for mut attribute in run_attributes {
                attribute.set_start_index(saturating_u32(run.range.start));
                attribute.set_end_index(saturating_u32(run.range.end));
                attributes.insert(attribute);
            }
        }
        layout.set_attributes(Some(&attributes));
        let lines = lines(&layout);

        let x = MARGIN + indent;
        self.set_lines(
            &lines,
            x + CODE_PADDING,
            CODE_PADDING,
            &Paragraph::default(),
            CODE_SIZE,
            // Framed like a table, as the preview frames code blocks.
            &|setter, top, bottom| {
                setter.quote_bars(quote, top, bottom);
                setter.set_color(LINE_COLOR);
                setter.cr.set_line_width(0.6);
                setter.rounded_rectangle(x, top, width, bottom - top);
                let _ = setter.cr.stroke();
            },
        )?;
        self.y += PARAGRAPH_GAP + 2.0;
        Ok(())
    }

    fn table(
        &mut self,
        rows: &[Vec<Paragraph>],
        alignments: &[Alignment],
        indent: f64,
        quote: Quote,
    ) -> Result<(), cairo::Error> {
        let columns = rows.iter().map(Vec::len).max().unwrap_or(0);
        if columns == 0 {
            return Ok(());
        }
        let available = COLUMN_WIDTH - indent;
        let cell_layout = |row: usize, column: usize, size: f64| {
            let layout = self.layout(&self.options.text_font, size);
            layout.set_wrap(pango::WrapMode::WordChar);
            layout.set_line_spacing(LINE_SPACING);
            let paragraph = rows[row].get(column);
            layout.set_text(paragraph.map_or("", |paragraph| paragraph.text.as_str()));
            if let Some(paragraph) = paragraph {
                let attributes = self.attributes(paragraph, size);
                // The first row is the header, bold as in the preview.
                if row == 0 {
                    let mut bold: pango::Attribute =
                        pango::AttrInt::new_weight(pango::Weight::Bold).upcast();
                    bold.set_start_index(0);
                    bold.set_end_index(saturating_u32(paragraph.text.len()));
                    attributes.insert(bold);
                }
                layout.set_attributes(Some(&attributes));
            }
            layout.set_alignment(match alignments.get(column) {
                Some(Alignment::Center) => pango::Alignment::Center,
                Some(Alignment::Right) => pango::Alignment::Right,
                _ => pango::Alignment::Left,
            });
            layout
        };

        // Each column is at least as wide as its longest word, and would like to be as wide
        // as its widest cell, a cell wrapping, as in the preview, after about forty
        // characters.
        let scale = f64::from(pango::SCALE);
        let measure = |size: f64| -> (Vec<f64>, Vec<f64>) {
            let wrap_width = {
                let layout = cell_layout(0, 0, size);
                let metrics = layout
                    .context()
                    .metrics(layout.font_description().as_ref(), None);
                f64::from(metrics.approximate_char_width()) / scale * CELL_WRAP_CHARS
            };
            (0..columns)
                .map(|column| {
                    (0..rows.len())
                        .map(|row| {
                            let layout = cell_layout(row, column, size);
                            let natural = f64::from(layout.extents().1.width()) / scale;
                            // And a point more, for the rounding of the widths of glyphs.
                            let minimum = longest_word(&layout) + 1.0;
                            (minimum, natural.min(wrap_width).max(minimum))
                        })
                        .fold(
                            (0.0, 0.0),
                            |(minimum, natural), (cell_minimum, cell_natural)| {
                                (
                                    f64::max(minimum, cell_minimum),
                                    f64::max(natural, cell_natural),
                                )
                            },
                        )
                })
                .map(|(minimum, natural)| {
                    (
                        minimum + 2.0 * cell_padding(size),
                        natural + 2.0 * cell_padding(size),
                    )
                })
                .unzip()
        };
        // A table too wide for its longest words is set smaller, and its cells padded less,
        // down to a size still easy to read, before words break.
        let mut size = BODY_SIZE;
        let (mut minimum, mut natural) = measure(size);
        while minimum.iter().sum::<f64>() > available && size > MIN_TABLE_SIZE {
            size = (size - 0.5).max(MIN_TABLE_SIZE);
            (minimum, natural) = measure(size);
        }
        let widths = column_widths(&minimum, &natural, available);

        let x = MARGIN + indent;
        let layouts: Vec<Vec<pango::Layout>> = (0..rows.len())
            .map(|row| {
                (0..columns)
                    .map(|column| {
                        let layout = cell_layout(row, column, size);
                        layout.set_width(saturating_i32_points(
                            widths[column] - 2.0 * cell_padding(size),
                        ));
                        layout
                    })
                    .collect()
            })
            .collect();
        // The lines of each cell, moved down so that the first lines of a row share a
        // baseline, and the height of each row.
        let cells: Vec<Vec<(f64, Vec<Line>)>> = layouts
            .iter()
            .map(|row| {
                let lines: Vec<Vec<Line>> = row.iter().map(lines).collect();
                let first_baseline =
                    |lines: &[Line]| lines.first().map_or(0.0, |line| line.baseline - line.top);
                let baseline = lines
                    .iter()
                    .map(|lines| first_baseline(lines))
                    .fold(0.0, f64::max);
                lines
                    .into_iter()
                    .map(|lines| (baseline - first_baseline(&lines), lines))
                    .collect()
            })
            .collect();
        let heights: Vec<f64> = cells
            .iter()
            .map(|row| {
                row.iter()
                    .map(|(shift, lines)| {
                        let (Some(first), Some(last)) = (lines.first(), lines.last()) else {
                            return 0.0;
                        };
                        shift + last.top + last.height - first.top
                    })
                    .fold(0.0, f64::max)
                    + 2.0 * CELL_PADDING_Y
            })
            .collect();

        self.y += 4.0;
        self.reserve(heights.iter().take(2).sum())?;
        let mut top = self.y;
        let mut row = 0;
        while row < rows.len() {
            let repeat_header = row > 0 && self.y == top && top <= MARGIN && rows.len() > 1;
            if repeat_header {
                self.table_row(0, &cells[0], &widths, heights[0], x, rows, size, quote)?;
            }
            if self.y + heights[row] > Self::bottom() && self.y > top {
                self.table_frame(x, top, &widths);
                self.new_page()?;
                top = self.y;
                continue;
            }
            self.table_row(
                row,
                &cells[row],
                &widths,
                heights[row],
                x,
                rows,
                size,
                quote,
            )?;
            row += 1;
        }
        self.table_frame(x, top, &widths);
        self.y += PARAGRAPH_GAP + 6.0;
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn table_row(
        &mut self,
        row: usize,
        cells: &[(f64, Vec<Line>)],
        widths: &[f64],
        height: f64,
        x: f64,
        rows: &[Vec<Paragraph>],
        size: f64,
        quote: Quote,
    ) -> Result<(), cairo::Error> {
        let top = self.y;
        self.quote_bars(quote, top, top + height);
        if row == 0 {
            // The header opens each part of the table, so its tint follows the rounded
            // top corners of the frame.
            let width: f64 = widths.iter().sum();
            self.cr.save()?;
            self.rounded_rectangle(x, top, width, height + 2.0 * CORNER_RADIUS);
            self.cr.clip();
            self.set_color(TINT_COLOR);
            self.cr.rectangle(x, top, width, height);
            self.cr.fill()?;
            self.cr.restore()?;
        } else {
            self.set_color(LINE_COLOR);
            self.cr.set_line_width(0.6);
            self.cr.move_to(x, top);
            self.cr.line_to(x + widths.iter().sum::<f64>(), top);
            self.cr.stroke()?;
        }
        let mut left = x;
        for (column, (shift, lines)) in cells.iter().enumerate() {
            let cell_x = left + cell_padding(size);
            let cell_top =
                top + CELL_PADDING_Y + shift - lines.first().map_or(0.0, |line| line.top);
            let paragraph = rows[row].get(column);
            let links = paragraph.map_or(&[][..], |paragraph| paragraph.links.as_slice());
            for line in lines {
                self.set_color(TEXT_COLOR);
                self.cr.move_to(cell_x + line.x, cell_top + line.baseline);
                pangocairo::functions::show_layout_line(&self.cr, &line.line);
                if let Some(paragraph) = paragraph {
                    self.draw_objects(line, cell_x, cell_top + line.top, paragraph, size);
                }
                self.link_line(line, cell_x, cell_top + line.top, links);
            }
            left += widths[column];
        }
        self.y += height;
        Ok(())
    }

    /// The border of the part of a table from `top` to the current position.
    fn table_frame(&self, x: f64, top: f64, widths: &[f64]) {
        let width: f64 = widths.iter().sum();
        self.set_color(LINE_COLOR);
        self.cr.set_line_width(0.6);
        let mut left = x;
        for column_width in &widths[..widths.len() - 1] {
            left += column_width;
            self.cr.move_to(left, top);
            self.cr.line_to(left, self.y);
        }
        let _ = self.cr.stroke();
        self.rounded_rectangle(x, top, width, self.y - top);
        let _ = self.cr.stroke();
    }

    fn image(
        &mut self,
        path: &std::path::Path,
        alt: &str,
        (shown_width, align): (Option<i32>, Option<HtmlAlign>),
        indent: f64,
        quote: Quote,
    ) -> Result<(), cairo::Error> {
        let available = COLUMN_WIDTH - indent;
        let Some((_, pixel_width, pixel_height)) = gdk_pixbuf::Pixbuf::file_info(path) else {
            return self.image_alt(alt, indent, quote);
        };
        let (shown_width, shown_height) =
            markdown::shown_size((pixel_width, pixel_height), shown_width);
        let (pixel_width, pixel_height) = (
            f64::from(pixel_width.max(1)),
            f64::from(pixel_height.max(1)),
        );
        let mut width = (f64::from(shown_width.max(1)) * POINTS_PER_PIXEL).min(available);
        let mut height = width * f64::from(shown_height.max(1)) / f64::from(shown_width.max(1));
        let page_height = Self::bottom() - MARGIN;
        if height > page_height {
            height = page_height;
            width = height * pixel_width / pixel_height;
        }
        let decode_width = (width * DECODED_PIXELS_PER_POINT).ceil();
        let pixbuf = if pixel_width > decode_width {
            gdk_pixbuf::Pixbuf::from_file_at_scale(path, decode_width as i32, -1, true)
        } else {
            gdk_pixbuf::Pixbuf::from_file(path)
        };
        let Ok(pixbuf) = pixbuf else {
            return self.image_alt(alt, indent, quote);
        };

        self.y += 4.0;
        self.reserve(height)?;
        let x = MARGIN
            + indent
            + match align {
                Some(HtmlAlign::Right) => available - width,
                _ => (available - width) / 2.0,
            };
        self.quote_bars(quote, self.y, self.y + height);
        self.cr.save()?;
        self.cr.translate(x, self.y);
        self.cr.scale(
            width / f64::from(pixbuf.width()),
            height / f64::from(pixbuf.height()),
        );
        self.cr.set_source_pixbuf(&pixbuf, 0.0, 0.0);
        self.cr.paint()?;
        self.cr.restore()?;
        self.y += height + PARAGRAPH_GAP + 4.0;
        Ok(())
    }

    /// The alternative text of an image that could not be decoded.
    fn image_alt(&mut self, alt: &str, indent: f64, quote: Quote) -> Result<(), cairo::Error> {
        if alt.is_empty() {
            return Ok(());
        }
        let mut paragraph = Paragraph::default();
        paragraph.push(
            alt,
            Inline {
                italic: true,
                ..Inline::default()
            },
        );
        self.text(&paragraph, None, indent, 0, quote, PARAGRAPH_GAP)
    }

    fn rule(&mut self) -> Result<(), cairo::Error> {
        self.reserve(12.0)?;
        self.y += 6.0;
        self.set_color(LINE_COLOR);
        self.cr.set_line_width(0.8);
        self.cr.move_to(MARGIN, self.y);
        self.cr.line_to(MARGIN + COLUMN_WIDTH, self.y);
        self.cr.stroke()?;
        self.y += 6.0 + PARAGRAPH_GAP;
        Ok(())
    }
}

/// Attributes that break a word without a hyphen, which would read as part of code or of
/// an address.
fn no_hyphens() -> pango::AttrList {
    let list = pango::AttrList::new();
    list.insert(pango::AttrInt::new_insert_hyphens(false));
    list
}

/// The space at either side of the text of a table cell whose text is `size` points.
fn cell_padding(size: f64) -> f64 {
    CELL_PADDING_X * size / BODY_SIZE
}

/// The width of the widest word of the text of `layout`, which is not wrapped, in points.
/// Words are what lies between spaces, where lines of a table cell mostly break, and each
/// ideograph or kana is a word of its own, as lines of Chinese and Japanese break between
/// any two of them.
fn longest_word(layout: &pango::Layout) -> f64 {
    use gtk::glib::{Unichar, UnicodeBreakType};

    let text = layout.text();
    // The left and the right edge of the character at `index`.
    let edges = |index: usize| {
        let position = layout.index_to_pos(saturating_i32(index));
        let scale = f64::from(pango::SCALE);
        let (x, width) = (f64::from(position.x()), f64::from(position.width()));
        ((x + width.min(0.0)) / scale, (x + width.max(0.0)) / scale)
    };
    let width = |(first, last): (usize, usize)| {
        let ((first_left, first_right), (last_left, last_right)) = (edges(first), edges(last));
        last_right.max(first_right) - first_left.min(last_left)
    };
    let mut widest = 0.0_f64;
    // The first and the last character of the word read so far.
    let mut word: Option<(usize, usize)> = None;
    for (index, c) in text.char_indices().chain([(text.len(), ' ')]) {
        let break_type = c.break_type();
        let alone = matches!(
            break_type,
            UnicodeBreakType::Ideographic | UnicodeBreakType::ConditionalJapaneseStarter
        );
        if c.is_whitespace() || break_type == UnicodeBreakType::ZeroWidthSpace || alone {
            if let Some(read) = word.take() {
                widest = widest.max(width(read));
            }
            if alone {
                widest = widest.max(width((index, index)));
            }
        } else {
            word = Some((word.map_or(index, |(first, _)| first), index));
        }
    }
    widest
}

/// Widths of columns that need at least `minimum` points and would like `natural`, to fill
/// `available`. Past their minimums, columns share the width in proportion to how much
/// more they would like, and fill what is left over the same way.
fn column_widths(minimum: &[f64], natural: &[f64], available: f64) -> Vec<f64> {
    let total_natural: f64 = natural.iter().sum();
    if total_natural <= available {
        return natural
            .iter()
            .map(|width| width + (available - total_natural) * width / total_natural.max(1.0))
            .collect();
    }
    let total_minimum: f64 = minimum.iter().sum();
    if total_minimum >= available {
        // Not even the longest words fit: words break, in proportion.
        return minimum
            .iter()
            .map(|width| available * width / total_minimum.max(1.0))
            .collect();
    }
    let wanted = total_natural - total_minimum;
    minimum
        .iter()
        .zip(natural)
        .map(|(minimum, natural)| {
            minimum + (available - total_minimum) * (natural - minimum) / wanted.max(1.0)
        })
        .collect()
}

fn saturating_u32(value: usize) -> u32 {
    u32::try_from(value).unwrap_or(u32::MAX)
}

fn saturating_i32(value: usize) -> i32 {
    i32::try_from(value).unwrap_or(i32::MAX)
}

/// A length in points as Pango units.
fn saturating_i32_points(points: f64) -> i32 {
    (points.max(1.0) * f64::from(pango::SCALE)) as i32
}

/// The colour of the title and the bar of an alert of `kind`, as in the light style of the HTML
/// export. Not libadwaita's accent colours, which cannot be asked for off the main thread
/// the PDF is set on.
fn alert_color(kind: BlockQuoteKind) -> Rgb {
    let (red, green, blue) = match kind {
        BlockQuoteKind::Note => (0x04, 0x61, 0xbe),
        BlockQuoteKind::Tip => (0x15, 0x77, 0x2e),
        BlockQuoteKind::Important => (0x89, 0x39, 0xa4),
        BlockQuoteKind::Warning => (0x90, 0x53, 0x00),
        BlockQuoteKind::Caution => (0xc0, 0x00, 0x23),
    };
    let channel = |value: u8| f64::from(value) / 255.0;
    (channel(red), channel(green), channel(blue))
}

/// Add a rectangle with corners rounded by `radius`, or less for a small one, to the path of
/// `cr`.
fn rounded_path(cr: &cairo::Context, x: f64, y: f64, width: f64, height: f64, radius: f64) {
    let radius = radius.min(width / 2.0).min(height / 2.0);
    let degrees = std::f64::consts::PI / 180.0;
    cr.new_sub_path();
    cr.arc(x + width - radius, y + radius, radius, -90.0 * degrees, 0.0);
    cr.arc(
        x + width - radius,
        y + height - radius,
        radius,
        0.0,
        90.0 * degrees,
    );
    cr.arc(
        x + radius,
        y + height - radius,
        radius,
        90.0 * degrees,
        180.0 * degrees,
    );
    cr.arc(
        x + radius,
        y + radius,
        radius,
        180.0 * degrees,
        270.0 * degrees,
    );
    cr.close_path();
}

/// `text` as a string in the attributes of a cairo tag.
fn tag_string(text: &str) -> String {
    text.replace('\\', "\\\\").replace('\'', "\\'")
}

/// Where a cairo link to `url` goes, as a tag attribute: a web or mail address, or the
/// destination of one of `headings`. Cairo makes a link to a destination the document lacks
/// go to a page of its choosing, so such a link is left out.
fn link_attribute(url: &str, headings: &HashSet<String>) -> Option<String> {
    match markdown::link_target(url, None)? {
        markdown::LinkTarget::Web(uri) => Some(format!("uri='{}'", tag_string(&uri))),
        markdown::LinkTarget::Heading(id) if headings.contains(&id) => {
            Some(format!("dest='{}'", tag_string(&id)))
        }
        _ => None,
    }
}

/// The document as a PDF file.
pub fn render_pdf(text: &str, options: &Options) -> Result<Vec<u8>, cairo::Error> {
    typeset(text, options).map(|(pdf, _)| pdf)
}

/// The document as a PDF file, and how many pages it has.
fn typeset(text: &str, options: &Options) -> Result<(Vec<u8>, u32), cairo::Error> {
    let blocks = Reader::new(options).read(text);
    let mut setter = Typesetter::new(options)?;
    setter.set(&blocks)?;
    let pages = setter.page;
    Ok((setter.finish()?, pages))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn options() -> Options {
        Options {
            title: String::from("doc"),
            base_dir: None,
            text_font: String::from("Sans"),
            monospace_font: String::from("Monospace"),
            width: 900,
            light: Vec::new(),
            dark: Vec::new(),
        }
    }

    fn blocks(text: &str) -> Vec<Block> {
        Reader::new(&options()).read(text)
    }

    #[test]
    fn list_items_hang_after_their_markers() {
        let blocks = blocks("- one\n- two\n  1. nested");
        let texts: Vec<(&str, f64, usize)> = blocks
            .iter()
            .filter_map(|block| match block {
                Block::Text {
                    paragraph,
                    indent,
                    marker,
                    ..
                } => Some((paragraph.text.as_str(), *indent, *marker)),
                _ => None,
            })
            .collect();
        assert_eq!(
            texts,
            [
                ("• one", 0.0, "• ".len()),
                ("• two", 0.0, "• ".len()),
                ("1. nested", LIST_INDENT, "1. ".len()),
            ]
        );
    }

    #[test]
    fn links_are_kept_only_for_web_and_mail() {
        let blocks = blocks("[web](https://example.com) [bad](javascript:x) [mail](mailto:a@b)");
        let Some(Block::Text { paragraph, .. }) = blocks.first() else {
            panic!("a paragraph");
        };
        let links: Vec<(&str, &str)> = paragraph
            .links
            .iter()
            .map(|(range, url)| (&paragraph.text[range.clone()], url.as_str()))
            .collect();
        assert_eq!(
            links,
            [("web", "https://example.com"), ("mail", "mailto:a@b")]
        );
    }

    #[test]
    fn images_outside_the_document_folder_leave_their_text() {
        let blocks = blocks("![a picture](/etc/hostname)");
        let Some(Block::Text { paragraph, .. }) = blocks.first() else {
            panic!("a paragraph");
        };
        assert_eq!(paragraph.text, "a picture");
    }

    #[test]
    fn alerts_are_set_off_the_main_thread() {
        // Tests run on threads of their own, as the export sets a PDF on one.
        let (pdf, _) = typeset("> [!WARNING]\n> Careful\n", &options()).expect("a PDF");
        assert!(pdf.starts_with(b"%PDF-"));
    }

    #[test]
    fn raw_html_sets_no_empty_lines() {
        let texts: Vec<String> = blocks(
            "<div>\n  <b>Bold</b><br>\n  next\n</div>\n\n<details>\n<summary>More</summary>\n\nInside\n\n</details>\n",
        )
        .into_iter()
        .filter_map(|block| match block {
            Block::Text { paragraph, .. } => Some(paragraph.text),
            _ => None,
        })
        .collect();
        assert_eq!(texts, ["Bold\nnext", "More", "Inside"]);
    }

    #[test]
    fn blocks_of_raw_html_are_paragraphs_aligned_as_they_say() {
        let texts: Vec<(String, Option<HtmlAlign>)> =
            blocks("<p align=\"center\">One</p><p>Two</p>\n\n<div align=\"right\">\nThree\n")
                .into_iter()
                .filter_map(|block| match block {
                    Block::Text { paragraph, .. } => Some((paragraph.text, paragraph.align)),
                    _ => None,
                })
                .collect();
        assert_eq!(
            texts,
            [
                (String::from("One"), Some(HtmlAlign::Center)),
                (String::from("Two"), None),
                (String::from("Three"), Some(HtmlAlign::Right)),
            ]
        );
    }

    #[test]
    fn task_boxes_take_the_place_of_bullets() {
        let texts: Vec<String> = blocks("- [x] done\n- [ ] to do\n\n1. [ ] first\n")
            .into_iter()
            .filter_map(|block| match block {
                Block::Text { paragraph, .. } => Some(paragraph.text),
                _ => None,
            })
            .collect();
        assert_eq!(
            texts,
            ["\u{FFFC} done", "\u{FFFC} to do", "1. \u{FFFC} first"]
        );
    }

    #[test]
    fn links_to_headings_that_exist_are_kept() {
        let headings = HashSet::from([String::from("top")]);
        assert_eq!(
            link_attribute("#top", &headings).as_deref(),
            Some("dest='top'")
        );
        assert_eq!(link_attribute("#missing", &headings), None);
        assert_eq!(
            link_attribute("https://a.example/it's", &headings).as_deref(),
            Some("uri='https://a.example/it\\'s'")
        );
        let (pdf, _) = typeset("# Top\n\n[up](#top)\n", &options()).expect("a PDF");
        assert!(pdf.starts_with(b"%PDF-"));
    }

    #[test]
    fn columns_fill_the_width_or_share_it() {
        assert_eq!(
            column_widths(&[5.0, 5.0], &[10.0, 30.0], 80.0),
            [20.0, 60.0]
        );
        // A column that wants little keeps its longest word whole while a long one wraps.
        let widths = column_widths(&[20.0, 60.0, 40.0], &[20.0, 300.0, 100.0], 200.0);
        assert_eq!(widths[0], 20.0);
        assert!(widths[1] > 60.0 && widths[2] > 40.0);
        assert!((widths.iter().sum::<f64>() - 200.0).abs() < 1e-9);
        assert_eq!(
            column_widths(&[100.0, 300.0], &[200.0, 400.0], 200.0),
            [50.0, 150.0]
        );
    }

    #[test]
    fn ideographs_are_words_of_their_own() {
        let options = options();
        let setter = Typesetter::new(&options).expect("a typesetter");
        let layout = setter.layout("Sans", BODY_SIZE);
        layout.set_text("日本語のテキスト");
        let sentence = f64::from(layout.extents().1.width()) / f64::from(pango::SCALE);
        assert!(longest_word(&layout) < sentence / 4.0);
        layout.set_text("a longword b");
        let widest = longest_word(&layout);
        layout.set_text("longword");
        let word = f64::from(layout.extents().1.width()) / f64::from(pango::SCALE);
        assert!((widest - word).abs() < 1.0);
    }

    #[test]
    fn renders_a_pdf_of_several_pages() {
        let mut text =
            String::from("# Title\n\n| a | b |\n|---|---|\n| 1 | 2 |\n\n```\ncode\n```\n\n");
        for index in 0..200 {
            text.push_str(&format!(
                "Paragraph {index} with [a link](https://example.com).\n\n"
            ));
        }
        let (pdf, pages) = typeset(&text, &options()).expect("a PDF");
        assert!(pdf.starts_with(b"%PDF-"));
        assert!(pages > 1, "{pages} pages");
    }
}
