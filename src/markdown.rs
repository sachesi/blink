use adw::prelude::*;
use gettextrs::gettext;
use gtk::{Grid, Label, TextBuffer, TextView, gio, glib};
use pulldown_cmark::{Alignment, CodeBlockKind, Event, Parser, Tag, TagEnd};
use sourceview5::prelude::*;
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

thread_local! {
    // Decoded images by path, with the mtime they were decoded at. Each render keeps
    // only the images it showed, so the cache never holds more than one document's.
    static TEXTURES: RefCell<HashMap<PathBuf, (SystemTime, gtk::gdk::Texture)>> =
        RefCell::new(HashMap::new());
}

/// Decoded texture for `path`, reused across renders until the file's mtime
/// changes. Rendering runs after every pause in typing, and decoding the images
/// each time would make typing stutter in documents with many of them.
fn cached_texture(path: &Path) -> Option<gtk::gdk::Texture> {
    let mtime = std::fs::metadata(path).ok()?.modified().ok()?;
    TEXTURES.with(|cache| {
        let mut cache = cache.borrow_mut();
        if let Some((cached_mtime, texture)) = cache.get(path)
            && *cached_mtime == mtime
        {
            return Some(texture.clone());
        }
        let texture = gtk::gdk::Texture::from_filename(path).ok()?;
        cache.insert(path.to_path_buf(), (mtime, texture.clone()));
        Some(texture)
    })
}

/// Horizontal margin of the preview text view. Tag margins are absolute (a
/// `left-margin` on a tag replaces the view's own margin rather than adding to
/// it), so indenting tags have to start from this value.
pub const TEXT_MARGIN: i32 = 32;

/// Width available to a block widget at the given viewport width.
fn column_width(page_size: f64, indent: i32) -> f64 {
    let max_width = page_size.min(700.0);
    (max_width - 2.0 * f64::from(TEXT_MARGIN) - f64::from(indent)).max(100.0)
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
fn bind_image_to_page(picture: &gtk::Picture, hadj: &gtk::Adjustment, texture: &gtk::gdk::Texture) {
    let tex_width = f64::from(texture.width().max(1));
    let tex_height = f64::from(texture.height().max(1));
    hadj.bind_property("page-size", picture, "width-request")
        .transform_to(move |_, page_size: f64| {
            Some(column_width(page_size, 0).min(tex_width) as i32)
        })
        .sync_create()
        .build();
    hadj.bind_property("page-size", picture, "height-request")
        .transform_to(move |_, page_size: f64| {
            let width = column_width(page_size, 0).min(tex_width);
            Some((width * tex_height / tex_width).round() as i32)
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
pub enum Surface {
    Code {
        anchor_offset: i32,
        buffer: sourceview5::Buffer,
    },
    Cell {
        anchor_offset: i32,
        label: gtk::Label,
    },
}

/// Output of a render pass: clickable link ranges and the searchable child
/// surfaces (code blocks, table cells).
pub struct RenderResult {
    pub links: Vec<(i32, i32, String)>,
    pub surfaces: Vec<Surface>,
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
) -> (gtk::Overlay, sourceview5::Buffer) {
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
    (overlay, src_buffer)
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
                ("left-margin", &(TEXT_MARGIN + depth * 24)),
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
                ("left-margin", &(TEXT_MARGIN + (depth as i32 + 1) * 20)),
                ("indent", &-20),
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

/// Renders `text` into `view`'s buffer and returns the clickable link ranges and the
/// searchable child surfaces, for the caller to wire up clicks and search.
pub fn render_markdown(
    view: &TextView,
    text: &str,
    hadj: &gtk::Adjustment,
    image_base_dir: Option<&Path>,
) -> RenderResult {
    let buffer = view.buffer();
    let mut iter = buffer.bounds().0;
    buffer.delete(&mut iter, &mut buffer.bounds().1);

    let mut options = pulldown_cmark::Options::empty();
    options.insert(pulldown_cmark::Options::ENABLE_TABLES);
    options.insert(pulldown_cmark::Options::ENABLE_STRIKETHROUGH);
    options.insert(pulldown_cmark::Options::ENABLE_TASKLISTS);
    options.insert(pulldown_cmark::Options::ENABLE_FOOTNOTES);
    let parser = Parser::new_ext(text, options);
    let mut current_tags: Vec<String> = Vec::new();
    let mut iter = buffer.end_iter();

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
    let mut shown_images: HashSet<PathBuf> = HashSet::new();

    for event in parser {
        if in_code_block {
            match event {
                Event::Text(t) | Event::Code(t) => {
                    current_code.push_str(&t);
                }
                Event::End(TagEnd::CodeBlock) => {
                    in_code_block = false;
                    let indent = list_stack.len() as i32 * 16 + blockquote_depth * 24;
                    let clean_code = current_code.trim_end_matches('\n');
                    let (scroll, code_buffer) =
                        code_block_widget(clean_code, &current_code_lang, indent, hadj);

                    let anchor_offset = iter.offset();
                    let anchor = buffer.create_child_anchor(&mut iter);
                    view.add_child_at_anchor(&scroll, &anchor);
                    surfaces.push(Surface::Code {
                        anchor_offset,
                        buffer: code_buffer,
                    });
                    end_block(&buffer, &mut iter);
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
                    table_rows.push(current_row.clone());
                }
                Event::Start(Tag::TableRow) => current_row = Vec::new(),
                Event::End(TagEnd::TableRow) => {
                    table_rows.push(current_row.clone());
                }
                Event::Start(Tag::TableCell) => current_cell = String::new(),
                Event::End(TagEnd::TableCell) => {
                    current_row.push(current_cell.clone());
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
                    current_cell.push_str(&format!("<tt>{}</tt>", glib::markup_escape_text(&c)));
                }
                Event::Text(t) => {
                    current_cell.push_str(&glib::markup_escape_text(&t));
                }
                Event::Html(html) | Event::InlineHtml(html) => {
                    current_cell.push_str(&glib::markup_escape_text(&strip_html(&html)));
                }
                Event::End(TagEnd::Table) => {
                    in_table = false;
                    let indent = list_stack.len() as i32 * 16 + blockquote_depth * 24;
                    let grid = Grid::builder()
                        .margin_top(12)
                        .margin_bottom(12)
                        .margin_start(indent)
                        .hexpand(true)
                        .build();
                    grid.add_css_class("card");
                    bind_width_to_page(&grid, hadj, indent);

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
                                // Break inside long tokens (paths, identifiers)
                                // so one cell cannot force the whole window
                                // wider than the screen.
                                .wrap_mode(gtk::pango::WrapMode::WordChar)
                                .xalign(xalign)
                                .hexpand(true)
                                // Selectable so table text can be copied.
                                .selectable(true)
                                .build();
                            label.set_markup(cell_text);
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
                    let anchor_offset = iter.offset();
                    let anchor = buffer.create_child_anchor(&mut iter);
                    view.add_child_at_anchor(&grid, &anchor);
                    for label in cell_labels {
                        surfaces.push(Surface::Cell {
                            anchor_offset,
                            label,
                        });
                    }
                    end_block(&buffer, &mut iter);
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
                        shown_images.insert(path.clone());
                    }
                    if let Some(texture) = path.and_then(|path| cached_texture(&path)) {
                        let picture = gtk::Picture::for_paintable(&texture);
                        current_image = Some((Some(picture.clone()), String::new()));
                        picture.set_focusable(false);
                        picture.set_margin_top(12);
                        picture.set_margin_bottom(12);
                        picture.set_hexpand(false);
                        picture.set_halign(gtk::Align::Center);
                        bind_image_to_page(&picture, hadj, &texture);

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

    TEXTURES.with(|cache| {
        cache
            .borrow_mut()
            .retain(|path, _| shown_images.contains(path))
    });

    RenderResult { links, surfaces }
}

#[cfg(test)]
mod tests {
    use super::{
        close_tag, is_safe_link, lang_candidates, list_marker, local_image_path, strip_html,
    };
    use std::fs;

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
