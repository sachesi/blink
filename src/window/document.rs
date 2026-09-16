//! The document's file: opening, saving, autosave, export, changes made on disk by
//! other programs, and closing.
//!
//! Everything here that can wait on a dialog or the disk runs as a [`Command`] through one
//! queue, one at a time. A file monitor event, a timer or a second request therefore never
//! lands in the middle of a save or a question to the user; it waits its turn, and sees the
//! state the operation before it left.

use adw::prelude::*;
use adw::subclass::prelude::*;
use gettextrs::gettext;
use gtk::{gio, glib};
use std::path::{Path, PathBuf};

use super::{BlinkWindow, ViewMode, buffer_text};
use crate::conflict::{self, AutosaveOutcome, FileFingerprint};
use crate::export;
use crate::markdown;
use crate::pdf;

/// How often unsaved changes to a file are written to it.
const AUTOSAVE_INTERVAL_SECS: u32 = 10;
/// How many files "Open Recent" remembers.
const MAX_RECENT: usize = 10;

#[derive(Debug, Clone)]
pub enum Command {
    New,
    Open,
    /// Open a file, asking about unsaved changes first.
    OpenFile(gio::File),
    Save,
    SaveAs,
    ExportHtml,
    ExportPdf,
    Close,
    Autosave,
    Backup,
    DiskChanged,
    DiskDeleted,
    CheckRecovery,
}

#[derive(Default)]
pub struct State {
    /// The file the document was opened from or saved to; none while untitled.
    pub file: Option<gio::File>,
    /// The file as it was when last read or written, to tell other programs' changes.
    pub fingerprint: Option<FileFingerprint>,
    pub monitor: Option<gio::FileMonitor>,
    /// A change on disk is waiting for the user's decision; autosave holds off until then.
    pub in_conflict: bool,
}

impl BlinkWindow {
    pub(super) fn setup_document(&self) {
        let (sender, receiver) = async_channel::unbounded::<Command>();
        self.imp().commands.set(sender).ok();
        // The loop holds the window only while a command runs, so the window can go away
        // between them; dropping it drops the sender and ends the loop.
        let window = self.downgrade();
        glib::spawn_future_local(async move {
            while let Ok(command) = receiver.recv().await {
                let Some(window) = window.upgrade() else {
                    break;
                };
                window.run(command).await;
            }
        });

        let id = glib::timeout_add_seconds_local(
            AUTOSAVE_INTERVAL_SECS,
            glib::clone!(
                #[weak(rename_to = win)]
                self,
                #[upgrade_or]
                glib::ControlFlow::Break,
                move || {
                    win.enqueue(Command::Autosave);
                    glib::ControlFlow::Continue
                }
            ),
        );
        self.imp().autosave_timer.replace(Some(id));

        self.setup_recovery();
    }

    pub(super) fn enqueue(&self, command: Command) {
        if let Some(sender) = self.imp().commands.get() {
            sender.try_send(command).ok();
        }
    }

    async fn run(&self, command: Command) {
        match command {
            Command::New => self.new_document().await,
            Command::Open => self.open().await,
            Command::OpenFile(file) => {
                if self.confirm_discard_if_modified().await {
                    self.load_file(file).await;
                }
            }
            Command::Save => self.save().await,
            Command::SaveAs => {
                self.save_as().await;
            }
            Command::ExportHtml => self.export_html().await,
            Command::ExportPdf => self.export_pdf().await,
            Command::Close => self.close_guarded().await,
            Command::Autosave => self.autosave().await,
            Command::Backup => self.write_backup_now().await,
            Command::DiskChanged => self.handle_disk_changed().await,
            Command::DiskDeleted => self.handle_disk_deleted().await,
            Command::CheckRecovery => self.check_recovery().await,
        }
    }

    pub(super) fn current_file(&self) -> Option<gio::File> {
        self.imp().document.borrow().file.clone()
    }

    fn current_path(&self) -> Option<PathBuf> {
        self.current_file().and_then(|file| file.path())
    }

    async fn open(&self) {
        if !self.confirm_discard_if_modified().await {
            return;
        }
        let dialog = gtk::FileDialog::new();
        let (filters, markdown) = markdown_filters();
        dialog.set_filters(Some(&filters));
        dialog.set_default_filter(Some(&markdown));
        if let Ok(file) = dialog.open_future(Some(self)).await {
            self.load_file(file).await;
        }
    }

    async fn new_document(&self) {
        if !self.confirm_discard_if_modified().await {
            return;
        }
        let imp = self.imp();
        self.clear_backup().await;
        {
            let mut document = imp.document.borrow_mut();
            if let Some(monitor) = document.monitor.take() {
                monitor.cancel();
            }
            *document = State::default();
        }
        self.use_untitled_backup();
        imp.edit_buffer.set_text("");
        imp.edit_buffer.set_modified(false);
        self.update_title();
        imp.status_label.set_label("");
        imp.last_single_mode.set(ViewMode::Edit);
        self.set_view_mode(ViewMode::Edit);
    }

    async fn load_file(&self, file: gio::File) {
        let Some(path) = file.path() else {
            self.present_error(
                gettext("Error Opening File"),
                gettext("Only local files are supported"),
            );
            return;
        };
        let read_path = path.clone();
        // The fingerprint is taken right after the read, so a change made in between is
        // not taken for the text that was read.
        let read = blocking(move || {
            std::fs::read_to_string(&read_path)
                .map(|text| (text, FileFingerprint::read_from_path(&read_path).ok()))
        })
        .await;
        match read {
            Ok((text, fingerprint)) => {
                let imp = self.imp();
                imp.edit_buffer.set_text(&text);
                imp.edit_buffer.set_modified(false);
                {
                    let mut document = imp.document.borrow_mut();
                    document.fingerprint = fingerprint;
                    document.file = Some(file);
                }
                self.update_title();
                self.adopt_file(&path).await;
                self.add_recent(&path);
                imp.preview.dirty.set(true);
                self.set_view_mode(ViewMode::Preview);
            }
            Err(err) => self.present_error(
                gettext("Error Opening File"),
                format!(
                    "{}\n\n{}",
                    gettext("Could not open the file"),
                    describe_io_error(&err)
                ),
            ),
        }
    }

    async fn save(&self) {
        if self.current_file().is_none() {
            self.save_as().await;
        } else if self.current_file_changed_on_disk().await {
            // A manual save must not silently overwrite another program's change either.
            self.resolve_disk_conflict().await;
        } else {
            self.save_current().await;
        }
    }

    async fn save_to(&self, file: gio::File) -> bool {
        let Some(path) = file.path() else {
            self.present_error(
                gettext("Error Saving File"),
                gettext("Only local files are supported"),
            );
            return false;
        };
        let imp = self.imp();
        let text = buffer_text(&*imp.edit_buffer);
        let (write_path, write_text) = (path.clone(), text.clone());
        let written = blocking(move || {
            conflict::write_text_atomically(&write_path, &write_text)
                .map(|()| FileFingerprint::read_from_path(&write_path).ok())
        })
        .await;
        match written {
            Ok(fingerprint) => {
                // Typing goes on while the write runs; the document is only saved if what
                // reached the disk is still what it holds.
                if buffer_text(&*imp.edit_buffer) == text {
                    imp.edit_buffer.set_modified(false);
                }
                {
                    let mut document = imp.document.borrow_mut();
                    document.fingerprint = fingerprint;
                    document.file = Some(file);
                }
                self.update_title();
                self.adopt_file(&path).await;
                self.add_recent(&path);
                true
            }
            Err(err) => {
                self.present_error(
                    gettext("Error Saving File"),
                    format!(
                        "{}\n\n{}",
                        gettext("Could not save the file"),
                        describe_io_error(&err)
                    ),
                );
                false
            }
        }
    }

    async fn save_current(&self) -> bool {
        match self.current_file() {
            Some(file) => self.save_to(file).await,
            None => false,
        }
    }

    async fn save_as(&self) -> bool {
        let dialog = gtk::FileDialog::new();
        let (filters, markdown) = markdown_filters();
        dialog.set_filters(Some(&filters));
        dialog.set_default_filter(Some(&markdown));
        match self.current_file() {
            Some(file) => dialog.set_initial_file(Some(&file)),
            None => dialog.set_initial_name(Some(&gettext("Untitled.md"))),
        }
        match dialog.save_future(Some(self)).await {
            Ok(file) => self.save_to(file).await,
            Err(_) => false,
        }
    }

    /// Ask what to do with unsaved changes before they would be replaced. False when the
    /// user cancelled, or chose to save and the save did not happen.
    async fn confirm_discard_if_modified(&self) -> bool {
        if !self.imp().edit_buffer.is_modified() {
            return true;
        }
        let alert = adw::AlertDialog::builder()
            .heading(gettext("Unsaved Changes"))
            .body(gettext(
                "Opening another file will discard unsaved changes.",
            ))
            .build();
        alert.add_response("cancel", &gettext("Cancel"));
        alert.add_response("discard", &gettext("Discard Changes"));
        alert.add_response("save", &gettext("Save"));
        alert.set_response_appearance("discard", adw::ResponseAppearance::Destructive);
        alert.set_response_appearance("save", adw::ResponseAppearance::Suggested);
        alert.set_default_response(Some("save"));
        alert.set_close_response("cancel");
        match alert.choose_future(Some(self)).await.as_str() {
            "save" => {
                if self.current_file().is_some() {
                    self.save_current().await
                } else {
                    self.save_as().await
                }
            }
            "discard" => true,
            _ => false,
        }
    }

    /// Ask where to export to, as a file of `mime_type` ending in `.suffix`.
    async fn choose_export_file(
        &self,
        filter_name: String,
        mime_type: &str,
        suffix: &str,
    ) -> Option<PathBuf> {
        let dialog = gtk::FileDialog::new();
        let filter = gtk::FileFilter::new();
        filter.set_name(Some(&filter_name));
        filter.add_mime_type(mime_type);
        filter.add_suffix(suffix);
        let filters = gio::ListStore::new::<gtk::FileFilter>();
        filters.append(&filter);
        dialog.set_filters(Some(&filters));
        dialog.set_default_filter(Some(&filter));
        let stem = self
            .current_path()
            .and_then(|path| path.file_stem().map(|s| s.to_string_lossy().into_owned()))
            .unwrap_or_else(|| gettext("Untitled"));
        dialog.set_initial_name(Some(&format!("{stem}.{suffix}")));
        dialog.save_future(Some(self)).await.ok()?.path()
    }

    async fn export_html(&self) {
        let Some(path) = self
            .choose_export_file(gettext("HTML Files"), "text/html", "html")
            .await
        else {
            return;
        };
        let text = buffer_text(&*self.imp().edit_buffer);
        let options = export::Options {
            dark: markdown::code_highlights(&text, true).await,
            ..self.export_options(&text).await
        };
        let written = blocking(move || {
            conflict::write_text_atomically(&path, export::render_html(&text, &options))
        })
        .await;
        if let Err(err) = written {
            self.present_error(
                gettext("Error Exporting HTML"),
                format!(
                    "{}\n\n{}",
                    gettext("Could not export the file"),
                    describe_io_error(&err)
                ),
            );
        }
    }

    async fn export_pdf(&self) {
        let Some(path) = self
            .choose_export_file(gettext("PDF Files"), "application/pdf", "pdf")
            .await
        else {
            return;
        };
        let text = buffer_text(&*self.imp().edit_buffer);
        let options = self.export_options(&text).await;
        let written = blocking(move || {
            let pdf = pdf::render_pdf(&text, &options).map_err(std::io::Error::other)?;
            conflict::write_text_atomically(&path, pdf)
        })
        .await;
        if let Err(err) = written {
            self.present_error(
                gettext("Error Exporting PDF"),
                format!(
                    "{}\n\n{}",
                    gettext("Could not export the file"),
                    describe_io_error(&err)
                ),
            );
        }
    }

    /// What an export of `text` takes from the window: the title, the folder of images, the
    /// fonts, the width and the colours of the code in the light style.
    async fn export_options(&self, text: &str) -> export::Options {
        let (text_font, monospace_font) = self.font_families();
        export::Options {
            title: self
                .current_file()
                .as_ref()
                .map(file_title)
                .unwrap_or_else(|| gettext("Untitled Document")),
            base_dir: self
                .current_path()
                .and_then(|path| path.parent().map(Path::to_path_buf)),
            text_font,
            monospace_font,
            width: self.content_width(),
            light: markdown::code_highlights(text, false).await,
            dark: Vec::new(),
        }
    }

    async fn close_guarded(&self) {
        if self.imp().edit_buffer.is_modified() {
            let alert = adw::AlertDialog::builder()
                .heading(gettext("Unsaved Changes"))
                .body(gettext(
                    "You have unsaved changes. Do you want to close without saving?",
                ))
                .build();
            alert.add_response("cancel", &gettext("Cancel"));
            alert.add_response("close", &gettext("Close Without Saving"));
            alert.set_response_appearance("close", adw::ResponseAppearance::Destructive);
            alert.set_close_response("cancel");
            if alert.choose_future(Some(self)).await != "close" {
                return;
            }
        }
        self.cleanup_on_exit().await;
        self.imp().closing.set(true);
        self.close();
    }

    async fn cleanup_on_exit(&self) {
        self.clear_backup().await;
        self.release_lock().await;
        // The default size is the size the window has when it is not maximized, which is
        // the one to open with next time.
        let (width, height) = self.default_size();
        let settings = self.settings();
        let _ = settings.set("window-size", (width, height));
        let _ = settings.set_boolean("window-maximized", self.is_maximized());
    }

    async fn autosave(&self) {
        let imp = self.imp();
        if imp.document.borrow().in_conflict || !imp.edit_buffer.is_modified() {
            return;
        }
        let Some(file) = self.current_file() else {
            return;
        };
        let Some(path) = file.path() else {
            return;
        };
        let text = buffer_text(&*imp.edit_buffer);
        let fingerprint = imp.document.borrow().fingerprint;
        let (write_path, write_text) = (path.clone(), text.clone());
        let (outcome, written_fingerprint) = blocking(move || {
            let outcome = match fingerprint {
                Some(fingerprint) => {
                    conflict::autosave_with_conflict_check(&write_path, &write_text, &fingerprint)
                }
                None => match conflict::write_text_atomically(&write_path, &write_text) {
                    Ok(()) => AutosaveOutcome::Saved,
                    Err(err) => AutosaveOutcome::Failed(err),
                },
            };
            let written_fingerprint = matches!(outcome, AutosaveOutcome::Saved)
                .then(|| FileFingerprint::read_from_path(&write_path).ok())
                .flatten();
            (outcome, written_fingerprint)
        })
        .await;
        match outcome {
            AutosaveOutcome::Saved => {
                if buffer_text(&*imp.edit_buffer) == text {
                    imp.edit_buffer.set_modified(false);
                }
                imp.document.borrow_mut().fingerprint = written_fingerprint;
                self.clear_backup().await;
            }
            AutosaveOutcome::ConflictDetected => {
                self.toast(&gettext("File changed on disk — autosave paused"));
            }
            AutosaveOutcome::FileDeleted => {
                self.toast(&gettext("File no longer exists"));
            }
            AutosaveOutcome::Failed(err) => {
                self.toast(&format!(
                    "{}: {}",
                    gettext("Autosave failed"),
                    describe_io_error(&err)
                ));
            }
        }
    }

    fn add_recent(&self, path: &Path) {
        let settings = self.settings();
        let recent = push_recent(settings.strv("recent-files"), path);
        let _ = settings.set_strv("recent-files", recent);
    }

    /// Make a freshly opened or saved file the document's own: its backup, its monitor.
    async fn adopt_file(&self, path: &Path) {
        self.use_backup_for(path).await;
        self.imp().document.borrow_mut().in_conflict = false;
        self.watch_file(path);
    }

    pub(super) fn watch_file(&self, path: &Path) {
        let imp = self.imp();
        if let Some(old) = imp.document.borrow_mut().monitor.take() {
            old.cancel();
        }
        match gio::File::for_path(path)
            .monitor_file(gio::FileMonitorFlags::NONE, gio::Cancellable::NONE)
        {
            Ok(monitor) => {
                monitor.set_rate_limit(500);
                monitor.connect_changed(glib::clone!(
                    #[weak(rename_to = win)]
                    self,
                    move |_, _, _, event| match event {
                        gio::FileMonitorEvent::Changed
                        | gio::FileMonitorEvent::ChangesDoneHint
                        | gio::FileMonitorEvent::Created
                        | gio::FileMonitorEvent::AttributeChanged => {
                            win.enqueue(Command::DiskChanged)
                        }
                        gio::FileMonitorEvent::Deleted | gio::FileMonitorEvent::MovedOut => {
                            win.enqueue(Command::DiskDeleted)
                        }
                        _ => {}
                    }
                ));
                imp.document.borrow_mut().monitor = Some(monitor);
                imp.monitor_banner.set_revealed(false);
            }
            // Without the monitor another program's change is only found when the
            // document is next saved; say so rather than drop it quietly.
            Err(_) => imp.monitor_banner.set_revealed(true),
        }
    }

    /// Whether another program changed the file since it was last read or written.
    async fn current_file_changed_on_disk(&self) -> bool {
        let Some(path) = self.current_path() else {
            return false;
        };
        let Ok(current) = blocking(move || FileFingerprint::read_from_path(&path)).await else {
            return false;
        };
        self.imp()
            .document
            .borrow()
            .fingerprint
            .is_some_and(|known| current.conflicts_with(&known))
    }

    async fn handle_disk_changed(&self) {
        if self.imp().document.borrow().in_conflict || !self.current_file_changed_on_disk().await {
            return;
        }
        self.resolve_disk_conflict().await;
    }

    /// The file was reported deleted. If it is back (another program replaced it by
    /// renaming a new file over it), that is a change. Otherwise the document becomes
    /// untitled, so nothing is lost and the next save asks where to.
    async fn handle_disk_deleted(&self) {
        let path = self.current_path();
        if blocking(move || path.is_some_and(|path| path.exists())).await {
            self.handle_disk_changed().await;
            return;
        }
        let imp = self.imp();
        {
            let mut document = imp.document.borrow_mut();
            if let Some(monitor) = document.monitor.take() {
                monitor.cancel();
            }
            *document = State::default();
        }
        self.use_untitled_backup();
        imp.edit_buffer.set_modified(true);
        self.update_title();
        self.toast(&gettext(
            "File was deleted on disk — save to keep your changes",
        ));
        self.write_backup_now().await;
    }

    /// Ask whether to reload, overwrite or save elsewhere after another program changed
    /// the file. Autosave holds off until a load or a save settles it; cancelling the
    /// dialog, or a save that fails, leaves it holding off.
    async fn resolve_disk_conflict(&self) {
        let imp = self.imp();
        imp.document.borrow_mut().in_conflict = true;
        let alert = adw::AlertDialog::builder()
            .heading(gettext("File Changed on Disk"))
            .body(gettext(
                "This file was modified by another program. Reload to discard your changes, or overwrite to keep them.",
            ))
            .build();
        alert.add_response("save-as", &gettext("Save As…"));
        alert.add_response("overwrite", &gettext("Overwrite"));
        alert.add_response("reload", &gettext("Reload"));
        alert.set_response_appearance("overwrite", adw::ResponseAppearance::Destructive);
        match alert.choose_future(Some(self)).await.as_str() {
            "reload" => {
                if let Some(file) = self.current_file() {
                    // Reloading discards the changes but keeps the view the user is in.
                    let mode = imp.view_mode.get();
                    let last_single = imp.last_single_mode.get();
                    imp.edit_buffer.set_modified(false);
                    self.load_file(file).await;
                    imp.last_single_mode.set(last_single);
                    self.set_view_mode(mode);
                }
            }
            "overwrite" => {
                self.save_current().await;
            }
            "save-as" => {
                self.save_as().await;
            }
            _ => {}
        }
    }

    pub(super) fn present_error(&self, heading: String, body: String) {
        let alert = adw::AlertDialog::builder()
            .heading(heading)
            .body(body)
            .build();
        alert.add_response("ok", &gettext("OK"));
        alert.present(Some(self));
    }
}

/// Run blocking file IO on a worker thread.
pub(super) async fn blocking<T: Send + 'static>(work: impl FnOnce() -> T + Send + 'static) -> T {
    match gio::spawn_blocking(work).await {
        Ok(value) => value,
        Err(panic) => std::panic::resume_unwind(panic),
    }
}

pub fn file_title(file: &gio::File) -> String {
    file.basename()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| gettext("Untitled Document"))
}

/// The recent files list with `path` moved or added to the front, and no longer than
/// [`MAX_RECENT`].
fn push_recent(recent: glib::StrV, path: &Path) -> Vec<String> {
    let path = path.to_string_lossy();
    let mut list: Vec<String> = recent
        .iter()
        .map(|entry| entry.to_string())
        .filter(|entry| *entry != path)
        .collect();
    list.insert(0, path.into_owned());
    list.truncate(MAX_RECENT);
    list
}

/// Open and save dialogs offer Markdown first, then everything.
fn markdown_filters() -> (gio::ListStore, gtk::FileFilter) {
    let markdown = gtk::FileFilter::new();
    markdown.set_name(Some(&gettext("Markdown Files")));
    markdown.add_mime_type("text/markdown");
    markdown.add_suffix("md");
    markdown.add_suffix("markdown");
    let all = gtk::FileFilter::new();
    all.set_name(Some(&gettext("All Files")));
    all.add_pattern("*");
    let store = gio::ListStore::new::<gtk::FileFilter>();
    store.append(&markdown);
    store.append(&all);
    (store, markdown)
}

/// A short, translatable description of a file IO error, preferring friendly
/// phrasing for the common cases over the raw OS string.
pub(super) fn describe_io_error(err: &std::io::Error) -> String {
    use std::io::ErrorKind;
    match err.kind() {
        ErrorKind::PermissionDenied => gettext("You do not have permission to access this file."),
        ErrorKind::NotFound => gettext("The file no longer exists."),
        ErrorKind::StorageFull => gettext("There is not enough space left on the disk."),
        ErrorKind::ReadOnlyFilesystem => gettext("The location is read-only."),
        _ => err.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::{MAX_RECENT, push_recent};
    use gtk::glib;
    use std::path::Path;

    #[test]
    fn push_recent_dedups_and_caps() {
        let mut recent = glib::StrV::new();
        for i in 0..15 {
            recent = push_recent(recent, Path::new(&format!("/f{i}.md"))).into();
        }
        assert_eq!(recent.len(), MAX_RECENT);
        assert_eq!(recent[0].as_str(), "/f14.md");

        // Opening a listed file again moves it to the front without growing the list.
        let recent = push_recent(recent, Path::new("/f10.md"));
        assert_eq!(recent.len(), MAX_RECENT);
        assert_eq!(recent[0], "/f10.md");
        assert_eq!(recent.iter().filter(|p| *p == "/f10.md").count(), 1);
    }
}
