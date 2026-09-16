//! HTML export. The document may come from anyone and the file may be opened in a browser,
//! so raw HTML is written again only as its text, its `<details>` elements and the styles it
//! gives text, and link and image addresses are limited to safe schemes.

use gtk::{gio, glib};
use pulldown_cmark::{CodeBlockKind, Event, Tag, TagEnd};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use crate::markdown::{self, CodeRun, CodeStyle, HtmlAlign, HtmlPart, HtmlStyle, OpenHtml};
use crate::math;

/// True when `url` carries an explicit URI scheme (`scheme:`), per the RFC 3986
/// grammar (`ALPHA *( ALPHA / DIGIT / "+" / "-" / "." ) ":"`). Scheme-relative,
/// relative, and fragment references have no scheme.
fn url_has_scheme(url: &str) -> bool {
    let bytes = url.as_bytes();
    if bytes.first().is_none_or(|c| !c.is_ascii_alphabetic()) {
        return false;
    }
    for &c in bytes {
        if c == b':' {
            return true;
        }
        if !(c.is_ascii_alphanumeric() || matches!(c, b'+' | b'-' | b'.')) {
            return false;
        }
    }
    false
}

/// Whether a link/image URL is safe to emit into exported HTML. Allows the web
/// and mail schemes plus any scheme-less (relative/anchor) reference; everything
/// else — notably `javascript:`, `data:`, `file:`, `vbscript:` — is rejected so
/// exported documents cannot execute script when opened in a browser.
fn is_safe_export_url(url: &str) -> bool {
    let url = url.trim();
    if url.is_empty() {
        return true;
    }
    let lower = url.to_ascii_lowercase();
    lower.starts_with("http://")
        || lower.starts_with("https://")
        || lower.starts_with("mailto:")
        || !url_has_scheme(&lower)
}

fn sanitize_export_url(url: pulldown_cmark::CowStr<'_>) -> pulldown_cmark::CowStr<'static> {
    if is_safe_export_url(&url) {
        pulldown_cmark::CowStr::from(url.into_string())
    } else {
        pulldown_cmark::CowStr::from("")
    }
}

/// Escape text for an HTML element or a quoted attribute.
fn escape_html(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// An element of raw HTML the export writes again, other than a style.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Opened {
    /// A `<div>`, for a `<p>` if `paragraph`.
    Block {
        paragraph: bool,
    },
    Details,
}

/// The end tags of `elements`, in their order.
fn end_tags(elements: Vec<Opened>) -> String {
    elements
        .into_iter()
        .map(|element| match element {
            Opened::Block { .. } => "</div>",
            Opened::Details => "</details>",
        })
        .collect()
}

/// The end tags of the styles raw HTML left open, innermost first.
fn close_styles(styles: &mut Vec<HtmlStyle>) -> String {
    styles
        .drain(..)
        .rev()
        .map(|style| format!("</{}>", style.element()))
        .collect()
}

/// `text` as a quoted CSS string, which cannot end the string or the stylesheet around it.
fn css_string(text: &str) -> String {
    let mut out = String::from("\"");
    for c in text.chars() {
        match c {
            '"' | '\\' | '<' | '>' | '\n' | '\r' => out.push_str(&format!("\\{:x} ", u32::from(c))),
            _ => out.push(c),
        }
    }
    out.push('"');
    out
}

/// What an export takes besides the Markdown text.
pub struct Options {
    pub title: String,
    /// The document's folder, which images are taken from; `None` for an untitled document.
    pub base_dir: Option<PathBuf>,
    pub text_font: String,
    pub monospace_font: String,
    /// The width of the reading column, in pixels.
    pub width: i32,
    /// The syntax colours of the code blocks in the light style, from
    /// [`markdown::code_highlights`].
    pub light: Vec<Vec<CodeRun>>,
    /// The same in the dark style.
    pub dark: Vec<Vec<CodeRun>>,
}

/// An image file as a `data:` URI, when it is an image.
fn image_data_uri(path: &Path) -> Option<String> {
    let bytes = std::fs::read(path).ok()?;
    let (content_type, _) = gio::content_type_guess(Some(path), Some(bytes.as_slice()));
    let mime = gio::content_type_get_mime_type(&content_type)?;
    mime.starts_with("image/")
        .then(|| format!("data:{mime};base64,{}", glib::base64_encode(&bytes)))
}

/// The style of the run of `runs` that holds byte `offset`.
fn style_at(runs: &[CodeRun], offset: usize) -> Option<CodeStyle> {
    let index = runs.partition_point(|run| run.range.end <= offset);
    runs.get(index)
        .filter(|run| run.range.start <= offset)
        .map(|run| run.style)
}

/// A code block, coloured with the light and the dark runs. Each span carries both
/// colours, and the stylesheet picks one for the reader's appearance.
fn code_block_html(code: &str, info: &str, light: &[CodeRun], dark: &[CodeRun]) -> String {
    let mut bounds: Vec<usize> = light
        .iter()
        .chain(dark)
        .flat_map(|run| [run.range.start, run.range.end])
        .chain([0, code.len()])
        .filter(|offset| *offset <= code.len() && code.is_char_boundary(*offset))
        .collect();
    bounds.sort_unstable();
    bounds.dedup();

    let language = info.split_whitespace().next().unwrap_or_default();
    let mut html = if language.is_empty() {
        String::from("<pre><code>")
    } else {
        format!("<pre><code class=\"language-{}\">", escape_html(language))
    };
    for pair in bounds.windows(2) {
        let text = escape_html(&code[pair[0]..pair[1]]);
        let (light, dark) = (style_at(light, pair[0]), style_at(dark, pair[0]));
        let mut style = String::new();
        for (name, run) in [("--light", light), ("--dark", dark)] {
            if let Some((red, green, blue)) = run.and_then(|run| run.color) {
                style.push_str(&format!("{name}:#{red:02x}{green:02x}{blue:02x};"));
            }
        }
        if light.is_some_and(|run| run.bold) {
            style.push_str("font-weight:bold;");
        }
        if light.is_some_and(|run| run.italic) {
            style.push_str("font-style:italic;");
        }
        if style.is_empty() {
            html.push_str(&text);
        } else {
            html.push_str(&format!("<span style=\"{style}\">{text}</span>"));
        }
    }
    html.push_str("</code></pre>\n");
    html
}

/// A standalone HTML page of the document: images from its folder embedded, code coloured
/// as in the preview, and a stylesheet of its own for the light and the dark appearance.
pub fn render_html(text: &str, options: &Options) -> String {
    let mut events = Vec::new();
    let mut code: Option<(String, String)> = None;
    let mut code_blocks = 0;
    // Whether each open image is kept; one that is not leaves its text behind.
    let mut images: Vec<bool> = Vec::new();
    // Footnotes are numbered in the order they first come in, as pulldown-cmark numbers
    // them, and the first reference to each is the one its note links back to.
    let mut footnote_numbers: HashMap<String, usize> = HashMap::new();
    let mut referenced: HashSet<String> = HashSet::new();
    let mut footnotes: Vec<String> = Vec::new();
    // The styles raw HTML opened and has not closed, and its other elements.
    let mut html_styles: Vec<HtmlStyle> = Vec::new();
    let mut html_open: OpenHtml<Opened> = OpenHtml::default();
    // An image an `<img>` tag gave a width: its address, its width and its alternative text.
    let mut sized_image: Option<(String, i32, String)> = None;
    for (event, _) in markdown::events(text) {
        let closed = html_open.follow(&event);
        if !closed.is_empty() {
            let close = close_styles(&mut html_styles) + &end_tags(closed);
            events.push(Event::Html(format!("{close}\n").into()));
        }
        if let Some((source, width, alt)) = sized_image.as_mut() {
            match event {
                Event::Text(text) | Event::Code(text) => alt.push_str(&text),
                Event::End(TagEnd::Image) => {
                    images.pop();
                    events.push(Event::InlineHtml(
                        format!(
                            "<img src=\"{}\" alt=\"{}\" width=\"{width}\" />",
                            escape_html(source),
                            escape_html(alt)
                        )
                        .into(),
                    ));
                    sized_image = None;
                }
                _ => {}
            }
            continue;
        }
        if let Some((info, block)) = code.as_mut() {
            match event {
                Event::Text(text) => block.push_str(&text),
                Event::End(TagEnd::CodeBlock) => {
                    let empty = Vec::new();
                    events.push(Event::Html(
                        code_block_html(
                            markdown::shown_code(block),
                            info,
                            options.light.get(code_blocks).unwrap_or(&empty),
                            options.dark.get(code_blocks).unwrap_or(&empty),
                        )
                        .into(),
                    ));
                    code_blocks += 1;
                    code = None;
                }
                _ => {}
            }
            continue;
        }
        match event {
            // Untrusted document content is exported to a file that may be opened in a
            // browser. Raw HTML is not passed through (no `<script>`/`onerror=`), and link and
            // image URLs are scheme-filtered, mirroring the in-app preview's own allow-list.
            // pulldown-cmark performs no sanitization of its own. The text of raw HTML is
            // written escaped, and only `<details>`, `<summary>`, the elements of text styles
            // and blocks, as `<div>`, are written again, without their attributes but `open`
            // and alignment, which is written as a class.
            Event::Html(ref chunk) | Event::InlineHtml(ref chunk) => {
                let block = matches!(event, Event::Html(_));
                let parts = markdown::html_parts(chunk);
                let mut html = String::new();
                for part in &parts {
                    match part {
                        HtmlPart::Text(text) => {
                            html.push_str(&escape_html(text).replace('\n', "<br>\n"));
                        }
                        HtmlPart::DetailsStart { open } => {
                            html.push_str(if *open { "<details open>" } else { "<details>" });
                            html_open.open(Opened::Details);
                        }
                        HtmlPart::DetailsEnd => {
                            let closed = html_open.close(true, |open| *open == Opened::Details);
                            if !closed.is_empty() {
                                html.push_str(&close_styles(&mut html_styles));
                                html.push_str(&end_tags(closed));
                            }
                        }
                        HtmlPart::SummaryStart => html.push_str("<summary>"),
                        HtmlPart::SummaryEnd => html.push_str("</summary>"),
                        // A block written inside the text of a paragraph would end the paragraph.
                        HtmlPart::BlockStart(opened) if block => {
                            html.push_str(&close_styles(&mut html_styles));
                            html.push_str(match opened.align {
                                Some(HtmlAlign::Center) => "<div class=\"align-center\">",
                                Some(HtmlAlign::Right) => "<div class=\"align-right\">",
                                None => "<div>",
                            });
                            html_open.open(Opened::Block {
                                paragraph: opened.paragraph,
                            });
                        }
                        HtmlPart::BlockEnd { paragraph } if block => {
                            let closed = html_open.close(true, |open| {
                                *open
                                    == Opened::Block {
                                        paragraph: *paragraph,
                                    }
                            });
                            if !closed.is_empty() {
                                html.push_str(&close_styles(&mut html_styles));
                                html.push_str(&end_tags(closed));
                            }
                        }
                        HtmlPart::BlockStart(_) | HtmlPart::BlockEnd { .. } => {}
                        HtmlPart::StyleStart(style) => {
                            html.push_str(&format!("<{}>", style.element()));
                            html_styles.push(*style);
                        }
                        // Elements have to nest: the styles opened inside the one that ends
                        // are ended with it and started again.
                        HtmlPart::StyleEnd(style) => {
                            if let Some(index) = html_styles.iter().rposition(|open| open == style)
                            {
                                let inner = html_styles.split_off(index);
                                for open in inner.iter().rev() {
                                    html.push_str(&format!("</{}>", open.element()));
                                }
                                for open in &inner[1..] {
                                    html.push_str(&format!("<{}>", open.element()));
                                    html_styles.push(*open);
                                }
                            }
                        }
                    }
                }
                if block {
                    html.push_str(&close_styles(&mut html_styles));
                    // The text of a block of its own is a paragraph, as in the preview.
                    let text = parts.iter().any(
                        |part| matches!(part, HtmlPart::Text(text) if !text.trim().is_empty()),
                    );
                    let structure = parts.iter().any(|part| {
                        !matches!(
                            part,
                            HtmlPart::Text(_) | HtmlPart::StyleStart(_) | HtmlPart::StyleEnd(_)
                        )
                    });
                    if text && !structure {
                        html = format!("<p>{html}</p>");
                    }
                }
                if !html.is_empty() {
                    events.push(if block {
                        Event::Html(format!("{html}\n").into())
                    } else {
                        Event::InlineHtml(html.into())
                    });
                }
            }
            // The paragraphs of raw HTML it leaves open end with it.
            Event::End(TagEnd::HtmlBlock) => {
                let closed =
                    html_open.close(false, |open| *open == Opened::Block { paragraph: true });
                if !closed.is_empty() {
                    let close = close_styles(&mut html_styles) + &end_tags(closed);
                    events.push(Event::Html(format!("{close}\n").into()));
                }
                events.push(Event::End(TagEnd::HtmlBlock));
            }
            // As the end of a paragraph closes them in a browser.
            Event::End(
                end @ (TagEnd::Paragraph | TagEnd::Heading(_) | TagEnd::TableCell | TagEnd::Item),
            ) => {
                let close = close_styles(&mut html_styles);
                if !close.is_empty() {
                    events.push(Event::InlineHtml(close.into()));
                }
                events.push(Event::End(end));
            }
            Event::Start(Tag::CodeBlock(kind)) => {
                let info = match kind {
                    CodeBlockKind::Fenced(info) => info.to_string(),
                    CodeBlockKind::Indented => String::new(),
                };
                code = Some((info, String::new()));
            }
            // A wide table scrolls sideways in its own box, as in the preview.
            Event::Start(Tag::Table(alignments)) => {
                events.push(Event::Html("<div class=\"table\">\n".into()));
                events.push(Event::Start(Tag::Table(alignments)));
            }
            Event::End(TagEnd::Table) => {
                events.push(Event::End(TagEnd::Table));
                events.push(Event::Html("</div>\n".into()));
            }
            Event::Start(Tag::Link {
                link_type,
                dest_url,
                title,
                id,
            }) => events.push(Event::Start(Tag::Link {
                link_type,
                dest_url: sanitize_export_url(dest_url),
                title,
                id,
            })),
            // An image of the document's folder is embedded, a web image stays a link to
            // it, and anything else, as in the preview, shows its alternative text.
            Event::Start(Tag::Image {
                link_type,
                dest_url,
                title,
                id,
            }) => {
                let lower = dest_url.trim().to_ascii_lowercase();
                let source = if lower.starts_with("http://") || lower.starts_with("https://") {
                    Some(dest_url.to_string())
                } else {
                    markdown::local_image_path(&dest_url, options.base_dir.as_deref())
                        .and_then(|path| image_data_uri(&path))
                };
                images.push(source.is_some());
                if let Some(source) = source {
                    match markdown::image_width(link_type, &id) {
                        Some(width) => sized_image = Some((source, width, String::new())),
                        None => events.push(Event::Start(Tag::Image {
                            link_type,
                            dest_url: source.into(),
                            title,
                            id,
                        })),
                    }
                }
            }
            Event::End(TagEnd::Image) => {
                if images.pop().unwrap_or(false) {
                    events.push(Event::End(TagEnd::Image));
                }
            }
            // Math that typesets is drawn in SVG, which every browser shows without a script
            // or a font; math that does not has been made code already.
            Event::InlineMath(ref latex) | Event::DisplayMath(ref latex) => {
                let display = matches!(event, Event::DisplayMath(_));
                if let Some(formula) = math::typeset(latex, display) {
                    let svg = formula.svg(latex);
                    events.push(Event::InlineHtml(
                        if display {
                            format!("<span class=\"math-display\">{svg}</span>")
                        } else {
                            svg
                        }
                        .into(),
                    ));
                }
            }
            Event::FootnoteReference(name) => {
                let next = footnote_numbers.len() + 1;
                let number = *footnote_numbers.entry(name.to_string()).or_insert(next);
                let id = if referenced.insert(name.to_string()) {
                    format!(
                        " id=\"{}\"",
                        escape_html(&markdown::footnote_reference_id(&name))
                    )
                } else {
                    String::new()
                };
                events.push(Event::InlineHtml(
                    format!(
                        "<sup class=\"footnote-reference\"{id}><a href=\"#{}\">{number}</a></sup>",
                        escape_html(&markdown::footnote_id(&name))
                    )
                    .into(),
                ));
            }
            Event::Start(Tag::FootnoteDefinition(name)) => {
                let next = footnote_numbers.len() + 1;
                let number = *footnote_numbers.entry(name.to_string()).or_insert(next);
                events.push(Event::Html(
                    format!(
                        "<div class=\"footnote-definition\" id=\"{}\"><sup class=\"footnote-definition-label\">{number}</sup>\n",
                        escape_html(&markdown::footnote_id(&name))
                    )
                    .into(),
                ));
                footnotes.push(name.to_string());
            }
            Event::End(TagEnd::FootnoteDefinition) => {
                let name = footnotes.pop().unwrap_or_default();
                events.push(Event::Html(
                    format!(
                        "<a href=\"#{}\" class=\"footnote-backref\">{}</a></div>\n",
                        escape_html(&markdown::footnote_reference_id(&name)),
                        markdown::FOOTNOTE_BACKLINK.trim_start_matches('\u{a0}')
                    )
                    .into(),
                ));
            }
            other => events.push(other),
        }
    }
    let closed = html_open.close_all();
    if !closed.is_empty() {
        let close = close_styles(&mut html_styles) + &end_tags(closed);
        events.push(Event::Html(format!("{close}\n").into()));
    }
    let mut body = String::new();
    pulldown_cmark::html::push_html(&mut body, events.into_iter());

    let text_font = css_string(&options.text_font);
    let monospace_font = css_string(&options.monospace_font);
    let width = options.width;
    let css = format!(
        r#"
:root {{ color-scheme: light dark; }}
* {{ box-sizing: border-box; }}
body {{ margin: 0; padding: 32px 16px; background: #ffffff; color: rgba(0, 0, 6, 0.8); font-family: {text_font}, sans-serif; line-height: 1.6; overflow-wrap: break-word; }}
main {{ max-width: {width}px; margin: 0 auto; padding: 0 16px; }}
a {{ color: #1c71d8; }}
h1 {{ font-size: 2em; }}
h2 {{ font-size: 1.75em; }}
h3 {{ font-size: 1.5em; }}
h4, h5, h6 {{ font-size: 1.2em; }}
h1, h2, h3, h4, h5, h6 {{ line-height: 1.25; }}
code, pre {{ font-family: {monospace_font}, monospace; }}
code {{ font-size: 0.9em; padding: 0.1em 0.3em; border-radius: 4px; background: rgba(128, 128, 128, 0.15); }}
pre {{ padding: 12px; border: 1px solid rgba(128, 128, 128, 0.25); border-radius: 12px; overflow-x: auto; line-height: 1.4; }}
pre code {{ padding: 0; background: none; }}
pre span {{ color: var(--light); }}
blockquote {{ margin: 1em 0; padding: 0.25em 1em; border-left: 3px solid rgba(128, 128, 128, 0.3); background: rgba(128, 128, 128, 0.04); color: rgba(0, 0, 0, 0.6); font-style: italic; }}
.table {{ margin: 1em 0; overflow-x: auto; border: 1px solid rgba(128, 128, 128, 0.25); border-radius: 12px; }}
table {{ min-width: 100%; border-collapse: collapse; }}
th, td {{ padding: 10px 12px; border-top: 1px solid rgba(128, 128, 128, 0.25); border-left: 1px solid rgba(128, 128, 128, 0.25); text-align: left; vertical-align: top; }}
thead th {{ border-top: none; background: rgba(128, 128, 128, 0.08); }}
th:first-child, td:first-child {{ border-left: none; }}
img {{ display: block; max-width: 100%; height: auto; margin: 12px auto; }}
.align-center {{ text-align: center; }}
.align-right {{ text-align: right; }}
.align-right img {{ margin-right: 0; }}
hr {{ margin: 1.5em 0; border: none; border-top: 1px solid rgba(128, 128, 128, 0.3); }}
li > input[type="checkbox"], li > p > input[type="checkbox"] {{ margin: 0 0.4em 0 0; }}
ul > li:has(> input[type="checkbox"]), ul > li:has(> p > input[type="checkbox"]) {{ list-style: none; }}
dt {{ font-weight: bold; }}
dd {{ margin: 0 0 0.5em 20px; }}
.markdown-alert-note > p:first-child {{ color: #0461be; }}
.markdown-alert-tip > p:first-child {{ color: #15772e; }}
.markdown-alert-important > p:first-child {{ color: #8939a4; }}
.markdown-alert-warning > p:first-child {{ color: #905300; }}
.markdown-alert-caution > p:first-child {{ color: #c00023; }}
.footnote-definition {{ margin: 0.5em 0; }}
.footnote-definition p {{ display: inline; }}
svg.math {{ overflow: visible; }}
.math-display {{ display: block; margin: 0.5em 0; text-align: center; }}
.footnote-backref {{ margin-left: 0.25em; text-decoration: none; }}
@media (prefers-color-scheme: dark) {{
  body {{ background: #1d1d20; color: #ffffff; }}
  a {{ color: #78aeed; }}
  pre span {{ color: var(--dark); }}
  blockquote {{ color: rgba(255, 255, 255, 0.7); }}
  .markdown-alert-note > p:first-child {{ color: #81d0ff; }}
  .markdown-alert-tip > p:first-child {{ color: #8de698; }}
  .markdown-alert-important > p:first-child {{ color: #fba7ff; }}
  .markdown-alert-warning > p:first-child {{ color: #ffc057; }}
  .markdown-alert-caution > p:first-child {{ color: #ff888c; }}
}}
@media print {{
  body {{ padding: 0; }}
  main {{ max-width: none; }}
  pre, .table {{ overflow: visible; }}
  pre {{ white-space: pre-wrap; }}
}}"#
    );
    let title = escape_html(&options.title);
    format!(
        "<!DOCTYPE html>\n<html>\n<head>\n<meta charset=\"utf-8\">\n<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n<title>{title}</title>\n<style>{css}\n</style>\n</head>\n<body>\n<main>\n{body}</main>\n</body>\n</html>\n"
    )
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

    fn render(text: &str) -> String {
        render_html(text, &options())
    }

    #[test]
    fn export_strips_raw_html_and_scripts() {
        let html = render("Hello\n\n<script>alert('xss')</script>\n\n<b>raw</b> text");
        assert!(!html.contains("<script>"));
        assert!(!html.contains("alert("));
        // The inline raw <b> tag is dropped, but its text content survives.
        assert!(html.contains("raw"));
        assert!(html.contains("Hello"));
    }

    #[test]
    fn export_neutralizes_dangerous_link_schemes() {
        let html = render("[click](javascript:alert(1))");
        assert!(!html.contains("javascript:"));
        // The link text is preserved; only the destination is emptied.
        assert!(html.contains("click"));
    }

    #[test]
    fn export_preserves_safe_links_and_relative_paths() {
        let html = render("[web](https://example.com) and [rel](images/a.png) and [a](#sec)");
        assert!(html.contains("https://example.com"));
        assert!(html.contains("images/a.png"));
        assert!(html.contains("#sec"));
    }

    #[test]
    fn export_title_is_derived_and_escaped() {
        let html = render_html(
            "body",
            &Options {
                title: String::from("a & b <x>.md"),
                ..options()
            },
        );
        assert!(html.contains("<title>a &amp; b &lt;x&gt;.md</title>"));
    }

    #[test]
    fn export_renders_footnotes() {
        let html = render("Text[^note] and[^note].\n\n[^note]: The note.");
        assert!(html.contains(
            "<sup class=\"footnote-reference\" id=\"fnref:note\"><a href=\"#fn:note\">1</a></sup> and<sup class=\"footnote-reference\"><a href=\"#fn:note\">1</a></sup>"
        ));
        assert!(html.contains("<div class=\"footnote-definition\" id=\"fn:note\">"));
        assert!(
            html.contains("The note.</p>\n<a href=\"#fnref:note\" class=\"footnote-backref\">")
        );
    }

    #[test]
    fn export_embeds_images_of_the_document_folder_only() {
        let dir = std::env::temp_dir().join(format!("blink-export-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("doc")).unwrap();
        // The smallest GIF: one transparent pixel.
        let gif = b"GIF89a\x01\x00\x01\x00\x80\x00\x00\x00\x00\x00\xff\xff\xff\x21\xf9\x04\x01\x00\x00\x00\x00\x2c\x00\x00\x00\x00\x01\x00\x01\x00\x00\x02\x02\x44\x01\x00\x3b";
        std::fs::write(dir.join("doc/pixel.gif"), gif).unwrap();
        std::fs::write(dir.join("outside.gif"), gif).unwrap();
        let html = render_html(
            "![inside](pixel.gif) ![outside](../outside.gif)",
            &Options {
                base_dir: Some(dir.join("doc")),
                ..options()
            },
        );
        assert!(html.contains("src=\"data:image/gif;base64,"));
        assert!(!html.contains("outside.gif"));
        assert!(html.contains("outside"));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn export_colours_code_for_both_styles() {
        let style = |red| CodeStyle {
            color: Some((red, 0, 0)),
            bold: false,
            italic: false,
        };
        let html = render_html(
            "```rust\nfn main() {}\n```",
            &Options {
                light: vec![vec![CodeRun {
                    range: 0..2,
                    style: style(0x11),
                }]],
                dark: vec![vec![CodeRun {
                    range: 0..2,
                    style: style(0x22),
                }]],
                ..options()
            },
        );
        assert!(html.contains(
            "<pre><code class=\"language-rust\"><span style=\"--light:#110000;--dark:#220000;\">fn</span> main() {}</code></pre>"
        ));
    }

    #[test]
    fn export_renders_github_markdown() {
        let html = render(
            "## Intro\n\n- [x] done, see https://example.com.\n\n> [!TIP]\n> Hint\n\n[back](#intro)\n",
        );
        assert!(html.contains("<h2 id=\"intro\">"));
        assert!(html.contains("<a href=\"https://example.com\">https://example.com</a>."));
        assert!(html.contains("checked=\"\""));
        assert!(html.contains("class=\"markdown-alert-tip\""));
        assert!(html.contains("<strong>Tip</strong>"));
        assert!(html.contains("href=\"#intro\""));
    }

    #[test]
    fn export_draws_math() {
        let html = render("Inline $x^2$ and\n\n$$\n\\frac{a}{b}\n$$\n\nnot $\\nope$.\n");
        assert!(
            html.contains("<p>Inline <svg xmlns=\"http://www.w3.org/2000/svg\" class=\"math\"")
        );
        assert!(html.contains("<p><span class=\"math-display\"><svg"));
        assert!(html.contains("aria-label=\"\n\\frac{a}{b}\n\""));
        assert!(html.contains("not <code>\\nope</code>."));
    }

    #[test]
    fn export_keeps_the_text_and_styles_of_raw_html() {
        let html = render(
            "<div align=\"center\" onclick=\"x()\">\n  <b>Bold <i>both</b> italic</i><br>\n  next\n</div>\n\nA <sup>b</sup> <mark>c\n\n<img src=\"https://example.com/a.png\" alt=\"A\" width=\"40px\">\n",
        );
        assert!(html.contains(
            "<div class=\"align-center\"><strong>Bold <em>both</em></strong><em> italic</em><br>\nnext</div>"
        ));
        assert!(html.contains("<p>A <sup>b</sup> <mark>c</mark></p>"));
        assert!(html.contains("<img src=\"https://example.com/a.png\" alt=\"A\" width=\"40\" />"));
        assert!(!html.contains("onclick"));
        // A paragraph left open ends with the HTML block, a block left open with the document,
        // and a block in a paragraph is not written.
        let html = render("<p>\nOne\n\nTwo\n");
        assert!(html.contains("<div>One\n</div>\n<p>Two</p>"));
        let html =
            render("<center>\n<img src=\"https://example.com/a.png\">\n\nText <div>x</div>\n");
        assert!(html.contains("<div class=\"align-center\">"));
        assert!(html.contains("<p>Text x</p>\n</div>"));
        // A `<div>` holds the Markdown blocks up to its end, but not past the end of a quote,
        // and ends a `<details>` element opened in it.
        let html = render("<div align=\"center\">\n\n# Title\n\n</div>\n");
        assert!(html.contains("<div class=\"align-center\">\n<h1 id=\"title\">Title</h1>\n</div>"));
        let html = render("> <div>\n>\n> In\n\n</div>\n\n<div><details>\n\nx\n\n</div>\n");
        assert!(html.contains("<p>In</p>\n</div>\n</blockquote>"));
        assert!(html.contains("<p>x</p>\n</details></div>"));
        assert_eq!(html.matches("<div").count(), html.matches("</div>").count());
    }

    #[test]
    fn export_keeps_details_and_emoji() {
        let html = render(
            "<details open onclick=\"x()\"><summary>More <b>info</b></summary>\n\nHidden :tada:\n\n</details>\n",
        );
        assert!(html.contains("<details open><summary>More <strong>info</strong></summary>"));
        assert!(html.contains("<p>Hidden 🎉</p>"));
        assert!(html.contains("</details>"));
        assert!(!html.contains("onclick"));
    }

    #[test]
    fn export_fonts_cannot_end_the_stylesheet() {
        assert_eq!(css_string("A\"</style>"), "\"A\\22 \\3c /style\\3e \"");
    }

    #[test]
    fn url_scheme_detection() {
        assert!(url_has_scheme("javascript:x"));
        assert!(url_has_scheme("data:text"));
        assert!(url_has_scheme("HTTP://x"));
        assert!(!url_has_scheme("images/a.png"));
        assert!(!url_has_scheme("#anchor"));
        assert!(!url_has_scheme("./rel"));
        assert!(!url_has_scheme("page.html?q=1"));
    }
}
