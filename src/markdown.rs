use gtk::prelude::*;
use gtk::{TextBuffer, TextView, Grid, Label, TextChildAnchor};
use pulldown_cmark::{Event, Parser, Tag, TagEnd};

pub fn setup_tags(buffer: &TextBuffer) {
    buffer.create_tag(Some("h1"), &[("scale", &2.0), ("weight", &700)]);
    buffer.create_tag(Some("h2"), &[("scale", &1.75), ("weight", &700)]);
    buffer.create_tag(Some("h3"), &[("scale", &1.5), ("weight", &700)]);
    buffer.create_tag(Some("h4"), &[("scale", &1.2), ("weight", &700)]);
    buffer.create_tag(Some("bold"), &[("weight", &700)]);
    buffer.create_tag(Some("italic"), &[("style", &gtk::pango::Style::Italic)]);
    buffer.create_tag(Some("strikethrough"), &[("strikethrough", &true)]);
    buffer.create_tag(Some("link"), &[
        ("foreground", &"#3584e4"),
        ("underline", &gtk::pango::Underline::Single),
    ]);
    buffer.create_tag(Some("code"), &[
        ("family", &"Monospace"),
        ("background", &"rgba(128, 128, 128, 0.15)"),
    ]);
    buffer.create_tag(Some("code_block"), &[
        ("family", &"Monospace"),
        ("paragraph-background", &"rgba(128, 128, 128, 0.15)"),
        ("left-margin", &16),
        ("right-margin", &16),
        ("pixels-above-lines", &8),
        ("pixels-below-lines", &8),
    ]);
    buffer.create_tag(Some("blockquote"), &[
        ("indent", &24),
        ("style", &gtk::pango::Style::Italic),
        ("foreground", &"rgba(128, 128, 128, 0.9)"),
    ]);
    buffer.create_tag(Some("list"), &[("indent", &16)]);
}

pub fn highlight_editor(buffer: &TextBuffer, text: &str) {
    let start = buffer.bounds().0;
    let end = buffer.bounds().1;
    buffer.remove_all_tags(&start, &end);

    let mut byte_to_char = vec![0; text.len() + 1];
    let mut char_count = 0;
    for (byte_idx, _) in text.char_indices() {
        byte_to_char[byte_idx] = char_count;
        char_count += 1;
    }
    byte_to_char[text.len()] = char_count;

    let parser = Parser::new(text).into_offset_iter();
    for (event, range) in parser {
        let start_char = byte_to_char[range.start];
        let end_char = byte_to_char[range.end];
        let start_iter = buffer.iter_at_offset(start_char as i32);
        let end_iter = buffer.iter_at_offset(end_char as i32);

        match event {
            Event::Start(Tag::Heading { level, .. }) => {
                let tag_name = format!("h{}", level as u8);
                buffer.apply_tag_by_name(&tag_name, &start_iter, &end_iter);
            }
            Event::Code(_) | Event::Start(Tag::CodeBlock(_)) => {
                buffer.apply_tag_by_name("code", &start_iter, &end_iter);
            }
            Event::Start(Tag::Strong) => {
                buffer.apply_tag_by_name("bold", &start_iter, &end_iter);
            }
            Event::Start(Tag::Emphasis) => {
                buffer.apply_tag_by_name("italic", &start_iter, &end_iter);
            }
            Event::Start(Tag::BlockQuote(_)) => {
                buffer.apply_tag_by_name("blockquote", &start_iter, &end_iter);
            }
            Event::Start(Tag::Link { .. }) => {
                buffer.apply_tag_by_name("link", &start_iter, &end_iter);
            }
            Event::Start(Tag::Strikethrough) => {
                buffer.apply_tag_by_name("strikethrough", &start_iter, &end_iter);
            }
            _ => {}
        }
    }
}

pub fn render_markdown(view: &TextView, text: &str) {
    let buffer = view.buffer();
    let mut iter = buffer.bounds().0;
    buffer.delete(&mut iter, &mut buffer.bounds().1);
    
    // By re-creating the parser, we iterate through events.
    let parser = Parser::new(text);
    let mut current_tags: Vec<&'static str> = Vec::new();
    let mut iter = buffer.end_iter();
    
    let mut list_depth = 0;

    let mut in_table = false;
    let mut table_rows: Vec<Vec<String>> = Vec::new();
    let mut current_row: Vec<String> = Vec::new();
    let mut current_cell = String::new();
    let mut _in_head = false;

    for event in parser {
        if in_table {
            match event {
                Event::Start(Tag::TableHead) => _in_head = true,
                Event::End(TagEnd::TableHead) => _in_head = false,
                Event::Start(Tag::TableRow) => current_row = Vec::new(),
                Event::End(TagEnd::TableRow) => {
                    table_rows.push(current_row.clone());
                }
                Event::Start(Tag::TableCell) => current_cell = String::new(),
                Event::End(TagEnd::TableCell) => {
                    current_row.push(current_cell.clone());
                }
                Event::Text(t) | Event::Code(t) => {
                    current_cell.push_str(&t);
                }
                Event::End(TagEnd::Table) => {
                    in_table = false;
                    let grid = Grid::builder()
                        .margin_top(12)
                        .margin_bottom(12)
                        .column_spacing(16)
                        .row_spacing(8)
                        .build();
                    grid.add_css_class("card");
                    
                    for (row_idx, row) in table_rows.iter().enumerate() {
                        for (col_idx, cell_text) in row.iter().enumerate() {
                            let label = Label::builder()
                                .label(cell_text)
                                .margin_top(8).margin_bottom(8).margin_start(8).margin_end(8)
                                .build();
                            if row_idx == 0 {
                                label.add_css_class("heading");
                            }
                            grid.attach(&label, col_idx as i32, row_idx as i32, 1, 1);
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
                Tag::CodeBlock(_) => current_tags.push("code_block"),
                Tag::BlockQuote(_) => current_tags.push("blockquote"),
                Tag::List(_) => {
                    list_depth += 1;
                    current_tags.push("list");
                }
                Tag::Item => {
                    let bullet = format!("{} ", if list_depth % 2 == 1 { "•" } else { "◦" });
                    let start_offset = iter.offset();
                    buffer.insert(&mut iter, &bullet);
                    let start_iter = buffer.iter_at_offset(start_offset);
                    buffer.apply_tag_by_name("bold", &start_iter, &iter);
                }
                _ => {}
            },
            Event::End(tag_end) => match tag_end {
                TagEnd::Heading(_) => { current_tags.retain(|&t| t != "h1" && t != "h2" && t != "h3" && t != "h4"); buffer.insert(&mut iter, "\n\n"); },
                TagEnd::Strong => { current_tags.retain(|&t| t != "bold"); },
                TagEnd::Emphasis => { current_tags.retain(|&t| t != "italic"); },
                TagEnd::Strikethrough => { current_tags.retain(|&t| t != "strikethrough"); },
                TagEnd::Link => { current_tags.retain(|&t| t != "link"); },
                TagEnd::CodeBlock => { current_tags.retain(|&t| t != "code_block"); buffer.insert(&mut iter, "\n\n"); },
                TagEnd::BlockQuote(_) => { current_tags.retain(|&t| t != "blockquote"); buffer.insert(&mut iter, "\n\n"); },
                TagEnd::List(_) => { current_tags.retain(|&t| t != "list"); list_depth -= 1; },
                TagEnd::Item => { buffer.insert(&mut iter, "\n"); },
                TagEnd::Paragraph => { buffer.insert(&mut iter, "\n\n"); },
                _ => {}
            },
            Event::Text(t) => {
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
            Event::SoftBreak | Event::HardBreak => {
                buffer.insert(&mut iter, "\n");
            }
            _ => {}
        }
    }
}
