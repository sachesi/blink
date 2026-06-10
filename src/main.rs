use adw::prelude::*;
use adw::{Application, ApplicationWindow, HeaderBar, ToolbarView, ViewStack};
use gtk::{gio, glib, Box, FileDialog, Label, MenuButton, ScrolledWindow, TextBuffer, TextView, ToggleButton};
use std::cell::RefCell;
use std::rc::Rc;

mod markdown;

#[tokio::main]
async fn main() -> glib::ExitCode {
    let app = Application::builder()
        .application_id("com.example.blink")
        .build();

    app.connect_startup(|app| {
        app.set_accels_for_action("app.open", &["<Ctrl>o"]);
        app.set_accels_for_action("app.save", &["<Ctrl>s"]);
        app.set_accels_for_action("app.save-as", &["<Ctrl><Shift>s"]);
        app.set_accels_for_action("app.quit", &["<Ctrl>q"]);
    });

    app.connect_activate(build_ui);
    app.run()
}

fn build_ui(app: &Application) {
    let provider = gtk::CssProvider::new();
    provider.load_from_data("
        textview { background: transparent; }
    ");
    gtk::style_context_add_provider_for_display(
        &gtk::gdk::Display::default().expect("Could not connect to a display."),
        &provider,
        gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
    );

    let stack = gtk::Stack::builder()
        .transition_type(gtk::StackTransitionType::Crossfade)
        .transition_duration(300)
        .build();

    let edit_buffer = TextBuffer::new(None);
    let edit_view = TextView::builder()
        .buffer(&edit_buffer)
        .wrap_mode(gtk::WrapMode::Word)
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
        .build();
    stack.add_named(&edit_scroll, Some("edit"));

    markdown::setup_tags(&edit_buffer);

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
    let edit_buffer_clone = edit_buffer.clone();
    edit_buffer.connect_changed(move |b| {
        let text = b.text(&b.start_iter(), &b.end_iter(), false);
        markdown::highlight_editor(&edit_buffer_clone, text.as_str());
        markdown::render_markdown(&preview_view_clone, text.as_str());
        let chars = text.chars().count();
        let words = text.split_whitespace().count();
        status_label_clone.set_label(&format!("{} words, {} chars", words, chars));
    });

    let preview_clamp = adw::Clamp::builder()
        .child(&preview_view)
        .maximum_size(700)
        .build();
    let preview_scroll = ScrolledWindow::builder()
        .child(&preview_clamp)
        .hscrollbar_policy(gtk::PolicyType::Never)
        .build();
    stack.add_named(&preview_scroll, Some("preview"));

    let menu = gio::Menu::new();
    menu.append(Some("Save As…"), Some("app.save-as"));
    menu.append(Some("Quit"), Some("app.quit"));

    let menu_button = MenuButton::builder()
        .menu_model(&menu)
        .icon_name("open-menu-symbolic")
        .build();

    let preview_toggle = ToggleButton::builder()
        .icon_name("view-reveal-symbolic")
        .tooltip_text("Toggle Preview")
        .active(true)
        .build();
    
    stack.set_visible_child_name("preview");

    let stack_clone = stack.clone();
    preview_toggle.connect_toggled(move |btn| {
        if btn.is_active() {
            stack_clone.set_visible_child_name("preview");
        } else {
            stack_clone.set_visible_child_name("edit");
        }
    });

    let header_bar = adw::HeaderBar::new();
    
    let open_btn = gtk::Button::builder()
        .icon_name("document-open-symbolic")
        .tooltip_text("Open Document")
        .action_name("app.open")
        .build();
        
    let save_btn = gtk::Button::builder()
        .icon_name("document-save-symbolic")
        .tooltip_text("Save Document")
        .action_name("app.save")
        .build();

    header_bar.pack_start(&open_btn);
    header_bar.pack_start(&save_btn);
    header_bar.pack_end(&menu_button);
    header_bar.pack_end(&preview_toggle);

    let bottom_box = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .css_classes(["toolbar"])
        .build();
    let spacer = gtk::Box::builder().hexpand(true).build();
    bottom_box.append(&spacer);
    bottom_box.append(&status_label);

    let toolbar_view = adw::ToolbarView::builder()
        .content(&stack)
        .build();
    toolbar_view.add_top_bar(&header_bar);
    toolbar_view.add_bottom_bar(&bottom_box);

    let window = adw::ApplicationWindow::builder()
        .application(app)
        .title("Untitled Document")
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
            if let Ok(file) = dialog.open_future(Some(&window_clone)).await {
                if let Some(path) = file.path() {
                    if let Ok(text) = tokio::fs::read_to_string(&path).await {
                        edit_buffer_clone.set_text(&text);
                        window_clone.set_title(Some(&file.basename().unwrap_or_default().to_string_lossy()));
                        *current_file_clone.borrow_mut() = Some(file);
                    }
                }
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
            if let Ok(file) = dialog.save_future(Some(&window_clone)).await {
                if let Some(path) = file.path() {
                    let text = edit_buffer_clone.text(&edit_buffer_clone.start_iter(), &edit_buffer_clone.end_iter(), false);
                    let _ = tokio::fs::write(&path, text.as_str()).await;
                    window_clone.set_title(Some(&file.basename().unwrap_or_default().to_string_lossy()));
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
    action_save.connect_activate(move |_, _| {
        let file_opt = current_file_clone.borrow().clone();
        if let Some(file) = file_opt {
            if let Some(path) = file.path() {
                let text = edit_buffer_clone.text(&edit_buffer_clone.start_iter(), &edit_buffer_clone.end_iter(), false);
                glib::spawn_future_local(async move {
                    let _ = tokio::fs::write(&path, text.as_str()).await;
                });
            }
        } else {
            app_clone.activate_action("save-as", None);
        }
    });
    app.add_action(&action_save);

    let app_clone = app.clone();
    let action_quit = gio::SimpleAction::new("quit", None);
    action_quit.connect_activate(move |_, _| app_clone.quit());
    app.add_action(&action_quit);

    window.present();
}
