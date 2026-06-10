use gtk::prelude::*;
use gtk::TextBuffer;
use pulldown_cmark::{Event, Parser, Tag, TagEnd};

pub fn setup_tags(buffer: &TextBuffer) {
    buffer.create_tag(Some("h1"), &[("scale", &2.0), ("weight", &700)]);
    buffer.create_tag(Some("h2"), &[("scale", &1.75), ("weight", &700)]);
    buffer.create_tag(Some("h3"), &[("scale", &1.5), ("weight", &700)]);
    buffer.create_tag(Some("h4"), &[("scale", &1.2), ("weight", &700)]);
    buffer.create_tag(Some("bold"), &[("weight", &700)]);
    buffer.create_tag(Some("italic"), &[("style", &gtk::pango::Style::Italic)]);
    buffer.create_tag(Some("code"), &[("family", &"Monospace")]);
    buffer.create_tag(Some("code_block"), &[
        ("family", &"Monospace"),
        ("indent", &16),
    ]);
    buffer.create_tag(Some("blockquote"), &[
        ("indent", &24),
        ("style", &gtk::pango::Style::Italic),
    ]);
    buffer.create_tag(Some("list"), &[("indent", &16)]);
}

pub fn render_markdown(buffer: &TextBuffer, text: &str) {
    let mut iter = buffer.bounds().0;
    buffer.delete(&mut iter, &mut buffer.bounds().1);
    
    let parser = Parser::new(text);
    let mut current_tags: Vec<&'static str> = Vec::new();
    let mut iter = buffer.end_iter();
    
    let mut list_depth = 0;

    for event in parser {
        match event {
            Event::Start(tag) => match tag {
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
