use adw::Application;
use adw::prelude::*;
use gtk::{
    gio, glib, FileDialog, Label, MenuButton, ScrolledWindow, TextBuffer, TextView, ToggleButton,
};
use gettextrs::{gettext, setlocale, textdomain, bindtextdomain, LocaleCategory};
use sourceview5::prelude::*;
use sourceview5::{Buffer as SourceBuffer, View as SourceView, LanguageManager};
use std::cell::RefCell;
use std::rc::Rc;

mod markdown;

#[tokio::main]
async fn main() -> glib::ExitCode {
    // Initialize i18n
    setlocale(LocaleCategory::LcAll, "");
    let _ = bindtextdomain("blink", "locale");
    let _ = textdomain("blink");

    let app = Application::builder()
        .application_id("com.github.sachesi.blink")
        .flags(gio::ApplicationFlags::HANDLES_OPEN)
        .build();

    app.connect_startup(|app| {
        app.set_accels_for_action("app.open", &["<Ctrl>o"]);
        app.set_accels_for_action("app.save", &["<Ctrl>s"]);
        app.set_accels_for_action("app.save-as", &["<Ctrl><Shift>s"]);
        app.set_accels_for_action("app.quit", &["<Ctrl>q"]);
        app.set_accels_for_action("app.format-bold", &["<Ctrl>b"]);
        app.set_accels_for_action("app.format-italic", &["<Ctrl>i"]);
        app.set_accels_for_action("app.format-link", &["<Ctrl>k"]);
        app.set_accels_for_action("app.find", &["<Ctrl>f"]);
        app.set_accels_for_action("app.focus-mode", &["F11"]);
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
        paned > separator {
            background-image: image(alpha(currentColor, 0.06));
            background-size: 1px 100%;
            background-position: center center;
            background-repeat: no-repeat;
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
        let scheme_name = if manager.is_dark() { "Adwaita-dark" } else { "Adwaita" };
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

    let paned = gtk::Paned::builder()
        .orientation(gtk::Orientation::Horizontal)
        .start_child(&edit_scroll)
        .end_child(&preview_scroll)
        .position(350)
        .wide_handle(true)
        .resize_start_child(true)
        .resize_end_child(true)
        .build();

    let adj = edit_scroll.vadjustment();
    preview_scroll.set_vadjustment(Some(&adj));

    // Set visibility later after buttons are created

    let status_label = Label::builder()
        .margin_top(4)
        .margin_bottom(4)
        .margin_end(12)
        .halign(gtk::Align::End)
        .hexpand(true)
        .css_classes(["dim-label"])
        .build();

    let preview_view_clone = preview_view.clone();
    let status_label_clone = status_label.clone();
    let preview_scroll_clone_for_render = preview_scroll.clone();
    
    let render_source_id: Rc<RefCell<Option<glib::SourceId>>> = Rc::new(RefCell::new(None));
    
    edit_buffer.connect_changed(move |b| {
        if let Some(source_id) = render_source_id.borrow_mut().take() {
            source_id.remove();
        }
        
        let preview_view_inner = preview_view_clone.clone();
        let status_label_inner = status_label_clone.clone();
        let preview_scroll_inner = preview_scroll_clone_for_render.clone();
        let b_clone = b.clone();
        let source_id_ref = render_source_id.clone();
        
        let id = glib::timeout_add_local(std::time::Duration::from_millis(300), move || {
            let text = b_clone.text(&b_clone.start_iter(), &b_clone.end_iter(), false);
            let adj = preview_scroll_inner.hadjustment();
            markdown::render_markdown(&preview_view_inner, text.as_str(), &adj);
            let chars = text.chars().count();
            let words = text.split_whitespace().count();
            let status_str = gettext("{} words, {} chars")
                .replacen("{}", &words.to_string(), 1)
                .replacen("{}", &chars.to_string(), 1);
            status_label_inner.set_label(&status_str);
            
            *source_id_ref.borrow_mut() = None;
            glib::ControlFlow::Break
        });
        
        *render_source_id.borrow_mut() = Some(id);
    });

    let menu = gio::Menu::new();
    menu.append(Some(&gettext("Focus Mode")), Some("app.focus-mode"));
    menu.append(Some(&gettext("Export HTML…")), Some("app.export-html"));
    menu.append(Some(&gettext("Save As…")), Some("app.save-as"));
    menu.append(Some(&gettext("About Blink")), Some("app.about"));
    menu.append(Some(&gettext("Quit")), Some("app.quit"));

    let menu_button = MenuButton::builder()
        .menu_model(&menu)
        .icon_name("open-menu-symbolic")
        .build();

    let view_switcher = gtk::Box::builder().css_classes(["linked"]).build();
    
    // Edit/Render toggle button
    let btn_mode_toggle = gtk::Button::builder()
        .icon_name("document-edit-symbolic")
        .tooltip_text(&gettext("Edit Document"))
        .build();

    // Split view toggle button
    let btn_split_toggle = ToggleButton::builder()
        .icon_name("view-split-left-right-symbolic")
        .tooltip_text(&gettext("Split View"))
        .build();

    view_switcher.append(&btn_mode_toggle);
    view_switcher.append(&btn_split_toggle);

    let edit_scroll_clone = edit_scroll.clone();
    let preview_scroll_clone = preview_scroll.clone();
    btn_mode_toggle.connect_clicked(move |btn| {
        let currently_preview = preview_scroll_clone.is_visible() && !edit_scroll_clone.is_visible();
        if currently_preview {
            // Switch to Edit
            edit_scroll_clone.set_visible(true);
            preview_scroll_clone.set_visible(false);
            btn.set_icon_name("view-reveal-symbolic");
            btn.set_tooltip_text(Some(&gettext("Preview Document")));
        } else {
            // Switch to Preview
            edit_scroll_clone.set_visible(false);
            preview_scroll_clone.set_visible(true);
            btn.set_icon_name("document-edit-symbolic");
            btn.set_tooltip_text(Some(&gettext("Edit Document")));
        }
    });

    let edit_scroll_clone2 = edit_scroll.clone();
    let preview_scroll_clone2 = preview_scroll.clone();
    let btn_mode_toggle_clone = btn_mode_toggle.clone();
    let paned_clone = paned.clone();
    btn_split_toggle.connect_toggled(move |btn| {
        let is_split = btn.is_active();
        if is_split {
            edit_scroll_clone2.set_visible(true);
            preview_scroll_clone2.set_visible(true);
            btn_mode_toggle_clone.set_sensitive(false);
            
            // Force exactly 50-50 split by checking the current width of the paned
            let width = paned_clone.width();
            if width > 0 {
                paned_clone.set_position(width / 2);
            } else {
                paned_clone.set_position(350);
            }
        } else {
            btn_mode_toggle_clone.set_sensitive(true);
            // Restore state based on what the toggle button says
            let wants_preview = btn_mode_toggle_clone.icon_name() == Some(glib::GString::from("document-edit-symbolic"));
            edit_scroll_clone2.set_visible(!wants_preview);
            preview_scroll_clone2.set_visible(wants_preview);
        }
    });

    // Default to edit only when launched empty
    edit_scroll.set_visible(true);
    preview_scroll.set_visible(false);
    btn_split_toggle.set_active(false);
    btn_mode_toggle.set_icon_name("view-reveal-symbolic");
    btn_mode_toggle.set_tooltip_text(Some(&gettext("Preview Document")));

    let header_bar = adw::HeaderBar::new();

    let open_btn = gtk::Button::builder()
        .icon_name("document-open-symbolic")
        .tooltip_text(&gettext("Open Document"))
        .action_name("app.open")
        .build();

    let save_btn = gtk::Button::builder()
        .icon_name("document-save-symbolic")
        .tooltip_text(&gettext("Save Document"))
        .action_name("app.save")
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

    let search_bar = gtk::SearchBar::builder().build();
    let search_entry = gtk::SearchEntry::builder().hexpand(true).build();
    search_bar.set_child(Some(&search_entry));

    let search_settings = sourceview5::SearchSettings::builder().build();
    let _search_context = sourceview5::SearchContext::builder()
        .buffer(&edit_buffer)
        .settings(&search_settings)
        .highlight(true)
        .build();

    search_entry.connect_search_changed(move |entry| {
        search_settings.set_search_text(Some(entry.text().as_str()));
    });

    let toolbar_view = adw::ToolbarView::builder().content(&paned).build();
    toolbar_view.add_top_bar(&header_bar);
    toolbar_view.add_top_bar(&search_bar);
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
        .label(&gettext("Drop Markdown file to open"))
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

    let overlay = gtk::Overlay::builder()
        .child(&toolbar_view)
        .build();
    overlay.add_overlay(&drop_overlay);

    let window = adw::ApplicationWindow::builder()
        .application(app)
        .title(&gettext("Untitled Document"))
        .default_width(700)
        .default_height(900)
        .content(&overlay)
        .build();

    let current_file: Rc<RefCell<Option<gio::File>>> = Rc::new(RefCell::new(None));

    let drop_target = gtk::DropTarget::new(gio::File::static_type(), gtk::gdk::DragAction::COPY);
    let window_clone_drop = window.clone();
    let edit_buffer_clone_drop = edit_buffer.clone();
    let current_file_clone_drop = current_file.clone();
    let btn_split_toggle_drop = btn_split_toggle.clone();
    let btn_mode_toggle_drop = btn_mode_toggle.clone();
    let edit_scroll_drop = edit_scroll.clone();
    let preview_scroll_drop = preview_scroll.clone();
    
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
            let window_clone = window_clone_drop.clone();
            let edit_buffer_clone = edit_buffer_clone_drop.clone();
            let current_file_clone = current_file_clone_drop.clone();
            let btn_split_toggle_open = btn_split_toggle_drop.clone();
            let btn_mode_toggle_open = btn_mode_toggle_drop.clone();
            let edit_scroll_open = edit_scroll_drop.clone();
            let preview_scroll_open = preview_scroll_drop.clone();
            
            glib::spawn_future_local(async move {
                if let Some(path) = file.path() {
                    if let Ok(text) = tokio::fs::read_to_string(&path).await {
                        edit_buffer_clone.set_text(&text);
                        edit_buffer_clone.set_modified(false);
                        window_clone.set_title(Some(&file.basename().unwrap_or_default().to_string_lossy()));
                        *current_file_clone.borrow_mut() = Some(file);

                        btn_split_toggle_open.set_active(false);
                        edit_scroll_open.set_visible(false);
                        preview_scroll_open.set_visible(true);
                        btn_mode_toggle_open.set_icon_name("document-edit-symbolic");
                        btn_mode_toggle_open.set_tooltip_text(Some(&gettext("Edit Document")));
                    } else {
                        let alert = adw::AlertDialog::builder()
                            .heading(&gettext("Error Opening File"))
                            .body(&format!("{}: {}", gettext("Could not open the file"), path.display()))
                            .build();
                        alert.add_response("ok", &gettext("OK"));
                        alert.present(Some(&window_clone));
                    }
                }
            });
            return true;
        }
        false
    });
    window.add_controller(drop_target);

    // Actions
    let action_open = gio::SimpleAction::new("open", None);
    let window_clone = window.clone();
    let edit_buffer_clone = edit_buffer.clone();
    let current_file_clone = current_file.clone();
    let btn_split_toggle_open = btn_split_toggle.clone();
    let btn_mode_toggle_open = btn_mode_toggle.clone();
    let edit_scroll_open = edit_scroll.clone();
    let preview_scroll_open = preview_scroll.clone();
    action_open.connect_activate(move |_, _| {
        let dialog = FileDialog::new();
        let window_clone = window_clone.clone();
        let edit_buffer_clone = edit_buffer_clone.clone();
        let current_file_clone = current_file_clone.clone();
        let btn_split_toggle_open = btn_split_toggle_open.clone();
        let btn_mode_toggle_open = btn_mode_toggle_open.clone();
        let edit_scroll_open = edit_scroll_open.clone();
        let preview_scroll_open = preview_scroll_open.clone();
        glib::spawn_future_local(async move {
            if let Ok(file) = dialog.open_future(Some(&window_clone)).await
                && let Some(path) = file.path()
                && let Ok(text) = tokio::fs::read_to_string(&path).await
            {
                edit_buffer_clone.set_text(&text);
                edit_buffer_clone.set_modified(false);
                window_clone
                    .set_title(Some(&file.basename().unwrap_or_default().to_string_lossy()));
                *current_file_clone.borrow_mut() = Some(file);

                btn_split_toggle_open.set_active(false);
                edit_scroll_open.set_visible(false);
                preview_scroll_open.set_visible(true);
                btn_mode_toggle_open.set_icon_name("document-edit-symbolic");
                btn_mode_toggle_open.set_tooltip_text(Some(&gettext("Edit Document")));
            }
        });
    });
    app.add_action(&action_open);

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
            if let Ok(file) = dialog.save_future(Some(&window_clone)).await
                && let Some(path) = file.path()
            {
                let text = edit_buffer_clone.text(
                    &edit_buffer_clone.start_iter(),
                    &edit_buffer_clone.end_iter(),
                    false,
                );
                if let Err(e) = tokio::fs::write(&path, text.as_str()).await {
                    let alert = adw::AlertDialog::builder()
                        .heading(&gettext("Error Saving File"))
                        .body(&format!("{}: {}", gettext("Could not save the file"), e))
                        .build();
                    alert.add_response("ok", &gettext("OK"));
                    alert.present(Some(&window_clone));
                } else {
                    edit_buffer_clone.set_modified(false);
                    window_clone
                        .set_title(Some(&file.basename().unwrap_or_default().to_string_lossy()));
                    *current_file_clone.borrow_mut() = Some(file);
                }
            }
        });
    });
    app.add_action(&action_save_as);

    let action_save = gio::SimpleAction::new("save", None);
    let edit_buffer_clone = edit_buffer.clone();
    let current_file_clone = current_file.clone();
    let app_clone = app.clone();
    let window_clone_save = window.clone();
    action_save.connect_activate(move |_, _| {
        let file_opt = current_file_clone.borrow().clone();
        if let Some(file) = file_opt {
            if let Some(path) = file.path() {
                let text = edit_buffer_clone.text(
                    &edit_buffer_clone.start_iter(),
                    &edit_buffer_clone.end_iter(),
                    false,
                );
                let window_clone_inner = window_clone_save.clone();
                let edit_buffer_inner = edit_buffer_clone.clone();
                glib::spawn_future_local(async move {
                    if let Err(e) = tokio::fs::write(&path, text.as_str()).await {
                        let alert = adw::AlertDialog::builder()
                            .heading(&gettext("Error Saving File"))
                            .body(&format!("{}: {}", gettext("Could not save the file"), e))
                            .build();
                        alert.add_response("ok", &gettext("OK"));
                        alert.present(Some(&window_clone_inner));
                    } else {
                        edit_buffer_inner.set_modified(false);
                    }
                });
            }
        } else {
            app_clone.activate_action("save-as", None);
        }
    });
    app.add_action(&action_save);

    let current_file_autosave = current_file.clone();
    let edit_buffer_autosave = edit_buffer.clone();
    glib::timeout_add_seconds_local(10, move || {
        if edit_buffer_autosave.is_modified() {
            if let Some(file) = current_file_autosave.borrow().as_ref() {
                if let Some(path) = file.path() {
                    let text = edit_buffer_autosave.text(
                        &edit_buffer_autosave.start_iter(),
                        &edit_buffer_autosave.end_iter(),
                        false,
                    );
                    let edit_buf_clone = edit_buffer_autosave.clone();
                    glib::spawn_future_local(async move {
                        if tokio::fs::write(&path, text.as_str()).await.is_ok() {
                            edit_buf_clone.set_modified(false);
                        }
                    });
                }
            }
        }
        glib::ControlFlow::Continue
    });

    let app_clone = app.clone();
    let window_clone_quit = window.clone();
    let edit_buffer_clone_quit = edit_buffer.clone();
    let action_quit = gio::SimpleAction::new("quit", None);
    action_quit.connect_activate(move |_, _| {
        if edit_buffer_clone_quit.is_modified() {
            let alert = adw::AlertDialog::builder()
                .heading(&gettext("Unsaved Changes"))
                .body(&gettext("You have unsaved changes. Do you want to close without saving?"))
                .build();
            alert.add_response("cancel", &gettext("Cancel"));
            alert.add_response("close", &gettext("Close Without Saving"));
            alert.set_response_appearance("close", adw::ResponseAppearance::Destructive);
            
            let app_clone_inner = app_clone.clone();
            alert.connect_response(None, move |_, response| {
                if response == "close" {
                    app_clone_inner.quit();
                }
            });
            alert.present(Some(&window_clone_quit));
        } else {
            app_clone.quit();
        }
    });
    app.add_action(&action_quit);

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
    app.add_action(&action_bold);

    let action_italic = gio::SimpleAction::new("format-italic", None);
    let edit_buf_italic = edit_buffer.clone();
    action_italic.connect_activate(move |_, _| {
        if let Some((mut start, mut end)) = edit_buf_italic.selection_bounds() {
            let text = edit_buf_italic.text(&start, &end, false);
            edit_buf_italic.delete(&mut start, &mut end);
            edit_buf_italic.insert(&mut start, &format!("*{}*", text.as_str()));
        }
    });
    app.add_action(&action_italic);

    let action_link = gio::SimpleAction::new("format-link", None);
    let edit_buf_link = edit_buffer.clone();
    action_link.connect_activate(move |_, _| {
        if let Some((mut start, mut end)) = edit_buf_link.selection_bounds() {
            let text = edit_buf_link.text(&start, &end, false);
            edit_buf_link.delete(&mut start, &mut end);
            edit_buf_link.insert(&mut start, &format!("[{}](url)", text.as_str()));
        }
    });
    app.add_action(&action_link);

    let search_bar_clone = search_bar.clone();
    let action_find = gio::SimpleAction::new("find", None);
    action_find.connect_activate(move |_, _| {
        search_bar_clone.set_search_mode(true);
    });
    app.add_action(&action_find);

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

                if let Err(e) = tokio::fs::write(&path, html_doc).await {
                    let alert = adw::AlertDialog::builder()
                        .heading(&gettext("Error Exporting HTML"))
                        .body(&format!("{}: {}", gettext("Could not export the file"), e))
                        .build();
                    alert.add_response("ok", &gettext("OK"));
                    alert.present(Some(&window_clone));
                }
            }
        });
    });
    app.add_action(&action_export_html);

    let action_about = gio::SimpleAction::new("about", None);
    let window_clone_about = window.clone();
    action_about.connect_activate(move |_, _| {
        let about = adw::AboutDialog::builder()
            .application_name("Blink")
            .application_icon("com.github.sachesi.blink")
            .developer_name("sachesi")
            .version("0.1.1")
            .comments(&gettext("A fast and minimal Markdown editor"))
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
    app.add_action(&action_about);

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
    app.add_action(&action_focus);

    let app_clone_close = app.clone();
    window.connect_close_request(move |_| {
        app_clone_close.activate_action("quit", None);
        glib::Propagation::Stop
    });

    if let Some(file) = initial_file {
        let window_clone = window.clone();
        let edit_buffer_clone = edit_buffer.clone();
        let current_file_clone = current_file.clone();
        let btn_split_toggle_open = btn_split_toggle.clone();
        let btn_mode_toggle_open = btn_mode_toggle.clone();
        let edit_scroll_open = edit_scroll.clone();
        let preview_scroll_open = preview_scroll.clone();
        
        glib::spawn_future_local(async move {
            if let Some(path) = file.path() {
                if let Ok(text) = tokio::fs::read_to_string(&path).await {
                    edit_buffer_clone.set_text(&text);
                    edit_buffer_clone.set_modified(false);
                    window_clone.set_title(Some(&file.basename().unwrap_or_default().to_string_lossy()));
                    *current_file_clone.borrow_mut() = Some(file);

                    btn_split_toggle_open.set_active(false);
                    edit_scroll_open.set_visible(false);
                    preview_scroll_open.set_visible(true);
                    btn_mode_toggle_open.set_icon_name("document-edit-symbolic");
                    btn_mode_toggle_open.set_tooltip_text(Some(&gettext("Edit Document")));
                } else {
                    let alert = adw::AlertDialog::builder()
                        .heading(&gettext("Error Opening File"))
                        .body(&format!("{}: {}", gettext("Could not open the file"), path.display()))
                        .build();
                    alert.add_response("ok", &gettext("OK"));
                    alert.present(Some(&window_clone));
                }
            }
        });
    }

    window.present();
}
