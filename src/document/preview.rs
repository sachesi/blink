//! The rendered preview: debounced rendering, the word count, links, and the scroll
//! position the preview and the editor share in the split view.

use adw::prelude::*;
use adw::subclass::prelude::*;
use gettextrs::ngettext;
use gtk::{gio, glib};
use sourceview5::prelude::*;
use std::cell::{Cell, RefCell};
use std::path::Path;
use std::time::Duration;

use super::{BlinkDocument, ViewMode, buffer_text, saturating_u32};
use crate::config;
use crate::markdown;

/// How long typing has to pause before the preview is rendered again.
const RENDER_DELAY: Duration = Duration::from_millis(300);

#[derive(Default)]
pub struct State {
    /// The text changed while the preview was out of sight.
    pub dirty: Cell<bool>,
    pub render_timer: RefCell<Option<glib::SourceId>>,
    /// Set while one pane's scroll position is being copied to the other.
    pub syncing: Cell<bool>,
    /// Clickable ranges of the preview buffer, as (start offset, end offset, address).
    pub links: RefCell<Vec<(i32, i32, String)>>,
    /// Code blocks and table cells, whose text lives outside the preview buffer.
    pub surfaces: RefCell<Vec<markdown::Surface>>,
    /// The blocks the preview buffer holds, which the next render keeps if unchanged.
    pub rendered: RefCell<markdown::Rendered>,
}

impl BlinkDocument {
    pub(super) fn setup_preview(&self) {
        let imp = self.imp();
        let buffer = imp.preview_view.buffer();
        markdown::setup_tags(&buffer);
        markdown::set_monospace_family(&buffer, &config::font_family(self.settings(), true));
        // Indenting text tags carry absolute left margins, so the renderer owns this value.
        imp.preview_view.set_left_margin(markdown::TEXT_MARGIN);
        imp.preview_view.set_right_margin(markdown::TEXT_MARGIN);

        let edit = imp.edit_scroll.vadjustment();
        let preview = imp.preview_scroll.vadjustment();
        self.sync_scroll(&edit, &preview);
        self.sync_scroll(&preview, &edit);

        // Links open on release, so the click neither takes the focus nor fights the
        // selection. Only web and mail links are followed, since documents can come from
        // anyone.
        let click = gtk::GestureClick::new();
        click.connect_released(glib::clone!(
            #[weak(rename_to = document)]
            self,
            move |_, _, x, y| {
                if let Some(url) = document.link_at(x, y) {
                    gtk::UriLauncher::new(&url).launch(
                        document.dialog_parent().as_ref(),
                        gio::Cancellable::NONE,
                        |_| {},
                    );
                }
            }
        ));
        imp.preview_view.add_controller(click);

        let motion = gtk::EventControllerMotion::new();
        motion.connect_motion(glib::clone!(
            #[weak(rename_to = document)]
            self,
            move |_, x, y| {
                let cursor = if document.link_at(x, y).is_some() {
                    "pointer"
                } else {
                    "text"
                };
                document
                    .imp()
                    .preview_view
                    .set_cursor_from_name(Some(cursor));
            }
        ));
        imp.preview_view.add_controller(motion);

        // Code blocks and table cells keep selections of their own, which the preview's
        // copy (Ctrl+C or its menu) does not see: only one selection is kept at a time, and
        // copying takes it from wherever it is.
        buffer.connect_has_selection_notify(glib::clone!(
            #[weak(rename_to = document)]
            self,
            move |buffer| {
                if buffer.has_selection() {
                    document.clear_surface_selections(None);
                }
            }
        ));
        imp.preview_view.connect_copy_clipboard(glib::clone!(
            #[weak(rename_to = document)]
            self,
            move |view| {
                if let Some(text) = document.surface_selection() {
                    view.clipboard().set_text(&text);
                    view.stop_signal_emission_by_name("copy-clipboard");
                }
            }
        ));
    }

    /// Clear the selections of the preview's code blocks and table cells, except the
    /// one of `keep`.
    pub(super) fn clear_surface_selections(&self, keep: Option<&glib::Object>) {
        for surface in self.imp().preview.surfaces.borrow().iter() {
            match surface {
                markdown::Surface::Code { buffer, .. } => {
                    if keep != Some(buffer.upcast_ref()) {
                        let insert = buffer.iter_at_offset(buffer.cursor_position());
                        buffer.select_range(&insert, &insert);
                    }
                }
                markdown::Surface::Cell { label, .. } => {
                    if keep != Some(label.upcast_ref()) {
                        label.select_region(-1, -1);
                    }
                }
            }
        }
    }

    /// A selection starting in a code block or table cell replaces every other one.
    fn watch_surface_selections(&self, surfaces: &[markdown::Surface]) {
        for surface in surfaces {
            let select = glib::clone!(
                #[weak(rename_to = document)]
                self,
                move |source: &glib::Object| {
                    let buffer = document.imp().preview_view.buffer();
                    let insert = buffer.iter_at_offset(buffer.cursor_position());
                    buffer.select_range(&insert, &insert);
                    document.clear_surface_selections(Some(source));
                }
            );
            match surface {
                markdown::Surface::Code { buffer, .. } => {
                    buffer.connect_has_selection_notify(move |buffer| {
                        if buffer.has_selection() {
                            select(buffer.upcast_ref());
                        }
                    });
                }
                markdown::Surface::Cell { label, .. } => {
                    label.connect_notify_local(Some("selection-bound"), move |label, _| {
                        if label.selection_bounds().is_some() {
                            select(label.upcast_ref());
                        }
                    });
                }
            }
        }
    }

    /// Give the code blocks the style scheme of the current light or dark appearance.
    pub(super) fn restyle_code_blocks(&self) {
        let scheme = markdown::current_scheme();
        for surface in self.imp().preview.surfaces.borrow().iter() {
            if let markdown::Surface::Code { buffer, .. } = surface {
                buffer.set_style_scheme(scheme.as_ref());
            }
        }
    }

    /// The text selected in a code block or table cell, if any.
    fn surface_selection(&self) -> Option<String> {
        self.imp()
            .preview
            .surfaces
            .borrow()
            .iter()
            .find_map(|surface| match surface {
                markdown::Surface::Code { buffer, .. } => buffer
                    .selection_bounds()
                    .map(|(start, end)| buffer.text(&start, &end, false).to_string()),
                markdown::Surface::Cell { label, .. } => {
                    label.selection_bounds().map(|(start, end)| {
                        label
                            .text()
                            .chars()
                            .skip(start as usize)
                            .take((end - start) as usize)
                            .collect()
                    })
                }
            })
    }

    /// In the split view, scrolling `source` scrolls `target` to the same proportion.
    fn sync_scroll(&self, source: &gtk::Adjustment, target: &gtk::Adjustment) {
        source.connect_value_changed(glib::clone!(
            #[weak(rename_to = document)]
            self,
            #[weak]
            target,
            move |source| {
                let imp = document.imp();
                if imp.view_mode.get() != ViewMode::Split || imp.preview.syncing.get() {
                    return;
                }
                document.set_ratio(&target, adjustment_ratio(source));
            }
        ));
    }

    /// Scroll `adjustment` without the other pane following.
    fn set_ratio(&self, adjustment: &gtk::Adjustment, ratio: f64) {
        let syncing = &self.imp().preview.syncing;
        syncing.set(true);
        set_adjustment_ratio(adjustment, ratio);
        syncing.set(false);
    }

    /// Scroll the preview to `y` without the editor following.
    pub(super) fn scroll_preview_to_y(&self, y: i32) {
        let imp = self.imp();
        let vadj = imp.preview_scroll.vadjustment();
        let max = (vadj.upper() - vadj.page_size()).max(0.0);
        let target = (f64::from(y) - vadj.page_size() * 0.1).clamp(0.0, max);
        imp.preview.syncing.set(true);
        vadj.set_value(target);
        imp.preview.syncing.set(false);
    }

    /// The web or mail link under `(x, y)` in the preview.
    fn link_at(&self, x: f64, y: f64) -> Option<String> {
        let view = &self.imp().preview_view;
        let (bx, by) =
            view.window_to_buffer_coords(gtk::TextWindowType::Widget, x as i32, y as i32);
        let offset = view.iter_at_location(bx, by)?.offset();
        self.imp()
            .preview
            .links
            .borrow()
            .iter()
            .find(|(start, end, _)| offset >= *start && offset < *end)
            .map(|(_, _, url)| url.as_str())
            .filter(|url| markdown::is_safe_link(url))
            .map(str::to_owned)
    }

    fn preview_visible(&self) -> bool {
        matches!(
            self.imp().view_mode.get(),
            ViewMode::Preview | ViewMode::Split
        )
    }

    fn render_preview(&self) {
        let imp = self.imp();
        // This render covers any change still waiting for one.
        if let Some(id) = imp.preview.render_timer.take() {
            id.remove();
        }
        let text = buffer_text(&*imp.edit_buffer);
        self.update_status(&text);
        let hadj = imp.preview_scroll.hadjustment();
        let base = imp
            .document
            .borrow()
            .file
            .as_ref()
            .and_then(|file| file.path())
            .and_then(|path| path.parent().map(Path::to_path_buf));
        let result = markdown::render_markdown(
            &imp.preview_view,
            &text,
            &hadj,
            base.as_deref(),
            &mut imp.preview.rendered.borrow_mut(),
        );
        imp.preview.links.replace(result.links);
        imp.preview.surfaces.replace(result.surfaces);
        self.watch_surface_selections(&result.added);
        for surface in &result.added {
            if let markdown::Surface::Code { view, .. } = surface {
                self.settings()
                    .bind("tab-width", view, "tab-width")
                    .get_only()
                    .build();
            }
        }
        // Match positions do not survive the rebuilt content.
        self.reset_preview_match();
        imp.preview.dirty.set(false);
    }

    fn flush_preview(&self) {
        if self.imp().preview.dirty.get() {
            self.render_preview();
        }
    }

    pub(super) fn schedule_render(&self) {
        let imp = self.imp();
        if let Some(id) = imp.preview.render_timer.take() {
            id.remove();
        }
        let id = glib::timeout_add_local_once(
            RENDER_DELAY,
            glib::clone!(
                #[weak(rename_to = document)]
                self,
                move || {
                    document.imp().preview.render_timer.take();
                    document.render_tick();
                }
            ),
        );
        imp.preview.render_timer.replace(Some(id));
    }

    /// The word and character count in the status bar.
    fn update_status(&self, text: &str) {
        let chars = text.chars().count();
        let words = text.split_whitespace().count();
        let status = format!(
            "{}, {}",
            ngettext("{} word", "{} words", saturating_u32(words)).replacen(
                "{}",
                &words.to_string(),
                1
            ),
            ngettext("{} character", "{} characters", saturating_u32(chars)).replacen(
                "{}",
                &chars.to_string(),
                1
            )
        );
        self.imp().status_label.set_label(&status);
    }

    /// Render what the preview missed while the tab was not selected.
    pub(super) fn render_missed(&self) {
        if self.imp().preview.dirty.get() && self.preview_visible() {
            self.render_tick();
        }
    }

    /// Render the preview, or leave it for later while it is out of sight: in the source,
    /// or in a tab that is not selected.
    pub(super) fn render_tick(&self) {
        let imp = self.imp();
        if self.preview_visible() && self.is_selected() {
            // Rendering replaces the whole buffer, so the preview is briefly much shorter
            // and its position is clamped. The editor must not follow that, and the preview
            // goes back to where the reader was, or in the split view to the editor's place.
            let vadj = imp.preview_scroll.vadjustment();
            let ratio = if imp.view_mode.get() == ViewMode::Split {
                adjustment_ratio(&imp.edit_scroll.vadjustment())
            } else {
                adjustment_ratio(&vadj)
            };
            imp.preview.syncing.set(true);
            self.render_preview();
            // The rebuilt preview is laid out before idle callbacks run.
            glib::idle_add_local_once(glib::clone!(
                #[weak(rename_to = document)]
                self,
                move || document.set_ratio(&vadj, ratio)
            ));
        } else {
            self.update_status(&buffer_text(&*imp.edit_buffer));
            imp.preview.dirty.set(true);
        }
    }

    pub(super) fn apply_view_mode(&self) {
        let imp = self.imp();
        let mode = imp.view_mode.get();
        if mode == ViewMode::Split {
            // Cleared by the scroll below, once a render here has been laid out.
            imp.preview.syncing.set(true);
        }
        if mode != ViewMode::Edit && self.is_selected() {
            self.flush_preview();
        }
        imp.edit_scroll.set_visible(mode != ViewMode::Preview);
        imp.preview_scroll.set_visible(mode != ViewMode::Edit);
        if mode == ViewMode::Split {
            imp.preview_scroll.add_css_class("split-preview");
            let preview = imp.preview_scroll.vadjustment();
            let ratio = adjustment_ratio(&imp.edit_scroll.vadjustment());
            glib::idle_add_local_once(glib::clone!(
                #[weak(rename_to = document)]
                self,
                move || document.set_ratio(&preview, ratio)
            ));
        } else {
            imp.preview_scroll.remove_css_class("split-preview");
        }
    }
}

fn adjustment_scroll_range(adjustment: &gtk::Adjustment) -> f64 {
    (adjustment.upper() - adjustment.lower() - adjustment.page_size()).max(0.0)
}

fn adjustment_ratio(adjustment: &gtk::Adjustment) -> f64 {
    let range = adjustment_scroll_range(adjustment);
    if range <= f64::EPSILON {
        return 0.0;
    }
    ((adjustment.value() - adjustment.lower()) / range).clamp(0.0, 1.0)
}

fn set_adjustment_ratio(adjustment: &gtk::Adjustment, ratio: f64) {
    let range = adjustment_scroll_range(adjustment);
    let value = adjustment.lower() + range * ratio.clamp(0.0, 1.0);
    adjustment.set_value(value.clamp(adjustment.lower(), adjustment.lower() + range));
}
