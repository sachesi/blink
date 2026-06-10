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
        .application_id("com.example.blink")
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
    });

    app.connect_activate(build_ui);
    app.run()
}

fn build_ui(app: &Application) {
    let provider = gtk::CssProvider::new();
    provider.load_from_data(
        "
        textview { background: transparent; }
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

    let edit_buffer = SourceBuffer::builder()
        .language(&markdown_lang.expect("Markdown language not found"))
        .build();
    let edit_view = SourceView::builder()
        .buffer(&edit_buffer)
        .wrap_mode(gtk::WrapMode::Word)
        .show_line_numbers(true)
        .left_margin(32)
        .right_margin(32)
        .top_margin(32)
        .bottom_margin(32)
        .pixels_above_lines(4)
        .pixels_below_lines(4)
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
        .build();

    let adj = edit_scroll.vadjustment();
    preview_scroll.set_vadjustment(Some(&adj));

    // Default to preview only
    edit_scroll.set_visible(false);

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
    edit_buffer.connect_changed(move |b| {
        let text = b.text(&b.start_iter(), &b.end_iter(), false);
        markdown::render_markdown(&preview_view_clone, text.as_str());
        let chars = text.chars().count();
        let words = text.split_whitespace().count();
        let status_str = gettext("{} words, {} chars")
            .replacen("{}", &words.to_string(), 1)
            .replacen("{}", &chars.to_string(), 1);
        status_label_clone.set_label(&status_str);
    });

    let menu = gio::Menu::new();
    menu.append(Some(&gettext("Save As…")), Some("app.save-as"));
    menu.append(Some(&gettext("Quit")), Some("app.quit"));

    let menu_button = MenuButton::builder()
        .menu_model(&menu)
        .icon_name("open-menu-symbolic")
        .build();

    let view_switcher = gtk::Box::builder().css_classes(["linked"]).build();
    let btn_edit = ToggleButton::builder().label(&gettext("Edit")).build();
    let btn_split = ToggleButton::builder().label(&gettext("Split")).build();
    let btn_preview = ToggleButton::builder().label(&gettext("Preview")).active(true).build();

    btn_split.set_group(Some(&btn_edit));
    btn_preview.set_group(Some(&btn_edit));

    view_switcher.append(&btn_edit);
    view_switcher.append(&btn_split);
    view_switcher.append(&btn_preview);

    let edit_scroll_clone = edit_scroll.clone();
    let preview_scroll_clone = preview_scroll.clone();
    btn_edit.connect_toggled(move |btn| {
        if btn.is_active() {
            edit_scroll_clone.set_visible(true);
            preview_scroll_clone.set_visible(false);
        }
    });

    let edit_scroll_clone2 = edit_scroll.clone();
    let preview_scroll_clone2 = preview_scroll.clone();
    btn_split.connect_toggled(move |btn| {
        if btn.is_active() {
            edit_scroll_clone2.set_visible(true);
            preview_scroll_clone2.set_visible(true);
        }
    });

    let edit_scroll_clone3 = edit_scroll.clone();
    let preview_scroll_clone3 = preview_scroll.clone();
    btn_preview.connect_toggled(move |btn| {
        if btn.is_active() {
            edit_scroll_clone3.set_visible(false);
            preview_scroll_clone3.set_visible(true);
        }
    });

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

    let window = adw::ApplicationWindow::builder()
        .application(app)
        .title(&gettext("Untitled Document"))
        .default_width(700)
        .default_height(900)
        .content(&toolbar_view)
        .build();

    let current_file: Rc<RefCell<Option<gio::File>>> = Rc::new(RefCell::new(None));

    // Actions
    let action_open = gio::SimpleAction::new("open", None);
    let window_clone = window.clone();
    let edit_buffer_clone = edit_buffer.clone();
    let current_file_clone = current_file.clone();
    action_open.connect_activate(move |_, _| {
        let dialog = FileDialog::new();
        let window_clone = window_clone.clone();
        let edit_buffer_clone = edit_buffer_clone.clone();
        let current_file_clone = current_file_clone.clone();
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

    let app_clone_close = app.clone();
    window.connect_close_request(move |_| {
        app_clone_close.activate_action("quit", None);
        glib::Propagation::Stop
    });

    window.present();
}
