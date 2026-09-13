//! HTML export. The document may come from anyone and the file may be opened in a browser,
//! so raw HTML is dropped and link and image addresses are limited to safe schemes.

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

/// Escape text for safe inclusion in an HTML element (used for the `<title>`).
fn escape_html(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

pub fn render_html(text: &str, title: &str) -> String {
    use pulldown_cmark::{Event, Tag};

    let mut options = pulldown_cmark::Options::empty();
    options.insert(pulldown_cmark::Options::ENABLE_TABLES);
    options.insert(pulldown_cmark::Options::ENABLE_STRIKETHROUGH);
    options.insert(pulldown_cmark::Options::ENABLE_TASKLISTS);
    // Untrusted document content is exported to a file that may be opened in a
    // browser. Raw HTML is dropped (no `<script>`/`onerror=` passthrough) and
    // link/image URLs are scheme-filtered, mirroring the in-app preview's own
    // allow-list. pulldown-cmark performs no sanitization of its own.
    let parser = pulldown_cmark::Parser::new_ext(text, options).filter_map(|event| match event {
        Event::Html(_) | Event::InlineHtml(_) => None,
        Event::Start(Tag::Link {
            link_type,
            dest_url,
            title,
            id,
        }) => Some(Event::Start(Tag::Link {
            link_type,
            dest_url: sanitize_export_url(dest_url),
            title,
            id,
        })),
        Event::Start(Tag::Image {
            link_type,
            dest_url,
            title,
            id,
        }) => Some(Event::Start(Tag::Image {
            link_type,
            dest_url: sanitize_export_url(dest_url),
            title,
            id,
        })),
        other => Some(other),
    });
    let mut html_output = String::new();
    pulldown_cmark::html::push_html(&mut html_output, parser);

    let css = "
* { box-sizing: border-box; }
body { font-family: system-ui, -apple-system, sans-serif; line-height: 1.6; max-width: 100%; margin: 0; padding: 20px; color: #333; }
pre { background: #f5f5f5; padding: 12px; border-radius: 6px; overflow-x: auto; }
code { font-family: ui-monospace, monospace; font-weight: bold; font-size: 0.9em; }
pre code { background: transparent; padding: 0; font-weight: normal; }
blockquote { border-left: 4px solid #ddd; margin: 0; padding-left: 12px; color: #666; }
table { border-collapse: collapse; width: 100%; margin: 16px 0; table-layout: fixed; overflow-wrap: break-word; }
th, td { border: 1px solid #ddd; padding: 8px; text-align: left; vertical-align: top; word-wrap: break-word; }
th { background: #f9f9f9; }
img { max-width: 100%; border-radius: 8px; }
@media (prefers-color-scheme: dark) {
    body { background: #1e1e1e; color: #eee; }
    pre { background: #2d2d2d; }
    blockquote { border-left-color: #555; color: #aaa; }
    th, td { border-color: #444; }
    th { background: #2a2a2a; }
}";
    let title = escape_html(title);
    format!(
        "<!DOCTYPE html>\n<html>\n<head>\n<meta charset=\"utf-8\">\n<title>{title}</title>\n<style>\n{css}\n</style>\n</head>\n<body>\n{html_output}\n</body>\n</html>"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn export_strips_raw_html_and_scripts() {
        let html = render_html(
            "Hello\n\n<script>alert('xss')</script>\n\n<b>raw</b> text",
            "doc",
        );
        assert!(!html.contains("<script>"));
        assert!(!html.contains("alert("));
        // The inline raw <b> tag is dropped, but its text content survives.
        assert!(html.contains("raw"));
        assert!(html.contains("Hello"));
    }

    #[test]
    fn export_neutralizes_dangerous_link_schemes() {
        let html = render_html("[click](javascript:alert(1))", "doc");
        assert!(!html.contains("javascript:"));
        // The link text is preserved; only the destination is emptied.
        assert!(html.contains("click"));
    }

    #[test]
    fn export_preserves_safe_links_and_relative_paths() {
        let html = render_html(
            "[web](https://example.com) and [rel](images/a.png) and [a](#sec)",
            "doc",
        );
        assert!(html.contains("https://example.com"));
        assert!(html.contains("images/a.png"));
        assert!(html.contains("#sec"));
    }

    #[test]
    fn export_title_is_derived_and_escaped() {
        let html = render_html("body", "a & b <x>.md");
        assert!(html.contains("<title>a &amp; b &lt;x&gt;.md</title>"));
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
