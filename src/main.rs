use adw::prelude::*;
use adw::{Application, ApplicationWindow, HeaderBar, ToolbarView, ViewStack};
use gtk::{gio, glib, MenuButton, ScrolledWindow, TextBuffer, TextView, ToggleButton};

mod markdown;

fn main() -> glib::ExitCode {
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
    let stack = ViewStack::new();

    let edit_buffer = TextBuffer::new(None);
    let edit_view = TextView::builder()
        .buffer(&edit_buffer)
        .wrap_mode(gtk::WrapMode::Word)
        .left_margin(12)
        .right_margin(12)
        .top_margin(12)
        .bottom_margin(12)
        .build();
    let edit_scroll = ScrolledWindow::builder()
        .child(&edit_view)
        .hscrollbar_policy(gtk::PolicyType::Never)
        .build();
    stack.add_named(&edit_scroll, Some("edit"));

    let preview_buffer = TextBuffer::new(None);
    markdown::setup_tags(&preview_buffer);
    let preview_view = TextView::builder()
        .buffer(&preview_buffer)
        .editable(false)
        .wrap_mode(gtk::WrapMode::Word)
        .left_margin(12)
        .right_margin(12)
        .top_margin(12)
        .bottom_margin(12)
        .build();

    let preview_buffer_clone = preview_buffer.clone();
    edit_buffer.connect_changed(move |b| {
        let text = b.text(&b.start_iter(), &b.end_iter(), false);
        markdown::render_markdown(&preview_buffer_clone, text.as_str());
    });

    let preview_scroll = ScrolledWindow::builder()
        .child(&preview_view)
        .hscrollbar_policy(gtk::PolicyType::Never)
        .build();
    stack.add_named(&preview_scroll, Some("preview"));

    let menu = gio::Menu::new();
    menu.append(Some("Open…"), Some("app.open"));
    menu.append(Some("Save"), Some("app.save"));
    menu.append(Some("Save As…"), Some("app.save-as"));
    menu.append(Some("Quit"), Some("app.quit"));

    let menu_button = MenuButton::builder()
        .menu_model(&menu)
        .icon_name("open-menu-symbolic")
        .build();

    let preview_toggle = ToggleButton::builder()
        .icon_name("view-reveal-symbolic")
        .tooltip_text("Toggle Preview")
        .build();

    let stack_clone = stack.clone();
    preview_toggle.connect_toggled(move |btn| {
        if btn.is_active() {
            stack_clone.set_visible_child_name("preview");
        } else {
            stack_clone.set_visible_child_name("edit");
        }
    });

    let header_bar = HeaderBar::builder().build();
    header_bar.pack_end(&menu_button);
    header_bar.pack_end(&preview_toggle);

    let toolbar_view = ToolbarView::builder()
        .content(&stack)
        .build();
    toolbar_view.add_top_bar(&header_bar);

    let action_open = gio::SimpleAction::new("open", None);
    action_open.connect_activate(|_, _| println!("Open trigger"));
    app.add_action(&action_open);

    let action_save = gio::SimpleAction::new("save", None);
    action_save.connect_activate(|_, _| println!("Save trigger"));
    app.add_action(&action_save);

    let action_save_as = gio::SimpleAction::new("save-as", None);
    action_save_as.connect_activate(|_, _| println!("Save As trigger"));
    app.add_action(&action_save_as);

    let app_clone = app.clone();
    let action_quit = gio::SimpleAction::new("quit", None);
    action_quit.connect_activate(move |_, _| app_clone.quit());
    app.add_action(&action_quit);

    let window = ApplicationWindow::builder()
        .application(app)
        .title("Untitled Document")
        .default_width(800)
        .default_height(600)
        .content(&toolbar_view)
        .build();

    window.present();
}
