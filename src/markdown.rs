use adw::prelude::*;
use gettextrs::gettext;
use gtk::{Grid, Label, TextBuffer, TextView, gio, glib};
use pulldown_cmark::{
    Alignment, BlockQuoteKind, CodeBlockKind, CowStr, Event, LinkType, MetadataBlockKind, Parser,
    RefDefs, Tag, TagEnd,
};
use sourceview5::prelude::*;
use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, Instant, SystemTime};

use crate::math;
use crate::math_view::{self, BlinkMathView};

/// The width images are decoded at, at most: the widest reading column at a display scale
/// of 2. A photo decoded at its full size holds tens of megabytes to show a few hundred
/// pixels.
const IMAGE_DECODE_WIDTH: i32 = 2000;

/// An image of the document, and once decoded its texture and pixel size, or else the
/// pictures waiting for them.
struct CachedImage {
    mtime: SystemTime,
    /// `None` until decoded, and for good if the file could not be decoded.
    image: Option<(gtk::gdk::Texture, (i32, i32))>,
    decoded: bool,
    /// The pictures, the adjustments they are sized to and the widths they are shown at.
    waiting: Vec<(
        glib::WeakRef<gtk::Picture>,
        glib::WeakRef<gtk::Adjustment>,
        Option<i32>,
    )>,
}

thread_local! {
    // Images by path. Each render keeps only the images it showed, so the cache never
    // holds more than one document's.
    static IMAGES: RefCell<HashMap<PathBuf, CachedImage>> = RefCell::new(HashMap::new());
}

/// A picture of the image at `path`, `width` pixels wide if given and at most as wide as the
/// reading column, or `None` for a file
/// that could not be decoded. Rendering runs after every pause in typing, so an image is
/// decoded once, until the file changes, and in the background, so that a document with
/// large images opens without freezing the window. The picture takes its size when the
/// decoding is done.
fn image_picture(path: &Path, hadj: &gtk::Adjustment, width: Option<i32>) -> Option<gtk::Picture> {
    let mtime = std::fs::metadata(path).ok()?.modified().ok()?;
    IMAGES.with(|cache| {
        let mut cache = cache.borrow_mut();
        let entry = cache
            .entry(path.to_path_buf())
            .and_modify(|entry| {
                if entry.mtime != mtime {
                    *entry = CachedImage::loading(path, mtime);
                }
            })
            .or_insert_with(|| CachedImage::loading(path, mtime));
        let picture = gtk::Picture::new();
        match &entry.image {
            Some((texture, size)) => {
                picture.set_paintable(Some(texture));
                bind_image_to_page(&picture, hadj, shown_size(*size, width));
            }
            None if entry.decoded => return None,
            None => entry
                .waiting
                .push((picture.downgrade(), hadj.downgrade(), width)),
        }
        Some(picture)
    })
}

/// The size an image of `size` is shown at: `width` wide if given, in proportion.
pub fn shown_size((image_width, image_height): (i32, i32), width: Option<i32>) -> (i32, i32) {
    match width {
        Some(width) => (
            width,
            (f64::from(width) * f64::from(image_height) / f64::from(image_width.max(1))).round()
                as i32,
        ),
        None => (image_width, image_height),
    }
}

impl CachedImage {
    fn loading(path: &Path, mtime: SystemTime) -> Self {
        load_image(path.to_path_buf(), mtime);
        Self {
            mtime,
            image: None,
            decoded: false,
            waiting: Vec::new(),
        }
    }
}

/// Decoded pixels, which unlike a texture can be made off the main thread.
struct DecodedImage {
    /// The size of the image in the file, which it is shown at.
    size: (i32, i32),
    width: i32,
    height: i32,
    has_alpha: bool,
    stride: usize,
    pixels: glib::Bytes,
}

fn decode_image(path: &Path) -> Option<DecodedImage> {
    let (_, width, height) = gtk::gdk_pixbuf::Pixbuf::file_info(path)?;
    let pixbuf = if width > IMAGE_DECODE_WIDTH {
        gtk::gdk_pixbuf::Pixbuf::from_file_at_scale(path, IMAGE_DECODE_WIDTH, -1, true)
    } else {
        gtk::gdk_pixbuf::Pixbuf::from_file(path)
    }
    .ok()?;
    Some(DecodedImage {
        size: (width, height),
        width: pixbuf.width(),
        height: pixbuf.height(),
        has_alpha: pixbuf.has_alpha(),
        stride: usize::try_from(pixbuf.rowstride()).ok()?,
        pixels: pixbuf.read_pixel_bytes(),
    })
}

/// Decode the image at `path` on a worker thread and hand it to the pictures waiting.
fn load_image(path: PathBuf, mtime: SystemTime) {
    glib::spawn_future_local(async move {
        let decode_path = path.clone();
        let decoded = gio::spawn_blocking(move || decode_image(&decode_path))
            .await
            .ok()
            .flatten();
        IMAGES.with(|cache| {
            let mut cache = cache.borrow_mut();
            // Dropped from the document, or changed on disk since, while it was decoding.
            let Some(entry) = cache.get_mut(&path).filter(|entry| entry.mtime == mtime) else {
                return;
            };
            entry.decoded = true;
            let waiting = std::mem::take(&mut entry.waiting);
            let Some(image) = decoded else {
                return;
            };
            let format = if image.has_alpha {
                gtk::gdk::MemoryFormat::R8g8b8a8
            } else {
                gtk::gdk::MemoryFormat::R8g8b8
            };
            let texture = gtk::gdk::MemoryTexture::new(
                image.width,
                image.height,
                format,
                &image.pixels,
                image.stride,
            )
            .upcast::<gtk::gdk::Texture>();
            for (picture, hadj, width) in &waiting {
                if let (Some(picture), Some(hadj)) = (picture.upgrade(), hadj.upgrade()) {
                    picture.set_paintable(Some(&texture));
                    bind_image_to_page(&picture, &hadj, shown_size(image.size, *width));
                }
            }
            entry.image = Some((texture, image.size));
        });
    });
}

/// Horizontal margin of the preview text view. Tag margins are absolute (a
/// `left-margin` on a tag replaces the view's own margin rather than adding to
/// it), so indenting tags have to start from this value.
pub const TEXT_MARGIN: i32 = 32;

/// How far each level of list nesting indents its items.
const LIST_INDENT: i32 = 20;
/// The space above and below a code block, a table or an image, on top of the blank line
/// that parts every block from the next, which is room enough for a paragraph.
const BLOCK_MARGIN: i32 = 4;
/// How far each level of blockquote nesting indents its text, from the left edge of its box.
const QUOTE_INDENT: i32 = 24;
/// The space between the text of a blockquote and the right edge of its box.
const QUOTE_PADDING: i32 = 16;

/// Characters after which the text of a table cell wraps.
const CELL_WRAP_CHARS: i32 = 40;

thread_local! {
    // The widest the preview may get, as the window last set it.
    static CONTENT_WIDTH: Cell<f64> = const { Cell::new(f64::MAX) };
    // The family of inline code and of code in table cells, as the window last set it.
    static MONOSPACE_FAMILY: RefCell<String> = RefCell::new(String::from("Monospace"));
}

/// Show the inline code of `buffer`, and of table cells rendered from now on, in `family`.
pub fn set_monospace_family(buffer: &TextBuffer, family: &str) {
    MONOSPACE_FAMILY.replace(family.to_owned());
    if let Some(code) = buffer.tag_table().lookup("code") {
        code.set_family(Some(family));
    }
}

/// Limit the width of the preview behind `hadj` to `width`. Block widgets are sized from
/// the width of the preview, which cannot become narrower than they are, so they apply the
/// limit themselves, and are sized again here.
pub fn set_content_width(hadj: &gtk::Adjustment, width: i32) {
    CONTENT_WIDTH.set(f64::from(width));
    hadj.notify("page-size");
}

/// Width available to a block widget in a preview of the given width.
fn column_width(page_size: f64, indent: i32) -> f64 {
    let width = page_size.min(CONTENT_WIDTH.get());
    (width - 2.0 * f64::from(TEXT_MARGIN) - f64::from(indent)).max(100.0)
}

/// Size a preview child widget to the visible page width. GtkTextView allocates
/// anchored children at their *minimum* width, so `width-request` is the only
/// lever; the binding tracks the viewport so the value follows resizes.
fn bind_width_to_page(child: &impl IsA<gtk::Widget>, hadj: &gtk::Adjustment, indent: i32) {
    hadj.bind_property("page-size", child, "width-request")
        .transform_to(move |_, page_size: f64| Some(column_width(page_size, indent) as i32))
        .sync_create()
        .build();
}

/// Size an image to the reading column: never wider than the column and never
/// scaled up past its own pixel size, with the height following from the
/// aspect ratio. The height matters because a GtkPicture's *minimum* height is
/// zero and a GtkTextView allocates anchored children at their minimum, so an
/// image without a height request would not show at all.
fn bind_image_to_page(picture: &gtk::Picture, hadj: &gtk::Adjustment, (width, height): (i32, i32)) {
    let image_width = f64::from(width.max(1));
    let image_height = f64::from(height.max(1));
    hadj.bind_property("page-size", picture, "width-request")
        .transform_to(move |_, page_size: f64| {
            Some(column_width(page_size, 0).min(image_width) as i32)
        })
        .sync_create()
        .build();
    hadj.bind_property("page-size", picture, "height-request")
        .transform_to(move |_, page_size: f64| {
            let width = column_width(page_size, 0).min(image_width);
            Some((width * image_height / image_width).round() as i32)
        })
        .sync_create()
        .build();
}

/// The GtkSourceView style scheme matching the current light/dark appearance.
pub fn current_scheme() -> Option<sourceview5::StyleScheme> {
    let name = if adw::StyleManager::default().is_dark() {
        "Adwaita-dark"
    } else {
        "Adwaita"
    };
    sourceview5::StyleSchemeManager::default().scheme(name)
}

/// Re-colour theme-dependent tags. Text tags cannot reference CSS variables, so
/// the window calls this again on every theme or accent change.
pub fn apply_theme_colors(buffer: &TextBuffer) {
    let table = buffer.tag_table();
    if let Some(link) = table.lookup("link") {
        // The accent colour for text, as links in labels have it, such as those of tables.
        let style = adw::StyleManager::default();
        let accent = style.accent_color().to_standalone_rgba(style.is_dark());
        link.set_foreground_rgba(Some(&accent));
    }
    // Blockquote tags are created per nesting depth during a render, so the
    // ones already in the table have to be re-coloured here too.
    let dim = dim_foreground();
    table.foreach(|tag| {
        if tag
            .name()
            .is_some_and(|name| name.starts_with("blockquote-"))
        {
            tag.set_foreground(Some(dim));
        }
    });
    let dark = adw::StyleManager::default().is_dark();
    for (name, color) in ALERT_COLORS {
        if let Some(tag) = table.lookup(name) {
            tag.set_foreground_rgba(Some(&color.to_standalone_rgba(dark)));
        }
    }
}

/// A child widget embedded in the preview whose text lives outside the main
/// buffer (so the buffer's own search can't see it). `anchor_offset` is the
/// position of its anchor in the main buffer, used to order and scroll to it.
#[derive(Clone)]
pub enum Surface {
    Code {
        anchor_offset: i32,
        buffer: sourceview5::Buffer,
        view: sourceview5::View,
    },
    Cell {
        anchor_offset: i32,
        label: gtk::Label,
    },
}

impl Surface {
    /// The same surface with its anchor offset moved by `by`.
    fn shifted(&self, by: i32) -> Self {
        match self {
            Self::Code {
                anchor_offset,
                buffer,
                view,
            } => Self::Code {
                anchor_offset: anchor_offset + by,
                buffer: buffer.clone(),
                view: view.clone(),
            },
            Self::Cell {
                anchor_offset,
                label,
            } => Self::Cell {
                anchor_offset: anchor_offset + by,
                label: label.clone(),
            },
        }
    }
}

/// A task list item's box in the preview.
#[derive(Clone)]
pub struct Task {
    pub anchor: gtk::TextChildAnchor,
    pub check: gtk::CheckButton,
    /// The byte offset of the item's `[ ]` or `[x]` in the source.
    pub source: usize,
}

/// Output of a render pass: clickable link ranges, the searchable child surfaces (code
/// blocks, table cells), all of them and the ones this pass made, the boxes of the task
/// list items, those this pass made, and where the headings start, by identifier.
pub struct RenderResult {
    pub links: Vec<(i32, i32, String)>,
    pub surfaces: Vec<Surface>,
    pub added: Vec<Surface>,
    pub tasks: Vec<Task>,
    pub added_tasks: Vec<Task>,
    pub headings: Vec<(i32, String)>,
    pub details: Vec<Details>,
    pub quotes: Vec<Quote>,
}

/// A blockquote in the preview, whose box is drawn behind its text: the offsets of its first
/// and past its last character, and the left and right edges of the box, from the edges of
/// the view.
#[derive(Clone, Copy, Debug)]
pub struct Quote {
    pub start: i32,
    pub end: i32,
    pub left: i32,
    pub right: i32,
    pub alert: Option<BlockQuoteKind>,
}

/// The colour of the title and the bar of an alert of `kind`, in the light or the dark style.
pub fn alert_color(kind: BlockQuoteKind, dark: bool) -> gtk::gdk::RGBA {
    ALERT_COLORS[alert_index(kind)].1.to_standalone_rgba(dark)
}

pub fn setup_tags(buffer: &TextBuffer) {
    buffer.create_tag(
        Some("h1"),
        &[
            ("scale", &2.0),
            ("weight", &700),
            ("pixels-above-lines", &16),
            ("pixels-below-lines", &8),
        ],
    );
    buffer.create_tag(
        Some("h2"),
        &[
            ("scale", &1.75),
            ("weight", &700),
            ("pixels-above-lines", &12),
            ("pixels-below-lines", &6),
        ],
    );
    buffer.create_tag(
        Some("h3"),
        &[
            ("scale", &1.5),
            ("weight", &700),
            ("pixels-above-lines", &8),
            ("pixels-below-lines", &4),
        ],
    );
    buffer.create_tag(
        Some("h4"),
        &[
            ("scale", &1.2),
            ("weight", &700),
            ("pixels-above-lines", &8),
            ("pixels-below-lines", &4),
        ],
    );
    buffer.create_tag(Some("bold"), &[("weight", &700)]);
    buffer.create_tag(Some("italic"), &[("style", &gtk::pango::Style::Italic)]);
    buffer.create_tag(Some("strikethrough"), &[("strikethrough", &true)]);
    buffer.create_tag(
        Some("underline"),
        &[("underline", &gtk::pango::Underline::Single)],
    );
    buffer.create_tag(Some("superscript"), &[("rise", &4000), ("scale", &0.8)]);
    buffer.create_tag(Some("subscript"), &[("rise", &-2000), ("scale", &0.8)]);
    buffer.create_tag(Some("mark"), &[("background", &"rgba(255, 200, 0, 0.3)")]);
    buffer.create_tag(
        Some("link"),
        &[("underline", &gtk::pango::Underline::Single)],
    );
    // Inline code: a subtle background tint reads as code without competing
    // with bold text, as a bold weight would.
    buffer.create_tag(
        Some("code"),
        &[
            ("family", &"Monospace"),
            ("background", &"rgba(128, 128, 128, 0.15)"),
        ],
    );
    // Footnote markers, raised and shrunk so they read as references.
    buffer.create_tag(
        Some("footnote"),
        &[("rise", &4000), ("scale", &0.8), ("weight", &700)],
    );
    // Placeholder for an image that could not be shown.
    buffer.create_tag(Some("image-alt"), &[("style", &gtk::pango::Style::Italic)]);
    // A displayed formula that is a paragraph of its own.
    buffer.create_tag(
        Some("align-center"),
        &[("justification", &gtk::Justification::Center)],
    );
    buffer.create_tag(
        Some("align-right"),
        &[("justification", &gtk::Justification::Right)],
    );
    buffer.create_tag(
        Some("math-display"),
        &[("justification", &gtk::Justification::Center)],
    );
    // The definition of a term, under the term.
    buffer.create_tag(
        Some("definition"),
        &[("left-margin", &(TEXT_MARGIN + LIST_INDENT))],
    );
    for (name, _) in ALERT_COLORS {
        buffer.create_tag(Some(name), &[]);
    }
}

/// The text tag of the title of each kind of alert, and the accent colour it is shown in,
/// as on GitHub.
const ALERT_COLORS: [(&str, adw::AccentColor); 5] = [
    ("alert-note", adw::AccentColor::Blue),
    ("alert-tip", adw::AccentColor::Green),
    ("alert-important", adw::AccentColor::Purple),
    ("alert-warning", adw::AccentColor::Yellow),
    ("alert-caution", adw::AccentColor::Red),
];

/// The name of the text tag of the title of an alert of `kind`.
fn alert_tag(kind: BlockQuoteKind) -> &'static str {
    ALERT_COLORS[alert_index(kind)].0
}

fn alert_index(kind: BlockQuoteKind) -> usize {
    match kind {
        BlockQuoteKind::Note => 0,
        BlockQuoteKind::Tip => 1,
        BlockQuoteKind::Important => 2,
        BlockQuoteKind::Warning => 3,
        BlockQuoteKind::Caution => 4,
    }
}

/// Close the innermost open `name` tag, so that when the same inline tag nests
/// the outer run stays open.
fn close_tag(tags: &mut Vec<String>, name: &str) {
    if let Some(index) = tags.iter().rposition(|tag| tag == name) {
        tags.remove(index);
    }
}

/// `text` escaped for the markup of a table cell, without the object replacement characters
/// that stand for the formulas of the cell.
fn cell_text(text: &str) -> glib::GString {
    glib::markup_escape_text(&text.replace('\u{FFFC}', ""))
}

/// Close the tags of the styles raw HTML opened in `html_styles` and left open, as the end of
/// a paragraph closes them in a browser.
fn close_html_styles(tags: &mut Vec<String>, html_styles: &mut Vec<&'static str>) {
    for name in html_styles.drain(..).rev() {
        close_tag(tags, name);
    }
}

/// Markup open in a table cell: how it starts and ends, and the style of raw HTML it is, if
/// it is one.
struct CellMarkup {
    start: String,
    end: &'static str,
    html: Option<HtmlStyle>,
}

impl CellMarkup {
    fn new(start: impl Into<String>, end: &'static str) -> Self {
        Self {
            start: start.into(),
            end,
            html: None,
        }
    }

    /// The markup of `style`.
    fn html(style: HtmlStyle) -> Self {
        let (start, end) = match style {
            HtmlStyle::Bold => (String::from("<b>"), "</b>"),
            HtmlStyle::Italic => (String::from("<i>"), "</i>"),
            HtmlStyle::Strikethrough => (String::from("<s>"), "</s>"),
            HtmlStyle::Underline => (String::from("<u>"), "</u>"),
            HtmlStyle::Superscript => (String::from("<sup>"), "</sup>"),
            HtmlStyle::Subscript => (String::from("<sub>"), "</sub>"),
            HtmlStyle::Code | HtmlStyle::Keyboard => (monospace_markup(), "</span>"),
            HtmlStyle::Mark => (
                String::from("<span background=\"#ffc800\" bgalpha=\"30%\">"),
                "</span>",
            ),
        };
        Self {
            start,
            end,
            html: Some(style),
        }
    }
}

/// The markup that starts text in the monospace family in a table cell.
fn monospace_markup() -> String {
    MONOSPACE_FAMILY.with_borrow(|family| {
        format!(
            "<span font_family=\"{}\">",
            glib::markup_escape_text(family)
        )
    })
}

/// Start `markup` in `cell`, and keep it in `open` until it ends.
fn start_cell_markup(cell: &mut String, open: &mut Vec<CellMarkup>, markup: CellMarkup) {
    cell.push_str(&markup.start);
    open.push(markup);
}

/// End the innermost markup of `open` that `ends` picks in `cell`. Markup has to nest, as raw
/// HTML may not, so the markup started inside it is ended with it and started again.
fn end_cell_markup(
    cell: &mut String,
    open: &mut Vec<CellMarkup>,
    ends: impl Fn(&CellMarkup) -> bool,
) {
    let Some(index) = open.iter().rposition(ends) else {
        return;
    };
    let inner = open.split_off(index + 1);
    for markup in inner.iter().rev() {
        cell.push_str(markup.end);
    }
    if let Some(ended) = open.pop() {
        cell.push_str(ended.end);
    }
    for markup in inner {
        start_cell_markup(cell, open, markup);
    }
}

/// Insert whatever newlines are still missing for a blank line to separate the
/// block just written from the next one. A fixed `\n\n` would double up when an
/// inner paragraph has already ended the block.
fn end_block(buffer: &TextBuffer, iter: &mut gtk::TextIter) {
    if iter.offset() == 0 {
        return;
    }
    let start = buffer.iter_at_offset((iter.offset() - 2).max(0));
    // A slice, which unlike the text keeps a character for a widget: after a code block,
    // the newline before it is not one after it.
    let tail = buffer.slice(&start, iter, true);
    let present = tail.chars().rev().take_while(|c| *c == '\n').count();
    for _ in present..2 {
        buffer.insert(iter, "\n");
    }
}

/// Start a new line unless one is already started. A code block or table in a tight list
/// item follows the item's text directly, and anchored on that line it would take the
/// item's hanging indent.
fn start_line(buffer: &TextBuffer, iter: &mut gtk::TextIter) {
    if !iter.starts_line() {
        buffer.insert(iter, "\n");
    }
}

/// End the line of a code block or table: inside a list only the line, so the next item
/// follows as closely as after text; elsewhere with the blank line after any block.
fn end_widget_block(buffer: &TextBuffer, iter: &mut gtk::TextIter, in_list: bool) {
    if in_list {
        start_line(buffer, iter);
    } else {
        end_block(buffer, iter);
    }
}

/// Left margin of a code block or table, in line with the text of the list item or
/// blockquote around it.
fn block_indent(list_stack: &[Option<u64>], blockquote_depth: i32) -> i32 {
    i32::try_from(list_stack.len()).unwrap_or(0) * LIST_INDENT + blockquote_depth * QUOTE_INDENT
}

/// Keep presses in the padding of a code block or table to the block. Passed on to the
/// preview, such a press starts a selection at the block's anchor, and the preview scrolls
/// to the top of the block. Presses on the block's text or buttons go on as usual, and the
/// preview still takes the focus, so that copying finds a selection made in the block.
fn keep_presses(block: &impl IsA<gtk::Widget>, view: &TextView) {
    let click = gtk::GestureClick::new();
    click.connect_pressed(|gesture, _, x, y| {
        let Some(block) = gesture.widget() else {
            return;
        };
        let on_content = block
            .pick(x, y, gtk::PickFlags::DEFAULT)
            .is_some_and(|target| {
                target.is::<gtk::Label>()
                    || target.is::<gtk::TextView>()
                    || target.ancestor(gtk::Button::static_type()).is_some()
            });
        if !on_content {
            gesture.set_state(gtk::EventSequenceState::Claimed);
        }
    });
    block.add_controller(click);
    // The focus moves before the label or text view pressed takes the press for its
    // selection, which would keep it from the gesture above.
    let focus = gtk::GestureClick::new();
    focus.set_propagation_phase(gtk::PropagationPhase::Capture);
    focus.connect_pressed(glib::clone!(
        #[weak]
        view,
        move |_, _, _, _| {
            view.grab_focus();
        }
    ));
    block.add_controller(focus);
}

/// The Markdown the preview and the exports understand: CommonMark with tables,
/// strikethrough, task lists, footnotes, GitHub's alerts, front matter, math, definition lists
/// and wiki links.
fn parser_options() -> pulldown_cmark::Options {
    let mut options = pulldown_cmark::Options::empty();
    options.insert(pulldown_cmark::Options::ENABLE_TABLES);
    options.insert(pulldown_cmark::Options::ENABLE_STRIKETHROUGH);
    options.insert(pulldown_cmark::Options::ENABLE_TASKLISTS);
    options.insert(pulldown_cmark::Options::ENABLE_FOOTNOTES);
    options.insert(pulldown_cmark::Options::ENABLE_GFM);
    options.insert(pulldown_cmark::Options::ENABLE_YAML_STYLE_METADATA_BLOCKS);
    options.insert(pulldown_cmark::Options::ENABLE_PLUSES_DELIMITED_METADATA_BLOCKS);
    options.insert(pulldown_cmark::Options::ENABLE_MATH);
    options.insert(pulldown_cmark::Options::ENABLE_DEFINITION_LIST);
    options.insert(pulldown_cmark::Options::ENABLE_WIKILINKS);
    options
}

/// The events of `text` as the exports read them, with their source ranges, with every emoji
/// shortcode made an emoji.
pub fn events(text: &str) -> Vec<(Event<'_>, Range<usize>)> {
    events_with_emoji(text, |_| true)
}

/// The events of `text`, with the emoji shortcodes made emoji that `emoji` accepts.
pub fn events_with_emoji(text: &str, emoji: EmojiFilter) -> Vec<(Event<'_>, Range<usize>)> {
    extend_events(
        text,
        Parser::new_ext(text, parser_options())
            .into_offset_iter()
            .collect(),
        emoji,
    )
}

/// Whether an emoji is shown for its shortcode.
pub type EmojiFilter = fn(&str) -> bool;

/// Whether a font here has `emoji`, which is shown for its shortcode only then: a missing
/// glyph box says less than the shortcode. Asked once for each emoji.
pub fn font_has_emoji(emoji: &str) -> bool {
    thread_local! {
        static COVERED: RefCell<HashMap<String, bool>> = RefCell::new(HashMap::new());
    }
    COVERED.with_borrow_mut(|covered| {
        *covered.entry(emoji.to_owned()).or_insert_with(|| {
            let context = pangocairo::FontMap::default().create_context();
            let description = gtk::pango::FontDescription::from_string("sans 12");
            let Some(fontset) =
                context.load_fontset(&description, &gtk::pango::Language::default())
            else {
                return false;
            };
            // The joiners and selectors of a sequence are not drawn themselves.
            emoji
                .chars()
                .filter(|c| !matches!(c, '\u{200d}' | '\u{fe0e}' | '\u{fe0f}'))
                .all(|c| fontset.font(u32::from(c)).has_char(c))
        })
    })
}

/// The title GitHub gives an alert of `kind`.
fn alert_title(kind: BlockQuoteKind) -> String {
    match kind {
        // Translators: the titles of the five kinds of alert a document can hold, as GitHub
        // shows them over a note, a tip, an important note, a warning and a caution.
        BlockQuoteKind::Note => gettext("Note"),
        BlockQuoteKind::Tip => gettext("Tip"),
        BlockQuoteKind::Important => gettext("Important"),
        BlockQuoteKind::Warning => gettext("Warning"),
        BlockQuoteKind::Caution => gettext("Caution"),
    }
}

/// Turn what the parser reads but the renderers have no layout of their own for into what
/// they have, so that the preview and the exports show it alike:
///
/// - front matter becomes a code block, and so does math standing alone in a paragraph that
///   does not typeset, and other such math inline code;
/// - web addresses written out become links, and emoji shortcodes emoji, as on GitHub;
/// - the lines of a raw HTML block, which the parser gives one by one, become one chunk, so
///   that a tag or a script over several lines is read whole;
/// - `<img>` tags in raw HTML become images, held to the same rules as Markdown images;
/// - an alert starts with its title in bold;
/// - headings get the identifiers GitHub gives them, which links to `#section` point at;
/// - a wiki link points at the Markdown file of its page.
fn extend_events<'a>(
    text: &'a str,
    events: Vec<(Event<'a>, Range<usize>)>,
    emoji: EmojiFilter,
) -> Vec<(Event<'a>, Range<usize>)> {
    let events = join_html_blocks(events);
    let mut out = Vec::with_capacity(events.len());
    // Addresses are not made links inside links, image descriptions and code.
    let mut in_link = 0usize;
    let mut in_code = false;
    let mut slugs: HashMap<String, usize> = HashMap::new();
    let mut index = 0;
    while index < events.len() {
        let (event, range) = events[index].clone();
        index += 1;
        match event {
            Event::Start(Tag::MetadataBlock(kind)) => {
                in_code = true;
                let language = match kind {
                    MetadataBlockKind::YamlStyle => "yaml",
                    MetadataBlockKind::PlusesStyle => "toml",
                };
                out.push((
                    Event::Start(Tag::CodeBlock(CodeBlockKind::Fenced(language.into()))),
                    range,
                ));
            }
            Event::End(TagEnd::MetadataBlock(_)) => {
                in_code = false;
                out.push((Event::End(TagEnd::CodeBlock), range));
            }
            Event::Start(Tag::CodeBlock(_)) => {
                in_code = true;
                out.push((event, range));
            }
            Event::End(TagEnd::CodeBlock) => {
                in_code = false;
                out.push((event, range));
            }
            Event::Start(Tag::Paragraph)
                if matches!(
                    (events.get(index), events.get(index + 1)),
                    (
                        Some((Event::DisplayMath(math), _)),
                        Some((Event::End(TagEnd::Paragraph), _))
                    ) if math::typeset(math, true).is_none()
                ) =>
            {
                if let Event::DisplayMath(math) = &events[index].0 {
                    let fence = CodeBlockKind::Fenced("latex".into());
                    out.push((Event::Start(Tag::CodeBlock(fence)), range.clone()));
                    let code = format!("{}\n", math.trim_matches('\n'));
                    out.push((Event::Text(code.into()), events[index].1.clone()));
                    out.push((Event::End(TagEnd::CodeBlock), range));
                }
                index += 2;
            }
            Event::InlineMath(ref math) if math::typeset(math, false).is_some() => {
                out.push((event, range));
            }
            Event::DisplayMath(ref math) if math::typeset(math, true).is_some() => {
                out.push((event, range));
            }
            Event::InlineMath(math) | Event::DisplayMath(math) => {
                out.push((Event::Code(math), range));
            }
            Event::Start(Tag::Link {
                link_type: link_type @ LinkType::WikiLink { .. },
                dest_url,
                title,
                id,
            }) => {
                in_link += 1;
                let tag = Tag::Link {
                    link_type,
                    dest_url: wiki_destination(&dest_url).into(),
                    title,
                    id,
                };
                out.push((Event::Start(tag), range));
            }
            Event::Start(Tag::Link { .. } | Tag::Image { .. }) => {
                in_link += 1;
                out.push((event, range));
            }
            Event::End(TagEnd::Link | TagEnd::Image) => {
                in_link = in_link.saturating_sub(1);
                out.push((event, range));
            }
            Event::Start(Tag::BlockQuote(Some(kind))) => {
                out.push((event, range.clone()));
                out.push((Event::Start(Tag::Paragraph), range.clone()));
                out.push((Event::Start(Tag::Strong), range.clone()));
                out.push((Event::Text(alert_title(kind).into()), range.clone()));
                out.push((Event::End(TagEnd::Strong), range.clone()));
                out.push((Event::End(TagEnd::Paragraph), range));
            }
            Event::Start(Tag::Heading {
                level,
                id: None,
                classes,
                attrs,
            }) => {
                let mut title = String::new();
                for (event, _) in &events[index..] {
                    match event {
                        Event::End(TagEnd::Heading(_)) => break,
                        Event::Text(text) | Event::Code(text) | Event::InlineMath(text) => {
                            title.push_str(text);
                        }
                        _ => {}
                    }
                }
                let slug = heading_slug(&title);
                let count = slugs.entry(slug.clone()).or_default();
                let id = if *count == 0 {
                    slug
                } else {
                    format!("{slug}-{count}")
                };
                *count += 1;
                let tag = Tag::Heading {
                    level,
                    id: Some(id.into()),
                    classes,
                    attrs,
                };
                out.push((Event::Start(tag), range));
            }
            Event::Text(first) => {
                // The parser splits text where markup might have started.
                let mut merged = first.to_string();
                let mut source = range;
                while let Some((Event::Text(next), next_range)) = events.get(index) {
                    merged.push_str(next);
                    source.end = next_range.end;
                    index += 1;
                }
                if in_code {
                    out.push((Event::Text(merged.into()), source));
                    continue;
                }
                if in_link > 0 {
                    out.push((
                        Event::Text(replace_shortcodes(&merged, emoji).into()),
                        source,
                    ));
                    continue;
                }
                // Only text as written in the source has ranges within it; the rest keeps
                // the range of the whole.
                let exact = text.get(source.clone()) == Some(merged.as_str());
                let part = |part: Range<usize>| {
                    if exact {
                        source.start + part.start..source.start + part.end
                    } else {
                        source.clone()
                    }
                };
                let mut last = 0;
                for (link, url) in bare_links(&merged) {
                    if link.start > last {
                        let before = replace_shortcodes(&merged[last..link.start], emoji);
                        out.push((Event::Text(before.into()), part(last..link.start)));
                    }
                    let tag = Tag::Link {
                        link_type: LinkType::Autolink,
                        dest_url: url.into(),
                        title: "".into(),
                        id: "".into(),
                    };
                    out.push((Event::Start(tag), part(link.clone())));
                    let shown = merged[link.clone()].to_owned();
                    out.push((Event::Text(shown.into()), part(link.clone())));
                    out.push((Event::End(TagEnd::Link), part(link.clone())));
                    last = link.end;
                }
                if last < merged.len() {
                    let rest = replace_shortcodes(&merged[last..], emoji);
                    out.push((Event::Text(rest.into()), part(last..merged.len())));
                }
            }
            Event::Html(ref chunk) | Event::InlineHtml(ref chunk) if in_link == 0 => {
                let images = html_images(chunk);
                if images.is_empty() {
                    out.push((event, range));
                    continue;
                }
                let block = matches!(event, Event::Html(_));
                let html = |part: &str| -> Event<'a> {
                    if block {
                        Event::Html(part.to_owned().into())
                    } else {
                        Event::InlineHtml(part.to_owned().into())
                    }
                };
                let mut last = 0;
                for (tag, source, alt, width) in images {
                    if tag.start > last {
                        out.push((html(&chunk[last..tag.start]), range.clone()));
                    }
                    let image = Tag::Image {
                        link_type: LinkType::Inline,
                        dest_url: source.into(),
                        title: "".into(),
                        id: width
                            .map(|width| width.to_string())
                            .unwrap_or_default()
                            .into(),
                    };
                    out.push((Event::Start(image), range.clone()));
                    if !alt.is_empty() {
                        out.push((Event::Text(alt.into()), range.clone()));
                    }
                    out.push((Event::End(TagEnd::Image), range.clone()));
                    last = tag.end;
                }
                if last < chunk.len() {
                    out.push((html(&chunk[last..]), range));
                }
            }
            _ => out.push((event, range)),
        }
    }
    out
}

/// `events` with the lines of each raw HTML block joined into one chunk.
fn join_html_blocks(events: Vec<(Event<'_>, Range<usize>)>) -> Vec<(Event<'_>, Range<usize>)> {
    let mut out: Vec<(Event, Range<usize>)> = Vec::with_capacity(events.len());
    for (event, range) in events {
        if let Event::Html(chunk) = &event
            && let Some((Event::Html(last), last_range)) = out.last_mut()
        {
            *last = CowStr::from(format!("{last}{chunk}"));
            last_range.end = range.end;
            continue;
        }
        out.push((event, range));
    }
    out
}

/// The width in pixels an image is shown at, which an `<img>` tag may give. The width is
/// carried as the identifier of the image, which otherwise only an image by reference has.
pub fn image_width(link_type: LinkType, id: &str) -> Option<i32> {
    (link_type == LinkType::Inline)
        .then(|| id.parse().ok())
        .flatten()
        .filter(|width| *width > 0)
}

/// The identifier GitHub gives a heading titled `title`, before a number is added to tell
/// headings of the same title apart.
fn heading_slug(title: &str) -> String {
    title
        .chars()
        .filter_map(|c| match c {
            ' ' => Some('-'),
            c if c.is_alphanumeric() || c == '-' || c == '_' => Some(c),
            _ => None,
        })
        .flat_map(char::to_lowercase)
        .collect()
}

/// Where a wiki link to `page` points: the Markdown file of the page, with the section the
/// link names, if any.
fn wiki_destination(page: &str) -> String {
    let (file, section) = match page.split_once('#') {
        Some((file, section)) => (file, Some(section)),
        None => (page, None),
    };
    let mut destination = file.to_owned();
    if !file.is_empty() && Path::new(file).extension().is_none() {
        destination.push_str(".md");
    }
    if let Some(section) = section {
        destination.push('#');
        destination.push_str(&heading_slug(section));
    }
    destination
}

/// The web addresses written out in `text`, as GitHub makes links of them: their byte
/// ranges, and the address each one follows.
fn bare_links(text: &str) -> Vec<(Range<usize>, String)> {
    const PREFIXES: [&str; 3] = ["https://", "http://", "www."];
    // Lowercased ASCII keeps the byte offsets of the text.
    let lower = text.to_ascii_lowercase();
    let mut links = Vec::new();
    let mut from = 0;
    while let Some((start, prefix)) = PREFIXES
        .iter()
        .filter_map(|prefix| lower[from..].find(prefix).map(|at| (from + at, *prefix)))
        .min_by_key(|(start, _)| *start)
    {
        from = start + prefix.len();
        // An address starts a word, or follows an opening bracket or emphasis.
        if text[..start]
            .chars()
            .next_back()
            .is_some_and(|c| !c.is_whitespace() && !matches!(c, '(' | '*' | '_' | '~'))
        {
            continue;
        }
        let end = text[start..]
            .find(|c: char| c.is_whitespace() || c == '<')
            .map_or(text.len(), |length| start + length);
        let mut address = &text[start..end];
        // Punctuation that ends a sentence, and a closing bracket without its opening one,
        // are not part of the address.
        loop {
            if let Some(trimmed) = address.strip_suffix(|c| {
                matches!(
                    c,
                    '?' | '!' | '.' | ',' | ':' | ';' | '*' | '_' | '~' | '\'' | '"'
                )
            }) {
                address = trimmed;
            } else if address.ends_with(')')
                && address.matches(')').count() > address.matches('(').count()
            {
                address = &address[..address.len() - 1];
            } else {
                break;
            }
        }
        let Some(after_prefix) = address.get(prefix.len()..) else {
            continue;
        };
        let host = after_prefix
            .split(['/', '?', '#'])
            .next()
            .unwrap_or_default();
        let valid_host = !host.is_empty()
            && host
                .chars()
                .all(|c| c.is_alphanumeric() || matches!(c, '.' | '-' | '_' | ':'))
            && (prefix != "www." || !host.starts_with('.'));
        if !valid_host {
            continue;
        }
        let url = if prefix == "www." {
            format!("http://{address}")
        } else {
            address.to_owned()
        };
        links.push((start..start + address.len(), url));
        from = start + address.len();
    }
    links
}

/// The `<img>` tags of a raw HTML chunk: the byte range of each, its `src`, its `alt` and
/// its `width` in pixels, if it has one.
fn html_images(chunk: &str) -> Vec<(Range<usize>, String, String, Option<i32>)> {
    html_tags(chunk, "img")
        .into_iter()
        .map(|(range, attributes)| {
            let attribute = |wanted: &str| {
                attributes
                    .iter()
                    .find(|(name, _)| name == wanted)
                    .map(|(_, value)| value.clone())
                    .unwrap_or_default()
            };
            let width = attribute("width");
            let width = width
                .strip_suffix("px")
                .unwrap_or(&width)
                .trim()
                .parse()
                .ok();
            (range, attribute("src"), attribute("alt"), width)
        })
        .collect()
}

/// The attributes of an HTML tag, as names in lower case and values.
type Attributes = Vec<(String, String)>;

/// The opening tags named `name` in a raw HTML chunk: the byte range of each, and its
/// attributes.
fn html_tags(chunk: &str, name: &str) -> Vec<(Range<usize>, Attributes)> {
    let lower = chunk.to_ascii_lowercase();
    let opening = format!("<{name}");
    let mut tags = Vec::new();
    let mut from = 0;
    while let Some(at) = lower[from..].find(&opening) {
        let start = from + at;
        from = start + opening.len();
        if !lower[from..].starts_with(|c: char| c.is_ascii_whitespace() || c == '/' || c == '>') {
            continue;
        }
        let mut attributes = Vec::new();
        let mut rest = &chunk[from..];
        let end = loop {
            rest = rest.trim_start_matches(|c: char| c.is_ascii_whitespace() || c == '/');
            if rest.is_empty() {
                break None;
            }
            if let Some(after) = rest.strip_prefix('>') {
                break Some(chunk.len() - after.len());
            }
            let name_length = rest
                .find(|c: char| c.is_ascii_whitespace() || matches!(c, '=' | '>' | '/'))
                .unwrap_or(rest.len());
            let name = rest[..name_length].to_ascii_lowercase();
            rest = rest[name_length..].trim_start();
            let mut value = String::new();
            if let Some(after) = rest.strip_prefix('=') {
                rest = after.trim_start();
                let (raw, remainder) = match rest.chars().next() {
                    Some(quote @ ('"' | '\'')) => match rest[1..].find(quote) {
                        Some(length) => (&rest[1..=length], &rest[length + 2..]),
                        None => (&rest[1..], ""),
                    },
                    _ => {
                        let length = rest
                            .find(|c: char| c.is_ascii_whitespace() || c == '>')
                            .unwrap_or(rest.len());
                        (&rest[..length], &rest[length..])
                    }
                };
                value = decode_entities(raw);
                rest = remainder;
            }
            attributes.push((name, value));
        };
        let Some(end) = end else {
            break;
        };
        tags.push((start..end, attributes));
        from = end;
    }
    tags
}

/// A piece of a raw HTML chunk, as far as the preview and the exports lay HTML out.
#[derive(Debug, PartialEq, Eq)]
pub enum HtmlPart {
    /// Text, as [`strip_html`] leaves it, with emoji for shortcodes.
    Text(String),
    /// A `<details>` element starts, open or closed.
    DetailsStart {
        open: bool,
    },
    DetailsEnd,
    SummaryStart,
    SummaryEnd,
    StyleStart(HtmlStyle),
    StyleEnd(HtmlStyle),
    /// A `<p>`, `<div>` or `<center>` element starts.
    BlockStart(HtmlBlock),
    /// A `<p>` element ends, or a `<div>` or `<center>` element.
    BlockEnd {
        paragraph: bool,
    },
}

/// A `<p>`, `<div>` or `<center>` element: how it aligns its content, and whether it is a
/// paragraph, which the end of its HTML block ends too, as a paragraph cannot hold the
/// Markdown blocks after it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HtmlBlock {
    pub align: Option<HtmlAlign>,
    pub paragraph: bool,
}

/// How a block of raw HTML aligns its content, other than to the start of its lines.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HtmlAlign {
    Center,
    Right,
}

impl HtmlAlign {
    /// The alignment an `align` attribute of `value` gives.
    fn from_attribute(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "center" | "middle" => Some(Self::Center),
            "right" => Some(Self::Right),
            _ => None,
        }
    }

    /// The text tag of the preview the alignment is shown with.
    fn text_tag(self) -> &'static str {
        match self {
            Self::Center => "align-center",
            Self::Right => "align-right",
        }
    }
}

/// The elements raw HTML opened and has not closed, across the Markdown blocks after them,
/// each with how deep in quotes, list items and notes it was opened. As in a browser, an end
/// tag ends only an element opened in the same container, and the end of a container ends
/// the elements opened in it.
#[derive(Debug)]
pub struct OpenHtml<T> {
    open: Vec<(T, usize)>,
    depth: usize,
}

impl<T> Default for OpenHtml<T> {
    fn default() -> Self {
        Self {
            open: Vec::new(),
            depth: 0,
        }
    }
}

impl<T> OpenHtml<T> {
    /// The open elements, outermost first.
    pub fn iter(&self) -> impl DoubleEndedIterator<Item = &T> {
        self.open.iter().map(|(element, _)| element)
    }

    pub fn open(&mut self, element: T) {
        self.open.push((element, self.depth));
    }

    /// End the innermost element opened in the current container that `is` accepts, or the
    /// outermost if not `innermost`, and those opened after it. The ended elements are
    /// returned innermost first.
    pub fn close(&mut self, innermost: bool, is: impl Fn(&T) -> bool) -> Vec<T> {
        let accepted = |(element, depth): &(T, usize)| *depth == self.depth && is(element);
        let index = if innermost {
            self.open.iter().rposition(accepted)
        } else {
            self.open.iter().position(accepted)
        };
        index.map_or_else(Vec::new, |index| self.close_from(index))
    }

    /// Follow `event` into and out of the containers of Markdown, and end the elements
    /// opened in a container that ends. The ended elements are returned innermost first.
    pub fn follow(&mut self, event: &Event) -> Vec<T> {
        match event {
            Event::Start(
                Tag::BlockQuote(_)
                | Tag::Item
                | Tag::FootnoteDefinition(_)
                | Tag::DefinitionListDefinition,
            ) => {
                self.depth += 1;
                Vec::new()
            }
            Event::End(
                TagEnd::BlockQuote(_)
                | TagEnd::Item
                | TagEnd::FootnoteDefinition
                | TagEnd::DefinitionListDefinition,
            ) => {
                let depth = self.depth;
                self.depth = depth.saturating_sub(1);
                let index = self
                    .open
                    .iter()
                    .position(|(_, opened)| *opened >= depth)
                    .unwrap_or(self.open.len());
                self.close_from(index)
            }
            _ => Vec::new(),
        }
    }

    /// End every open element, returned innermost first.
    pub fn close_all(&mut self) -> Vec<T> {
        self.close_from(0)
    }

    fn close_from(&mut self, index: usize) -> Vec<T> {
        let mut closed: Vec<T> = self
            .open
            .split_off(index)
            .into_iter()
            .map(|(element, _)| element)
            .collect();
        closed.reverse();
        closed
    }
}

/// A style that raw HTML gives its text, which the preview and the exports show.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HtmlStyle {
    Bold,
    Italic,
    Strikethrough,
    Underline,
    Code,
    Keyboard,
    Superscript,
    Subscript,
    Mark,
}

impl HtmlStyle {
    /// The style of the HTML elements named `name`, in lower case.
    fn named(name: &str) -> Option<Self> {
        Some(match name {
            "b" | "strong" => Self::Bold,
            "i" | "em" => Self::Italic,
            "s" | "del" | "strike" => Self::Strikethrough,
            "u" | "ins" => Self::Underline,
            "code" | "tt" | "samp" => Self::Code,
            "kbd" => Self::Keyboard,
            "sup" => Self::Superscript,
            "sub" => Self::Subscript,
            "mark" => Self::Mark,
            _ => return None,
        })
    }

    /// The element the style is written as in HTML.
    pub fn element(self) -> &'static str {
        match self {
            Self::Bold => "strong",
            Self::Italic => "em",
            Self::Strikethrough => "del",
            Self::Underline => "u",
            Self::Code => "code",
            Self::Keyboard => "kbd",
            Self::Superscript => "sup",
            Self::Subscript => "sub",
            Self::Mark => "mark",
        }
    }

    /// The text tag of the preview the style is shown with.
    fn text_tag(self) -> &'static str {
        match self {
            Self::Bold => "bold",
            Self::Italic => "italic",
            Self::Strikethrough => "strikethrough",
            Self::Underline => "underline",
            Self::Code | Self::Keyboard => "code",
            Self::Superscript => "superscript",
            Self::Subscript => "subscript",
            Self::Mark => "mark",
        }
    }
}

/// The pieces of a raw HTML chunk: its `<details>` and `<summary>` tags, the tags of the
/// styles it gives its text, and the text around them. A chunk without any is text alone.
pub fn html_parts(chunk: &str) -> Vec<HtmlPart> {
    html_parts_with_emoji(chunk, |_| true)
}

/// The pieces of a raw HTML chunk, with the emoji shortcodes made emoji that `emoji` accepts.
pub fn html_parts_with_emoji(chunk: &str, emoji: EmojiFilter) -> Vec<HtmlPart> {
    let mut tags = Vec::new();
    let text = collapse_spaces(&tag_text(chunk, &mut tags));
    let mut tags = tags.into_iter();
    let mut parts = Vec::new();
    for (index, piece) in text.split(TAG_MARK).enumerate() {
        if index > 0
            && let Some(tag) = tags.next()
        {
            parts.push(tag);
        }
        if !piece.is_empty() {
            parts.push(HtmlPart::Text(replace_shortcodes(piece, emoji)));
        }
    }
    parts
}

/// How many `<details>` elements a raw HTML chunk opens, and `<div>` and `<center>` elements
/// if it is a `block` of its own, less those it closes.
fn html_depth_change(chunk: &str, block: bool) -> isize {
    html_parts(chunk)
        .iter()
        .map(|part| match part {
            HtmlPart::DetailsStart { .. } => 1,
            HtmlPart::BlockStart(HtmlBlock {
                paragraph: false, ..
            }) if block => 1,
            HtmlPart::DetailsEnd => -1,
            HtmlPart::BlockEnd { paragraph: false } if block => -1,
            _ => 0,
        })
        .sum()
}

/// `text` with GitHub's emoji shortcodes, such as `:tada:`, replaced by their emoji where
/// `emoji` accepts them, and the emoji written out that it does not accept replaced by their
/// shortcodes, which say more than the boxes of missing glyphs.
fn replace_shortcodes(text: &str, emoji: EmojiFilter) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find(':') {
        let after = &rest[start + 1..];
        let length = after
            .find(|c: char| !(c.is_ascii_alphanumeric() || matches!(c, '_' | '+' | '-')))
            .unwrap_or(after.len());
        if length > 0
            && after[length..].starts_with(':')
            && let Some(found) = emojis::get_by_shortcode(&after[..length])
            && emoji(found.as_str())
        {
            out.push_str(&rest[..start]);
            out.push_str(found.as_str());
            rest = &after[length + 1..];
        } else {
            out.push_str(&rest[..=start]);
            rest = after;
        }
    }
    out.push_str(rest);
    shortcodes_of_missing_emoji(&out, emoji)
}

/// The most characters an emoji has, as a family joined of several people.
const EMOJI_MAX_CHARS: usize = 10;

/// `text` with the emoji `emoji` does not accept replaced by their shortcodes, where they
/// have one.
fn shortcodes_of_missing_emoji(text: &str, emoji: EmojiFilter) -> String {
    // The blocks emoji start in; the characters before them are all letters and symbols
    // of text fonts.
    let may_start_emoji = |c: char| matches!(u32::from(c), 0x203c..=0x2bff | 0x3030 | 0x303d | 0x3297 | 0x3299 | 0x1f000..);
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some((start, first)) = rest.char_indices().find(|(_, c)| may_start_emoji(*c)) {
        out.push_str(&rest[..start]);
        // The longest emoji starting here.
        let ends: Vec<usize> = rest[start..]
            .char_indices()
            .take(EMOJI_MAX_CHARS)
            .map(|(index, c)| start + index + c.len_utf8())
            .collect();
        let found = ends
            .iter()
            .rev()
            .find_map(|end| emojis::get(&rest[start..*end]).map(|found| (*end, found)));
        let end = match found {
            Some((end, found)) => {
                match found.shortcode().filter(|_| !emoji(found.as_str())) {
                    Some(shortcode) => out.push_str(&format!(":{shortcode}:")),
                    None => out.push_str(&rest[start..end]),
                }
                end
            }
            None => {
                out.push(first);
                start + first.len_utf8()
            }
        };
        rest = &rest[end..];
    }
    out.push_str(rest);
    out
}

/// `text` with the character references of HTML that addresses and descriptions commonly
/// hold replaced by their characters.
fn decode_entities(text: &str) -> String {
    text.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&amp;", "&")
}

/// How many words `text` has, as the runs of characters between spaces. The empty box of a
/// task list item, `[ ]`, is one word, as a ticked one, `[x]`, is, so that ticking a box
/// leaves the count as it was.
pub fn word_count(text: &str) -> usize {
    let mut words = text.split_whitespace().peekable();
    let mut count = 0;
    while let Some(word) = words.next() {
        if word == "[" && words.peek() == Some(&"]") {
            words.next();
        }
        count += 1;
    }
    count
}

/// What ends a footnote: a space that keeps it on the line of the last word, and an arrow up
/// that links back to the reference. Not GitHub's hooked arrow, which few text fonts have.
pub const FOOTNOTE_BACKLINK: &str = "\u{a0}\u{2191}";

/// The identifier of the footnote labelled `label`, which its references link to. The colon
/// never appears in the identifier of a heading.
pub fn footnote_id(label: &str) -> String {
    format!("fn:{label}")
}

/// The identifier of the first reference to the footnote labelled `label`, which the
/// footnote links back to.
pub fn footnote_reference_id(label: &str) -> String {
    format!("fnref:{label}")
}

/// Whether a link may be handed to the system URI launcher. Documents can come
/// from untrusted sources, so only web and mail links are ever followed.
pub fn is_safe_link(url: &str) -> bool {
    url.starts_with("http://") || url.starts_with("https://") || url.starts_with("mailto:")
}

/// What a link of the preview is followed to.
#[derive(Debug, PartialEq, Eq)]
pub enum LinkTarget {
    /// A web or mail address, for the system to open.
    Web(String),
    /// The heading of the document with this identifier.
    Heading(String),
    /// A Markdown file, which may not exist.
    Document(PathBuf),
}

/// What the link to `url` of a document in `base_dir` is followed to, if anything: web and
/// mail addresses, headings of the document, and Markdown files by their path from the
/// document's folder. The section of another file a link names is not looked for.
pub fn link_target(url: &str, base_dir: Option<&Path>) -> Option<LinkTarget> {
    if is_safe_link(url) {
        return Some(LinkTarget::Web(url.to_owned()));
    }
    if let Some(id) = url.strip_prefix('#') {
        return Some(LinkTarget::Heading(
            glib::Uri::unescape_string(id, None::<&str>).map_or_else(|| id.to_owned(), Into::into),
        ));
    }
    if glib::Uri::peek_scheme(url).is_some() {
        return None;
    }
    let path = url.split(['#', '?']).next().unwrap_or_default();
    let path = glib::Uri::unescape_string(path, None::<&str>)?;
    let path = Path::new(path.as_str());
    let markdown = path.extension().is_some_and(|extension| {
        extension.eq_ignore_ascii_case("md") || extension.eq_ignore_ascii_case("markdown")
    });
    let base_dir = base_dir?;
    (markdown && path.is_relative()).then(|| LinkTarget::Document(base_dir.join(path)))
}

/// What stands in the text of a raw HTML chunk for a tag that [`html_parts`] keeps, and for
/// a `<br>`, until the text is cut at it. Noncharacters, which are for such use, and are
/// dropped from the chunk first.
const TAG_MARK: char = '\u{FDD0}';
const BREAK_MARK: char = '\u{FDD1}';

/// The readable text of a raw HTML chunk: tags removed, with the scripts and style sheets
/// they hold, `<br>` turned into a line break, and spaces run together as a browser runs
/// them together, leaving out the lines of the source that hold no text. This renderer
/// cannot lay out HTML, but dropping the chunk outright lost the text inside it and ran the
/// surrounding words together.
pub fn strip_html(chunk: &str) -> String {
    collapse_spaces(&tag_text(chunk, &mut Vec::new())).replace(TAG_MARK, "")
}

/// The text of a raw HTML chunk with its tags removed, the tags [`html_parts`] keeps marked
/// and added to `tags`, and a `<br>` marked.
fn tag_text(chunk: &str, tags: &mut Vec<HtmlPart>) -> String {
    let chunk = chunk.replace([TAG_MARK, BREAK_MARK], "");
    let mut out = String::new();
    let mut rest = chunk.as_str();
    while let Some(start) = rest.find('<') {
        out.push_str(&rest[..start]);
        // A comment ends at `-->`, not at the first `>`, which a comment may
        // well contain.
        if rest[start..].starts_with("<!--") {
            match rest[start..].find("-->") {
                Some(end) => {
                    rest = &rest[start + end + 3..];
                    continue;
                }
                None => return out,
            }
        }
        let Some(end) = rest[start..].find('>') else {
            // Unterminated tag: keep the remainder as literal text.
            out.push_str(&rest[start..]);
            return out;
        };
        let tag = &rest[start..=start + end];
        let name = tag[1..tag.len() - 1]
            .trim_matches('/')
            .trim()
            .to_ascii_lowercase();
        let name = name.split_whitespace().next().unwrap_or_default();
        let opening = !tag[1..].starts_with('/');
        rest = &rest[start + end + 1..];
        let part = match (name, opening) {
            ("details", true) => Some(HtmlPart::DetailsStart {
                open: html_tags(tag, "details")
                    .first()
                    .is_some_and(|(_, attributes)| {
                        attributes.iter().any(|(name, _)| name == "open")
                    }),
            }),
            ("details", false) => Some(HtmlPart::DetailsEnd),
            ("summary", true) => Some(HtmlPart::SummaryStart),
            ("summary", false) => Some(HtmlPart::SummaryEnd),
            ("p" | "div", true) => Some(HtmlPart::BlockStart(HtmlBlock {
                align: html_tags(tag, name)
                    .first()
                    .and_then(|(_, attributes)| {
                        attributes
                            .iter()
                            .find(|(attribute, _)| attribute == "align")
                    })
                    .and_then(|(_, value)| HtmlAlign::from_attribute(value)),
                paragraph: name == "p",
            })),
            ("center", true) => Some(HtmlPart::BlockStart(HtmlBlock {
                align: Some(HtmlAlign::Center),
                paragraph: false,
            })),
            ("p" | "div" | "center", false) => Some(HtmlPart::BlockEnd {
                paragraph: name == "p",
            }),
            (name, true) => HtmlStyle::named(name).map(HtmlPart::StyleStart),
            (name, false) => HtmlStyle::named(name).map(HtmlPart::StyleEnd),
        };
        if let Some(part) = part {
            out.push(TAG_MARK);
            tags.push(part);
        } else if name == "br" {
            out.push(BREAK_MARK);
            // The line break of the source after it is not another one.
            rest = rest.trim_start_matches([' ', '\t']);
            rest = rest.strip_prefix('\n').unwrap_or(rest);
        } else if opening && matches!(name, "script" | "style") {
            let closing = format!("</{name}");
            match rest.to_ascii_lowercase().find(&closing) {
                Some(at) => rest = &rest[at..],
                None => return out,
            }
        }
    }
    out.push_str(rest);
    out
}

/// The text of [`tag_text`] with the spaces of each line run together, the line breaks of
/// the source kept only between lines with text, and each `<br>` a line break. Tag marks are
/// kept.
fn collapse_spaces(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    // Whether the line has text so far, whether spaces followed its last word, and whether
    // a line with text ended in the source since the last text.
    let (mut words, mut space, mut ended) = (false, false, false);
    for c in text.chars() {
        match c {
            '\n' => {
                ended |= words;
                words = false;
                space = false;
            }
            BREAK_MARK => {
                out.push('\n');
                (words, space, ended) = (false, false, false);
            }
            // A space before a tag stays before it, on the side of the tag it is on in the
            // source.
            TAG_MARK => {
                if space && words {
                    out.push(' ');
                    space = false;
                }
                out.push(c);
            }
            c if c.is_whitespace() => space = true,
            c => {
                if ended {
                    out.push('\n');
                } else if space && words {
                    out.push(' ');
                }
                (words, space, ended) = (true, false, false);
                out.push(c);
            }
        }
    }
    out
}

/// Ordered candidate GtkSourceView language ids for a fenced-code info string.
/// The raw token is tried first (so canonical ids like `rust`, `c`, `json`
/// work directly); a small alias map follows for common shorthands. An empty
/// or unknown info string yields no candidates, falling back to plain text.
fn lang_candidates(info: &str) -> Vec<String> {
    let token = info
        .split_whitespace()
        .next()
        .unwrap_or("")
        .to_ascii_lowercase();
    if token.is_empty() {
        return Vec::new();
    }

    let alias = match token.as_str() {
        "js" | "node" | "nodejs" | "javascript" => Some("js"),
        "ts" => Some("typescript"),
        "c++" | "cxx" | "cc" => Some("cpp"),
        "c#" | "csharp" | "cs" => Some("c-sharp"),
        "sh" | "shell" | "zsh" | "fish" | "bash" => Some("sh"),
        "py" | "python3" | "python" => Some("python3"),
        "rb" => Some("ruby"),
        "yml" => Some("yaml"),
        "rs" => Some("rust"),
        "md" => Some("markdown"),
        "htm" => Some("html"),
        "ps1" | "powershell" => Some("powershell"),
        _ => None,
    };

    let mut candidates = vec![token.clone()];
    if let Some(alias) = alias
        && alias != token
    {
        candidates.push(alias.to_string());
    }
    candidates
}

fn resolve_language(info: &str) -> Option<sourceview5::Language> {
    let manager = sourceview5::LanguageManager::default();
    lang_candidates(info)
        .into_iter()
        .find_map(|id| manager.language(&id))
}

/// How a stretch of highlighted code looks.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CodeStyle {
    /// Red, green and blue.
    pub color: Option<(u8, u8, u8)>,
    pub bold: bool,
    pub italic: bool,
}

/// A stretch of highlighted code, as a byte range of the code.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CodeRun {
    pub range: Range<usize>,
    pub style: CodeStyle,
}

/// The code of a code block as it is shown, without the newlines that end it.
pub fn shown_code(code: &str) -> &str {
    code.trim_end_matches('\n')
}

/// How long colouring code for an export holds the main loop before it lets the window
/// handle input and redraw.
const HIGHLIGHT_SLICE: Duration = Duration::from_millis(8);

/// The syntax colours of every code block of `text`, in order, as the preview gives them in
/// the light or the dark style. The runs are ranges of the code as [`shown_code`] gives it.
///
/// GtkSourceView objects belong to the main thread, so the colouring runs there, a slice
/// at a time: a document with hundreds of code blocks takes a good part of a second.
pub async fn code_highlights(text: &str, dark: bool) -> Vec<Vec<CodeRun>> {
    let scheme = sourceview5::StyleSchemeManager::default().scheme(if dark {
        "Adwaita-dark"
    } else {
        "Adwaita"
    });
    let mut blocks = Vec::new();
    let mut block: Option<(String, String)> = None;
    for (event, _) in events(text) {
        match event {
            Event::Start(Tag::CodeBlock(kind)) => {
                let info = match kind {
                    CodeBlockKind::Fenced(info) => info.to_string(),
                    CodeBlockKind::Indented => String::new(),
                };
                block = Some((info, String::new()));
            }
            Event::Text(text) => {
                if let Some((_, code)) = block.as_mut() {
                    code.push_str(&text);
                }
            }
            Event::End(TagEnd::CodeBlock) => blocks.extend(block.take()),
            _ => {}
        }
    }

    let buffer = sourceview5::Buffer::new(None);
    buffer.set_style_scheme(scheme.as_ref());
    let mut highlights = Vec::with_capacity(blocks.len());
    let mut slice = Instant::now();
    for (info, code) in blocks {
        highlights.push(highlight(&buffer, shown_code(&code), &info));
        if slice.elapsed() >= HIGHLIGHT_SLICE {
            // Below the priority of redrawing, so the window is drawn before this goes on.
            glib::timeout_future_with_priority(glib::Priority::DEFAULT_IDLE, Duration::ZERO).await;
            slice = Instant::now();
        }
    }
    highlights
}

/// The runs of `code` that GtkSourceView colours for the language `info` names, coloured
/// in `buffer`.
fn highlight(buffer: &sourceview5::Buffer, code: &str, info: &str) -> Vec<CodeRun> {
    let Some(language) = resolve_language(info) else {
        return Vec::new();
    };
    buffer.set_language(Some(&language));
    buffer.set_text(code);
    let (start, end) = buffer.bounds();
    buffer.ensure_highlight(&start, &end);

    let mut runs = Vec::new();
    let mut iter = start;
    let mut offset = 0;
    while !iter.is_end() {
        let mut next = iter;
        next.forward_to_tag_toggle(None::<&gtk::TextTag>);
        // Tags come in order of priority, so a later one wins.
        let style = iter
            .tags()
            .iter()
            .fold(CodeStyle::default(), |mut style, tag| {
                if tag.is_foreground_set()
                    && let Some(color) = tag.foreground_rgba()
                {
                    let channel = |value: f32| (value.clamp(0.0, 1.0) * 255.0).round() as u8;
                    style.color = Some((
                        channel(color.red()),
                        channel(color.green()),
                        channel(color.blue()),
                    ));
                }
                if tag.is_weight_set() {
                    style.bold = tag.weight() >= 600;
                }
                if tag.is_style_set() {
                    style.italic = tag.style() != gtk::pango::Style::Normal;
                }
                style
            });
        let length = buffer.text(&iter, &next, true).len();
        if style != CodeStyle::default() {
            runs.push(CodeRun {
                range: offset..offset + length,
                style,
            });
        }
        offset += length;
        iter = next;
    }
    runs
}

/// Builds a read-only, syntax-highlighted code block widget for the preview, with a
/// button that copies the code. Kept out of the focus chain so clicks never scroll the
/// preview, and width is bound to the viewport so it stays inside the reading column.
fn code_block_widget(
    code: &str,
    info: &str,
    indent: i32,
    padding: i32,
    hadj: &gtk::Adjustment,
) -> (gtk::Overlay, sourceview5::View, sourceview5::Buffer) {
    let src_buffer = sourceview5::Buffer::new(None);
    src_buffer.set_highlight_syntax(true);
    src_buffer.set_highlight_matching_brackets(false);
    if let Some(language) = resolve_language(info) {
        src_buffer.set_language(Some(&language));
    }
    // Every re-render rebuilds the block, so a runtime light/dark switch is
    // reflected on the next render.
    if let Some(scheme) = current_scheme() {
        src_buffer.set_style_scheme(Some(&scheme));
    }
    src_buffer.set_text(code);

    let src_view = sourceview5::View::with_buffer(&src_buffer);
    src_view.set_editable(false);
    src_view.set_cursor_visible(false);
    src_view.set_monospace(true);
    src_view.set_can_focus(false);
    src_view.set_focusable(false);
    src_view.set_show_line_numbers(false);
    src_view.set_highlight_current_line(false);
    src_view.set_wrap_mode(gtk::WrapMode::None);
    src_view.set_margin_top(12);
    src_view.set_margin_bottom(12);
    src_view.set_margin_start(12);
    src_view.set_margin_end(12);
    src_view.add_css_class("transparent-bg");
    src_view.add_css_class("code-view");

    let scroll = gtk::ScrolledWindow::builder()
        .hexpand(true)
        .propagate_natural_height(true)
        .hscrollbar_policy(gtk::PolicyType::Automatic)
        .vscrollbar_policy(gtk::PolicyType::Never)
        // Keep out of the focus chain so a click never makes the preview
        // scroll this block into view.
        .focusable(false)
        .build();
    scroll.add_css_class("card");
    scroll.set_child(Some(&src_view));

    let copy = gtk::Button::builder()
        .icon_name("edit-copy-symbolic")
        .tooltip_text(gettext("Copy Code"))
        .action_name("win.copy-code")
        .action_target(&code.to_variant())
        .build();
    copy.add_css_class("flat");
    // On the block's own surface, so the code scrolled under it does not show through.
    let copy_box = gtk::Box::builder()
        .halign(gtk::Align::End)
        .valign(gtk::Align::Start)
        .margin_top(6)
        .margin_end(6)
        .build();
    copy_box.add_css_class("code-copy");
    copy_box.append(&copy);

    let overlay = gtk::Overlay::builder()
        .margin_top(BLOCK_MARGIN)
        .margin_bottom(BLOCK_MARGIN)
        .margin_start(indent)
        .hexpand(true)
        .child(&scroll)
        .build();
    overlay.add_css_class("code-block");
    overlay.add_overlay(&copy_box);
    bind_width_to_page(&overlay, hadj, indent + padding);
    (overlay, src_view, src_buffer)
}

/// A muted foreground that stays readable in either appearance. Text tags
/// cannot reference CSS variables, so the two cases are picked by hand.
fn dim_foreground() -> &'static str {
    if adw::StyleManager::default().is_dark() {
        "rgba(255, 255, 255, 0.7)"
    } else {
        "rgba(0, 0, 0, 0.6)"
    }
}

/// Per-depth blockquote tag, created on demand so nesting stacks visually.
/// The indent is a `left-margin` rather than the `indent` property, which
/// only moves a paragraph's first line and so left every wrapped line back at
/// the page margin.
fn ensure_blockquote_tag(buffer: &TextBuffer, depth: i32, list_depth: usize) -> String {
    let name = format!("blockquote-{depth}");
    let tag_name = format!("{name}-{list_depth}");
    if buffer.tag_table().lookup(&tag_name).is_none() {
        buffer.create_tag(
            Some(&tag_name),
            &[
                (
                    "left-margin",
                    &(TEXT_MARGIN + list_depth as i32 * LIST_INDENT + depth * QUOTE_INDENT),
                ),
                // The box of the quote, which the preview draws behind it, ends before the
                // right edge of the column, and the text ends before the right edge of the
                // box.
                ("right-margin", &(TEXT_MARGIN + depth * QUOTE_PADDING)),
                ("style", &gtk::pango::Style::Italic),
                ("foreground", &dim_foreground()),
            ],
        );
    }
    tag_name
}

/// Per-depth list tag, created on demand so nested lists indent correctly.
/// The negative `indent` hangs the marker: GTK keeps the first line at the
/// left margin and shifts the wrapped lines right by that amount, so
/// continuation text lines up under the item text instead of under the bullet.
fn ensure_list_tag(buffer: &TextBuffer, depth: usize, quote_depth: i32) -> String {
    let name = format!("list-{depth}-{quote_depth}");
    if buffer.tag_table().lookup(&name).is_none() {
        buffer.create_tag(
            Some(&name),
            &[
                (
                    "left-margin",
                    &(TEXT_MARGIN + quote_depth * QUOTE_INDENT + (depth as i32 + 1) * LIST_INDENT),
                ),
                ("indent", &-LIST_INDENT),
            ],
        );
    }
    name
}

/// Resolve an image reference to a real local file, but only one contained
/// inside the document's own directory.
///
/// A Markdown document can come from an untrusted source (an email attachment, a
/// download). Rendering happens automatically on open, so an unrestricted image
/// reference would let a crafted document read and display arbitrary local files
/// (`![](/etc/passwd)`, `![](../../.ssh/id_rsa)`, `![](file:///...)`) and use the
/// render-vs-blank result as a file-existence oracle. Every candidate is
/// therefore canonicalized (which collapses `..` and resolves symlinks) and
/// required to stay within the canonicalized base directory. Remote schemes are
/// never fetched.
pub fn local_image_path(dest_url: &str, base_dir: Option<&Path>) -> Option<PathBuf> {
    let dest_url = dest_url.trim();
    if dest_url.is_empty() || dest_url.starts_with("data:") {
        return None;
    }

    let base_dir = base_dir?;

    let candidate = if dest_url.starts_with("file://") {
        gio::File::for_uri(dest_url).path()?
    } else if dest_url.contains("://") {
        return None;
    } else {
        let path = Path::new(dest_url);
        if path.is_absolute() {
            path.to_path_buf()
        } else {
            base_dir.join(path)
        }
    };

    let resolved = candidate.canonicalize().ok()?;
    let base = base_dir.canonicalize().ok()?;
    (resolved.starts_with(&base) && resolved.is_file()).then_some(resolved)
}

/// The visual marker for the next list item, advancing the ordered counter.
/// `Some(n)` at the top of the stack is an ordered list (renders `n. `);
/// otherwise a bullet that alternates by nesting depth.
pub fn list_marker(list_stack: &mut [Option<u64>]) -> String {
    match list_stack.last_mut() {
        Some(Some(number)) => {
            let marker = format!("{number}. ");
            *number += 1;
            marker
        }
        _ => format!(
            "{} ",
            if list_stack.len() % 2 == 1 {
                "•"
            } else {
                "◦"
            }
        ),
    }
}

/// A top-level block as it was last rendered: its source, where its output starts in the
/// preview buffer, and what it holds, with offsets from that start.
struct RenderedBlock {
    source: String,
    start: gtk::TextMark,
    links: Vec<(i32, i32, String)>,
    surfaces: Vec<Surface>,
    images: Vec<PathBuf>,
    /// The boxes of task list items, with the offset of their marker from the start of the
    /// block's source.
    tasks: Vec<Task>,
    headings: Vec<(i32, String)>,
    details: Vec<Details>,
    quotes: Vec<Quote>,
}

impl RenderedBlock {
    /// Let go of what the block's output leaves in `buffer` once the output is deleted, and
    /// keep in `released` the summary of each of its `<details>` elements and whether it was
    /// opened or closed in the preview.
    fn release(self, buffer: &TextBuffer, released: &mut Vec<(String, Option<bool>)>) {
        buffer.delete_mark(&self.start);
        for details in self.details {
            released.push((details.key, details.chosen.get()));
            buffer.tag_table().remove(&details.tag);
        }
    }
}

/// A `<details>` element in the preview: its summary, which shows or hides the rest, as
/// offsets in the preview buffer.
#[derive(Clone)]
pub struct Details {
    /// The triangle before the summary, which points at the content while it shows.
    pub icon: gtk::Image,
    /// The summary, from its triangle on.
    pub summary: Range<i32>,
    /// What the summary shows and hides, from the end of the summary's line.
    pub content: Range<i32>,
    /// The tag that hides the content.
    pub tag: gtk::TextTag,
    /// The text of the summary, which the element is known by from one render to the next.
    pub key: String,
    /// Whether the element is open until it is opened or closed in the preview.
    pub open: bool,
    /// Whether the element was opened or closed in the preview, which the copies of it share.
    pub chosen: Rc<Cell<Option<bool>>>,
}

impl Details {
    fn shifted(&self, by: i32) -> Self {
        Self {
            summary: self.summary.start + by..self.summary.end + by,
            content: self.content.start + by..self.content.end + by,
            ..self.clone()
        }
    }
}

/// What the preview buffer holds, so a render can keep the blocks that did not change.
#[derive(Default)]
pub struct Rendered {
    blocks: Vec<RenderedBlock>,
    base_dir: Option<PathBuf>,
    definitions: Vec<String>,
    /// The summaries of the `<details>` elements of the output deleted since the last render,
    /// in order, and whether each was opened or closed in the preview.
    released_details: Vec<(String, Option<bool>)>,
}

impl Rendered {
    /// Empty the buffer of `view`, so that the next render builds every block again.
    pub fn clear(&mut self, view: &TextView) {
        let buffer = view.buffer();
        let (mut start, mut end) = buffer.bounds();
        delete_output(view, &mut start, &mut end);
        for block in self.blocks.drain(..) {
            block.release(&buffer, &mut self.released_details);
        }
    }
}

/// Show or hide the content of `details` in `view`, and remember that it was chosen.
pub fn set_details_open(view: &TextView, details: &Details, open: bool) {
    let buffer = view.buffer();
    details.chosen.set(Some(open));
    show_details(details, open);
    let start = buffer.iter_at_offset(details.content.start);
    let end = buffer.iter_at_offset(details.content.end);
    show_widgets(&start, &end);
    // Hidden or shown text alone changes the height of the view without a new layout of the
    // window, which is then drawn without one.
    view.queue_resize();
}

/// Show or hide the text of the content of `details`, and turn its triangle. An open element
/// leaves its content as the elements around it have it, so that a closed one around it or
/// in it still hides.
fn show_details(details: &Details, open: bool) {
    if open {
        details.tag.set_invisible(false);
        details.tag.set_property("invisible-set", false);
    } else {
        details.tag.set_invisible(true);
    }
    details.icon.set_icon_name(Some(details_icon(open)));
}

/// Show the widgets between `start` and `end` whose place in the text shows, and hide the
/// others, which the view would otherwise draw where the hidden text is.
fn show_widgets(start: &gtk::TextIter, end: &gtk::TextIter) {
    for_each_widget(start, end, |anchor, widget| {
        widget.set_visible(!anchor.tags().iter().any(gtk::TextTag::is_invisible));
    });
}

/// The triangle before the summary of an open or a closed `<details>` element. An icon, as
/// few fonts have the triangles.
fn details_icon(open: bool) -> &'static str {
    if open {
        "pan-down-symbolic"
    } else {
        "pan-end-symbolic"
    }
}

/// Whether `event` can come between the start of a `<details>` element and its summary.
fn leads_to_summary(event: &Event) -> bool {
    match event {
        Event::Start(Tag::HtmlBlock) | Event::End(TagEnd::HtmlBlock) => true,
        Event::Html(chunk) | Event::InlineHtml(chunk) => html_parts(chunk)
            .iter()
            .find(|part| !matches!(part, HtmlPart::Text(text) if text.trim().is_empty()))
            .is_none_or(|part| *part == HtmlPart::SummaryStart),
        _ => false,
    }
}

/// Call `f` on each widget anchored between `start` and `end`, with where it is anchored.
fn for_each_widget(
    start: &gtk::TextIter,
    end: &gtk::TextIter,
    mut f: impl FnMut(&gtk::TextIter, &gtk::Widget),
) {
    let mut from = *start;
    // Child widgets match the object replacement character.
    while let Some((anchor_start, anchor_end)) =
        from.forward_search("\u{FFFC}", gtk::TextSearchFlags::empty(), Some(end))
    {
        if let Some(anchor) = anchor_start.child_anchor() {
            for widget in anchor.widgets() {
                f(&anchor_start, &widget);
            }
        }
        from = anchor_end;
    }
}

/// Start the summary of the innermost of `open_details` at `iter` with its triangle, and
/// end it with `text` unless the summary's text is still to come.
fn insert_summary(
    view: &TextView,
    iter: &mut gtk::TextIter,
    open_details: &mut [Details],
    text: Option<&str>,
) {
    let buffer = view.buffer();
    let Some(element) = open_details.last_mut() else {
        return;
    };
    start_line(&buffer, iter);
    element.summary.start = iter.offset();
    element.icon.add_css_class("details-icon");
    // Open, as the content shows until the element is ended.
    element.icon.set_icon_name(Some(details_icon(true)));
    let anchor = buffer.create_child_anchor(iter);
    view.add_child_at_anchor(&element.icon, &anchor);
    buffer.insert(iter, " ");
    if let Some(text) = text {
        buffer.insert(iter, text);
        element.key.push_str(text);
        end_summary(&buffer, iter, open_details);
    }
}

/// End the summary of the innermost of `open_details` at `iter`; its content follows.
fn end_summary(buffer: &TextBuffer, iter: &mut gtk::TextIter, open_details: &mut [Details]) {
    let Some(element) = open_details.last_mut() else {
        return;
    };
    element.summary.end = iter.offset();
    element.content.start = iter.offset();
    // A blank line parts the summary from the content, as it parts other blocks, and is
    // hidden with the content.
    buffer.insert(iter, "\n\n");
}

/// End the innermost of `open_details` at `iter`, open or closed as it was left. `released`
/// holds the elements of the output this render replaces that are not yet matched, and one
/// with the same summary is the same element; an element without one is added to
/// `unmatched`, to be matched once all are ended. Hiding the widgets is left to the caller.
fn end_details(
    buffer: &TextBuffer,
    iter: &mut gtk::TextIter,
    open_details: &mut Vec<Details>,
    released: &mut [Option<(String, Option<bool>)>],
    unmatched: &mut Vec<Details>,
) -> Option<Details> {
    let mut element = open_details.pop()?;
    // The content is hidden with the newline that ends it, as a line whose newline shows
    // takes up a line, and the blank line after it stays, to part the summary from what
    // follows while the content is hidden.
    let iter_offset = iter.offset();
    let mut end = *iter;
    while end.offset() > element.content.start && {
        let mut before = end;
        before.backward_char() && before.char() == '\n'
    } {
        end.backward_char();
    }
    if end.offset() < iter_offset {
        end.forward_char();
    }
    element.content.end = end.offset();
    let start = buffer.iter_at_offset(element.content.start);
    buffer.tag_table().add(&element.tag);
    buffer.apply_tag(&element.tag, &start, &end);
    let same = released
        .iter_mut()
        .find(|entry| entry.as_ref().is_some_and(|(key, _)| *key == element.key))
        .and_then(Option::take);
    match same {
        Some((_, chosen)) => element.chosen.set(chosen),
        None => unmatched.push(element.clone()),
    }
    show_details(&element, element.chosen.get().unwrap_or(element.open));
    // Applying the tag left the iterator behind.
    *iter = buffer.iter_at_offset(iter_offset);
    Some(element)
}

/// Delete the output between `start` and `end` from the buffer of `view`, and the widgets in
/// it.
///
/// The widgets are taken out of the view first. Deleting the text of a widget under the
/// pointer unmaps it in the middle of the deletion, and GTK then reads the half-changed
/// buffer to tell the view that the pointer is back over it, and crashes.
fn delete_output(view: &TextView, start: &mut gtk::TextIter, end: &mut gtk::TextIter) {
    for_each_widget(start, end, |_, widget| view.remove(widget));
    view.buffer().delete(start, end);
}

/// The link reference and footnote definitions of a document, in a comparable form. A
/// block can use either wherever they are defined, so a change to them changes blocks
/// elsewhere.
fn definitions(link_definitions: &RefDefs, events: &[(Event, Range<usize>)]) -> Vec<String> {
    let mut definitions: Vec<String> = link_definitions
        .iter()
        .map(|(label, def)| {
            format!(
                "[{label}]: {} {}",
                def.dest,
                def.title.as_deref().unwrap_or_default()
            )
        })
        .chain(events.iter().filter_map(|(event, _)| match event {
            Event::Start(Tag::FootnoteDefinition(label)) => Some(format!("[^{label}]")),
            _ => None,
        }))
        .collect();
    definitions.sort();
    definitions
}

/// The top-level blocks of a document, as the range of their events and of their source.
/// A `<details>` element is one block with all it holds, which is shown or hidden together,
/// and so is a `<div>` or `<center>` element, whose alignment its blocks take; one that is
/// not closed, as while it is typed, holds nothing but itself.
fn top_level_blocks(events: &[(Event, Range<usize>)]) -> Vec<(Range<usize>, Range<usize>)> {
    let mut blocks = Vec::new();
    let mut first = 0;
    while first < events.len() {
        let rest = &events[first..];
        let length = block_length(rest, true)
            .or_else(|| block_length(rest, false))
            .unwrap_or(rest.len());
        let last = &events[first + length - 1].1;
        blocks.push((
            first..first + length,
            events[first].1.start.min(last.start)..last.end,
        ));
        first += length;
    }
    blocks
}

/// How many events the first top-level block of `events` has, with the `<details>`, `<div>`
/// and `<center>` elements it opens if `with_details`, or `None` if it does not end.
fn block_length(events: &[(Event, Range<usize>)], with_details: bool) -> Option<usize> {
    let mut depth = 0usize;
    let mut details = 0isize;
    for (index, (event, _)) in events.iter().enumerate() {
        match event {
            Event::Start(_) => depth += 1,
            Event::End(_) => depth = depth.saturating_sub(1),
            Event::Html(chunk) | Event::InlineHtml(chunk) if with_details => {
                let block = matches!(event, Event::Html(_));
                details = (details + html_depth_change(chunk, block)).max(0);
            }
            _ => {}
        }
        if depth == 0 && details == 0 {
            return Some(index + 1);
        }
    }
    None
}

/// How many blocks at the start and at the end of `new` are the same as in `old`, without
/// the two overlapping.
fn unchanged_ends(old: &[&str], new: &[&str]) -> (usize, usize) {
    let prefix = old.iter().zip(new).take_while(|(a, b)| a == b).count();
    let suffix = old[prefix..]
        .iter()
        .rev()
        .zip(new[prefix..].iter().rev())
        .take_while(|(a, b)| a == b)
        .count();
    (prefix, suffix)
}

/// Renders `text` into `view`'s buffer and returns the clickable link ranges and the
/// searchable child surfaces, for the caller to wire up clicks and search. Only the
/// top-level blocks that changed since the last render are rebuilt; a document of
/// thousands of lines would otherwise stall on every pause in typing.
pub fn render_markdown(
    view: &TextView,
    text: &str,
    hadj: &gtk::Adjustment,
    image_base_dir: Option<&Path>,
    rendered: &mut Rendered,
) -> RenderResult {
    let buffer = view.buffer();

    // The offset iterator keeps the parser, and with it the link definitions.
    let mut parser = Parser::new_ext(text, parser_options()).into_offset_iter();
    let events = extend_events(text, parser.by_ref().collect(), font_has_emoji);
    let definitions = definitions(parser.reference_definitions(), &events);
    let blocks = top_level_blocks(&events);

    // Images resolve against the document's directory, and links against definitions
    // any block may hold: when either changes, every block is rendered again.
    let base_dir = image_base_dir.map(Path::to_path_buf);
    if rendered.base_dir != base_dir || rendered.definitions != definitions {
        rendered.clear(view);
        rendered.base_dir = base_dir;
        rendered.definitions = definitions;
    }

    let old_sources: Vec<&str> = rendered.blocks.iter().map(|b| b.source.as_str()).collect();
    let new_sources: Vec<&str> = blocks
        .iter()
        .map(|(_, source)| &text[source.clone()])
        .collect();
    let (prefix, suffix) = unchanged_ends(&old_sources, &new_sources);
    let removed = prefix..rendered.blocks.len() - suffix;
    let block_start = |index: usize| {
        rendered.blocks.get(index).map_or_else(
            || buffer.end_iter(),
            |block| buffer.iter_at_mark(&block.start),
        )
    };
    let mut iter = block_start(removed.start);
    let mut removed_end = block_start(removed.end);
    delete_output(view, &mut iter, &mut removed_end);
    for block in rendered.blocks.drain(removed) {
        block.release(&buffer, &mut rendered.released_details);
    }
    let mut released_details: Vec<_> = std::mem::take(&mut rendered.released_details)
        .into_iter()
        .map(Some)
        .collect();
    // The rebuilt `<details>` elements that no released one has the summary of.
    let mut unmatched_details = Vec::new();
    let insert_offset = iter.offset();
    // Text inserted where a tag starts takes the tag, so the output of the blocks after the
    // rebuilt ones sheds its tags until the rebuilt output is in, and then takes them back.
    let following_tags: Vec<(gtk::TextTag, i32)> = iter
        .tags()
        .into_iter()
        .map(|tag| {
            let mut end = iter;
            end.forward_to_tag_toggle(Some(&tag));
            (tag, end.offset() - insert_offset)
        })
        .collect();
    for (tag, length) in &following_tags {
        let start = buffer.iter_at_offset(insert_offset);
        let end = buffer.iter_at_offset(insert_offset + length);
        buffer.remove_tag(tag, &start, &end);
    }
    let mut iter = buffer.iter_at_offset(insert_offset);
    let mut new_blocks = Vec::new();
    let mut added = Vec::new();
    let mut added_tasks = Vec::new();

    let mut current_tags: Vec<String> = Vec::new();
    // The tags of the styles raw HTML opened, which it may never close, and the blocks it
    // opened, with how each aligns its content.
    let mut html_styles: Vec<&'static str> = Vec::new();
    let mut html_blocks: OpenHtml<HtmlBlock> = OpenHtml::default();

    // One entry per open list. `Some(n)` is an ordered list whose next item
    // number is `n`; `None` is a bullet list. Length doubles as nesting depth.
    let mut list_stack: Vec<Option<u64>> = Vec::new();
    // Open blockquote nesting level, used to indent block widgets.
    let mut blockquote_depth: i32 = 0;

    let mut in_table = false;
    // The markup of each cell, and the formulas in its text with their sources.
    let mut table_rows: Vec<Vec<(String, math_view::LabelFormulas)>> = Vec::new();
    let mut table_alignments: Vec<Alignment> = Vec::new();
    let mut current_row: Vec<(String, math_view::LabelFormulas)> = Vec::new();
    let mut current_cell = String::new();
    let mut cell_formulas: math_view::LabelFormulas = Vec::new();
    // The markup open in the current cell, and whether the cell has text that is not code.
    let mut cell_markup: Vec<CellMarkup> = Vec::new();
    let mut cell_has_text = false;

    let mut in_code_block = false;
    let mut current_code = String::new();
    let mut current_code_lang = String::new();

    // The open image tag: its picture once the reference resolved to a local
    // file, and the alt text collected so far. A reference that did not
    // resolve keeps `None` and falls back to rendering the alt text.
    let mut current_image: Option<(Option<gtk::Picture>, String)> = None;

    // Clickable link ranges and the open-link stack of (url, start_offset).
    let mut links: Vec<(i32, i32, String)> = Vec::new();
    let mut link_starts: Vec<(String, i32)> = Vec::new();
    // The labels of the footnotes being rendered.
    let mut footnote_labels: Vec<String> = Vec::new();
    // Searchable child surfaces (code blocks, table cells).
    let mut surfaces: Vec<Surface> = Vec::new();
    let mut shown_images: Vec<PathBuf> = Vec::new();
    let mut tasks: Vec<Task> = Vec::new();
    let mut headings: Vec<(i32, String)> = Vec::new();
    // The blockquotes being rendered, where each started, the depth of the lists around it and
    // its kind of alert, and those rendered.
    let mut quote_starts: Vec<(i32, usize, Option<BlockQuoteKind>)> = Vec::new();
    let mut quotes: Vec<Quote> = Vec::new();
    // The offsets of the bullet of the item just started, which the box of a task takes the
    // place of.
    let mut item_bullet: Option<(i32, i32)> = None;
    // The tag of the title of the alert just started, until the title's paragraph ends.
    let mut alert_title: Option<&'static str> = None;
    let mut details: Vec<Details> = Vec::new();

    let middle = &blocks[prefix..blocks.len() - suffix];
    let mut events = events
        .into_iter()
        .skip(middle.first().map_or(0, |(events, _)| events.start));
    for (block_events, source) in middle {
        let start_offset = iter.offset();
        let start = buffer.create_mark(None, &iter, true);
        // The `<details>` elements open around the text being rendered, and whether the text
        // is their summary. An element is closed within its block, or not at all.
        let mut open_details: Vec<Details> = Vec::new();
        let mut in_summary = false;
        // A `<details>` element started, and its summary may still come.
        let mut awaiting_summary = false;
        for (event, event_range) in events.by_ref().take(block_events.len()) {
            for block in html_blocks.follow(&event) {
                if let Some(align) = block.align {
                    close_tag(&mut current_tags, align.text_tag());
                }
            }
            // Content before any summary is summed up as GitHub sums it up.
            if awaiting_summary && !leads_to_summary(&event) {
                awaiting_summary = false;
                // Translators: the summary of a part of a document that is shown and hidden
                // by clicking it, when the document gives it none.
                let summary = gettext("Details");
                insert_summary(view, &mut iter, &mut open_details, Some(&summary));
            }
            if in_code_block {
                match event {
                    Event::Text(t) | Event::Code(t) => {
                        current_code.push_str(&t);
                    }
                    Event::End(TagEnd::CodeBlock) => {
                        in_code_block = false;
                        let indent = block_indent(&list_stack, blockquote_depth);
                        let clean_code = shown_code(&current_code);
                        // A block in a quote ends inside the quote's box.
                        let (scroll, code_view, code_buffer) = code_block_widget(
                            clean_code,
                            &current_code_lang,
                            indent,
                            blockquote_depth * QUOTE_PADDING,
                            hadj,
                        );

                        start_line(&buffer, &mut iter);
                        let anchor_offset = iter.offset();
                        let anchor = buffer.create_child_anchor(&mut iter);
                        view.add_child_at_anchor(&scroll, &anchor);
                        keep_presses(&scroll, view);
                        surfaces.push(Surface::Code {
                            anchor_offset,
                            buffer: code_buffer,
                            view: code_view,
                        });
                        end_widget_block(&buffer, &mut iter, !list_stack.is_empty());
                    }
                    _ => {}
                }
                continue;
            }

            if in_table {
                match event {
                    Event::Start(Tag::TableHead) => {
                        current_row = Vec::new();
                    }
                    Event::End(TagEnd::TableHead) => {
                        table_rows.push(std::mem::take(&mut current_row));
                    }
                    Event::Start(Tag::TableRow) => current_row = Vec::new(),
                    Event::End(TagEnd::TableRow) => {
                        table_rows.push(std::mem::take(&mut current_row));
                    }
                    Event::Start(Tag::TableCell) => {
                        current_cell = String::new();
                        cell_has_text = false;
                    }
                    Event::End(TagEnd::TableCell) => {
                        for markup in cell_markup.drain(..).rev() {
                            current_cell.push_str(markup.end);
                        }
                        // A line of code alone sits higher than the text of the cells beside
                        // it, as the code font rises less above the baseline. A word joiner in
                        // the text font, which shows nothing, sets the line on the same
                        // baseline.
                        if !cell_has_text && !current_cell.is_empty() {
                            current_cell.insert(0, '\u{2060}');
                        }
                        current_row.push((
                            std::mem::take(&mut current_cell),
                            std::mem::take(&mut cell_formulas),
                        ));
                    }
                    Event::Start(tag @ (Tag::Strong | Tag::Emphasis | Tag::Strikethrough)) => {
                        let markup = match tag {
                            Tag::Strong => CellMarkup::new("<b>", "</b>"),
                            Tag::Emphasis => CellMarkup::new("<i>", "</i>"),
                            _ => CellMarkup::new("<s>", "</s>"),
                        };
                        start_cell_markup(&mut current_cell, &mut cell_markup, markup);
                    }
                    Event::End(
                        tag @ (TagEnd::Strong | TagEnd::Emphasis | TagEnd::Strikethrough),
                    ) => {
                        let end = match tag {
                            TagEnd::Strong => "</b>",
                            TagEnd::Emphasis => "</i>",
                            _ => "</s>",
                        };
                        end_cell_markup(&mut current_cell, &mut cell_markup, |markup| {
                            markup.html.is_none() && markup.end == end
                        });
                    }
                    // Cell links become real Pango links, so they keep the theme's
                    // link colour and stay clickable, and the preview follows them as
                    // it follows the others. A link it does not follow is never put in
                    // an `href`, only underlined, since the label's default handler
                    // would hand it straight to the system launcher.
                    Event::Start(Tag::Link { dest_url, .. }) => {
                        let markup = if link_target(&dest_url, image_base_dir).is_some() {
                            CellMarkup::new(
                                format!("<a href=\"{}\">", glib::markup_escape_text(&dest_url)),
                                "</a>",
                            )
                        } else {
                            CellMarkup::new("<u>", "</u>")
                        };
                        start_cell_markup(&mut current_cell, &mut cell_markup, markup);
                    }
                    Event::End(TagEnd::Link) => {
                        end_cell_markup(&mut current_cell, &mut cell_markup, |markup| {
                            markup.html.is_none() && matches!(markup.end, "</a>" | "</u>")
                        });
                    }
                    // A formula is drawn in the place of an object replacement character, and set
                    // in the line of the cell, as a label cannot hold a formula on a line of its
                    // own.
                    Event::InlineMath(ref latex) | Event::DisplayMath(ref latex)
                        if let Some(formula) = math::typeset(latex, false) =>
                    {
                        current_cell.push('\u{FFFC}');
                        cell_formulas.push((formula, latex.to_string()));
                    }
                    Event::Code(c) | Event::InlineMath(c) | Event::DisplayMath(c) => {
                        current_cell.push_str(&format!(
                            "{}{}</span>",
                            monospace_markup(),
                            cell_text(&c)
                        ));
                    }
                    Event::Text(t) => {
                        cell_has_text |= !t.trim().is_empty();
                        current_cell.push_str(&cell_text(&t));
                    }
                    Event::Html(html) | Event::InlineHtml(html) => {
                        for part in html_parts_with_emoji(&html, font_has_emoji) {
                            match part {
                                HtmlPart::Text(text) => {
                                    cell_has_text |= !text.trim().is_empty();
                                    current_cell.push_str(&cell_text(&text));
                                }
                                HtmlPart::StyleStart(style) => start_cell_markup(
                                    &mut current_cell,
                                    &mut cell_markup,
                                    CellMarkup::html(style),
                                ),
                                HtmlPart::StyleEnd(style) => {
                                    end_cell_markup(
                                        &mut current_cell,
                                        &mut cell_markup,
                                        |markup| markup.html == Some(style),
                                    );
                                }
                                _ => {}
                            }
                        }
                    }
                    Event::End(TagEnd::Table) => {
                        in_table = false;
                        let indent = block_indent(&list_stack, blockquote_depth);
                        let grid = Grid::builder().hexpand(true).build();
                        grid.add_css_class("preview-table");
                        // A table wider than the column scrolls sideways, like a code block,
                        // rather than squeezing its columns until the words break apart.
                        let scroll = gtk::ScrolledWindow::builder()
                            .margin_top(BLOCK_MARGIN)
                            .margin_bottom(BLOCK_MARGIN)
                            .margin_start(indent)
                            .hexpand(true)
                            .propagate_natural_height(true)
                            .hscrollbar_policy(gtk::PolicyType::Automatic)
                            .vscrollbar_policy(gtk::PolicyType::Never)
                            .focusable(false)
                            .child(&grid)
                            .build();
                        scroll.add_css_class("card");
                        // Clipped to the card's rounded corners, which the header's tint
                        // would otherwise square off.
                        scroll.set_overflow(gtk::Overflow::Hidden);
                        bind_width_to_page(
                            &scroll,
                            hadj,
                            indent + blockquote_depth * QUOTE_PADDING,
                        );

                        let num_cols = table_rows.first().map_or(1, |r| r.len());
                        let grid_cols = (num_cols * 2).saturating_sub(1) as i32;

                        let mut cell_labels: Vec<gtk::Label> = Vec::new();
                        for (row_idx, row) in table_rows.iter().enumerate() {
                            let text_row = (row_idx * 2) as i32;

                            if row_idx > 0 {
                                let hsep = gtk::Separator::builder()
                                    .orientation(gtk::Orientation::Horizontal)
                                    .hexpand(true)
                                    .build();
                                grid.attach(&hsep, 0, text_row - 1, grid_cols, 1);
                            }

                            for (col_idx, (cell_text, formulas)) in row.iter().enumerate() {
                                let text_col = (col_idx * 2) as i32;

                                let xalign = match table_alignments.get(col_idx) {
                                    Some(Alignment::Right) => 1.0_f32,
                                    Some(Alignment::Center) => 0.5,
                                    _ => 0.0,
                                };

                                // Padded by the stylesheet rather than with margins, so that the
                                // header's background fills its cells.
                                let label = Label::builder()
                                    .wrap(true)
                                    .wrap_mode(gtk::pango::WrapMode::Word)
                                    .max_width_chars(CELL_WRAP_CHARS)
                                    .xalign(xalign)
                                    .hexpand(true)
                                    // Selectable so table text can be copied.
                                    .selectable(true)
                                    .build();
                                label.set_markup(cell_text);
                                // A cell is only as narrow as its text up to the wrapping width,
                                // so short cells never wrap and long ones wrap at that width.
                                let chars = label.text().chars().count();
                                label.set_width_chars(
                                    i32::try_from(chars)
                                        .unwrap_or(CELL_WRAP_CHARS)
                                        .min(CELL_WRAP_CHARS),
                                );
                                // Selectable labels take focus by default; keep
                                // them out of the focus chain so a click never
                                // makes the preview scroll the table into view.
                                label.set_focusable(false);
                                if row_idx == 0 {
                                    label.add_css_class("heading");
                                }
                                let cell = math_view::label_with_formulas(&label, formulas.clone());
                                grid.attach(&cell, text_col, text_row, 1, 1);
                                cell_labels.push(label);

                                if col_idx > 0 {
                                    let vsep = gtk::Separator::builder()
                                        .orientation(gtk::Orientation::Vertical)
                                        .vexpand(true)
                                        .build();
                                    grid.attach(&vsep, text_col - 1, text_row, 1, 1);
                                }
                            }
                        }
                        start_line(&buffer, &mut iter);
                        let anchor_offset = iter.offset();
                        let anchor = buffer.create_child_anchor(&mut iter);
                        view.add_child_at_anchor(&scroll, &anchor);
                        keep_presses(&scroll, view);
                        for label in cell_labels {
                            surfaces.push(Surface::Cell {
                                anchor_offset,
                                label,
                            });
                        }
                        end_widget_block(&buffer, &mut iter, !list_stack.is_empty());
                    }
                    _ => {}
                }
                continue;
            }

            match event {
                Event::Start(tag) => match tag {
                    Tag::CodeBlock(kind) => {
                        in_code_block = true;
                        current_code.clear();
                        current_code_lang = match kind {
                            CodeBlockKind::Fenced(info) => info.to_string(),
                            CodeBlockKind::Indented => String::new(),
                        };
                    }
                    Tag::Table(alignments) => {
                        in_table = true;
                        table_rows.clear();
                        table_alignments = alignments;
                    }
                    Tag::Heading { level, id, .. } => {
                        if let Some(id) = id {
                            headings.push((iter.offset() - start_offset, id.to_string()));
                        }
                        let level_num = level as u8;
                        current_tags.push(
                            match level_num {
                                1 => "h1",
                                2 => "h2",
                                3 => "h3",
                                _ => "h4",
                            }
                            .to_string(),
                        );
                    }
                    Tag::Strong => current_tags.push("bold".to_string()),
                    Tag::Emphasis => current_tags.push("italic".to_string()),
                    Tag::Strikethrough => current_tags.push("strikethrough".to_string()),
                    Tag::Link { dest_url, .. } => {
                        current_tags.push("link".to_string());
                        link_starts.push((dest_url.to_string(), iter.offset()));
                    }
                    Tag::Image {
                        link_type,
                        dest_url,
                        id,
                        ..
                    } => {
                        current_image = Some((None, String::new()));
                        let path = local_image_path(dest_url.as_ref(), image_base_dir);
                        if let Some(path) = &path {
                            shown_images.push(path.clone());
                        }
                        let width = image_width(link_type, &id);
                        if let Some(picture) =
                            path.and_then(|path| image_picture(&path, hadj, width))
                        {
                            current_image = Some((Some(picture.clone()), String::new()));
                            picture.set_focusable(false);
                            picture.set_margin_top(BLOCK_MARGIN);
                            picture.set_margin_bottom(BLOCK_MARGIN);
                            picture.set_hexpand(false);
                            picture.set_halign(gtk::Align::Center);

                            let anchor_offset = iter.offset();
                            let anchor = buffer.create_child_anchor(&mut iter);
                            view.add_child_at_anchor(&picture, &anchor);
                            // Aligned as the block of raw HTML it is in aligns it.
                            if let Some(align) = html_blocks.iter().rev().find_map(|b| b.align) {
                                let start = buffer.iter_at_offset(anchor_offset);
                                buffer.apply_tag_by_name(align.text_tag(), &start, &iter);
                            }
                        }
                    }
                    Tag::BlockQuote(kind) => {
                        blockquote_depth += 1;
                        let name =
                            ensure_blockquote_tag(&buffer, blockquote_depth, list_stack.len());
                        current_tags.push(name);
                        start_line(&buffer, &mut iter);
                        quote_starts.push((iter.offset(), list_stack.len(), kind));
                        if let Some(kind) = kind {
                            let table = buffer.tag_table();
                            // Over the colour of the quote, whose tag may be newer.
                            if let Some(tag) = table.lookup(alert_tag(kind)) {
                                tag.set_priority(table.size() - 1);
                            }
                            alert_title = Some(alert_tag(kind));
                            current_tags.push(alert_tag(kind).to_owned());
                        }
                    }
                    Tag::DefinitionListTitle => current_tags.push("bold".to_owned()),
                    Tag::DefinitionListDefinition => current_tags.push("definition".to_owned()),
                    Tag::List(first) => {
                        // A nested list opens while the parent item's line is still
                        // open, which ran its first marker on after the item text.
                        if !iter.starts_line() {
                            buffer.insert(&mut iter, "\n");
                        }
                        list_stack.push(first);
                        let name = ensure_list_tag(&buffer, list_stack.len() - 1, blockquote_depth);
                        current_tags.push(name);
                    }
                    Tag::Item => {
                        let marker = list_marker(&mut list_stack);
                        let start_offset = iter.offset();
                        buffer.insert(&mut iter, &marker);
                        if list_stack.last() == Some(&None) {
                            item_bullet = Some((start_offset, iter.offset()));
                        }
                        let start_iter = buffer.iter_at_offset(start_offset);
                        buffer.apply_tag_by_name("bold", &start_iter, &iter);
                        // The marker opens the line, and GTK takes a paragraph's
                        // margins from its first characters, so the indenting tags
                        // have to cover the marker as well as the item text.
                        for tag in &current_tags {
                            buffer.apply_tag_by_name(tag, &start_iter, &iter);
                        }
                    }
                    Tag::FootnoteDefinition(label) => {
                        end_block(&buffer, &mut iter);
                        headings.push((iter.offset() - start_offset, footnote_id(&label)));
                        footnote_labels.push(label.to_string());
                        let start_offset = iter.offset();
                        buffer.insert(&mut iter, &format!("[{label}]: "));
                        let start_iter = buffer.iter_at_offset(start_offset);
                        buffer.apply_tag_by_name("bold", &start_iter, &iter);
                    }
                    _ => {}
                },
                Event::End(tag_end) => match tag_end {
                    TagEnd::Heading(_) => {
                        close_html_styles(&mut current_tags, &mut html_styles);
                        // Headings cannot nest, so every open heading tag is this one.
                        current_tags.retain(|t| !matches!(t.as_str(), "h1" | "h2" | "h3" | "h4"));
                        end_block(&buffer, &mut iter);
                    }
                    TagEnd::Strong => close_tag(&mut current_tags, "bold"),
                    TagEnd::Emphasis => close_tag(&mut current_tags, "italic"),
                    TagEnd::Strikethrough => close_tag(&mut current_tags, "strikethrough"),
                    TagEnd::Link => {
                        close_tag(&mut current_tags, "link");
                        if let Some((url, start)) = link_starts.pop() {
                            links.push((start, iter.offset(), url));
                        }
                    }
                    TagEnd::Image => {
                        if let Some((picture, alt)) = current_image.take() {
                            match picture {
                                Some(picture) if !alt.is_empty() => {
                                    picture.set_alternative_text(Some(&alt))
                                }
                                Some(_) => {}
                                // The reference did not resolve: a remote URL, a
                                // missing file, or a path outside the document's
                                // own directory. Show the alt text so the document
                                // does not silently lose the content.
                                None if !alt.is_empty() => {
                                    let start_offset = iter.offset();
                                    buffer.insert(&mut iter, &alt);
                                    let start_iter = buffer.iter_at_offset(start_offset);
                                    buffer.apply_tag_by_name("image-alt", &start_iter, &iter);
                                    for tag in &current_tags {
                                        buffer.apply_tag_by_name(tag, &start_iter, &iter);
                                    }
                                }
                                None => {}
                            }
                        }
                    }
                    TagEnd::DefinitionListTitle => {
                        close_tag(&mut current_tags, "bold");
                        start_line(&buffer, &mut iter);
                    }
                    TagEnd::DefinitionListDefinition => {
                        close_tag(&mut current_tags, "definition");
                        start_line(&buffer, &mut iter);
                    }
                    TagEnd::DefinitionList => end_block(&buffer, &mut iter),
                    // The paragraphs of raw HTML it leaves open end with it.
                    TagEnd::HtmlBlock => {
                        for block in html_blocks.close(false, |block| block.paragraph) {
                            if let Some(align) = block.align {
                                close_tag(&mut current_tags, align.text_tag());
                            }
                        }
                    }
                    // The note ends with a link back to where it is referred to, after its
                    // last word rather than on a line of its own.
                    TagEnd::FootnoteDefinition => {
                        if let Some(label) = footnote_labels.pop() {
                            let end_offset = iter.offset();
                            let mut at = iter;
                            while at.offset() > 0 {
                                let mut before = at;
                                before.backward_char();
                                if before.char() != '\n' {
                                    break;
                                }
                                at = before;
                            }
                            let link_start = at.offset() + 1;
                            buffer.insert(&mut at, FOOTNOTE_BACKLINK);
                            let start_iter = buffer.iter_at_offset(link_start);
                            buffer.apply_tag_by_name("link", &start_iter, &at);
                            let url = format!("#{}", footnote_reference_id(&label));
                            links.push((link_start, at.offset(), url));
                            iter =
                                buffer.iter_at_offset(end_offset + (at.offset() - link_start + 1));
                        }
                    }
                    TagEnd::BlockQuote(_) => {
                        let name = format!("blockquote-{blockquote_depth}-");
                        if let Some(index) = current_tags.iter().rposition(|t| t.starts_with(&name))
                        {
                            current_tags.remove(index);
                        }
                        // The box ends with the last line of the quote, not the blank line
                        // after it.
                        if let Some((start, list_depth, kind)) = quote_starts.pop() {
                            let mut end = iter;
                            while end.offset() > start {
                                let mut before = end;
                                before.backward_char();
                                if before.char() != '\n' {
                                    break;
                                }
                                end = before;
                            }
                            quotes.push(Quote {
                                start: start - start_offset,
                                end: end.offset() - start_offset,
                                left: TEXT_MARGIN
                                    + i32::try_from(list_depth).unwrap_or(0) * LIST_INDENT
                                    + (blockquote_depth - 1) * QUOTE_INDENT,
                                right: TEXT_MARGIN + (blockquote_depth - 1) * QUOTE_PADDING,
                                alert: kind,
                            });
                        }
                        blockquote_depth = (blockquote_depth - 1).max(0);
                        end_block(&buffer, &mut iter);
                    }
                    TagEnd::List(_) => {
                        let depth = list_stack.len().saturating_sub(1);
                        let name = format!("list-{depth}-");
                        if let Some(index) = current_tags.iter().rposition(|t| t.starts_with(&name))
                        {
                            current_tags.remove(index);
                        }
                        list_stack.pop();
                        // A nested list ends inside its parent item; the outermost one is a
                        // block of its own, followed by a blank line like any other.
                        if list_stack.is_empty() {
                            end_block(&buffer, &mut iter);
                        }
                    }
                    TagEnd::Item => {
                        // A loose item ends with its paragraph's blank line already.
                        if !iter.starts_line() {
                            buffer.insert(&mut iter, "\n");
                        }
                    }
                    TagEnd::Paragraph => {
                        close_html_styles(&mut current_tags, &mut html_styles);
                        // The title of an alert sits right over its text.
                        if let Some(name) = alert_title.take() {
                            close_tag(&mut current_tags, name);
                            start_line(&buffer, &mut iter);
                        } else {
                            end_block(&buffer, &mut iter);
                        }
                    }
                    _ => {}
                },
                Event::Text(t) => {
                    if let Some((_, alt)) = current_image.as_mut() {
                        alt.push_str(&t);
                        continue;
                    }
                    let start_offset = iter.offset();
                    buffer.insert(&mut iter, &t);
                    let start_iter = buffer.iter_at_offset(start_offset);
                    for tag in &current_tags {
                        buffer.apply_tag_by_name(tag, &start_iter, &iter);
                    }
                }
                Event::Code(c) => {
                    if let Some((_, alt)) = current_image.as_mut() {
                        alt.push_str(&c);
                        continue;
                    }
                    let start_offset = iter.offset();
                    buffer.insert(&mut iter, &c);
                    let start_iter = buffer.iter_at_offset(start_offset);
                    buffer.apply_tag_by_name("code", &start_iter, &iter);
                    for tag in &current_tags {
                        buffer.apply_tag_by_name(tag, &start_iter, &iter);
                    }
                }
                Event::InlineMath(ref latex) | Event::DisplayMath(ref latex) => {
                    if let Some((_, alt)) = current_image.as_mut() {
                        alt.push_str(latex);
                        continue;
                    }
                    let display = matches!(event, Event::DisplayMath(_));
                    // Math that does not typeset has been made code already.
                    let Some(formula) = math::typeset(latex, display) else {
                        continue;
                    };
                    // A displayed formula is a centred line of its own, as on GitHub.
                    if display {
                        start_line(&buffer, &mut iter);
                    }
                    let start_offset = iter.offset();
                    let anchor = buffer.create_child_anchor(&mut iter);
                    let math_view = BlinkMathView::new(formula, display, latex);
                    // As dim as the text of the quote it is in.
                    if blockquote_depth > 0 {
                        math_view.add_css_class("dim-label");
                    }
                    view.add_child_at_anchor(&math_view, &anchor);
                    let start_iter = buffer.iter_at_offset(start_offset);
                    if display {
                        buffer.apply_tag_by_name("math-display", &start_iter, &iter);
                    }
                    for tag in &current_tags {
                        buffer.apply_tag_by_name(tag, &start_iter, &iter);
                    }
                    if display {
                        buffer.insert(&mut iter, "\n");
                    }
                }
                Event::Html(ref html) | Event::InlineHtml(ref html)
                    if current_image.is_none()
                        && html_parts(html)
                            .iter()
                            .any(|part| !matches!(part, HtmlPart::Text(_))) =>
                {
                    for part in html_parts_with_emoji(html, font_has_emoji) {
                        if awaiting_summary
                            && !matches!(&part, HtmlPart::SummaryStart)
                            && !matches!(&part, HtmlPart::Text(text) if text.trim().is_empty())
                        {
                            awaiting_summary = false;
                            let summary = gettext("Details");
                            insert_summary(view, &mut iter, &mut open_details, Some(&summary));
                        }
                        match part {
                            HtmlPart::DetailsStart { open } => {
                                open_details.push(Details {
                                    icon: gtk::Image::new(),
                                    summary: 0..0,
                                    content: 0..0,
                                    tag: gtk::TextTag::new(None),
                                    key: String::new(),
                                    open,
                                    chosen: Rc::default(),
                                });
                                awaiting_summary = true;
                            }
                            HtmlPart::SummaryStart => {
                                if awaiting_summary {
                                    awaiting_summary = false;
                                    in_summary = true;
                                    insert_summary(view, &mut iter, &mut open_details, None);
                                }
                            }
                            HtmlPart::SummaryEnd => {
                                if in_summary {
                                    in_summary = false;
                                    end_summary(&buffer, &mut iter, &mut open_details);
                                }
                            }
                            HtmlPart::StyleStart(style) => {
                                current_tags.push(style.text_tag().to_owned());
                                html_styles.push(style.text_tag());
                            }
                            HtmlPart::StyleEnd(style) => {
                                if let Some(index) = html_styles
                                    .iter()
                                    .rposition(|name| *name == style.text_tag())
                                {
                                    html_styles.remove(index);
                                    close_tag(&mut current_tags, style.text_tag());
                                }
                            }
                            HtmlPart::Text(text) => {
                                if text.trim_matches('\n').is_empty() {
                                    continue;
                                }
                                let text = if iter.starts_line() {
                                    text.trim_start_matches('\n')
                                } else {
                                    &text
                                };
                                let start_offset = iter.offset();
                                buffer.insert(&mut iter, text);
                                let start_iter = buffer.iter_at_offset(start_offset);
                                for tag in &current_tags {
                                    buffer.apply_tag_by_name(tag, &start_iter, &iter);
                                }
                                if in_summary && let Some(open) = open_details.last_mut() {
                                    open.key.push_str(text);
                                }
                            }
                            // A block starts and ends a line; blocks inside text are not laid
                            // out.
                            HtmlPart::BlockStart(block) if matches!(event, Event::Html(_)) => {
                                start_line(&buffer, &mut iter);
                                if let Some(align) = block.align {
                                    current_tags.push(align.text_tag().to_owned());
                                }
                                html_blocks.open(block);
                            }
                            HtmlPart::BlockEnd { paragraph } if matches!(event, Event::Html(_)) => {
                                start_line(&buffer, &mut iter);
                                for block in html_blocks.close(true, |b| b.paragraph == paragraph) {
                                    if let Some(align) = block.align {
                                        close_tag(&mut current_tags, align.text_tag());
                                    }
                                }
                            }
                            HtmlPart::BlockStart(_) | HtmlPart::BlockEnd { .. } => {}
                            HtmlPart::DetailsEnd => {
                                if in_summary {
                                    in_summary = false;
                                    end_summary(&buffer, &mut iter, &mut open_details);
                                }
                                if let Some(element) = end_details(
                                    &buffer,
                                    &mut iter,
                                    &mut open_details,
                                    &mut released_details,
                                    &mut unmatched_details,
                                ) {
                                    details.push(element.shifted(-start_offset));
                                }
                            }
                        }
                    }
                    if matches!(event, Event::Html(_)) {
                        close_html_styles(&mut current_tags, &mut html_styles);
                    }
                }
                // A block chunk holding nothing but markup, such as a comment, is dropped,
                // or it would leave a stray blank line behind. An inline chunk is kept as it
                // comes, since a `<br>` legitimately reduces to just a newline.
                Event::Html(html) if strip_html(&html).trim().is_empty() => {}
                Event::Html(html) | Event::InlineHtml(html) => {
                    let text = strip_html(&html);
                    if text.is_empty() {
                        continue;
                    }
                    if let Some((_, alt)) = current_image.as_mut() {
                        alt.push_str(&text);
                        continue;
                    }
                    let start_offset = iter.offset();
                    buffer.insert(&mut iter, &text);
                    let start_iter = buffer.iter_at_offset(start_offset);
                    for tag in &current_tags {
                        buffer.apply_tag_by_name(tag, &start_iter, &iter);
                    }
                }
                Event::FootnoteReference(name) => {
                    headings.push((iter.offset() - start_offset, footnote_reference_id(&name)));
                    let start_offset = iter.offset();
                    buffer.insert(&mut iter, &format!("[{name}]"));
                    let start_iter = buffer.iter_at_offset(start_offset);
                    buffer.apply_tag_by_name("footnote", &start_iter, &iter);
                    buffer.apply_tag_by_name("link", &start_iter, &iter);
                    links.push((
                        start_offset,
                        iter.offset(),
                        format!("#{}", footnote_id(&name)),
                    ));
                }
                Event::TaskListMarker(checked) => {
                    // A bullet and a box would be two markers for one item.
                    if let Some((bullet_start, bullet_end)) = item_bullet.take()
                        && bullet_end == iter.offset()
                    {
                        let mut bullet = buffer.iter_at_offset(bullet_start);
                        buffer.delete(&mut bullet, &mut iter);
                    }
                    let start_offset = iter.offset();
                    let check = gtk::CheckButton::builder()
                        .active(checked)
                        .focus_on_click(false)
                        .build();
                    check.add_css_class("task-check");
                    let anchor = buffer.create_child_anchor(&mut iter);
                    view.add_child_at_anchor(&check, &anchor);
                    // The box opens the line, which takes its margins from it.
                    let start_iter = buffer.iter_at_offset(start_offset);
                    for tag in &current_tags {
                        buffer.apply_tag_by_name(tag, &start_iter, &iter);
                    }
                    tasks.push(Task {
                        anchor,
                        check,
                        source: event_range.start - source.start,
                    });
                }
                Event::Rule => {
                    let sep = gtk::Separator::builder()
                        .orientation(gtk::Orientation::Horizontal)
                        .hexpand(true)
                        .margin_top(8)
                        .margin_bottom(8)
                        .focusable(false)
                        .can_focus(false)
                        .build();
                    // Anchored children are allocated at their minimum width, and a
                    // separator's is one pixel, so without this the rule would be a
                    // stub.
                    bind_width_to_page(&sep, hadj, 0);
                    let anchor = buffer.create_child_anchor(&mut iter);
                    view.add_child_at_anchor(&sep, &anchor);
                    end_block(&buffer, &mut iter);
                }
                Event::SoftBreak | Event::HardBreak => {
                    if let Some((_, alt)) = current_image.as_mut() {
                        alt.push(' ');
                        continue;
                    }
                    // A single line break in the source only wraps the source; the paragraph
                    // reflows to the width of the preview, as it does in the HTML export.
                    let separator = if matches!(event, Event::SoftBreak) {
                        // Not at the start of the line after a displayed formula.
                        if iter.starts_line() {
                            continue;
                        }
                        " "
                    } else {
                        "\n"
                    };
                    let start_offset = iter.offset();
                    buffer.insert(&mut iter, separator);
                    let start_iter = buffer.iter_at_offset(start_offset);
                    for tag in &current_tags {
                        buffer.apply_tag_by_name(tag, &start_iter, &iter);
                    }
                }
            }
        }
        // The elements of raw HTML left open end with their block, which a block after it that
        // is rendered again would not know of.
        for block in html_blocks.close_all() {
            if let Some(align) = block.align {
                close_tag(&mut current_tags, align.text_tag());
            }
        }
        // Every block ends with the same blank line, so that what a block renders to never
        // depends on the block before it.
        end_block(&buffer, &mut iter);
        added.extend(surfaces.iter().cloned());
        added_tasks.extend(tasks.iter().cloned());
        new_blocks.push(RenderedBlock {
            source: text[source.clone()].to_owned(),
            start,
            links: links
                .drain(..)
                .map(|(link_start, end, url)| (link_start - start_offset, end - start_offset, url))
                .collect(),
            surfaces: surfaces
                .drain(..)
                .map(|surface| surface.shifted(-start_offset))
                .collect(),
            images: std::mem::take(&mut shown_images),
            tasks: std::mem::take(&mut tasks),
            headings: std::mem::take(&mut headings),
            quotes: std::mem::take(&mut quotes),
            details: std::mem::take(&mut details),
        });
    }

    let following_offset = iter.offset();
    // A rebuilt element whose summary changed is known by its place among those whose
    // summary did.
    for (element, (_, chosen)) in unmatched_details
        .iter()
        .zip(released_details.into_iter().flatten())
    {
        if let Some(chosen) = chosen {
            element.chosen.set(Some(chosen));
            show_details(element, chosen);
        }
    }
    show_widgets(
        &buffer.iter_at_offset(insert_offset),
        &buffer.iter_at_offset(following_offset),
    );
    for (tag, length) in &following_tags {
        let start = buffer.iter_at_offset(following_offset);
        let end = buffer.iter_at_offset(following_offset + length);
        buffer.apply_tag(tag, &start, &end);
    }
    let iter = buffer.iter_at_offset(following_offset);

    // The marks of the blocks after the rebuilt ones stay where the rebuilt output was
    // inserted, before it.
    if iter.offset() != insert_offset {
        for block in &rendered.blocks[prefix..] {
            if buffer.iter_at_mark(&block.start).offset() != insert_offset {
                break;
            }
            buffer.move_mark(&block.start, &iter);
        }
    }
    rendered.blocks.splice(prefix..prefix, new_blocks);

    let mut result = RenderResult {
        links: Vec::new(),
        surfaces: Vec::new(),
        added,
        tasks: Vec::new(),
        added_tasks,
        headings: Vec::new(),
        details: Vec::new(),
        quotes: Vec::new(),
    };
    let mut images = HashSet::new();
    // The rendered blocks are the blocks of the text, in order.
    for (block, (_, source)) in rendered.blocks.iter().zip(&blocks) {
        let offset = buffer.iter_at_mark(&block.start).offset();
        result.tasks.extend(block.tasks.iter().map(|task| Task {
            source: task.source + source.start,
            ..task.clone()
        }));
        result.headings.extend(
            block
                .headings
                .iter()
                .map(|(start, id)| (start + offset, id.clone())),
        );
        result
            .details
            .extend(block.details.iter().map(|details| details.shifted(offset)));
        result.quotes.extend(block.quotes.iter().map(|quote| Quote {
            start: quote.start + offset,
            end: quote.end + offset,
            ..*quote
        }));
        result.links.extend(
            block
                .links
                .iter()
                .map(|(start, end, url)| (start + offset, end + offset, url.clone())),
        );
        result
            .surfaces
            .extend(block.surfaces.iter().map(|surface| surface.shifted(offset)));
        images.extend(block.images.iter());
    }
    IMAGES.with(|cache| cache.borrow_mut().retain(|path, _| images.contains(path)));
    result
}

#[cfg(test)]
mod tests {
    use super::{
        HtmlAlign, HtmlBlock, HtmlPart, HtmlStyle, LinkTarget, OpenHtml, bare_links, close_tag,
        definitions, events, heading_slug, html_images, html_parts, image_width, is_safe_link,
        lang_candidates, link_target, list_marker, local_image_path, replace_shortcodes,
        shown_size, strip_html, top_level_blocks, unchanged_ends, wiki_destination, word_count,
    };
    use pulldown_cmark::{CodeBlockKind, Event, LinkType, Options, Parser, Tag, TagEnd};
    use std::fs;
    use std::ops::Range;
    use std::path::{Path, PathBuf};

    fn offset_events(text: &str) -> Vec<(Event<'_>, Range<usize>)> {
        Parser::new_ext(text, Options::ENABLE_TABLES | Options::ENABLE_FOOTNOTES)
            .into_offset_iter()
            .collect()
    }

    #[test]
    fn top_level_blocks_cover_whole_blocks() {
        let text = "# Title\n\nOne\ntwo\n\n- a\n  - b\n\n---\n\n```\ncode\n```\n";
        let events = offset_events(text);
        let sources: Vec<&str> = top_level_blocks(&events)
            .into_iter()
            .map(|(_, source)| text[source].trim_end())
            .collect();
        assert_eq!(
            sources,
            vec!["# Title", "One\ntwo", "- a\n  - b", "---", "```\ncode\n```"]
        );
    }

    #[test]
    fn top_level_blocks_partition_the_events() {
        let text = "Para\n\n> quote\n>\n> more\n\n| a |\n|---|\n| 1 |\n";
        let events = offset_events(text);
        let blocks = top_level_blocks(&events);
        assert_eq!(blocks.first().map(|(events, _)| events.start), Some(0));
        for pair in blocks.windows(2) {
            assert_eq!(pair[0].0.end, pair[1].0.start);
        }
        assert_eq!(
            blocks.last().map(|(events, _)| events.end),
            Some(events.len())
        );
    }

    #[test]
    fn unchanged_ends_do_not_overlap() {
        assert_eq!(unchanged_ends(&["a", "b", "c"], &["a", "x", "c"]), (1, 1));
        assert_eq!(unchanged_ends(&["a", "b"], &["a", "b"]), (2, 0));
        // A repeated block counts once, at the start or at the end.
        assert_eq!(unchanged_ends(&["a", "a"], &["a", "a", "a"]), (2, 0));
        assert_eq!(unchanged_ends(&["a", "b", "c"], &["a", "c"]), (1, 1));
        assert_eq!(unchanged_ends(&[], &["a"]), (0, 0));
    }

    #[test]
    fn definitions_track_links_and_footnotes() {
        let events = |text| {
            let mut parser = Parser::new_ext(text, Options::ENABLE_FOOTNOTES).into_offset_iter();
            let events: Vec<_> = parser.by_ref().collect();
            definitions(parser.reference_definitions(), &events)
        };
        assert_eq!(
            events("[a]: https://example.invalid\n\n[^n]: note\n"),
            vec!["[^n]", "[a]: https://example.invalid "]
        );
        assert_ne!(
            events("[a]: https://example.invalid/1\n"),
            events("[a]: https://example.invalid/2\n")
        );
    }

    #[test]
    fn list_marker_orders_and_alternates_bullets() {
        let mut ordered = vec![Some(1u64)];
        assert_eq!(list_marker(&mut ordered), "1. ");
        assert_eq!(list_marker(&mut ordered), "2. ");
        assert_eq!(ordered, vec![Some(3)]);

        // Odd nesting depth -> filled bullet, even -> hollow.
        assert_eq!(list_marker(&mut [None]), "• ");
        assert_eq!(list_marker(&mut [None, None]), "◦ ");
    }

    #[test]
    fn resolves_relative_images_against_document_directory() {
        let root = std::env::temp_dir().join(format!("blink-markdown-test-{}", std::process::id()));
        let image_dir = root.join("images");
        let image = image_dir.join("photo.png");
        fs::create_dir_all(&image_dir).unwrap();
        fs::write(&image, b"").unwrap();

        assert_eq!(
            local_image_path("images/photo.png", Some(&root)),
            Some(image.canonicalize().unwrap())
        );

        let _ = fs::remove_file(image);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn rejects_remote_images() {
        assert!(local_image_path("https://example.invalid/image.png", None).is_none());
        assert!(local_image_path("data:image/png;base64,AAAA", None).is_none());
    }

    #[test]
    fn rejects_paths_outside_document_directory() {
        // base/ holds the document; secret.png sits one level up, outside it.
        let root = std::env::temp_dir().join(format!("blink-md-escape-{}", std::process::id()));
        let base = root.join("doc");
        fs::create_dir_all(&base).unwrap();
        let secret = root.join("secret.png");
        fs::write(&secret, b"top secret").unwrap();

        // `..` traversal to a real file outside base is rejected.
        assert!(local_image_path("../secret.png", Some(&base)).is_none());
        // An absolute path to a real system file is rejected.
        assert!(local_image_path("/etc/hostname", Some(&base)).is_none());
        // A file:// URI pointing outside base is rejected.
        let secret_uri = format!("file://{}", secret.display());
        assert!(local_image_path(&secret_uri, Some(&base)).is_none());
        // No base directory (untitled document) rejects everything local.
        assert!(local_image_path("photo.png", None).is_none());

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn strip_html_keeps_text_and_breaks() {
        // Tags go, the text between them stays, and `<br>` becomes a break so
        // the surrounding words do not run together.
        assert_eq!(strip_html("<b>bold</b>"), "bold");
        assert_eq!(strip_html("a<br>b"), "a\nb");
        assert_eq!(strip_html("a<br />b"), "a\nb");
        assert_eq!(strip_html("<summary>Click</summary>"), "Click");
        assert_eq!(strip_html("plain"), "plain");
        // An unterminated tag is kept verbatim rather than swallowing the rest.
        assert_eq!(strip_html("a <b"), "a <b");
        // Comments go whole, including any `>` inside them.
        assert_eq!(strip_html("a<!-- x > y -->b"), "ab");
        assert_eq!(strip_html("a<!-- unterminated"), "a");
        // Scripts and style sheets are not text, and spaces run together.
        assert_eq!(
            strip_html("<div>\n  <b>Bold</b>,  <i>it</i><br>\n  next <!-- c --> word\n</div>\n"),
            "Bold, it\nnext word"
        );
        assert_eq!(strip_html("a<br><br>b<br>"), "a\n\nb\n");
        assert_eq!(strip_html("<script>alert(\"<b>\")</script>after"), "after");
        assert_eq!(strip_html("<STYLE type=x>p {}</style>"), "");
    }

    #[test]
    fn close_tag_closes_only_the_innermost() {
        let mut tags = vec!["bold".to_string(), "italic".to_string(), "bold".to_string()];
        close_tag(&mut tags, "bold");
        assert_eq!(tags, vec!["bold", "italic"]);
        close_tag(&mut tags, "link");
        assert_eq!(tags, vec!["bold", "italic"]);
    }

    #[test]
    fn only_web_and_mail_links_are_followed() {
        assert!(is_safe_link("https://example.invalid"));
        assert!(is_safe_link("mailto:someone@example.invalid"));
        assert!(!is_safe_link("file:///etc/passwd"));
        assert!(!is_safe_link("javascript:alert(1)"));
    }

    #[test]
    fn bare_addresses_become_links_without_the_punctuation_after_them() {
        let links = |text| -> Vec<(String, String)> {
            bare_links(text)
                .into_iter()
                .map(|(range, url)| (text[range].to_owned(), url))
                .collect()
        };
        assert_eq!(
            links("See https://example.com/a_b. And www.example.com, too"),
            [
                (
                    "https://example.com/a_b".into(),
                    "https://example.com/a_b".into()
                ),
                ("www.example.com".into(), "http://www.example.com".into()),
            ]
        );
        // A closing bracket is kept only with its opening one.
        assert_eq!(
            links("(https://en.wikipedia.org/wiki/Foo_(bar))"),
            [(
                "https://en.wikipedia.org/wiki/Foo_(bar)".into(),
                "https://en.wikipedia.org/wiki/Foo_(bar)".into()
            )]
        );
        // Not in the middle of a word, and not without a host.
        assert!(links("xhttps://example.com").is_empty());
        assert!(links("https:// and www.").is_empty());
    }

    #[test]
    fn img_tags_give_their_source_and_description() {
        let chunk = r#"<p align="center"><img width=200 src="logo.png" alt='A &amp; B'/></p>"#;
        let images = html_images(chunk);
        assert_eq!(images.len(), 1);
        let (range, source, alt, width) = &images[0];
        assert!(chunk[range.clone()].starts_with("<img") && chunk[range.clone()].ends_with("/>"));
        assert_eq!((source.as_str(), alt.as_str()), ("logo.png", "A & B"));
        assert_eq!(*width, Some(200));
        assert_eq!(html_images("<img src=a width='50%'>")[0].3, None);
        assert!(html_images("<imgx src=a>").is_empty());
        assert!(html_images("<img src=a").is_empty());
    }

    #[test]
    fn headings_get_the_identifiers_of_github() {
        assert_eq!(heading_slug("Hello, World! 2"), "hello-world-2");
        assert_eq!(heading_slug("snake_case and-dash"), "snake_case-and-dash");
        assert_eq!(heading_slug("Привіт світ"), "привіт-світ");
        let ids: Vec<String> = events(
            "# A

## A

### `b` c
",
        )
        .into_iter()
        .filter_map(|(event, _)| match event {
            Event::Start(Tag::Heading { id, .. }) => id.map(|id| id.to_string()),
            _ => None,
        })
        .collect();
        assert_eq!(ids, ["a", "a-1", "b-c"]);
    }

    #[test]
    fn headings_with_emoji_shortcodes_are_named_by_the_shortcode() {
        // As GitHub names `:rocket: Enhancement`.
        let ids: Vec<String> = events("#### :rocket: Enhancement\n")
            .into_iter()
            .filter_map(|(event, _)| match event {
                Event::Start(Tag::Heading { id, .. }) => id.map(|id| id.to_string()),
                _ => None,
            })
            .collect();
        assert_eq!(ids, ["rocket-enhancement"]);
    }

    #[test]
    fn wiki_links_point_at_markdown_files() {
        assert_eq!(wiki_destination("Other page"), "Other page.md");
        assert_eq!(wiki_destination("notes.txt"), "notes.txt");
        assert_eq!(
            wiki_destination("Page#Some Section"),
            "Page.md#some-section"
        );
        assert_eq!(wiki_destination("#Here"), "#here");
    }

    #[test]
    fn front_matter_and_math_that_does_not_typeset_read_as_code() {
        let events = events(
            "---\ntitle: x\n---\n\n$$\na^2\n$$\n\n$$\n\\nope\n$$\n\nInline $b$ and $\\nope$.\n",
        );
        let code_blocks: Vec<String> = events
            .iter()
            .filter_map(|(event, _)| match event {
                Event::Start(Tag::CodeBlock(CodeBlockKind::Fenced(info))) => Some(info.to_string()),
                _ => None,
            })
            .collect();
        assert_eq!(code_blocks, ["yaml", "latex"]);
        let has = |wanted: Event| events.iter().any(|(event, _)| *event == wanted);
        assert!(has(Event::DisplayMath("\na^2\n".into())));
        assert!(has(Event::InlineMath("b".into())));
        assert!(has(Event::Text("\\nope\n".into())));
        assert!(has(Event::Code("\\nope".into())));
    }

    #[test]
    fn addresses_become_links_outside_code_and_links_only() {
        let text = "Go to https://a.example now\n\n[https://b.example](https://c.example) `https://d.example`\n\n```\nhttps://e.example\n```\n";
        let events = events(text);
        let links: Vec<(String, LinkType)> = events
            .iter()
            .filter_map(|(event, _)| match event {
                Event::Start(Tag::Link {
                    dest_url,
                    link_type,
                    ..
                }) => Some((dest_url.to_string(), *link_type)),
                _ => None,
            })
            .collect();
        assert_eq!(
            links,
            [
                ("https://a.example".into(), LinkType::Autolink),
                ("https://c.example".into(), LinkType::Inline),
            ]
        );
        // The link's text keeps its place in the source.
        let range = events
            .iter()
            .find(|(event, _)| *event == Event::Text("https://a.example".into()))
            .map(|(_, range)| range.clone())
            .unwrap();
        assert_eq!(&text[range], "https://a.example");
    }

    #[test]
    fn alerts_start_with_their_title() {
        let events: Vec<Event> = events("> [!WARNING]\n> Careful\n")
            .into_iter()
            .map(|(event, _)| event)
            .collect();
        assert_eq!(
            events[1..6],
            [
                Event::Start(Tag::Paragraph),
                Event::Start(Tag::Strong),
                Event::Text("Warning".into()),
                Event::End(TagEnd::Strong),
                Event::End(TagEnd::Paragraph),
            ]
        );
    }

    #[test]
    fn html_images_become_images() {
        let events: Vec<Event> = events("<p><img src=\"a.png\" alt=\"A\"></p>\n")
            .into_iter()
            .map(|(event, _)| event)
            .collect();
        assert!(events.contains(&Event::Text("A".into())));
        assert!(events.iter().any(|event| matches!(
            event,
            Event::Start(Tag::Image { dest_url, .. }) if dest_url.as_ref() == "a.png"
        )));
    }

    #[test]
    fn links_are_followed_to_the_web_headings_and_markdown_files() {
        let base = Path::new("/docs");
        assert_eq!(
            link_target("https://example.com", Some(base)),
            Some(LinkTarget::Web("https://example.com".into()))
        );
        assert_eq!(
            link_target("#caf%C3%A9", Some(base)),
            Some(LinkTarget::Heading("café".into()))
        );
        assert_eq!(
            link_target("guide/My%20Notes.md#intro", Some(base)),
            Some(LinkTarget::Document(PathBuf::from(
                "/docs/guide/My Notes.md"
            )))
        );
        assert_eq!(
            link_target("../README.MARKDOWN", Some(base)),
            Some(LinkTarget::Document(PathBuf::from(
                "/docs/../README.MARKDOWN"
            )))
        );
        assert_eq!(link_target("script.sh", Some(base)), None);
        assert_eq!(link_target("/etc/notes.md", Some(base)), None);
        assert_eq!(link_target("file:///docs/a.md", Some(base)), None);
        assert_eq!(link_target("a.md", None), None);
    }

    #[test]
    fn ticking_a_box_leaves_the_word_count() {
        assert_eq!(word_count("- [ ] buy milk\n- [x] done"), 7);
        assert_eq!(word_count("- [x] buy milk\n- [x] done"), 7);
        assert_eq!(word_count("a [ b ]"), 4);
    }

    #[test]
    fn shortcodes_become_emoji() {
        assert_eq!(
            replace_shortcodes("Done :tada: :+1:", |_| true),
            "Done 🎉 👍"
        );
        assert_eq!(
            replace_shortcodes("at 10:30:45 :nope: a:b", |_| true),
            "at 10:30:45 :nope: a:b"
        );
        assert_eq!(replace_shortcodes("::smile::", |_| true), ":😄:");
        // An emoji no font has keeps its shortcode.
        assert_eq!(
            replace_shortcodes(":tada: :+1:", |emoji| emoji == "👍"),
            ":tada: 👍"
        );
        // So does one written out, whole, as a sequence of several characters.
        assert_eq!(
            replace_shortcodes("🎉 👍 👨‍👩‍👧 🇺🇦 → 日本", |emoji| emoji
                == "👍"),
            ":tada: 👍 :family_man_woman_girl: :ukraine: → 日本"
        );
        let events = events("`:tada:` :tada:");
        assert!(
            events
                .iter()
                .any(|(event, _)| *event == Event::Code(":tada:".into()))
        );
        assert!(
            events
                .iter()
                .any(|(event, _)| *event == Event::Text(" 🎉".into()))
        );
    }

    #[test]
    fn html_blocks_are_read_whole() {
        let chunks: Vec<String> = events(
            "<script>\nalert(1)\n</script>\n\n<p>\n<b>a</b>\n</p>\n\n<img\n  src=\"a.png\" width=\"40\">\n",
        )
        .into_iter()
        .filter_map(|(event, _)| match event {
            Event::Html(chunk) => Some(chunk.to_string()),
            Event::Start(Tag::Image { link_type, id, .. }) => {
                Some(format!("image {:?}", image_width(link_type, &id)))
            }
            _ => None,
        })
        .collect();
        assert_eq!(
            chunks,
            [
                "<script>\nalert(1)\n</script>\n",
                "<p>\n<b>a</b>\n</p>\n",
                "image Some(40)",
            ]
        );
        assert_eq!(strip_html(&chunks[0]), "");
        assert_eq!(shown_size((200, 100), Some(50)), (50, 25));
        assert_eq!(shown_size((200, 100), None), (200, 100));
    }

    #[test]
    fn details_and_summaries_are_found_in_html() {
        assert_eq!(
            html_parts("<DETAILS open>\n<summary class=x>Title <em>here</em></summary>\n"),
            [
                HtmlPart::DetailsStart { open: true },
                HtmlPart::SummaryStart,
                HtmlPart::Text("Title ".into()),
                HtmlPart::StyleStart(HtmlStyle::Italic),
                HtmlPart::Text("here".into()),
                HtmlPart::StyleEnd(HtmlStyle::Italic),
                HtmlPart::SummaryEnd,
            ]
        );
        assert_eq!(
            html_parts("<B>x</b> <kbd>y</kbd><span>z</span>"),
            [
                HtmlPart::StyleStart(HtmlStyle::Bold),
                HtmlPart::Text("x".into()),
                HtmlPart::StyleEnd(HtmlStyle::Bold),
                HtmlPart::Text(" ".into()),
                HtmlPart::StyleStart(HtmlStyle::Keyboard),
                HtmlPart::Text("y".into()),
                HtmlPart::StyleEnd(HtmlStyle::Keyboard),
                HtmlPart::Text("z".into()),
            ]
        );
        assert_eq!(
            html_parts("<p align=\"CENTER\">a</p><div align=right><center></center></div><div>"),
            [
                HtmlPart::BlockStart(HtmlBlock {
                    align: Some(HtmlAlign::Center),
                    paragraph: true
                }),
                HtmlPart::Text("a".into()),
                HtmlPart::BlockEnd { paragraph: true },
                HtmlPart::BlockStart(HtmlBlock {
                    align: Some(HtmlAlign::Right),
                    paragraph: false
                }),
                HtmlPart::BlockStart(HtmlBlock {
                    align: Some(HtmlAlign::Center),
                    paragraph: false
                }),
                HtmlPart::BlockEnd { paragraph: false },
                HtmlPart::BlockEnd { paragraph: false },
                HtmlPart::BlockStart(HtmlBlock {
                    align: None,
                    paragraph: false
                }),
            ]
        );
        // A tag in a comment or a script is not one, and the marks of the tags cannot be
        // forged.
        assert_eq!(
            html_parts("<!-- <b> --><script><b></script>a\u{FDD0}b"),
            [HtmlPart::Text("ab".into())]
        );
        assert_eq!(html_parts("</details >"), [HtmlPart::DetailsEnd]);
        assert!(
            html_parts("<detailsx>")
                .iter()
                .all(|part| matches!(part, HtmlPart::Text(_)))
        );
    }

    #[test]
    fn an_unclosed_details_element_leaves_the_blocks_after_it() {
        let text = "<details>\n<summary>S</summary>\n\nInside\n\nAfter\n";
        let events = super::events(text);
        let sources: Vec<&str> = top_level_blocks(&events)
            .into_iter()
            .map(|(_, source)| text[source].trim_end())
            .collect();
        assert_eq!(
            sources,
            ["<details>\n<summary>S</summary>", "Inside", "After"]
        );
    }

    #[test]
    fn a_details_element_is_one_block() {
        let text = "Before\n\n<details>\n<summary>S</summary>\n\nInside\n\n- item\n\n</details>\n\nAfter\n";
        let events = super::events(text);
        let sources: Vec<&str> = top_level_blocks(&events)
            .into_iter()
            .map(|(_, source)| text[source].trim_end())
            .collect();
        assert_eq!(
            sources,
            [
                "Before",
                "<details>\n<summary>S</summary>\n\nInside\n\n- item\n\n</details>",
                "After"
            ]
        );
    }

    #[test]
    fn a_div_element_is_one_block_but_in_text() {
        let text = "<div align=\"center\">\n\n# Title\n\n</div>\n\nA <div> b\n\nAfter\n";
        let events = super::events(text);
        let sources: Vec<&str> = top_level_blocks(&events)
            .into_iter()
            .map(|(_, source)| text[source].trim_end())
            .collect();
        assert_eq!(
            sources,
            [
                "<div align=\"center\">\n\n# Title\n\n</div>",
                "A <div> b",
                "After"
            ]
        );
    }

    #[test]
    fn open_html_ends_elements_in_their_container() {
        let mut open = OpenHtml::default();
        open.open('a');
        assert!(open.follow(&Event::Start(Tag::Item)).is_empty());
        open.open('b');
        open.open('c');
        // An end tag does not reach out of the container, and ends what was opened after it.
        assert!(open.close(true, |element| *element == 'a').is_empty());
        assert_eq!(open.close(false, |element| *element != 'a'), ['c', 'b']);
        open.open('d');
        assert_eq!(open.follow(&Event::End(TagEnd::Item)), ['d']);
        assert_eq!(open.close_all(), ['a']);
    }

    #[test]
    fn lang_candidates_maps_aliases() {
        assert_eq!(lang_candidates("rust"), vec!["rust"]);
        assert_eq!(lang_candidates("rs"), vec!["rs", "rust"]);
        assert_eq!(lang_candidates("JavaScript"), vec!["javascript", "js"]);
        assert_eq!(
            lang_candidates("python extra-flag"),
            vec!["python", "python3"]
        );
        assert_eq!(lang_candidates("bash"), vec!["bash", "sh"]);
        assert_eq!(lang_candidates("c++"), vec!["c++", "cpp"]);
        assert!(lang_candidates("").is_empty());
        assert!(lang_candidates("   ").is_empty());
    }
}
