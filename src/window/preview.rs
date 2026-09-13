//! The rendered preview: debounced rendering, the word count, links, and the scroll
//! position the preview and the editor share in the split view.

use adw::prelude::*;
use adw::subclass::prelude::*;
use gettextrs::ngettext;
use gtk::{gio, glib};
use std::cell::{Cell, RefCell};
use std::path::Path;
use std::time::Duration;

use super::{BlinkWindow, ViewMode, buffer_text, saturating_u32};
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
}

impl BlinkWindow {
    pub(super) fn setup_preview(&self) {
        let imp = self.imp();
        let buffer = imp.preview_view.buffer();
        markdown::setup_tags(&buffer);
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
            #[weak(rename_to = win)]
            self,
            move |_, _, x, y| {
                if let Some(url) = win.link_at(x, y) {
                    gtk::UriLauncher::new(&url).launch(Some(&win), gio::Cancellable::NONE, |_| {});
                }
            }
        ));
        imp.preview_view.add_controller(click);

        let motion = gtk::EventControllerMotion::new();
        motion.connect_motion(glib::clone!(
            #[weak(rename_to = win)]
            self,
            move |_, x, y| {
                let cursor = if win.link_at(x, y).is_some() {
                    "pointer"
                } else {
                    "text"
                };
                win.imp().preview_view.set_cursor_from_name(Some(cursor));
            }
        ));
        imp.preview_view.add_controller(motion);
    }

    /// In the split view, scrolling `source` scrolls `target` to the same proportion.
    fn sync_scroll(&self, source: &gtk::Adjustment, target: &gtk::Adjustment) {
        source.connect_value_changed(glib::clone!(
            #[weak(rename_to = win)]
            self,
            #[weak]
            target,
            move |source| {
                let imp = win.imp();
                if imp.view_mode.get() != ViewMode::Split || imp.preview.syncing.get() {
                    return;
                }
                win.set_ratio(&target, adjustment_ratio(source));
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
        let text = buffer_text(&*imp.edit_buffer);
        let hadj = imp.preview_scroll.hadjustment();
        let base = imp
            .document
            .borrow()
            .file
            .as_ref()
            .and_then(|file| file.path())
            .and_then(|path| path.parent().map(Path::to_path_buf));
        let result = markdown::render_markdown(&imp.preview_view, &text, &hadj, base.as_deref());
        imp.preview.links.replace(result.links);
        imp.preview.surfaces.replace(result.surfaces);
        // Match positions do not survive the rebuilt content.
        self.reset_preview_match();
        imp.preview.dirty.set(false);
    }

    /// Render now if the preview is on screen, otherwise when it next is.
    pub(super) fn invalidate_preview(&self) {
        if self.preview_visible() {
            self.render_preview();
        } else {
            self.imp().preview.dirty.set(true);
        }
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
                #[weak(rename_to = win)]
                self,
                move || {
                    win.imp().preview.render_timer.take();
                    win.render_tick();
                }
            ),
        );
        imp.preview.render_timer.replace(Some(id));
    }

    fn render_tick(&self) {
        let imp = self.imp();
        let text = buffer_text(&*imp.edit_buffer);
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
        imp.status_label.set_label(&status);

        if self.preview_visible() {
            // Rendering replaces the whole buffer; keep the reader where they were.
            let vadj = imp.preview_scroll.vadjustment();
            let ratio = adjustment_ratio(&vadj);
            self.render_preview();
            glib::idle_add_local_once(glib::clone!(
                #[weak(rename_to = win)]
                self,
                move || win.set_ratio(&vadj, ratio)
            ));
        } else {
            imp.preview.dirty.set(true);
        }
    }

    pub(super) fn apply_view_mode(&self) {
        let imp = self.imp();
        let mode = imp.view_mode.get();
        if mode != ViewMode::Edit {
            self.flush_preview();
        }
        imp.edit_scroll.set_visible(mode != ViewMode::Preview);
        imp.preview_scroll.set_visible(mode != ViewMode::Edit);
        if mode == ViewMode::Split {
            imp.preview_scroll.add_css_class("split-preview");
            let preview = imp.preview_scroll.vadjustment();
            let ratio = adjustment_ratio(&imp.edit_scroll.vadjustment());
            glib::idle_add_local_once(glib::clone!(
                #[weak(rename_to = win)]
                self,
                move || win.set_ratio(&preview, ratio)
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
