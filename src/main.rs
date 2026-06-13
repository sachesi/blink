use adw::Application;
use adw::prelude::*;
use gettextrs::{LocaleCategory, bindtextdomain, gettext, setlocale, textdomain};
use gtk::{
    FileDialog, Label, MenuButton, ScrolledWindow, TextBuffer, TextView, ToggleButton, gio, glib,
};
use sourceview5::prelude::*;
use sourceview5::{Buffer as SourceBuffer, LanguageManager, SearchContext, View as SourceView};
use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::rc::Rc;

mod markdown;

#[derive(Clone)]
struct DocumentTarget {
    window: adw::ApplicationWindow,
    edit_buffer: SourceBuffer,
    current_file: Rc<RefCell<Option<gio::File>>>,
    btn_split_toggle: ToggleButton,
    btn_mode_toggle: gtk::Button,
    edit_scroll: ScrolledWindow,
    preview_scroll: ScrolledWindow,
    // Tracks the intended non-split view (`true` = preview, `false` = edit) so
    // the split toggle can restore it without inspecting button icon names.
    preview_mode: Rc<Cell<bool>>,
}

fn present_error(window: &adw::ApplicationWindow, heading: String, body: String) {
    let alert = adw::AlertDialog::builder()
        .heading(heading)
        .body(body)
        .build();
    alert.add_response("ok", &gettext("OK"));
    alert.present(Some(window));
}

fn file_path(file: &gio::File) -> Result<PathBuf, String> {
    file.path()
        .ok_or_else(|| gettext("Only local files are supported"))
}

fn file_title(file: &gio::File) -> String {
    file.basename()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| gettext("Untitled Document"))
}

fn buffer_text(edit_buffer: &SourceBuffer) -> String {
    edit_buffer
        .text(&edit_buffer.start_iter(), &edit_buffer.end_iter(), false)
        .to_string()
}

fn show_preview_mode(
    btn_split_toggle: &ToggleButton,
    btn_mode_toggle: &gtk::Button,
    edit_scroll: &ScrolledWindow,
    preview_scroll: &ScrolledWindow,
) {
    btn_split_toggle.set_active(false);
    edit_scroll.set_visible(false);
    preview_scroll.set_visible(true);
    btn_mode_toggle.set_icon_name("document-edit-symbolic");
    btn_mode_toggle.set_tooltip_text(Some(&gettext("Edit Document")));
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

fn set_adjustment_ratio_guarded(
    adjustment: &gtk::Adjustment,
    ratio: f64,
    syncing: &Rc<Cell<bool>>,
) {
    syncing.set(true);
    set_adjustment_ratio(adjustment, ratio);
    syncing.set(false);
}

fn connect_scroll_sync(
    source: &gtk::Adjustment,
    target: &gtk::Adjustment,
    split_active: Rc<Cell<bool>>,
    syncing: Rc<Cell<bool>>,
) {
    let target = target.clone();
    source.connect_value_changed(move |source| {
        if !split_active.get() || syncing.get() {
            return;
        }

        let ratio = adjustment_ratio(source);
        set_adjustment_ratio_guarded(&target, ratio, &syncing);
    });
}

fn apply_loaded_file(file: gio::File, text: &str, target: &DocumentTarget) {
    target.edit_buffer.set_text(text);
    target.edit_buffer.set_modified(false);
    target.window.set_title(Some(&file_title(&file)));
    *target.current_file.borrow_mut() = Some(file);
    target.preview_mode.set(true);
    show_preview_mode(
        &target.btn_split_toggle,
        &target.btn_mode_toggle,
        &target.edit_scroll,
        &target.preview_scroll,
    );
}

async fn write_text_atomically(path: &Path, text: &str) -> std::io::Result<()> {
    let directory = path.parent().unwrap_or_else(|| Path::new("."));
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("document");
    let suffix = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    let tmp_path = directory.join(format!(
        ".{}.blink-tmp-{}-{}",
        file_name,
        std::process::id(),
        suffix
    ));

    let write_result = async {
        use tokio::io::AsyncWriteExt;
        let mut file = tokio::fs::File::create(&tmp_path).await?;
        file.write_all(text.as_bytes()).await?;
        file.sync_all().await
    }
    .await;
    if let Err(err) = write_result {
        let _ = tokio::fs::remove_file(&tmp_path).await;
        return Err(err);
    }

    if let Ok(metadata) = tokio::fs::metadata(path).await
        && let Err(err) = tokio::fs::set_permissions(&tmp_path, metadata.permissions()).await
    {
        let _ = tokio::fs::remove_file(&tmp_path).await;
        return Err(err);
    }

    match tokio::fs::rename(&tmp_path, path).await {
        Ok(()) => Ok(()),
        Err(err) => {
            let _ = tokio::fs::remove_file(&tmp_path).await;
            Err(err)
        }
    }
}

async fn save_document_to_file(
    file: gio::File,
    window: &adw::ApplicationWindow,
    edit_buffer: &SourceBuffer,
    current_file: &Rc<RefCell<Option<gio::File>>>,
) -> bool {
    let path = match file_path(&file) {
        Ok(path) => path,
        Err(err) => {
            present_error(window, gettext("Error Saving File"), err);
            return false;
        }
    };
    let text = buffer_text(edit_buffer);

    if let Err(err) = write_text_atomically(&path, &text).await {
        present_error(
            window,
            gettext("Error Saving File"),
            format!("{}: {}", gettext("Could not save the file"), err),
        );
        return false;
    }

    edit_buffer.set_modified(false);
    window.set_title(Some(&file_title(&file)));
    *current_file.borrow_mut() = Some(file);
    true
}

async fn save_document_as(
    window: &adw::ApplicationWindow,
    edit_buffer: &SourceBuffer,
    current_file: &Rc<RefCell<Option<gio::File>>>,
) -> bool {
    let dialog = FileDialog::new();
    match dialog.save_future(Some(window)).await {
        Ok(file) => save_document_to_file(file, window, edit_buffer, current_file).await,
        Err(_) => false,
    }
}

async fn save_current_document(
    window: &adw::ApplicationWindow,
    edit_buffer: &SourceBuffer,
    current_file: &Rc<RefCell<Option<gio::File>>>,
) -> bool {
    let file = current_file.borrow().clone();
    if let Some(file) = file {
        save_document_to_file(file, window, edit_buffer, current_file).await
    } else {
        save_document_as(window, edit_buffer, current_file).await
    }
}

async fn confirm_replace_modified_document(
    window: &adw::ApplicationWindow,
    edit_buffer: &SourceBuffer,
    current_file: &Rc<RefCell<Option<gio::File>>>,
) -> bool {
    if !edit_buffer.is_modified() {
        return true;
    }

    let alert = adw::AlertDialog::builder()
        .heading(gettext("Unsaved Changes"))
        .body(gettext(
            "Opening another file will discard unsaved changes.",
        ))
        .build();
    alert.add_response("cancel", &gettext("Cancel"));
    alert.add_response("save", &gettext("Save"));
    alert.add_response("discard", &gettext("Discard Changes"));
    alert.set_response_appearance("discard", adw::ResponseAppearance::Destructive);

    let response = alert.choose_future(Some(window)).await;
    match response.as_str() {
        "save" => save_current_document(window, edit_buffer, current_file).await,
        "discard" => true,
        _ => false,
    }
}

async fn load_document_file(file: gio::File, target: DocumentTarget, confirm_replace: bool) {
    if confirm_replace
        && !confirm_replace_modified_document(
            &target.window,
            &target.edit_buffer,
            &target.current_file,
        )
        .await
    {
        return;
    }

    let path = match file_path(&file) {
        Ok(path) => path,
        Err(err) => {
            present_error(&target.window, gettext("Error Opening File"), err);
            return;
        }
    };

    match tokio::fs::read_to_string(&path).await {
        Ok(text) => apply_loaded_file(file, &text, &target),
        Err(err) => present_error(
            &target.window,
            gettext("Error Opening File"),
            format!("{}: {}", gettext("Could not open the file"), err),
        ),
    }
}

fn has_search_text(search_context: &SearchContext) -> bool {
    search_context
        .settings()
        .search_text()
        .is_some_and(|text| !text.is_empty())
}

fn select_search_match(
    edit_buffer: &SourceBuffer,
    search_context: &SearchContext,
    forward: bool,
) -> bool {
    let offset = if let Some((start, end)) = edit_buffer.selection_bounds() {
        if forward {
            end.offset()
        } else {
            start.offset()
        }
    } else {
        edit_buffer.cursor_position()
    };

    select_search_match_from_offset(edit_buffer, search_context, offset, forward)
}

fn select_search_match_from_offset(
    edit_buffer: &SourceBuffer,
    search_context: &SearchContext,
    offset: i32,
    forward: bool,
) -> bool {
    if !has_search_text(search_context) {
        return false;
    }

    let iter = edit_buffer.iter_at_offset(offset);
    let found = if forward {
        search_context.forward(&iter)
    } else {
        search_context.backward(&iter)
    };

    if let Some((start, end, _)) = found {
        edit_buffer.select_range(&start, &end);
        true
    } else {
        false
    }
}

fn selection_is_search_match(edit_buffer: &SourceBuffer, search_context: &SearchContext) -> bool {
    edit_buffer
        .selection_bounds()
        .is_some_and(|(start, end)| search_context.occurrence_position(&start, &end) > 0)
}

fn replace_search_match(
    edit_buffer: &SourceBuffer,
    search_context: &SearchContext,
    replacement: &str,
) -> bool {
    if !has_search_text(search_context) {
        return false;
    }

    if selection_is_search_match(edit_buffer, search_context)
        && let Some((mut start, mut end)) = edit_buffer.selection_bounds()
        && search_context
            .replace(&mut start, &mut end, replacement)
            .is_ok()
    {
        return true;
    }

    if select_search_match(edit_buffer, search_context, true)
        && let Some((mut start, mut end)) = edit_buffer.selection_bounds()
        && search_context
            .replace(&mut start, &mut end, replacement)
            .is_ok()
    {
        return true;
    }

    false
}

fn replace_all_search_matches(
    edit_buffer: &SourceBuffer,
    search_context: &SearchContext,
    replacement: &str,
) -> usize {
    if !has_search_text(search_context) {
        return 0;
    }

    let settings = search_context.settings();
    let was_wrap_around = settings.wraps_around();
    settings.set_wrap_around(false);

    let mut matches = Vec::new();
    let mut iter = edit_buffer.start_iter();
    while let Some((start, end, _)) = search_context.forward(&iter) {
        if start.offset() == end.offset() {
            break;
        }
        matches.push((start.offset(), end.offset()));
        iter = end;
    }

    if matches.is_empty() {
        settings.set_wrap_around(was_wrap_around);
        return 0;
    }

    let mut replaced = 0;
    edit_buffer.begin_user_action();
    for (start_offset, end_offset) in matches.iter().rev() {
        let mut start = edit_buffer.iter_at_offset(*start_offset);
        let mut end = edit_buffer.iter_at_offset(*end_offset);
        if search_context
            .replace(&mut start, &mut end, replacement)
            .is_ok()
        {
            replaced += 1;
        }
    }
    edit_buffer.end_user_action();

    settings.set_wrap_around(was_wrap_around);
    replaced
}

fn clear_preview_search(buffer: &TextBuffer) {
    buffer.remove_tag_by_name("search-match", &buffer.start_iter(), &buffer.end_iter());
}

/// Searches the read-only preview buffer, highlighting and scrolling to the
/// match. Used in render mode, where the source-view search is unavailable.
/// `from_selection_start` keeps the current match in range while the query is
/// being typed; otherwise the search advances past the current selection.
fn search_preview_text(
    view: &TextView,
    query: &str,
    forward: bool,
    from_selection_start: bool,
) -> bool {
    let buffer = view.buffer();
    clear_preview_search(&buffer);
    if query.is_empty() {
        return false;
    }

    let flags = gtk::TextSearchFlags::CASE_INSENSITIVE | gtk::TextSearchFlags::VISIBLE_ONLY;
    let from = match buffer.selection_bounds() {
        Some((start, end)) => {
            if from_selection_start {
                start
            } else if forward {
                end
            } else {
                start
            }
        }
        None if forward => buffer.start_iter(),
        None => buffer.end_iter(),
    };

    let found = if forward {
        from.forward_search(query, flags, None)
    } else {
        from.backward_search(query, flags, None)
    }
    .or_else(|| {
        // Wrap around from the opposite edge of the buffer.
        let edge = if forward {
            buffer.start_iter()
        } else {
            buffer.end_iter()
        };
        if forward {
            edge.forward_search(query, flags, None)
        } else {
            edge.backward_search(query, flags, None)
        }
    });

    if let Some((start, end)) = found {
        buffer.select_range(&start, &end);
        buffer.apply_tag_by_name("search-match", &start, &end);
        let mut scroll_iter = start;
        view.scroll_to_iter(&mut scroll_iter, 0.1, false, 0.0, 0.0);
        true
    } else {
        false
    }
}

fn update_search_status(
    search_context: &SearchContext,
    edit_buffer: &SourceBuffer,
    status_label: &Label,
) {
    if !has_search_text(search_context) {
        status_label.set_label("");
        return;
    }

    let count = search_context.occurrences_count();
    if count < 0 {
        status_label.set_label(&gettext("Searching…"));
        return;
    }

    if count == 0 {
        status_label.set_label(&gettext("No matches"));
        return;
    }

    if let Some((start, end)) = edit_buffer.selection_bounds() {
        let position = search_context.occurrence_position(&start, &end);
        if position > 0 && count > 0 {
            let status = gettext("{} of {} matches")
                .replacen("{}", &position.to_string(), 1)
                .replacen("{}", &count.to_string(), 1);
            status_label.set_label(&status);
            return;
        }
    }

    if count > 0 {
        let status = gettext("{} matches").replacen("{}", &count.to_string(), 1);
        status_label.set_label(&status);
    }
}

fn set_replace_controls_sensitive(
    search_context: &SearchContext,
    replace_button: &gtk::Button,
    replace_all_button: &gtk::Button,
) {
    let enabled = has_search_text(search_context) && search_context.occurrences_count() > 0;
    replace_button.set_sensitive(enabled);
    replace_all_button.set_sensitive(enabled);
}

#[tokio::main]
async fn main() -> glib::ExitCode {
    // Initialize i18n
    setlocale(LocaleCategory::LcAll, "");
    let locale_dir = std::env::var("BLINK_LOCALE_DIR").unwrap_or_else(|_| {
        if std::path::Path::new("/usr/share/locale").exists() && !cfg!(debug_assertions) {
            "/usr/share/locale".to_string()
        } else {
            "locale".to_string()
        }
    });
    let _ = bindtextdomain("blink", locale_dir);
    let _ = textdomain("blink");

    let app = Application::builder()
        .application_id("com.github.sachesi.blink")
        .flags(gio::ApplicationFlags::HANDLES_OPEN)
        .build();

    app.connect_startup(|app| {
        app.set_accels_for_action("win.open", &["<Ctrl>o"]);
        app.set_accels_for_action("win.save", &["<Ctrl>s"]);
        app.set_accels_for_action("win.save-as", &["<Ctrl><Shift>s"]);
        app.set_accels_for_action("win.quit", &["<Ctrl>q"]);
        app.set_accels_for_action("win.format-bold", &["<Ctrl>b"]);
        app.set_accels_for_action("win.format-italic", &["<Ctrl>i"]);
        app.set_accels_for_action("win.format-link", &["<Ctrl>k"]);
        app.set_accels_for_action("win.find", &["<Ctrl>f"]);
        app.set_accels_for_action("win.find-next", &["<Ctrl>g"]);
        app.set_accels_for_action("win.find-previous", &["<Ctrl><Shift>g"]);
        app.set_accels_for_action("win.replace", &["<Ctrl>h"]);
        app.set_accels_for_action("win.replace-all", &["<Ctrl><Shift>h"]);
        app.set_accels_for_action("win.focus-mode", &["F11"]);
    });

    app.connect_activate(|app| {
        build_ui(app, None);
    });

    app.connect_open(|app, files, _hint| {
        if let Some(file) = files.first() {
            build_ui(app, Some(file.clone()));
        }
    });

    app.run()
}

fn build_ui(app: &Application, initial_file: Option<gio::File>) {
    let provider = gtk::CssProvider::new();
    provider.load_from_data(
        "
        textview.transparent-bg { background: transparent; }
        textview.editor-view {
            border-radius: 12px;
        }
        .split-preview {
            border-left: 1px solid alpha(currentColor, 0.06);
        }
        .drop-overlay {
            background: alpha(@theme_bg_color, 0.9);
        }
    ",
    );
    gtk::style_context_add_provider_for_display(
        &gtk::gdk::Display::default().expect("Could not connect to a display."),
        &provider,
        gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
    );

    sourceview5::init();
    let lang_manager = LanguageManager::default();
    let markdown_lang = lang_manager.language("markdown");

    let edit_buffer = SourceBuffer::builder().build();
    if let Some(lang) = markdown_lang {
        edit_buffer.set_language(Some(&lang));
    }

    let scheme_manager = sourceview5::StyleSchemeManager::default();
    let is_dark = adw::StyleManager::default().is_dark();
    if let Some(scheme) = scheme_manager.scheme(if is_dark { "Adwaita-dark" } else { "Adwaita" }) {
        edit_buffer.set_style_scheme(Some(&scheme));
    }

    let edit_buffer_style = edit_buffer.clone();
    adw::StyleManager::default().connect_dark_notify(move |manager| {
        let scheme_name = if manager.is_dark() {
            "Adwaita-dark"
        } else {
            "Adwaita"
        };
        if let Some(scheme) = sourceview5::StyleSchemeManager::default().scheme(scheme_name) {
            edit_buffer_style.set_style_scheme(Some(&scheme));
        }
    });

    let edit_view = SourceView::builder()
        .buffer(&edit_buffer)
        .wrap_mode(gtk::WrapMode::Word)
        .show_line_numbers(true)
        .monospace(true)
        .left_margin(32)
        .right_margin(32)
        .top_margin(32)
        .bottom_margin(32)
        .pixels_above_lines(4)
        .pixels_below_lines(4)
        .margin_top(12)
        .margin_bottom(12)
        .css_classes(["editor-view"])
        .build();
    let edit_clamp = adw::Clamp::builder()
        .child(&edit_view)
        .maximum_size(700)
        .build();
    let edit_scroll = ScrolledWindow::builder()
        .child(&edit_clamp)
        .hscrollbar_policy(gtk::PolicyType::Never)
        .hexpand(true)
        .build();

    markdown::setup_tags(edit_buffer.upcast_ref::<gtk::TextBuffer>());

    let preview_buffer = TextBuffer::new(None);
    markdown::setup_tags(&preview_buffer);
    preview_buffer.create_tag(
        Some("search-match"),
        &[("background", &"#f5c211"), ("foreground", &"#000000")],
    );
    let preview_view = TextView::builder()
        .buffer(&preview_buffer)
        .editable(false)
        .cursor_visible(false)
        .wrap_mode(gtk::WrapMode::Word)
        .left_margin(32)
        .right_margin(32)
        .top_margin(32)
        .bottom_margin(32)
        .pixels_above_lines(4)
        .pixels_below_lines(4)
        // The preview is read-only output. Taking it out of the focus chain
        // stops a click from scrolling its (stale, top-of-buffer) insertion
        // mark on-screen, which made the page jump on click.
        .can_focus(false)
        .focusable(false)
        .css_classes(["transparent-bg"])
        .build();

    let preview_clamp = adw::Clamp::builder()
        .child(&preview_view)
        .maximum_size(700)
        .build();
    let preview_scroll = ScrolledWindow::builder()
        .child(&preview_clamp)
        .hscrollbar_policy(gtk::PolicyType::Never)
        .hexpand(true)
        .build();

    let split_box = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .homogeneous(true)
        .build();

    edit_scroll.set_hexpand(true);
    preview_scroll.set_hexpand(true);

    split_box.append(&edit_scroll);
    split_box.append(&preview_scroll);

    let split_scroll_sync = Rc::new(Cell::new(false));
    let scroll_syncing = Rc::new(Cell::new(false));
    // Intended non-split view: `false` = edit, `true` = preview.
    let preview_mode = Rc::new(Cell::new(false));
    connect_scroll_sync(
        &edit_scroll.vadjustment(),
        &preview_scroll.vadjustment(),
        split_scroll_sync.clone(),
        scroll_syncing.clone(),
    );
    connect_scroll_sync(
        &preview_scroll.vadjustment(),
        &edit_scroll.vadjustment(),
        split_scroll_sync.clone(),
        scroll_syncing.clone(),
    );

    // Set visibility later after buttons are created

    let status_label = Label::builder()
        .margin_top(4)
        .margin_bottom(4)
        .margin_end(12)
        .halign(gtk::Align::End)
        .hexpand(true)
        .css_classes(["dim-label"])
        .build();

    let current_file: Rc<RefCell<Option<gio::File>>> = Rc::new(RefCell::new(None));

    // Renders the source buffer into the preview. Pure: does not touch scroll
    // position, so callers decide how to place the viewport afterwards.
    // Reads the buffer text on each call so callers need no arguments.
    let render_preview: Rc<dyn Fn()> = Rc::new({
        let preview_view = preview_view.clone();
        let edit_buffer = edit_buffer.clone();
        let preview_scroll = preview_scroll.clone();
        let current_file = current_file.clone();
        move || {
            let text =
                edit_buffer.text(&edit_buffer.start_iter(), &edit_buffer.end_iter(), false);
            let adj = preview_scroll.hadjustment();
            let image_base_dir = current_file
                .borrow()
                .as_ref()
                .and_then(|file| file.path())
                .and_then(|path| path.parent().map(Path::to_path_buf));
            markdown::render_markdown(&preview_view, text.as_str(), &adj, image_base_dir.as_deref());
        }
    });

    // Set when the buffer changes while the preview is hidden, so the stale
    // preview is re-rendered on demand the next time it becomes visible.
    let preview_dirty = Rc::new(Cell::new(false));

    // Re-renders the preview only if it went stale while hidden.
    let flush_preview: Rc<dyn Fn()> = Rc::new({
        let render_preview = render_preview.clone();
        let preview_dirty = preview_dirty.clone();
        move || {
            if preview_dirty.get() {
                render_preview();
                preview_dirty.set(false);
            }
        }
    });

    let status_label_clone = status_label.clone();
    let preview_scroll_clone_for_render = preview_scroll.clone();
    let render_preview_changed = render_preview.clone();
    let preview_dirty_changed = preview_dirty.clone();
    let scroll_syncing_render = scroll_syncing.clone();
    let render_source_id: Rc<RefCell<Option<glib::SourceId>>> = Rc::new(RefCell::new(None));

    edit_buffer.connect_changed(move |b| {
        if let Some(source_id) = render_source_id.borrow_mut().take() {
            source_id.remove();
        }

        let status_label_inner = status_label_clone.clone();
        let preview_scroll_inner = preview_scroll_clone_for_render.clone();
        let render_preview_inner = render_preview_changed.clone();
        let preview_dirty_inner = preview_dirty_changed.clone();
        let scroll_syncing_inner = scroll_syncing_render.clone();
        let b_clone = b.clone();
        let source_id_ref = render_source_id.clone();

        let id = glib::timeout_add_local(std::time::Duration::from_millis(300), move || {
            let text = b_clone.text(&b_clone.start_iter(), &b_clone.end_iter(), false);
            let chars = text.chars().count();
            let words = text.split_whitespace().count();
            let status_str = gettext("{} words, {} chars")
                .replacen("{}", &words.to_string(), 1)
                .replacen("{}", &chars.to_string(), 1);
            status_label_inner.set_label(&status_str);

            // Skip the full preview rebuild while it is hidden (the default
            // edit-only view); just mark it stale for the next reveal.
            if preview_scroll_inner.is_visible() {
                // Re-rendering clears the buffer and resets scroll; capture the
                // ratio first and restore it after layout so the visible
                // viewport stays put instead of snapping to the top.
                let preview_vadj = preview_scroll_inner.vadjustment();
                let preview_ratio = adjustment_ratio(&preview_vadj);
                render_preview_inner();
                let scroll_syncing_restore = scroll_syncing_inner.clone();
                glib::idle_add_local_once(move || {
                    set_adjustment_ratio_guarded(
                        &preview_vadj,
                        preview_ratio,
                        &scroll_syncing_restore,
                    );
                });
                preview_dirty_inner.set(false);
            } else {
                preview_dirty_inner.set(true);
            }

            *source_id_ref.borrow_mut() = None;
            glib::ControlFlow::Break
        });

        *render_source_id.borrow_mut() = Some(id);
    });

    let menu = gio::Menu::new();
    menu.append(Some(&gettext("Focus Mode")), Some("win.focus-mode"));
    menu.append(Some(&gettext("Export HTML…")), Some("win.export-html"));
    menu.append(Some(&gettext("Save As…")), Some("win.save-as"));
    menu.append(Some(&gettext("About Blink")), Some("win.about"));
    menu.append(Some(&gettext("Quit")), Some("win.quit"));

    let menu_button = MenuButton::builder()
        .menu_model(&menu)
        .icon_name("open-menu-symbolic")
        .build();

    let view_switcher = gtk::Box::builder().css_classes(["linked"]).build();

    // Edit/Render toggle button
    let btn_mode_toggle = gtk::Button::builder()
        .icon_name("document-edit-symbolic")
        .tooltip_text(gettext("Edit Document"))
        .build();

    // Split view toggle button
    let btn_split_toggle = ToggleButton::builder()
        .icon_name("view-split-left-right-symbolic")
        .tooltip_text(gettext("Split View"))
        .build();

    view_switcher.append(&btn_mode_toggle);
    view_switcher.append(&btn_split_toggle);

    let edit_scroll_clone = edit_scroll.clone();
    let preview_scroll_clone = preview_scroll.clone();
    let preview_mode_toggle = preview_mode.clone();
    let flush_preview_mode = flush_preview.clone();
    btn_mode_toggle.connect_clicked(move |btn| {
        let currently_preview =
            preview_scroll_clone.is_visible() && !edit_scroll_clone.is_visible();
        if currently_preview {
            // Switch to Edit
            preview_mode_toggle.set(false);
            edit_scroll_clone.set_visible(true);
            preview_scroll_clone.set_visible(false);
            btn.set_icon_name("view-reveal-symbolic");
            btn.set_tooltip_text(Some(&gettext("Preview Document")));
        } else {
            // Switch to Preview
            preview_mode_toggle.set(true);
            edit_scroll_clone.set_visible(false);
            preview_scroll_clone.set_visible(true);
            btn.set_icon_name("document-edit-symbolic");
            btn.set_tooltip_text(Some(&gettext("Edit Document")));
            flush_preview_mode();
        }
    });

    // Split connection moved below window creation

    // Default to edit only when launched empty
    edit_scroll.set_visible(true);
    preview_scroll.set_visible(false);
    btn_split_toggle.set_active(false);
    btn_mode_toggle.set_icon_name("view-reveal-symbolic");
    btn_mode_toggle.set_tooltip_text(Some(&gettext("Preview Document")));

    let header_bar = adw::HeaderBar::new();

    let open_btn = gtk::Button::builder()
        .icon_name("document-open-symbolic")
        .tooltip_text(gettext("Open Document"))
        .action_name("win.open")
        .build();

    let save_btn = gtk::Button::builder()
        .icon_name("document-save-symbolic")
        .tooltip_text(gettext("Save Document"))
        .action_name("win.save")
        .build();

    header_bar.pack_start(&open_btn);
    header_bar.pack_start(&save_btn);
    header_bar.pack_end(&menu_button);
    header_bar.pack_end(&view_switcher);

    let bottom_box = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .css_classes(["toolbar"])
        .build();
    let spacer = gtk::Box::builder().hexpand(true).build();
    bottom_box.append(&spacer);
    bottom_box.append(&status_label);

    let search_panel = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .css_classes(["toolbar"])
        .spacing(6)
        .margin_start(6)
        .margin_end(6)
        .margin_top(6)
        .margin_bottom(6)
        .visible(false)
        .build();
    let search_row = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(6)
        .build();
    let replace_row = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(6)
        .build();
    let search_entry = gtk::SearchEntry::builder()
        .hexpand(true)
        .width_chars(24)
        .placeholder_text(gettext("Search"))
        .build();
    let replace_entry = gtk::Entry::builder()
        .hexpand(true)
        .width_chars(24)
        .placeholder_text(gettext("Replace"))
        .build();
    let search_status_label = Label::builder()
        .halign(gtk::Align::Start)
        .ellipsize(gtk::pango::EllipsizeMode::End)
        .max_width_chars(24)
        .width_chars(16)
        .build();
    let btn_search_prev = gtk::Button::builder()
        .icon_name("go-up-symbolic")
        .tooltip_text(gettext("Previous Match"))
        .build();
    let btn_search_next = gtk::Button::builder()
        .icon_name("go-down-symbolic")
        .tooltip_text(gettext("Next Match"))
        .build();
    let btn_replace = gtk::Button::builder()
        .label(gettext("Replace"))
        .tooltip_text(gettext("Replace Match"))
        .build();
    let btn_replace_all = gtk::Button::builder()
        .label(gettext("All"))
        .tooltip_text(gettext("Replace All Matches"))
        .build();
    let btn_search_close = gtk::Button::builder()
        .icon_name("window-close-symbolic")
        .tooltip_text(gettext("Close Search"))
        .build();
    search_row.append(&search_entry);
    search_row.append(&search_status_label);
    search_row.append(&btn_search_prev);
    search_row.append(&btn_search_next);
    search_row.append(&btn_search_close);
    replace_row.append(&replace_entry);
    replace_row.append(&btn_replace);
    replace_row.append(&btn_replace_all);
    search_panel.append(&search_row);
    search_panel.append(&replace_row);

    let search_settings = sourceview5::SearchSettings::builder().build();
    search_settings.set_wrap_around(true);
    let search_context = sourceview5::SearchContext::builder()
        .buffer(&edit_buffer)
        .settings(&search_settings)
        .highlight(false)
        .build();

    set_replace_controls_sensitive(&search_context, &btn_replace, &btn_replace_all);

    // True while the search panel targets the read-only preview (render mode):
    // search only, no replace.
    let searching_preview = Rc::new(Cell::new(false));

    let show_search_panel: Rc<dyn Fn()> = Rc::new({
        let search_panel = search_panel.clone();
        let search_context = search_context.clone();
        let btn_split_toggle = btn_split_toggle.clone();
        let edit_scroll = edit_scroll.clone();
        let preview_scroll = preview_scroll.clone();
        let replace_row = replace_row.clone();
        let searching_preview = searching_preview.clone();
        move || {
            search_panel.set_visible(true);

            // Render mode = preview visible and not split: search the preview
            // only, hide replace. Edit/split: source-view search with replace
            // and match highlighting in the editor. The view is left as-is.
            let preview_search = !btn_split_toggle.is_active()
                && preview_scroll.is_visible()
                && !edit_scroll.is_visible();
            searching_preview.set(preview_search);
            replace_row.set_visible(!preview_search);
            search_context.set_highlight(!preview_search);
        }
    });

    let close_search_panel: Rc<dyn Fn()> = Rc::new({
        let search_panel = search_panel.clone();
        let search_context = search_context.clone();
        let search_status_label = search_status_label.clone();
        let preview_view = preview_view.clone();
        let searching_preview = searching_preview.clone();
        move || {
            search_panel.set_visible(false);
            search_context.set_highlight(false);
            clear_preview_search(&preview_view.buffer());
            searching_preview.set(false);
            search_status_label.set_label("");
        }
    });

    // Switching view mode dismisses any open search so its state never lingers
    // across modes (e.g. preview highlights or a hidden replace row).
    let close_on_mode_toggle = close_search_panel.clone();
    btn_mode_toggle.connect_clicked(move |_| close_on_mode_toggle());
    let close_on_split_toggle = close_search_panel.clone();
    btn_split_toggle.connect_toggled(move |_| close_on_split_toggle());

    let refresh_search_state: Rc<dyn Fn()> = Rc::new({
        let search_context = search_context.clone();
        let edit_buffer = edit_buffer.clone();
        let search_status_label = search_status_label.clone();
        let btn_replace = btn_replace.clone();
        let btn_replace_all = btn_replace_all.clone();
        move || {
            update_search_status(&search_context, &edit_buffer, &search_status_label);
            set_replace_controls_sensitive(&search_context, &btn_replace, &btn_replace_all);
        }
    });

    let refresh_search_state_notify = refresh_search_state.clone();
    search_context.connect_occurrences_count_notify(move |_| {
        refresh_search_state_notify();
    });

    // One search step against the preview buffer, updating the status label.
    let preview_search_step: Rc<dyn Fn(bool, bool)> = Rc::new({
        let preview_view = preview_view.clone();
        let search_entry = search_entry.clone();
        let search_status_label = search_status_label.clone();
        move |forward, from_selection_start| {
            let query = search_entry.text();
            let found =
                search_preview_text(&preview_view, query.as_str(), forward, from_selection_start);
            if query.is_empty() || found {
                search_status_label.set_label("");
            } else {
                search_status_label.set_label(&gettext("No matches"));
            }
        }
    });

    let edit_buffer_search = edit_buffer.clone();
    let search_context_search = search_context.clone();
    let show_search_panel_search = show_search_panel.clone();
    let refresh_search = refresh_search_state.clone();
    let searching_preview_search = searching_preview.clone();
    let preview_search_changed = preview_search_step.clone();
    search_entry.connect_search_changed(move |entry| {
        show_search_panel_search();
        if searching_preview_search.get() {
            preview_search_changed(true, true);
            return;
        }
        let search_offset = edit_buffer_search
            .selection_bounds()
            .map(|(start, _)| start.offset())
            .unwrap_or_else(|| edit_buffer_search.cursor_position());
        search_settings.set_search_text(Some(entry.text().as_str()));
        select_search_match_from_offset(
            &edit_buffer_search,
            &search_context_search,
            search_offset,
            true,
        );
        refresh_search();
    });

    let edit_buffer_search_activate = edit_buffer.clone();
    let search_context_search_activate = search_context.clone();
    let show_search_panel_search_activate = show_search_panel.clone();
    let refresh_search_activate = refresh_search_state.clone();
    let searching_preview_activate = searching_preview.clone();
    let preview_search_activate = preview_search_step.clone();
    search_entry.connect_activate(move |_| {
        show_search_panel_search_activate();
        if searching_preview_activate.get() {
            preview_search_activate(true, false);
            return;
        }
        select_search_match(
            &edit_buffer_search_activate,
            &search_context_search_activate,
            true,
        );
        refresh_search_activate();
    });

    let edit_buffer_next = edit_buffer.clone();
    let search_context_next = search_context.clone();
    let show_search_panel_next = show_search_panel.clone();
    let refresh_search_next = refresh_search_state.clone();
    let searching_preview_next = searching_preview.clone();
    let preview_search_next = preview_search_step.clone();
    btn_search_next.connect_clicked(move |_| {
        show_search_panel_next();
        if searching_preview_next.get() {
            preview_search_next(true, false);
            return;
        }
        select_search_match(&edit_buffer_next, &search_context_next, true);
        refresh_search_next();
    });

    let edit_buffer_prev = edit_buffer.clone();
    let search_context_prev = search_context.clone();
    let show_search_panel_prev = show_search_panel.clone();
    let refresh_search_prev = refresh_search_state.clone();
    let searching_preview_prev = searching_preview.clone();
    let preview_search_prev = preview_search_step.clone();
    btn_search_prev.connect_clicked(move |_| {
        show_search_panel_prev();
        if searching_preview_prev.get() {
            preview_search_prev(false, false);
            return;
        }
        select_search_match(&edit_buffer_prev, &search_context_prev, false);
        refresh_search_prev();
    });

    let edit_buffer_replace = edit_buffer.clone();
    let search_context_replace = search_context.clone();
    let replace_entry_replace = replace_entry.clone();
    let show_search_panel_replace = show_search_panel.clone();
    let refresh_search_replace = refresh_search_state.clone();
    btn_replace.connect_clicked(move |_| {
        show_search_panel_replace();
        if replace_search_match(
            &edit_buffer_replace,
            &search_context_replace,
            replace_entry_replace.text().as_str(),
        ) {
            select_search_match(&edit_buffer_replace, &search_context_replace, true);
        }
        refresh_search_replace();
    });

    let edit_buffer_replace_activate = edit_buffer.clone();
    let search_context_replace_activate = search_context.clone();
    let replace_entry_activate = replace_entry.clone();
    let show_search_panel_replace_activate = show_search_panel.clone();
    let refresh_search_replace_activate = refresh_search_state.clone();
    replace_entry.connect_activate(move |_| {
        show_search_panel_replace_activate();
        if replace_search_match(
            &edit_buffer_replace_activate,
            &search_context_replace_activate,
            replace_entry_activate.text().as_str(),
        ) {
            select_search_match(
                &edit_buffer_replace_activate,
                &search_context_replace_activate,
                true,
            );
        }
        refresh_search_replace_activate();
    });

    let edit_buffer_replace_all = edit_buffer.clone();
    let search_context_replace_all = search_context.clone();
    let replace_entry_replace_all = replace_entry.clone();
    let search_status_label_replace_all = search_status_label.clone();
    let show_search_panel_replace_all = show_search_panel.clone();
    let refresh_search_replace_all = refresh_search_state.clone();
    btn_replace_all.connect_clicked(move |_| {
        show_search_panel_replace_all();
        let replaced = replace_all_search_matches(
            &edit_buffer_replace_all,
            &search_context_replace_all,
            replace_entry_replace_all.text().as_str(),
        );
        refresh_search_replace_all();
        let status = gettext("{} matches replaced").replacen("{}", &replaced.to_string(), 1);
        search_status_label_replace_all.set_label(&status);
    });

    let close_search_panel_button = close_search_panel.clone();
    btn_search_close.connect_clicked(move |_| {
        close_search_panel_button();
    });

    let close_search_panel_entry = close_search_panel.clone();
    search_entry.connect_stop_search(move |_| {
        close_search_panel_entry();
    });

    let toolbar_view = adw::ToolbarView::builder().content(&split_box).build();
    toolbar_view.add_top_bar(&header_bar);
    toolbar_view.add_top_bar(&search_panel);
    toolbar_view.add_bottom_bar(&bottom_box);

    let drop_overlay = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .halign(gtk::Align::Fill)
        .valign(gtk::Align::Fill)
        .css_classes(["drop-overlay"])
        .visible(false)
        .build();

    let drop_icon = gtk::Image::builder()
        .icon_name("document-open-symbolic")
        .pixel_size(64)
        .valign(gtk::Align::End)
        .halign(gtk::Align::Center)
        .vexpand(true)
        .margin_bottom(12)
        .build();
    let drop_label = gtk::Label::builder()
        .label(gettext("Drop Markdown file to open"))
        .css_classes(["title-1"])
        .valign(gtk::Align::Start)
        .halign(gtk::Align::Center)
        .vexpand(true)
        .margin_top(12)
        .build();

    let drop_content = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .valign(gtk::Align::Center)
        .halign(gtk::Align::Center)
        .build();
    drop_content.append(&drop_icon);
    drop_content.append(&drop_label);
    drop_overlay.append(&drop_content);

    let overlay = gtk::Overlay::builder().child(&toolbar_view).build();
    overlay.add_overlay(&drop_overlay);

    let window = adw::ApplicationWindow::builder()
        .application(app)
        .title(gettext("Untitled Document"))
        .default_width(700)
        .default_height(900)
        .content(&overlay)
        .build();

    let edit_scroll_clone2 = edit_scroll.clone();
    let preview_scroll_clone2 = preview_scroll.clone();
    let btn_mode_toggle_clone = btn_mode_toggle.clone();
    let split_scroll_sync_toggle = split_scroll_sync.clone();
    let scroll_syncing_toggle = scroll_syncing.clone();
    let preview_mode_split = preview_mode.clone();
    let flush_preview_split = flush_preview.clone();
    btn_split_toggle.connect_toggled(move |btn| {
        let is_split = btn.is_active();
        split_scroll_sync_toggle.set(is_split);
        if is_split {
            preview_scroll_clone2.add_css_class("split-preview");
            edit_scroll_clone2.set_visible(true);
            preview_scroll_clone2.set_visible(true);
            btn_mode_toggle_clone.set_sensitive(false);
            flush_preview_split();
            // Align the preview to the editor after a possible re-render; defer
            // so the freshly rebuilt preview has a valid scroll range.
            let preview_vadj = preview_scroll_clone2.vadjustment();
            let edit_ratio = adjustment_ratio(&edit_scroll_clone2.vadjustment());
            let scroll_syncing_align = scroll_syncing_toggle.clone();
            glib::idle_add_local_once(move || {
                set_adjustment_ratio_guarded(&preview_vadj, edit_ratio, &scroll_syncing_align);
            });
        } else {
            preview_scroll_clone2.remove_css_class("split-preview");
            btn_mode_toggle_clone.set_sensitive(true);
            let wants_preview = preview_mode_split.get();
            edit_scroll_clone2.set_visible(!wants_preview);
            preview_scroll_clone2.set_visible(wants_preview);
            if wants_preview {
                flush_preview_split();
            }
        }
    });

    let document_target = DocumentTarget {
        window: window.clone(),
        edit_buffer: edit_buffer.clone(),
        current_file: current_file.clone(),
        btn_split_toggle: btn_split_toggle.clone(),
        btn_mode_toggle: btn_mode_toggle.clone(),
        edit_scroll: edit_scroll.clone(),
        preview_scroll: preview_scroll.clone(),
        preview_mode: preview_mode.clone(),
    };

    let drop_target = gtk::DropTarget::new(gio::File::static_type(), gtk::gdk::DragAction::COPY);
    let document_target_drop = document_target.clone();

    let drop_overlay_enter = drop_overlay.clone();
    drop_target.connect_enter(move |_, _, _| {
        drop_overlay_enter.set_visible(true);
        gtk::gdk::DragAction::COPY
    });

    let drop_overlay_leave = drop_overlay.clone();
    drop_target.connect_leave(move |_| {
        drop_overlay_leave.set_visible(false);
    });

    let drop_overlay_drop = drop_overlay.clone();
    drop_target.connect_drop(move |_, value, _, _| {
        drop_overlay_drop.set_visible(false);
        if let Ok(file) = value.get::<gio::File>() {
            let target = document_target_drop.clone();

            glib::spawn_future_local(async move {
                load_document_file(file, target, true).await;
            });
            return true;
        }
        false
    });
    window.add_controller(drop_target);

    // Actions
    let action_open = gio::SimpleAction::new("open", None);
    let document_target_open = document_target.clone();
    action_open.connect_activate(move |_, _| {
        let dialog = FileDialog::new();
        let target = document_target_open.clone();
        glib::spawn_future_local(async move {
            let parent = target.window.clone();
            if let Ok(file) = dialog.open_future(Some(&parent)).await {
                load_document_file(file, target, true).await;
            }
        });
    });
    window.add_action(&action_open);

    let action_save_as = gio::SimpleAction::new("save-as", None);
    let window_clone = window.clone();
    let edit_buffer_clone = edit_buffer.clone();
    let current_file_clone = current_file.clone();
    action_save_as.connect_activate(move |_, _| {
        let dialog = FileDialog::new();
        let window_clone = window_clone.clone();
        let edit_buffer_clone = edit_buffer_clone.clone();
        let current_file_clone = current_file_clone.clone();
        glib::spawn_future_local(async move {
            if let Ok(file) = dialog.save_future(Some(&window_clone)).await {
                save_document_to_file(file, &window_clone, &edit_buffer_clone, &current_file_clone)
                    .await;
            }
        });
    });
    window.add_action(&action_save_as);

    let action_save = gio::SimpleAction::new("save", None);
    let edit_buffer_clone = edit_buffer.clone();
    let current_file_clone = current_file.clone();
    let window_clone_save = window.clone();
    action_save.connect_activate(move |_, _| {
        let window_clone_inner = window_clone_save.clone();
        let edit_buffer_inner = edit_buffer_clone.clone();
        let current_file_inner = current_file_clone.clone();
        glib::spawn_future_local(async move {
            if current_file_inner.borrow().is_some() {
                save_current_document(&window_clone_inner, &edit_buffer_inner, &current_file_inner)
                    .await;
            } else {
                let _ = WidgetExt::activate_action(&window_clone_inner, "win.save-as", None);
            }
        });
    });
    window.add_action(&action_save);

    let current_file_autosave = current_file.clone();
    let edit_buffer_autosave = edit_buffer.clone();
    let status_label_autosave = status_label.clone();
    glib::timeout_add_seconds_local(10, move || {
        let file = current_file_autosave.borrow().clone();
        if edit_buffer_autosave.is_modified()
            && let Some(file) = file
            && let Ok(path) = file_path(&file)
        {
            let text = buffer_text(&edit_buffer_autosave);
            let edit_buf_clone = edit_buffer_autosave.clone();
            let status_label = status_label_autosave.clone();
            glib::spawn_future_local(async move {
                if write_text_atomically(&path, &text).await.is_ok() {
                    edit_buf_clone.set_modified(false);
                } else {
                    status_label.set_label(&gettext("Autosave failed"));
                }
            });
        }
        glib::ControlFlow::Continue
    });

    let window_clone_quit = window.clone();
    let edit_buffer_clone_quit = edit_buffer.clone();
    let action_quit = gio::SimpleAction::new("quit", None);
    action_quit.connect_activate(move |_, _| {
        if edit_buffer_clone_quit.is_modified() {
            let alert = adw::AlertDialog::builder()
                .heading(gettext("Unsaved Changes"))
                .body(gettext(
                    "You have unsaved changes. Do you want to close without saving?",
                ))
                .build();
            alert.add_response("cancel", &gettext("Cancel"));
            alert.add_response("close", &gettext("Close Without Saving"));
            alert.set_response_appearance("close", adw::ResponseAppearance::Destructive);

            let window_close_inner = window_clone_quit.clone();
            alert.connect_response(None, move |_, response| {
                if response == "close" {
                    window_close_inner.destroy();
                }
            });
            alert.present(Some(&window_clone_quit));
        } else {
            window_clone_quit.destroy();
        }
    });
    window.add_action(&action_quit);

    // Formatting Actions
    let action_bold = gio::SimpleAction::new("format-bold", None);
    let edit_buf_bold = edit_buffer.clone();
    action_bold.connect_activate(move |_, _| {
        if let Some((mut start, mut end)) = edit_buf_bold.selection_bounds() {
            let text = edit_buf_bold.text(&start, &end, false);
            edit_buf_bold.delete(&mut start, &mut end);
            edit_buf_bold.insert(&mut start, &format!("**{}**", text.as_str()));
        }
    });
    window.add_action(&action_bold);

    let action_italic = gio::SimpleAction::new("format-italic", None);
    let edit_buf_italic = edit_buffer.clone();
    action_italic.connect_activate(move |_, _| {
        if let Some((mut start, mut end)) = edit_buf_italic.selection_bounds() {
            let text = edit_buf_italic.text(&start, &end, false);
            edit_buf_italic.delete(&mut start, &mut end);
            edit_buf_italic.insert(&mut start, &format!("*{}*", text.as_str()));
        }
    });
    window.add_action(&action_italic);

    let action_link = gio::SimpleAction::new("format-link", None);
    let edit_buf_link = edit_buffer.clone();
    action_link.connect_activate(move |_, _| {
        if let Some((mut start, mut end)) = edit_buf_link.selection_bounds() {
            let text = edit_buf_link.text(&start, &end, false);
            edit_buf_link.delete(&mut start, &mut end);
            edit_buf_link.insert(&mut start, &format!("[{}](url)", text.as_str()));
        }
    });
    window.add_action(&action_link);

    let search_entry_find = search_entry.clone();
    let show_search_panel_find = show_search_panel.clone();
    let refresh_search_find = refresh_search_state.clone();
    let searching_preview_find = searching_preview.clone();
    let preview_search_find = preview_search_step.clone();
    let action_find = gio::SimpleAction::new("find", None);
    action_find.connect_activate(move |_, _| {
        show_search_panel_find();
        search_entry_find.grab_focus();
        if searching_preview_find.get() {
            preview_search_find(true, true);
        } else {
            refresh_search_find();
        }
    });
    window.add_action(&action_find);

    let edit_buffer_find_next = edit_buffer.clone();
    let search_context_find_next = search_context.clone();
    let show_search_panel_find_next = show_search_panel.clone();
    let refresh_search_find_next = refresh_search_state.clone();
    let searching_preview_find_next = searching_preview.clone();
    let preview_search_find_next = preview_search_step.clone();
    let action_find_next = gio::SimpleAction::new("find-next", None);
    action_find_next.connect_activate(move |_, _| {
        show_search_panel_find_next();
        if searching_preview_find_next.get() {
            preview_search_find_next(true, false);
            return;
        }
        select_search_match(&edit_buffer_find_next, &search_context_find_next, true);
        refresh_search_find_next();
    });
    window.add_action(&action_find_next);

    let edit_buffer_find_previous = edit_buffer.clone();
    let search_context_find_previous = search_context.clone();
    let show_search_panel_find_previous = show_search_panel.clone();
    let refresh_search_find_previous = refresh_search_state.clone();
    let searching_preview_find_prev = searching_preview.clone();
    let preview_search_find_prev = preview_search_step.clone();
    let action_find_previous = gio::SimpleAction::new("find-previous", None);
    action_find_previous.connect_activate(move |_, _| {
        show_search_panel_find_previous();
        if searching_preview_find_prev.get() {
            preview_search_find_prev(false, false);
            return;
        }
        select_search_match(
            &edit_buffer_find_previous,
            &search_context_find_previous,
            false,
        );
        refresh_search_find_previous();
    });
    window.add_action(&action_find_previous);

    let replace_entry_action = replace_entry.clone();
    let search_entry_replace_action = search_entry.clone();
    let show_search_panel_replace_action = show_search_panel.clone();
    let refresh_search_replace_action = refresh_search_state.clone();
    let searching_preview_replace = searching_preview.clone();
    let action_replace = gio::SimpleAction::new("replace", None);
    action_replace.connect_activate(move |_, _| {
        show_search_panel_replace_action();
        // Replace is unavailable in render mode; fall back to plain search.
        if searching_preview_replace.get() {
            search_entry_replace_action.grab_focus();
            return;
        }
        replace_entry_action.grab_focus();
        refresh_search_replace_action();
    });
    window.add_action(&action_replace);

    let replace_entry_replace_all_action = replace_entry.clone();
    let search_entry_replace_all_action = search_entry.clone();
    let show_search_panel_replace_all_action = show_search_panel.clone();
    let refresh_search_replace_all_action = refresh_search_state.clone();
    let searching_preview_replace_all = searching_preview.clone();
    let action_replace_all = gio::SimpleAction::new("replace-all", None);
    action_replace_all.connect_activate(move |_, _| {
        show_search_panel_replace_all_action();
        if searching_preview_replace_all.get() {
            search_entry_replace_all_action.grab_focus();
            return;
        }
        replace_entry_replace_all_action.grab_focus();
        refresh_search_replace_all_action();
    });
    window.add_action(&action_replace_all);

    let action_export_html = gio::SimpleAction::new("export-html", None);
    let window_clone_export = window.clone();
    let edit_buffer_export = edit_buffer.clone();
    action_export_html.connect_activate(move |_, _| {
        let dialog = FileDialog::new();
        let window_clone = window_clone_export.clone();
        let edit_buffer_clone = edit_buffer_export.clone();
        glib::spawn_future_local(async move {
            if let Ok(file) = dialog.save_future(Some(&window_clone)).await
                && let Some(path) = file.path()
            {
                let text = edit_buffer_clone.text(
                    &edit_buffer_clone.start_iter(),
                    &edit_buffer_clone.end_iter(),
                    false,
                );

                let mut options = pulldown_cmark::Options::empty();
                options.insert(pulldown_cmark::Options::ENABLE_TABLES);
                options.insert(pulldown_cmark::Options::ENABLE_STRIKETHROUGH);
                options.insert(pulldown_cmark::Options::ENABLE_TASKLISTS);
                let parser = pulldown_cmark::Parser::new_ext(text.as_str(), options);

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
                let html_doc = format!("<!DOCTYPE html>\n<html>\n<head>\n<meta charset=\"utf-8\">\n<title>Export</title>\n<style>\n{}\n</style>\n</head>\n<body>\n{}\n</body>\n</html>", css, html_output);

                if let Err(e) = write_text_atomically(&path, &html_doc).await {
                    present_error(
                        &window_clone,
                        gettext("Error Exporting HTML"),
                        format!("{}: {}", gettext("Could not export the file"), e),
                    );
                }
            }
        });
    });
    window.add_action(&action_export_html);

    let action_about = gio::SimpleAction::new("about", None);
    let window_clone_about = window.clone();
    action_about.connect_activate(move |_, _| {
        let about = adw::AboutDialog::builder()
            .application_name("Blink")
            .application_icon("com.github.sachesi.blink")
            .developer_name("sachesi")
            .version("0.1.4")
            .comments(gettext("A fast and minimal Markdown editor"))
            .website("https://github.com/sachesi/blink")
            .issue_url("https://github.com/sachesi/blink/issues")
            .license_type(gtk::License::Gpl30)
            .build();

        // Provide standard End User License Agreement / Disclaimer
        about.add_legal_section(
            &gettext("End User Agreement"),
            None,
            gtk::License::Custom,
            Some(&gettext("This software is provided 'as is', without warranty of any kind, express or implied. By using this software, you agree to these terms and the terms of the GNU General Public License version 3."))
        );

        about.present(Some(&window_clone_about));
    });
    window.add_action(&action_about);

    let action_focus = gio::SimpleAction::new_stateful("focus-mode", None, &false.to_variant());
    let window_clone_focus = window.clone();
    let toolbar_view_clone = toolbar_view.clone();
    action_focus.connect_change_state(move |action, state| {
        if let Some(st) = state {
            let is_focus = st.get::<bool>().unwrap_or(false);
            action.set_state(st);
            toolbar_view_clone.set_reveal_top_bars(!is_focus);
            toolbar_view_clone.set_reveal_bottom_bars(!is_focus);
            if is_focus {
                window_clone_focus.fullscreen();
            } else {
                window_clone_focus.unfullscreen();
            }
        }
    });
    window.add_action(&action_focus);

    window.connect_close_request(move |win| {
        let _ = WidgetExt::activate_action(win, "win.quit", None);
        glib::Propagation::Stop
    });

    if let Some(file) = initial_file {
        let target = document_target.clone();

        glib::spawn_future_local(async move {
            load_document_file(file, target, false).await;
        });
    }

    window.present();
}

#[cfg(test)]
mod tests {
    use super::write_text_atomically;
    use std::fs;

    #[tokio::test]
    async fn atomic_write_replaces_existing_file() {
        let path = std::env::temp_dir().join(format!("blink-save-test-{}.md", std::process::id()));
        fs::write(&path, "old").unwrap();

        write_text_atomically(&path, "new").await.unwrap();

        assert_eq!(fs::read_to_string(&path).unwrap(), "new");
        let _ = fs::remove_file(path);
    }
}
