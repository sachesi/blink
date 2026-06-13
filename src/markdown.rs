use gtk::prelude::*;
use gtk::{Grid, Label, TextBuffer, TextView, gio, glib};
use pulldown_cmark::{Event, Parser, Tag, TagEnd};
use std::path::{Path, PathBuf};

pub fn setup_tags(buffer: &TextBuffer) {
    buffer.create_tag(Some("h1"), &[("scale", &2.0), ("weight", &700)]);
    buffer.create_tag(Some("h2"), &[("scale", &1.75), ("weight", &700)]);
    buffer.create_tag(Some("h3"), &[("scale", &1.5), ("weight", &700)]);
    buffer.create_tag(Some("h4"), &[("scale", &1.2), ("weight", &700)]);
    buffer.create_tag(Some("bold"), &[("weight", &700)]);
    buffer.create_tag(Some("italic"), &[("style", &gtk::pango::Style::Italic)]);
    buffer.create_tag(Some("strikethrough"), &[("strikethrough", &true)]);
    buffer.create_tag(
        Some("link"),
        &[
            ("foreground", &"#3584e4"),
            ("underline", &gtk::pango::Underline::Single),
        ],
    );
    buffer.create_tag(Some("code"), &[("family", &"Monospace"), ("weight", &700)]);
    buffer.create_tag(
        Some("code_block"),
        &[
            ("family", &"Monospace"),
            ("paragraph-background", &"rgba(128, 128, 128, 0.15)"),
            ("left-margin", &16),
            ("right-margin", &16),
            ("pixels-above-lines", &8),
            ("pixels-below-lines", &8),
        ],
    );
    buffer.create_tag(
        Some("blockquote"),
        &[
            ("indent", &24),
            ("style", &gtk::pango::Style::Italic),
            ("foreground", &"rgba(128, 128, 128, 0.9)"),
        ],
    );
    buffer.create_tag(Some("list"), &[("indent", &16)]);
}

fn local_image_path(dest_url: &str, base_dir: Option<&Path>) -> Option<PathBuf> {
    let dest_url = dest_url.trim();
    if dest_url.is_empty() || dest_url.starts_with("data:") {
        return None;
    }

    if dest_url.starts_with("file://") {
        return gio::File::for_uri(dest_url)
            .path()
            .filter(|path| path.is_file());
    }

    if dest_url.contains("://") {
        return None;
    }

    let path = Path::new(dest_url);
    let path = if path.is_absolute() {
        path.to_path_buf()
    } else {
        base_dir?.join(path)
    };

    path.is_file().then_some(path)
}

pub fn render_markdown(
    view: &TextView,
    text: &str,
    hadj: &gtk::Adjustment,
    image_base_dir: Option<&Path>,
) {
    let buffer = view.buffer();
    let mut iter = buffer.bounds().0;
    buffer.delete(&mut iter, &mut buffer.bounds().1);

    // By re-creating the parser, we iterate through events.
    let mut options = pulldown_cmark::Options::empty();
    options.insert(pulldown_cmark::Options::ENABLE_TABLES);
    options.insert(pulldown_cmark::Options::ENABLE_STRIKETHROUGH);
    options.insert(pulldown_cmark::Options::ENABLE_TASKLISTS);
    let parser = Parser::new_ext(text, options);
    let mut current_tags: Vec<&'static str> = Vec::new();
    let mut iter = buffer.end_iter();

    // One entry per open list. `Some(n)` is an ordered list whose next item
    // number is `n`; `None` is a bullet list. Length doubles as nesting depth.
    let mut list_stack: Vec<Option<u64>> = Vec::new();
    // Open blockquote nesting level, used to indent block widgets.
    let mut blockquote_depth: i32 = 0;

    let mut in_table = false;
    let mut table_rows: Vec<Vec<String>> = Vec::new();
    let mut current_row: Vec<String> = Vec::new();
    let mut current_cell = String::new();

    let mut in_code_block = false;
    let mut current_code = String::new();

    let mut in_image = false;

    for event in parser {
        if in_code_block {
            match event {
                Event::Text(t) | Event::Code(t) => {
                    current_code.push_str(&t);
                }
                Event::End(TagEnd::CodeBlock) => {
                    in_code_block = false;
                    let indent = list_stack.len() as i32 * 16 + blockquote_depth * 24;
                    let scroll = gtk::ScrolledWindow::builder()
                        .margin_top(12)
                        .margin_bottom(12)
                        .margin_start(indent)
                        .hexpand(true)
                        .propagate_natural_height(true)
                        .hscrollbar_policy(gtk::PolicyType::Automatic)
                        .vscrollbar_policy(gtk::PolicyType::Never)
                        // Keep out of the focus chain so a click never makes the
                        // preview scroll this block into view.
                        .focusable(false)
                        .build();
                    scroll.add_css_class("card");

                    let clean_code = current_code.trim_end_matches('\n');

                    let code_buffer = TextBuffer::builder().text(clean_code).build();
                    let code_view = TextView::builder()
                        .buffer(&code_buffer)
                        .editable(false)
                        .cursor_visible(false)
                        .monospace(true)
                        .margin_top(12)
                        .margin_bottom(12)
                        .margin_start(12)
                        .margin_end(12)
                        .can_focus(false)
                        .wrap_mode(gtk::WrapMode::None)
                        .focusable(false)
                        .css_classes(["transparent-bg"])
                        .build();

                    hadj.bind_property("page-size", &scroll, "width-request")
                        .transform_to(move |_, page_size: f64| {
                            let max_width = page_size.min(700.0);
                            Some((max_width - 64.0 - indent as f64).max(100.0) as i32)
                        })
                        .sync_create()
                        .build();

                    scroll.set_child(Some(&code_view));

                    let anchor = buffer.create_child_anchor(&mut iter);
                    view.add_child_at_anchor(&scroll, &anchor);
                    buffer.insert(&mut iter, "\n\n");
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
                Event::Code(c) => {
                    current_cell.push_str(&format!("<tt>{}</tt>", glib::markup_escape_text(&c)));
                }
                Event::Text(t) => {
                    current_cell.push_str(&glib::markup_escape_text(&t));
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

                    hadj.bind_property("page-size", &grid, "width-request")
                        .transform_to(move |_, page_size: f64| {
                            let max_width = page_size.min(700.0);
                            Some((max_width - 64.0 - indent as f64).max(100.0) as i32)
                        })
                        .sync_create()
                        .build();

                    let num_cols = table_rows.first().map_or(1, |r| r.len());
                    let grid_cols = (num_cols * 2).saturating_sub(1) as i32;

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

                            let label = Label::builder()
                                .margin_top(10)
                                .margin_bottom(10)
                                .margin_start(12)
                                .margin_end(12)
                                .wrap(true)
                                .xalign(0.0)
                                .hexpand(true)
                                .focusable(false)
                                .can_focus(false)
                                .build();
                            label.set_markup(cell_text);
                            if row_idx == 0 {
                                label.add_css_class("heading");
                            }
                            grid.attach(&label, text_col, text_row, 1, 1);

                            if col_idx > 0 {
                                let vsep = gtk::Separator::builder()
                                    .orientation(gtk::Orientation::Vertical)
                                    .vexpand(true)
                                    .build();
                                grid.attach(&vsep, text_col - 1, text_row, 1, 1);
                            }
                        }
                    }
                    let anchor = buffer.create_child_anchor(&mut iter);
                    view.add_child_at_anchor(&grid, &anchor);
                    buffer.insert(&mut iter, "\n\n");
                }
                _ => {}
            }
            continue;
        }

        match event {
            Event::Start(tag) => match tag {
                Tag::CodeBlock(_) => {
                    in_code_block = true;
                    current_code.clear();
                }
                Tag::Table(_) => {
                    in_table = true;
                    table_rows.clear();
                }
                Tag::Heading { level, .. } => {
                    let level_num = level as u8;
                    current_tags.push(match level_num {
                        1 => "h1",
                        2 => "h2",
                        3 => "h3",
                        _ => "h4",
                    });
                }
                Tag::Strong => current_tags.push("bold"),
                Tag::Emphasis => current_tags.push("italic"),
                Tag::Strikethrough => current_tags.push("strikethrough"),
                Tag::Link { .. } => current_tags.push("link"),
                Tag::Image { dest_url, .. } => {
                    in_image = true;
                    if let Some(path) = local_image_path(dest_url.as_ref(), image_base_dir) {
                        let picture = gtk::Picture::for_filename(path);
                        picture.set_focusable(false);
                        picture.set_margin_top(12);
                        picture.set_margin_bottom(12);
                        picture.set_hexpand(false);
                        picture.set_halign(gtk::Align::Center);

                        hadj.bind_property("page-size", &picture, "width-request")
                            .transform_to(|_, page_size: f64| {
                                let max_width = page_size.min(700.0);
                                Some((max_width - 64.0).max(100.0) as i32)
                            })
                            .sync_create()
                            .build();

                        let anchor = buffer.create_child_anchor(&mut iter);
                        view.add_child_at_anchor(&picture, &anchor);
                    }
                }
                Tag::BlockQuote(_) => {
                    blockquote_depth += 1;
                    current_tags.push("blockquote");
                }
                Tag::List(first) => {
                    list_stack.push(first);
                    current_tags.push("list");
                }
                Tag::Item => {
                    let marker = match list_stack.last_mut() {
                        Some(Some(number)) => {
                            let marker = format!("{}. ", number);
                            *number += 1;
                            marker
                        }
                        _ => format!("{} ", if list_stack.len() % 2 == 1 { "•" } else { "◦" }),
                    };
                    let start_offset = iter.offset();
                    buffer.insert(&mut iter, &marker);
                    let start_iter = buffer.iter_at_offset(start_offset);
                    buffer.apply_tag_by_name("bold", &start_iter, &iter);
                }
                _ => {}
            },
            Event::End(tag_end) => match tag_end {
                TagEnd::Heading(_) => {
                    current_tags.retain(|&t| t != "h1" && t != "h2" && t != "h3" && t != "h4");
                    buffer.insert(&mut iter, "\n\n");
                }
                TagEnd::Strong => {
                    current_tags.retain(|&t| t != "bold");
                }
                TagEnd::Emphasis => {
                    current_tags.retain(|&t| t != "italic");
                }
                TagEnd::Strikethrough => {
                    current_tags.retain(|&t| t != "strikethrough");
                }
                TagEnd::Link => {
                    current_tags.retain(|&t| t != "link");
                }
                TagEnd::Image => {
                    in_image = false;
                }
                TagEnd::BlockQuote(_) => {
                    current_tags.retain(|&t| t != "blockquote");
                    blockquote_depth = (blockquote_depth - 1).max(0);
                    buffer.insert(&mut iter, "\n\n");
                }
                TagEnd::List(_) => {
                    current_tags.retain(|&t| t != "list");
                    list_stack.pop();
                }
                TagEnd::Item => {
                    buffer.insert(&mut iter, "\n");
                }
                TagEnd::Paragraph => {
                    buffer.insert(&mut iter, "\n\n");
                }
                _ => {}
            },
            Event::Text(t) => {
                if in_image {
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
                let start_offset = iter.offset();
                buffer.insert(&mut iter, &c);
                let start_iter = buffer.iter_at_offset(start_offset);
                buffer.apply_tag_by_name("code", &start_iter, &iter);
            }
            Event::TaskListMarker(checked) => {
                let start_offset = iter.offset();
                buffer.insert(&mut iter, if checked { "☑ " } else { "☐ " });
                let start_iter = buffer.iter_at_offset(start_offset);
                buffer.apply_tag_by_name("bold", &start_iter, &iter);
            }
            Event::SoftBreak | Event::HardBreak => {
                buffer.insert(&mut iter, "\n");
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::local_image_path;
    use std::fs;

    #[test]
    fn resolves_relative_images_against_document_directory() {
        let root = std::env::temp_dir().join(format!("blink-markdown-test-{}", std::process::id()));
        let image_dir = root.join("images");
        let image = image_dir.join("photo.png");
        fs::create_dir_all(&image_dir).unwrap();
        fs::write(&image, b"").unwrap();

        assert_eq!(
            local_image_path("images/photo.png", Some(&root)),
            Some(image.clone())
        );

        let _ = fs::remove_file(image);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn rejects_remote_images() {
        assert!(local_image_path("https://example.invalid/image.png", None).is_none());
        assert!(local_image_path("data:image/png;base64,AAAA", None).is_none());
    }
}
