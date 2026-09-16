use adw::prelude::*;
use gettextrs::gettext;
use gtk::{Grid, Label, TextBuffer, TextView, gio, glib};
use pulldown_cmark::{Alignment, CodeBlockKind, Event, Parser, RefDefs, Tag, TagEnd};
use sourceview5::prelude::*;
use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

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
    waiting: Vec<(glib::WeakRef<gtk::Picture>, glib::WeakRef<gtk::Adjustment>)>,
}

thread_local! {
    // Images by path. Each render keeps only the images it showed, so the cache never
    // holds more than one document's.
    static IMAGES: RefCell<HashMap<PathBuf, CachedImage>> = RefCell::new(HashMap::new());
}

/// A picture of the image at `path`, sized to the reading column, or `None` for a file
/// that could not be decoded. Rendering runs after every pause in typing, so an image is
/// decoded once, until the file changes, and in the background, so that a document with
/// large images opens without freezing the window. The picture takes its size when the
/// decoding is done.
fn image_picture(path: &Path, hadj: &gtk::Adjustment) -> Option<gtk::Picture> {
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
                bind_image_to_page(&picture, hadj, *size);
            }
            None if entry.decoded => return None,
            None => entry.waiting.push((picture.downgrade(), hadj.downgrade())),
        }
        Some(picture)
    })
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
            for (picture, hadj) in &waiting {
                if let (Some(picture), Some(hadj)) = (picture.upgrade(), hadj.upgrade()) {
                    picture.set_paintable(Some(&texture));
                    bind_image_to_page(&picture, &hadj, image.size);
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
/// How far each level of blockquote nesting indents its text.
const QUOTE_INDENT: i32 = 24;

/// Characters after which the text of a table cell wraps.
const CELL_WRAP_CHARS: i32 = 40;

thread_local! {
    // The widest the preview may get, as the window last set it.
    static CONTENT_WIDTH: Cell<f64> = const { Cell::new(f64::MAX) };
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
        let accent = adw::StyleManager::default().accent_color_rgba();
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

/// Output of a render pass: clickable link ranges and the searchable child
/// surfaces (code blocks, table cells), all of them and the ones this pass made.
pub struct RenderResult {
    pub links: Vec<(i32, i32, String)>,
    pub surfaces: Vec<Surface>,
    pub added: Vec<Surface>,
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
}

/// Close the innermost open `name` tag, so that when the same inline tag nests
/// the outer run stays open.
fn close_tag(tags: &mut Vec<String>, name: &str) {
    if let Some(index) = tags.iter().rposition(|tag| tag == name) {
        tags.remove(index);
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
    let tail = buffer.text(&start, iter, true);
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
    click.connect_pressed(glib::clone!(
        #[weak]
        view,
        move |gesture, _, x, y| {
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
            view.grab_focus();
        }
    ));
    block.add_controller(click);
}

/// Whether a link may be handed to the system URI launcher. Documents can come
/// from untrusted sources, so only web and mail links are ever followed.
pub fn is_safe_link(url: &str) -> bool {
    url.starts_with("http://") || url.starts_with("https://") || url.starts_with("mailto:")
}

/// The readable text of a raw HTML chunk: tags removed, `<br>` turned into a
/// line break. This renderer cannot lay out HTML, but dropping the chunk
/// outright lost the text inside it and ran the surrounding words together.
fn strip_html(chunk: &str) -> String {
    let mut out = String::new();
    let mut rest = chunk;
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
        let name = rest[start + 1..start + end]
            .trim_matches('/')
            .trim()
            .to_ascii_lowercase();
        if name == "br" {
            out.push('\n');
        }
        rest = &rest[start + end + 1..];
    }
    out.push_str(rest);
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

/// Builds a read-only, syntax-highlighted code block widget for the preview, with a
/// button that copies the code. Kept out of the focus chain so clicks never scroll the
/// preview, and width is bound to the viewport so it stays inside the reading column.
fn code_block_widget(
    code: &str,
    info: &str,
    indent: i32,
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
        .margin_top(12)
        .margin_bottom(12)
        .margin_start(indent)
        .hexpand(true)
        .child(&scroll)
        .build();
    overlay.add_css_class("code-block");
    overlay.add_overlay(&copy_box);
    bind_width_to_page(&overlay, hadj, indent);
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
fn ensure_blockquote_tag(buffer: &TextBuffer, depth: i32) -> String {
    let name = format!("blockquote-{depth}");
    if buffer.tag_table().lookup(&name).is_none() {
        buffer.create_tag(
            Some(&name),
            &[
                ("left-margin", &(TEXT_MARGIN + depth * QUOTE_INDENT)),
                // The same as the view's, which it replaces; set on the tag, it also ends
                // the paragraph background at the column instead of the edge of the view.
                ("right-margin", &TEXT_MARGIN),
                ("style", &gtk::pango::Style::Italic),
                ("foreground", &dim_foreground()),
                ("paragraph-background", &"rgba(128, 128, 128, 0.04)"),
            ],
        );
    }
    name
}

/// Per-depth list tag, created on demand so nested lists indent correctly.
/// The negative `indent` hangs the marker: GTK keeps the first line at the
/// left margin and shifts the wrapped lines right by that amount, so
/// continuation text lines up under the item text instead of under the bullet.
fn ensure_list_tag(buffer: &TextBuffer, depth: usize) -> String {
    let name = format!("list-{depth}");
    if buffer.tag_table().lookup(&name).is_none() {
        buffer.create_tag(
            Some(&name),
            &[
                (
                    "left-margin",
                    &(TEXT_MARGIN + (depth as i32 + 1) * LIST_INDENT),
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
fn local_image_path(dest_url: &str, base_dir: Option<&Path>) -> Option<PathBuf> {
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
fn list_marker(list_stack: &mut [Option<u64>]) -> String {
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
}

/// What the preview buffer holds, so a render can keep the blocks that did not change.
#[derive(Default)]
pub struct Rendered {
    blocks: Vec<RenderedBlock>,
    base_dir: Option<PathBuf>,
    definitions: Vec<String>,
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
fn top_level_blocks(events: &[(Event, Range<usize>)]) -> Vec<(Range<usize>, Range<usize>)> {
    let mut blocks = Vec::new();
    let mut depth = 0usize;
    let mut first = 0;
    for (index, (event, range)) in events.iter().enumerate() {
        if depth == 0 {
            first = index;
        }
        match event {
            Event::Start(_) => depth += 1,
            Event::End(_) => depth = depth.saturating_sub(1),
            _ => {}
        }
        if depth == 0 {
            blocks.push((
                first..index + 1,
                events[first].1.start.min(range.start)..range.end,
            ));
        }
    }
    blocks
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

    let mut options = pulldown_cmark::Options::empty();
    options.insert(pulldown_cmark::Options::ENABLE_TABLES);
    options.insert(pulldown_cmark::Options::ENABLE_STRIKETHROUGH);
    options.insert(pulldown_cmark::Options::ENABLE_TASKLISTS);
    options.insert(pulldown_cmark::Options::ENABLE_FOOTNOTES);
    // The offset iterator keeps the parser, and with it the link definitions.
    let mut parser = Parser::new_ext(text, options).into_offset_iter();
    let events: Vec<(Event, Range<usize>)> = parser.by_ref().collect();
    let definitions = definitions(parser.reference_definitions(), &events);
    let blocks = top_level_blocks(&events);

    // Images resolve against the document's directory, and links against definitions
    // any block may hold: when either changes, every block is rendered again.
    let base_dir = image_base_dir.map(Path::to_path_buf);
    if rendered.base_dir != base_dir || rendered.definitions != definitions {
        for block in rendered.blocks.drain(..) {
            buffer.delete_mark(&block.start);
        }
        let (mut start, mut end) = buffer.bounds();
        buffer.delete(&mut start, &mut end);
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
    buffer.delete(&mut iter, &mut removed_end);
    for block in rendered.blocks.drain(removed) {
        buffer.delete_mark(&block.start);
    }
    let insert_offset = iter.offset();
    let mut new_blocks = Vec::new();
    let mut added = Vec::new();

    let mut current_tags: Vec<String> = Vec::new();

    // One entry per open list. `Some(n)` is an ordered list whose next item
    // number is `n`; `None` is a bullet list. Length doubles as nesting depth.
    let mut list_stack: Vec<Option<u64>> = Vec::new();
    // Open blockquote nesting level, used to indent block widgets.
    let mut blockquote_depth: i32 = 0;

    let mut in_table = false;
    let mut table_rows: Vec<Vec<String>> = Vec::new();
    let mut table_alignments: Vec<Alignment> = Vec::new();
    let mut current_row: Vec<String> = Vec::new();
    let mut current_cell = String::new();
    // Closing markup for the links open in the current cell.
    let mut cell_link_close: Vec<&'static str> = Vec::new();

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
    // Searchable child surfaces (code blocks, table cells).
    let mut surfaces: Vec<Surface> = Vec::new();
    let mut shown_images: Vec<PathBuf> = Vec::new();

    let middle = &blocks[prefix..blocks.len() - suffix];
    let mut events = events
        .into_iter()
        .map(|(event, _)| event)
        .skip(middle.first().map_or(0, |(events, _)| events.start));
    for (block_events, source) in middle {
        let start_offset = iter.offset();
        let start = buffer.create_mark(None, &iter, true);
        for event in events.by_ref().take(block_events.len()) {
            if in_code_block {
                match event {
                    Event::Text(t) | Event::Code(t) => {
                        current_code.push_str(&t);
                    }
                    Event::End(TagEnd::CodeBlock) => {
                        in_code_block = false;
                        let indent = block_indent(&list_stack, blockquote_depth);
                        let clean_code = current_code.trim_end_matches('\n');
                        let (scroll, code_view, code_buffer) =
                            code_block_widget(clean_code, &current_code_lang, indent, hadj);

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
                    Event::Start(Tag::TableCell) => current_cell = String::new(),
                    Event::End(TagEnd::TableCell) => {
                        current_row.push(std::mem::take(&mut current_cell));
                    }
                    Event::Start(Tag::Strong) => current_cell.push_str("<b>"),
                    Event::End(TagEnd::Strong) => current_cell.push_str("</b>"),
                    Event::Start(Tag::Emphasis) => current_cell.push_str("<i>"),
                    Event::End(TagEnd::Emphasis) => current_cell.push_str("</i>"),
                    Event::Start(Tag::Strikethrough) => current_cell.push_str("<s>"),
                    Event::End(TagEnd::Strikethrough) => current_cell.push_str("</s>"),
                    // Cell links become real Pango links, so they keep the theme's
                    // link colour and stay clickable. An unsafe scheme is never put
                    // in an `href`, only underlined, since the label's default
                    // handler would hand it straight to the system launcher.
                    Event::Start(Tag::Link { dest_url, .. }) => {
                        if is_safe_link(&dest_url) {
                            current_cell.push_str(&format!(
                                "<a href=\"{}\">",
                                glib::markup_escape_text(&dest_url)
                            ));
                            cell_link_close.push("</a>");
                        } else {
                            current_cell.push_str("<u>");
                            cell_link_close.push("</u>");
                        }
                    }
                    Event::End(TagEnd::Link) => {
                        if let Some(close) = cell_link_close.pop() {
                            current_cell.push_str(close);
                        }
                    }
                    Event::Code(c) => {
                        current_cell
                            .push_str(&format!("<tt>{}</tt>", glib::markup_escape_text(&c)));
                    }
                    Event::Text(t) => {
                        current_cell.push_str(&glib::markup_escape_text(&t));
                    }
                    Event::Html(html) | Event::InlineHtml(html) => {
                        current_cell.push_str(&glib::markup_escape_text(&strip_html(&html)));
                    }
                    Event::End(TagEnd::Table) => {
                        in_table = false;
                        let indent = block_indent(&list_stack, blockquote_depth);
                        let grid = Grid::builder().hexpand(true).build();
                        // A table wider than the column scrolls sideways, like a code block,
                        // rather than squeezing its columns until the words break apart.
                        let scroll = gtk::ScrolledWindow::builder()
                            .margin_top(12)
                            .margin_bottom(12)
                            .margin_start(indent)
                            .hexpand(true)
                            .propagate_natural_height(true)
                            .hscrollbar_policy(gtk::PolicyType::Automatic)
                            .vscrollbar_policy(gtk::PolicyType::Never)
                            .focusable(false)
                            .child(&grid)
                            .build();
                        scroll.add_css_class("card");
                        bind_width_to_page(&scroll, hadj, indent);

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

                            for (col_idx, cell_text) in row.iter().enumerate() {
                                let text_col = (col_idx * 2) as i32;

                                let xalign = match table_alignments.get(col_idx) {
                                    Some(Alignment::Right) => 1.0_f32,
                                    Some(Alignment::Center) => 0.5,
                                    _ => 0.0,
                                };

                                let label = Label::builder()
                                    .margin_top(10)
                                    .margin_bottom(10)
                                    .margin_start(12)
                                    .margin_end(12)
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
                                grid.attach(&label, text_col, text_row, 1, 1);
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
                    Tag::Heading { level, .. } => {
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
                    Tag::Image { dest_url, .. } => {
                        current_image = Some((None, String::new()));
                        let path = local_image_path(dest_url.as_ref(), image_base_dir);
                        if let Some(path) = &path {
                            shown_images.push(path.clone());
                        }
                        if let Some(picture) = path.and_then(|path| image_picture(&path, hadj)) {
                            current_image = Some((Some(picture.clone()), String::new()));
                            picture.set_focusable(false);
                            picture.set_margin_top(12);
                            picture.set_margin_bottom(12);
                            picture.set_hexpand(false);
                            picture.set_halign(gtk::Align::Center);

                            let anchor = buffer.create_child_anchor(&mut iter);
                            view.add_child_at_anchor(&picture, &anchor);
                        }
                    }
                    Tag::BlockQuote(_) => {
                        blockquote_depth += 1;
                        let name = ensure_blockquote_tag(&buffer, blockquote_depth);
                        current_tags.push(name);
                    }
                    Tag::List(first) => {
                        // A nested list opens while the parent item's line is still
                        // open, which ran its first marker on after the item text.
                        if !iter.starts_line() {
                            buffer.insert(&mut iter, "\n");
                        }
                        list_stack.push(first);
                        let name = ensure_list_tag(&buffer, list_stack.len() - 1);
                        current_tags.push(name);
                    }
                    Tag::Item => {
                        let marker = list_marker(&mut list_stack);
                        let start_offset = iter.offset();
                        buffer.insert(&mut iter, &marker);
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
                        let start_offset = iter.offset();
                        buffer.insert(&mut iter, &format!("[{label}]: "));
                        let start_iter = buffer.iter_at_offset(start_offset);
                        buffer.apply_tag_by_name("bold", &start_iter, &iter);
                    }
                    _ => {}
                },
                Event::End(tag_end) => match tag_end {
                    TagEnd::Heading(_) => {
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
                    TagEnd::BlockQuote(_) => {
                        let name = format!("blockquote-{blockquote_depth}");
                        current_tags.retain(|t| t != &name);
                        blockquote_depth = (blockquote_depth - 1).max(0);
                        end_block(&buffer, &mut iter);
                    }
                    TagEnd::List(_) => {
                        let depth = list_stack.len().saturating_sub(1);
                        let name = format!("list-{depth}");
                        current_tags.retain(|t| t != &name);
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
                    TagEnd::Paragraph => end_block(&buffer, &mut iter),
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
                // A block chunk holding nothing but markup — a comment, or a lone
                // opening tag on its own line — is dropped, or it would leave a
                // stray blank line behind. An inline chunk is kept as it comes,
                // since a `<br>` legitimately reduces to just a newline.
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
                    let start_offset = iter.offset();
                    buffer.insert(&mut iter, &format!("[{name}]"));
                    let start_iter = buffer.iter_at_offset(start_offset);
                    buffer.apply_tag_by_name("footnote", &start_iter, &iter);
                }
                Event::TaskListMarker(checked) => {
                    let start_offset = iter.offset();
                    // Squares rather than the ballot-box characters: no font in a
                    // default install covers U+2610/U+2611, which would show as
                    // missing-glyph boxes.
                    buffer.insert(&mut iter, if checked { "■ " } else { "□ " });
                    let start_iter = buffer.iter_at_offset(start_offset);
                    buffer.apply_tag_by_name("bold", &start_iter, &iter);
                    for tag in &current_tags {
                        buffer.apply_tag_by_name(tag, &start_iter, &iter);
                    }
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
                _ => {}
            }
        }
        // Every block ends with the same blank line, so that what a block renders to never
        // depends on the block before it.
        end_block(&buffer, &mut iter);
        added.extend(surfaces.iter().cloned());
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
        });
    }

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
    };
    let mut images = HashSet::new();
    for block in &rendered.blocks {
        let offset = buffer.iter_at_mark(&block.start).offset();
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
        close_tag, definitions, is_safe_link, lang_candidates, list_marker, local_image_path,
        strip_html, top_level_blocks, unchanged_ends,
    };
    use pulldown_cmark::{Event, Options, Parser};
    use std::fs;
    use std::ops::Range;

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
