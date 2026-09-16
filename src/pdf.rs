//! PDF export. The document is set on A4 pages with Pango and written by cairo, so the text
//! stays text, web links can be followed and the headings make the outline. Like the
//! preview, it shows only images of the document's folder and follows only web and mail
//! links.

use gtk::gdk::prelude::GdkCairoContextExt;
use gtk::{cairo, gdk_pixbuf, pango};
use pango::prelude::*;
use pulldown_cmark::{Alignment, CodeBlockKind, Event, Parser, Tag, TagEnd};
use std::ops::Range;
use std::path::PathBuf;

use crate::export::Options;
use crate::markdown::{self, CodeRun};

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

/// How a stretch of text looks.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Inline {
    bold: bool,
    italic: bool,
    strikethrough: bool,
    code: bool,
    link: bool,
    footnote: bool,
}

/// Text with its styled stretches and links, as byte ranges of the text.
#[derive(Debug, Default)]
struct Paragraph {
    text: String,
    styles: Vec<(Range<usize>, Inline)>,
    links: Vec<(Range<usize>, String)>,
}

impl Paragraph {
    fn push(&mut self, text: &str, style: Inline) {
        let start = self.text.len();
        self.text.push_str(text);
        if style != Inline::default() && !text.is_empty() {
            self.styles.push((start..self.text.len(), style));
        }
    }
}

#[derive(Debug)]
enum Block {
    Text {
        paragraph: Paragraph,
        /// The heading level, from 1.
        heading: Option<usize>,
        indent: f64,
        /// The length of the list marker that opens the text, which wrapped lines hang
        /// after.
        marker: usize,
        quote: usize,
        gap: f64,
    },
    Code {
        code: String,
        runs: Vec<CodeRun>,
        indent: f64,
        quote: usize,
    },
    Table {
        rows: Vec<Vec<Paragraph>>,
        alignments: Vec<Alignment>,
        indent: f64,
        quote: usize,
    },
    Image {
        path: PathBuf,
        alt: String,
        indent: f64,
        quote: usize,
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
    style: Inline,
    lists: Vec<Option<u64>>,
    quote: usize,
    links: Vec<(String, usize)>,
    /// Whether each open image is shown, which hides its alternative text.
    images: Vec<bool>,
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
            style: Inline::default(),
            lists: Vec::new(),
            quote: 0,
            links: Vec::new(),
            images: Vec::new(),
            code: None,
            code_blocks: 0,
            table: None,
        }
    }

    fn indent(&self) -> f64 {
        self.lists.len() as f64 * LIST_INDENT + self.quote as f64 * QUOTE_INDENT
    }

    fn has_text(&self) -> bool {
        !self.paragraph.text[self.prefix..].trim().is_empty()
    }

    /// Set the paragraph read so far, if it has more than its prefix, followed by `gap`.
    fn flush(&mut self, gap: f64) {
        let paragraph = std::mem::take(&mut self.paragraph);
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
            indent,
            marker,
            quote: self.quote,
            gap,
        });
    }

    /// Leave at least `gap` after the last block.
    fn widen_gap(&mut self, gap: f64) {
        if let Some(Block::Text { gap: last, .. }) = self.blocks.last_mut() {
            *last = last.max(gap);
        }
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
                ..self.style
            };
            self.paragraph.push(text, style);
            return;
        }
        self.paragraph.push(text, self.style);
    }

    fn read(mut self, text: &str) -> Vec<Block> {
        for event in Parser::new_ext(text, markdown::parser_options()) {
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
            Event::Html(html) | Event::InlineHtml(html) => {
                self.push_text(&markdown::strip_html(&html));
            }
            Event::SoftBreak => self.push_text(" "),
            Event::HardBreak => self.push_text("\n"),
            Event::FootnoteReference(name) => {
                let style = Inline {
                    footnote: true,
                    ..self.style
                };
                self.paragraph.push(&format!("[{name}]"), style);
            }
            Event::TaskListMarker(checked) => {
                let style = Inline {
                    bold: true,
                    ..self.style
                };
                self.paragraph
                    .push(if checked { "■ " } else { "□ " }, style);
            }
            Event::Rule => {
                self.flush(PARAGRAPH_GAP);
                self.blocks.push(Block::Rule);
            }
            _ => {}
        }
    }

    fn start(&mut self, tag: Tag) {
        match tag {
            Tag::Paragraph => {
                if self.has_text() {
                    self.flush(PARAGRAPH_GAP);
                }
            }
            Tag::Heading { level, .. } => {
                self.flush(PARAGRAPH_GAP);
                self.heading = Some(level as usize);
            }
            Tag::BlockQuote(_) => {
                self.flush(PARAGRAPH_GAP);
                self.quote += 1;
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
            Tag::Image { dest_url, .. } => {
                let path = markdown::local_image_path(&dest_url, self.options.base_dir.as_deref());
                self.images.push(path.is_some());
                if let Some(path) = path {
                    self.flush(PARAGRAPH_GAP);
                    self.blocks.push(Block::Image {
                        path,
                        alt: String::new(),
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
            TagEnd::Paragraph => self.flush(PARAGRAPH_GAP),
            TagEnd::Heading(_) => {
                self.flush(PARAGRAPH_GAP);
                self.heading = None;
            }
            TagEnd::BlockQuote(_) => {
                self.flush(PARAGRAPH_GAP);
                self.quote = self.quote.saturating_sub(1);
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
            TagEnd::FootnoteDefinition => self.flush(PARAGRAPH_GAP),
            TagEnd::TableCell => {
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
                    && markdown::is_safe_link(&url)
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
    fn attributes(&self, paragraph: &Paragraph) -> pango::AttrList {
        let list = no_hyphens();
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
        links: &[(Range<usize>, String)],
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
            let first_fits = lines[start].height <= room;
            if !first_fits && !self.at_top() {
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
                self.link_line(line, x, line_top, links);
            }
            self.y = top + height + 2.0 * padding;
            start = end;
            if start < lines.len() {
                self.new_page()?;
            }
        }
        self.cr.status()
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
                let uri = url.replace('\\', "\\\\").replace('\'', "\\'");
                self.cr.tag_begin(
                    LINK_TAG,
                    &format!(
                        "rect=[{left:.2} {top:.2} {width:.2} {:.2}] uri='{uri}'",
                        line.height
                    ),
                );
                self.cr.tag_end(LINK_TAG);
            }
        }
    }

    /// Paint the bars of `quote` levels of blockquote from `top` to `bottom`.
    fn quote_bars(&self, quote: usize, top: f64, bottom: f64) {
        self.set_color(LINE_COLOR);
        for level in 0..quote {
            let x = MARGIN + level as f64 * QUOTE_INDENT + (QUOTE_INDENT - QUOTE_BAR_WIDTH) / 2.0;
            self.cr.rectangle(x, top, QUOTE_BAR_WIDTH, bottom - top);
            let _ = self.cr.fill();
        }
    }

    fn rounded_rectangle(&self, x: f64, y: f64, width: f64, height: f64) {
        let radius = CORNER_RADIUS.min(width / 2.0).min(height / 2.0);
        let degrees = std::f64::consts::PI / 180.0;
        self.cr.new_sub_path();
        self.cr
            .arc(x + width - radius, y + radius, radius, -90.0 * degrees, 0.0);
        self.cr.arc(
            x + width - radius,
            y + height - radius,
            radius,
            0.0,
            90.0 * degrees,
        );
        self.cr.arc(
            x + radius,
            y + height - radius,
            radius,
            90.0 * degrees,
            180.0 * degrees,
        );
        self.cr.arc(
            x + radius,
            y + radius,
            radius,
            180.0 * degrees,
            270.0 * degrees,
        );
        self.cr.close_path();
    }

    fn set(&mut self, blocks: &[Block]) -> Result<(), cairo::Error> {
        for block in blocks {
            match block {
                Block::Text {
                    paragraph,
                    heading,
                    indent,
                    marker,
                    quote,
                    gap,
                } => self.text(paragraph, *heading, *indent, *marker, *quote, *gap)?,
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
                    indent,
                    quote,
                } => self.image(path, alt, *indent, *quote)?,
                Block::Rule => self.rule()?,
            }
        }
        Ok(())
    }

    fn text(
        &mut self,
        paragraph: &Paragraph,
        heading: Option<usize>,
        indent: f64,
        marker: usize,
        quote: usize,
        gap: f64,
    ) -> Result<(), cairo::Error> {
        let scale = heading.map_or(1.0, |level| {
            HEADING_SCALES[level.clamp(1, HEADING_SCALES.len()) - 1]
        });
        let layout = self.layout(&self.options.text_font, BODY_SIZE * scale);
        layout.set_width(saturating_i32_points(COLUMN_WIDTH - indent));
        layout.set_wrap(pango::WrapMode::WordChar);
        layout.set_line_spacing(LINE_SPACING);
        layout.set_text(&paragraph.text);
        let attributes = self.attributes(paragraph);
        if heading.is_some() {
            let mut bold: pango::Attribute =
                pango::AttrInt::new_weight(pango::Weight::Bold).upcast();
            bold.set_start_index(0);
            bold.set_end_index(saturating_u32(paragraph.text.len()));
            attributes.insert(bold);
        }
        if quote > 0 {
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
            // A heading does not end a page: it keeps a few lines of what follows with it.
            let height = lines.iter().map(|line| line.height).sum::<f64>();
            self.reserve(height + 3.0 * BODY_SIZE * f64::from(LINE_SPACING))?;
            self.add_outline(level, &paragraph.text)?;
        }

        self.set_lines(
            &lines,
            MARGIN + indent,
            0.0,
            &paragraph.links,
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
        quote: usize,
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
            &[],
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
        quote: usize,
    ) -> Result<(), cairo::Error> {
        let columns = rows.iter().map(Vec::len).max().unwrap_or(0);
        if columns == 0 {
            return Ok(());
        }
        let available = COLUMN_WIDTH - indent;
        let cell_layout = |row: usize, column: usize| {
            let layout = self.layout(&self.options.text_font, BODY_SIZE);
            layout.set_wrap(pango::WrapMode::WordChar);
            layout.set_line_spacing(LINE_SPACING);
            let paragraph = rows[row].get(column);
            layout.set_text(paragraph.map_or("", |paragraph| paragraph.text.as_str()));
            if let Some(paragraph) = paragraph {
                let attributes = self.attributes(paragraph);
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
        let wrap_width = {
            let layout = cell_layout(0, 0);
            let metrics = layout
                .context()
                .metrics(layout.font_description().as_ref(), None);
            f64::from(metrics.approximate_char_width()) / scale * CELL_WRAP_CHARS
        };
        let (minimum, natural): (Vec<f64>, Vec<f64>) = (0..columns)
            .map(|column| {
                (0..rows.len())
                    .map(|row| {
                        let layout = cell_layout(row, column);
                        let natural = f64::from(layout.extents().1.width()) / scale;
                        layout.set_wrap(pango::WrapMode::Word);
                        layout.set_width(1);
                        let minimum = f64::from(layout.extents().1.width()) / scale;
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
                    minimum + 2.0 * CELL_PADDING_X,
                    natural + 2.0 * CELL_PADDING_X,
                )
            })
            .unzip();
        let widths = column_widths(&minimum, &natural, available);

        let x = MARGIN + indent;
        let layouts: Vec<Vec<pango::Layout>> = (0..rows.len())
            .map(|row| {
                (0..columns)
                    .map(|column| {
                        let layout = cell_layout(row, column);
                        layout.set_width(saturating_i32_points(
                            widths[column] - 2.0 * CELL_PADDING_X,
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
                self.table_row(0, &cells[0], &widths, heights[0], x, rows, quote)?;
            }
            if self.y + heights[row] > Self::bottom() && self.y > top {
                self.table_frame(x, top, &widths);
                self.new_page()?;
                top = self.y;
                continue;
            }
            self.table_row(row, &cells[row], &widths, heights[row], x, rows, quote)?;
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
        quote: usize,
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
            let cell_x = left + CELL_PADDING_X;
            let cell_top =
                top + CELL_PADDING_Y + shift - lines.first().map_or(0.0, |line| line.top);
            let links = rows[row]
                .get(column)
                .map_or(&[][..], |paragraph| paragraph.links.as_slice());
            for line in lines {
                self.set_color(TEXT_COLOR);
                self.cr.move_to(cell_x + line.x, cell_top + line.baseline);
                pangocairo::functions::show_layout_line(&self.cr, &line.line);
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
        indent: f64,
        quote: usize,
    ) -> Result<(), cairo::Error> {
        let available = COLUMN_WIDTH - indent;
        let Some((_, pixel_width, pixel_height)) = gdk_pixbuf::Pixbuf::file_info(path) else {
            return self.image_alt(alt, indent, quote);
        };
        let (pixel_width, pixel_height) = (
            f64::from(pixel_width.max(1)),
            f64::from(pixel_height.max(1)),
        );
        let mut width = (pixel_width * POINTS_PER_PIXEL).min(available);
        let mut height = width * pixel_height / pixel_width;
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
        let x = MARGIN + indent + (available - width) / 2.0;
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
    fn image_alt(&mut self, alt: &str, indent: f64, quote: usize) -> Result<(), cairo::Error> {
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
