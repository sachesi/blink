//! Find and replace. In the editor it searches the Markdown source through GtkSourceView;
//! in the preview it searches the rendered text, code blocks and table cells included, and
//! replacing is not offered.

use adw::subclass::prelude::*;
use gettextrs::{gettext, ngettext};
use gtk::glib;
use gtk::prelude::*;
use sourceview5::prelude::*;
use std::cell::{Cell, OnceCell};

use super::{BlinkWindow, ViewMode, saturating_u32};
use crate::markdown;

/// No preview match selected yet.
const NO_MATCH: (i32, i32) = (-1, -1);

pub struct State {
    pub context: OnceCell<sourceview5::SearchContext>,
    /// The open search runs over the rendered preview rather than the editor.
    pub in_preview: Cell<bool>,
    /// Position of the selected preview match, which orders all of them: (offset of its
    /// prose or of its surface's anchor in the preview buffer, offset within the surface).
    pub match_key: Cell<(i32, i32)>,
}

impl Default for State {
    fn default() -> Self {
        Self {
            context: OnceCell::new(),
            in_preview: Cell::new(false),
            match_key: Cell::new(NO_MATCH),
        }
    }
}

/// One match in the preview, in document order by `key`.
#[derive(Clone, Copy)]
struct PreviewMatch {
    key: (i32, i32),
    target: PreviewTarget,
}

#[derive(Clone, Copy)]
enum PreviewTarget {
    Prose {
        start: i32,
        end: i32,
    },
    Code {
        surface: usize,
        start: i32,
        end: i32,
    },
    Cell {
        surface: usize,
        start: i32,
        end: i32,
    },
}

impl BlinkWindow {
    pub(super) fn setup_search(&self) {
        let imp = self.imp();
        let settings = sourceview5::SearchSettings::new();
        settings.set_wrap_around(true);
        let context = sourceview5::SearchContext::new(&*imp.edit_buffer, Some(&settings));
        context.set_highlight(false);
        context.connect_occurrences_count_notify(glib::clone!(
            #[weak(rename_to = win)]
            self,
            move |_| {
                if !win.imp().search.in_preview.get() {
                    win.refresh_search();
                }
            }
        ));
        imp.search.context.set(context).ok();
        self.refresh_replace_sensitivity();
    }

    fn context(&self) -> &sourceview5::SearchContext {
        self.imp()
            .search
            .context
            .get()
            .expect("search context set in constructed")
    }

    fn open_search(&self) {
        let imp = self.imp();
        imp.search_bar.set_visible(true);
        let in_preview = imp.view_mode.get() == ViewMode::Preview;
        imp.search.in_preview.set(in_preview);
        imp.replace_row.set_visible(!in_preview);
        self.context().set_highlight(!in_preview);
    }

    pub(super) fn close_search(&self) {
        let imp = self.imp();
        imp.search_bar.set_visible(false);
        self.context().set_highlight(false);
        self.clear_preview_highlights();
        imp.search.match_key.set(NO_MATCH);
        imp.search.in_preview.set(false);
        imp.search_status.set_label("");
    }

    pub(super) fn reset_preview_match(&self) {
        self.imp().search.match_key.set(NO_MATCH);
    }

    pub(super) fn toggle_find(&self) {
        let imp = self.imp();
        if imp.search_bar.is_visible() {
            self.close_search();
            return;
        }
        self.open_search();
        imp.search_entry.grab_focus();
        if imp.search.in_preview.get() {
            self.preview_search(true, true);
        } else {
            self.refresh_search();
        }
    }

    pub(super) fn show_replace(&self) {
        let imp = self.imp();
        self.open_search();
        if imp.search.in_preview.get() {
            imp.search_entry.grab_focus();
        } else {
            imp.replace_entry.grab_focus();
            self.refresh_search();
        }
    }

    pub(super) fn search_changed(&self) {
        let imp = self.imp();
        self.open_search();
        if imp.search.in_preview.get() {
            self.preview_search(true, true);
            return;
        }
        let buffer = &imp.edit_buffer;
        let offset = buffer
            .selection_bounds()
            .map(|(start, _)| start.offset())
            .unwrap_or_else(|| buffer.cursor_position());
        self.context()
            .settings()
            .set_search_text(Some(&imp.search_entry.text()));
        select_match_from_offset(buffer, self.context(), offset, true);
        self.refresh_search();
        self.scroll_editor_to_selection();
    }

    pub(super) fn find_next(&self, forward: bool) {
        self.open_search();
        if self.imp().search.in_preview.get() {
            self.preview_search(forward, false);
            return;
        }
        select_match(&self.imp().edit_buffer, self.context(), forward);
        self.refresh_search();
        self.scroll_editor_to_selection();
    }

    pub(super) fn replace_one(&self) {
        let imp = self.imp();
        self.open_search();
        if imp.search.in_preview.get() {
            return;
        }
        let buffer = &imp.edit_buffer;
        if replace_match(buffer, self.context(), imp.replace_entry.text().as_str()) {
            select_match(buffer, self.context(), true);
        }
        self.refresh_search();
        self.scroll_editor_to_selection();
    }

    pub(super) fn replace_all(&self) {
        let imp = self.imp();
        self.open_search();
        if imp.search.in_preview.get() {
            return;
        }
        let replaced = replace_all_matches(
            &imp.edit_buffer,
            self.context(),
            imp.replace_entry.text().as_str(),
        );
        self.refresh_search();
        let status = ngettext(
            "{} match replaced",
            "{} matches replaced",
            saturating_u32(replaced),
        )
        .replacen("{}", &replaced.to_string(), 1);
        imp.search_status.set_label(&status);
    }

    fn refresh_search(&self) {
        let imp = self.imp();
        imp.search_status
            .set_label(&search_status(self.context(), &imp.edit_buffer));
        self.refresh_replace_sensitivity();
    }

    fn refresh_replace_sensitivity(&self) {
        let imp = self.imp();
        let context = self.context();
        let enabled = has_search_text(context) && context.occurrences_count() > 0;
        imp.replace_button.set_sensitive(enabled);
        imp.replace_all_button.set_sensitive(enabled);
    }

    /// Select the next or previous match in the preview, relative to the selected one.
    /// `from_current` keeps the selected match when it still matches, as typing does.
    fn preview_search(&self, forward: bool, from_current: bool) {
        let imp = self.imp();
        let query = imp.search_entry.text().to_string();
        self.clear_preview_highlights();
        if query.is_empty() {
            imp.search.match_key.set(NO_MATCH);
            imp.search_status.set_label("");
            return;
        }
        let matches = self.collect_preview_matches(&query);
        if matches.is_empty() {
            imp.search.match_key.set(NO_MATCH);
            imp.search_status.set_label(&gettext("No matches"));
            return;
        }

        let current = imp.search.match_key.get();
        let index = if from_current {
            matches.iter().position(|m| m.key >= current).unwrap_or(0)
        } else if forward {
            matches.iter().position(|m| m.key > current).unwrap_or(0)
        } else {
            matches
                .iter()
                .rposition(|m| m.key < current)
                .unwrap_or(matches.len() - 1)
        };

        let found = matches[index];
        imp.search.match_key.set(found.key);
        self.select_preview_match(&found);
        let status = ngettext(
            "{} of {} match",
            "{} of {} matches",
            saturating_u32(matches.len()),
        )
        .replacen("{}", &(index + 1).to_string(), 1)
        .replacen("{}", &matches.len().to_string(), 1);
        imp.search_status.set_label(&status);
    }

    fn collect_preview_matches(&self, query: &str) -> Vec<PreviewMatch> {
        let imp = self.imp();
        let mut matches = Vec::new();

        let buffer = imp.preview_view.buffer();
        let flags = gtk::TextSearchFlags::CASE_INSENSITIVE | gtk::TextSearchFlags::VISIBLE_ONLY;
        let mut iter = buffer.start_iter();
        while let Some((start, end)) = iter.forward_search(query, flags, None) {
            if start.offset() == end.offset() {
                break;
            }
            matches.push(PreviewMatch {
                key: (start.offset(), 0),
                target: PreviewTarget::Prose {
                    start: start.offset(),
                    end: end.offset(),
                },
            });
            iter = end;
        }

        for (index, surface) in imp.preview.surfaces.borrow().iter().enumerate() {
            let (anchor_offset, text) = match surface {
                markdown::Surface::Code {
                    anchor_offset,
                    buffer,
                } => (*anchor_offset, super::buffer_text(buffer)),
                markdown::Surface::Cell {
                    anchor_offset,
                    label,
                } => (*anchor_offset, label.text().to_string()),
            };
            for (start, end) in find_all_ci(&text, query) {
                let target = match surface {
                    markdown::Surface::Code { .. } => PreviewTarget::Code {
                        surface: index,
                        start,
                        end,
                    },
                    markdown::Surface::Cell { .. } => PreviewTarget::Cell {
                        surface: index,
                        start,
                        end,
                    },
                };
                matches.push(PreviewMatch {
                    key: (anchor_offset, start),
                    target,
                });
            }
        }

        matches.sort_by_key(|m| m.key);
        matches
    }

    fn select_preview_match(&self, found: &PreviewMatch) {
        let imp = self.imp();
        let surfaces = imp.preview.surfaces.borrow();
        let anchor_offset = match found.target {
            PreviewTarget::Prose { start, end } => {
                let buffer = imp.preview_view.buffer();
                let start = buffer.iter_at_offset(start);
                buffer.select_range(&start, &buffer.iter_at_offset(end));
                start.offset()
            }
            PreviewTarget::Code {
                surface,
                start,
                end,
            } => {
                let Some(markdown::Surface::Code {
                    anchor_offset,
                    buffer,
                }) = surfaces.get(surface)
                else {
                    return;
                };
                buffer.select_range(&buffer.iter_at_offset(start), &buffer.iter_at_offset(end));
                *anchor_offset
            }
            PreviewTarget::Cell {
                surface,
                start,
                end,
            } => {
                let Some(markdown::Surface::Cell {
                    anchor_offset,
                    label,
                }) = surfaces.get(surface)
                else {
                    return;
                };
                label.select_region(start, end);
                *anchor_offset
            }
        };
        let buffer = imp.preview_view.buffer();
        let location = imp
            .preview_view
            .iter_location(&buffer.iter_at_offset(anchor_offset));
        self.scroll_preview_to_y(location.y());
    }

    fn clear_preview_highlights(&self) {
        let imp = self.imp();
        let buffer = imp.preview_view.buffer();
        let insert = buffer.iter_at_offset(buffer.cursor_position());
        buffer.select_range(&insert, &insert);
        self.clear_surface_selections(None);
    }

    /// Scroll the editor to the selected match when it is off screen: the editor does not
    /// have the focus during a search, so selecting does not scroll it. In the split view
    /// the preview follows.
    fn scroll_editor_to_selection(&self) {
        let imp = self.imp();
        if let Some((start, _)) = imp.edit_buffer.selection_bounds() {
            let y = f64::from(imp.edit_view.iter_location(&start).y());
            let vadj = imp.edit_scroll.vadjustment();
            if y < vadj.value() || y > vadj.value() + vadj.page_size() {
                let max = (vadj.upper() - vadj.page_size()).max(0.0);
                vadj.set_value((y - vadj.page_size() * 0.3).clamp(0.0, max));
            }
        }
    }
}

fn has_search_text(context: &sourceview5::SearchContext) -> bool {
    context
        .settings()
        .search_text()
        .is_some_and(|text| !text.is_empty())
}

fn search_status(context: &sourceview5::SearchContext, buffer: &sourceview5::Buffer) -> String {
    if !has_search_text(context) {
        return String::new();
    }
    let count = context.occurrences_count();
    if count < 0 {
        return gettext("Searching…");
    }
    if count == 0 {
        return gettext("No matches");
    }
    if let Some((start, end)) = buffer.selection_bounds() {
        let position = context.occurrence_position(&start, &end);
        if position > 0 {
            return ngettext("{} of {} match", "{} of {} matches", count as u32)
                .replacen("{}", &position.to_string(), 1)
                .replacen("{}", &count.to_string(), 1);
        }
    }
    gettext("{} matches").replacen("{}", &count.to_string(), 1)
}

/// Select the match after the selection (or before it), wrapping around.
fn select_match(
    buffer: &sourceview5::Buffer,
    context: &sourceview5::SearchContext,
    forward: bool,
) -> bool {
    let offset = match buffer.selection_bounds() {
        Some((start, end)) => {
            if forward {
                end.offset()
            } else {
                start.offset()
            }
        }
        None => buffer.cursor_position(),
    };
    select_match_from_offset(buffer, context, offset, forward)
}

fn select_match_from_offset(
    buffer: &sourceview5::Buffer,
    context: &sourceview5::SearchContext,
    offset: i32,
    forward: bool,
) -> bool {
    if !has_search_text(context) {
        return false;
    }
    let iter = buffer.iter_at_offset(offset);
    let found = if forward {
        context.forward(&iter)
    } else {
        context.backward(&iter)
    };
    match found {
        Some((start, end, _)) => {
            buffer.select_range(&start, &end);
            true
        }
        None => false,
    }
}

fn selection_is_match(buffer: &sourceview5::Buffer, context: &sourceview5::SearchContext) -> bool {
    buffer
        .selection_bounds()
        .is_some_and(|(start, end)| context.occurrence_position(&start, &end) > 0)
}

/// Replace the selected match, or else the next one.
fn replace_match(
    buffer: &sourceview5::Buffer,
    context: &sourceview5::SearchContext,
    replacement: &str,
) -> bool {
    if !has_search_text(context) {
        return false;
    }
    if !selection_is_match(buffer, context) && !select_match(buffer, context, true) {
        return false;
    }
    buffer
        .selection_bounds()
        .is_some_and(|(mut start, mut end)| {
            context.replace(&mut start, &mut end, replacement).is_ok()
        })
}

/// Replace every match as one undoable step, and say how many were replaced.
fn replace_all_matches(
    buffer: &sourceview5::Buffer,
    context: &sourceview5::SearchContext,
    replacement: &str,
) -> usize {
    if !has_search_text(context) {
        return 0;
    }
    let settings = context.settings();
    let wrapped_around = settings.wraps_around();
    settings.set_wrap_around(false);

    let mut matches = Vec::new();
    let mut iter = buffer.start_iter();
    while let Some((start, end, _)) = context.forward(&iter) {
        if start.offset() == end.offset() {
            break;
        }
        matches.push((start.offset(), end.offset()));
        iter = end;
    }

    let mut replaced = 0;
    if !matches.is_empty() {
        buffer.begin_user_action();
        // Last to first, so the offsets still ahead stay valid.
        for (start, end) in matches.iter().rev() {
            let mut start = buffer.iter_at_offset(*start);
            let mut end = buffer.iter_at_offset(*end);
            if context.replace(&mut start, &mut end, replacement).is_ok() {
                replaced += 1;
            }
        }
        buffer.end_user_action();
    }

    settings.set_wrap_around(wrapped_around);
    replaced
}

/// Case-insensitive, non-overlapping matches of `needle` as character offset ranges, which
/// is what text buffers and label selections count in. Exact for ASCII; a character whose
/// lower case has another length shifts the ranges after it.
fn find_all_ci(haystack: &str, needle: &str) -> Vec<(i32, i32)> {
    let mut result = Vec::new();
    if needle.is_empty() {
        return result;
    }
    let hay = haystack.to_lowercase();
    let need = needle.to_lowercase();
    let mut byte = 0;
    while let Some(pos) = hay[byte..].find(&need) {
        let byte_start = byte + pos;
        let byte_end = byte_start + need.len();
        let char_start = hay[..byte_start].chars().count() as i32;
        let char_end = hay[..byte_end].chars().count() as i32;
        result.push((char_start, char_end));
        byte = byte_end;
    }
    result
}

#[cfg(test)]
mod tests {
    use super::find_all_ci;

    #[test]
    fn find_all_ci_counts_characters_and_ignores_case() {
        assert_eq!(
            find_all_ci("Foo foo FOO", "foo"),
            vec![(0, 3), (4, 7), (8, 11)]
        );
        assert_eq!(
            find_all_ci("ПриВіт, привіт", "привіт"),
            vec![(0, 6), (8, 14)]
        );
        assert_eq!(find_all_ci("aaa", "aa"), vec![(0, 2)]);
        assert!(find_all_ci("text", "").is_empty());
        assert!(find_all_ci("text", "missing").is_empty());
    }
}
