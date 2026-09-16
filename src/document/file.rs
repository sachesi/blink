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

use super::{BlinkDocument, ViewMode, buffer_text};
use crate::application::BlinkApplication;
use crate::backup::BackupRecord;
use crate::config;
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
    /// Open a file in the document, which is blank.
    OpenFile(gio::File),
    Save,
    SaveAs,
    ExportHtml,
    ExportPdf,
    /// Ask about unsaved changes before the document closes, and answer whether it may.
    Close(async_channel::Sender<bool>),
    Autosave,
    Backup,
    DiskChanged,
    DiskDeleted,
    /// Put the text of a backup from a previous session into the document, which is blank.
    Restore(BackupRecord),
}

#[derive(Default)]
pub struct State {
    /// The file the document was opened from or saved to; none while untitled.
    pub file: Option<gio::File>,
    /// The file as it was when last read or written, to tell other programs' changes.
    pub fingerprint: Option<FileFingerprint>,
    /// The file's path with symbolic links resolved, which tells it apart from the other
    /// names the same file can be opened by.
    pub canonical: Option<PathBuf>,
    pub monitor: Option<gio::FileMonitor>,
    /// A change on disk is waiting for the user's decision; autosave holds off until then.
    pub in_conflict: bool,
}

impl BlinkDocument {
    pub(super) fn setup_document(&self) {
        let (sender, receiver) = async_channel::unbounded::<Command>();
        self.imp().commands.set(sender).ok();
        // The loop holds the document only while a command runs, so the document can go
        // away between them; dropping it drops the sender and ends the loop.
        let document = self.downgrade();
        glib::spawn_future_local(async move {
            while let Ok(command) = receiver.recv().await {
                let Some(document) = document.upgrade() else {
                    break;
                };
                document.run(command).await;
            }
        });

        let id = glib::timeout_add_seconds_local(
            AUTOSAVE_INTERVAL_SECS,
            glib::clone!(
                #[weak(rename_to = document)]
                self,
                #[upgrade_or]
                glib::ControlFlow::Break,
                move || {
                    document.enqueue(Command::Autosave);
                    glib::ControlFlow::Continue
                }
            ),
        );
        self.imp().autosave_timer.replace(Some(id));
    }

    pub fn enqueue(&self, command: Command) {
        if let Some(sender) = self.imp().commands.get() {
            sender.try_send(command).ok();
        }
    }

    async fn run(&self, command: Command) {
        match command {
            Command::OpenFile(file) => {
                let loaded = self.load_file(file).await;
                self.release_claim();
                // A tab opened for the file alone is of no use without it.
                if !loaded
                    && self.is_blank()
                    && let Some(window) = self.window()
                {
                    window.discard_blank(self);
                }
            }
            Command::Save => self.save().await,
            Command::SaveAs => {
                self.save_as().await;
            }
            Command::ExportHtml => self.export_html().await,
            Command::ExportPdf => self.export_pdf().await,
            Command::Close(answer) => {
                let close = self.confirm_close().await;
                answer.send(close).await.ok();
            }
            Command::Autosave => self.autosave().await,
            Command::Backup => self.write_backup_now().await,
            Command::DiskChanged => self.handle_disk_changed().await,
            Command::DiskDeleted => self.handle_disk_deleted().await,
            Command::Restore(record) => {
                self.restore_backup(record).await;
                self.release_claim();
            }
        }
    }

    /// Ask about unsaved changes, through the queue, and answer whether the document may
    /// close. Its backup goes once it may.
    pub async fn request_close(&self) -> bool {
        let (sender, receiver) = async_channel::bounded(1);
        self.enqueue(Command::Close(sender));
        receiver.recv().await.unwrap_or(false)
    }

    pub(super) fn current_file(&self) -> Option<gio::File> {
        self.imp().document.borrow().file.clone()
    }

    fn current_path(&self) -> Option<PathBuf> {
        self.current_file().and_then(|file| file.path())
    }

    /// Read `file` into the document. False when it could not be read.
    async fn load_file(&self, file: gio::File) -> bool {
        let Some(path) = file.path() else {
            self.present_error(
                gettext("Error Opening File"),
                gettext("Only local files are supported"),
            );
            return false;
        };
        let read_path = path.clone();
        // The fingerprint is taken right after the read, so a change made in between is
        // not taken for the text that was read.
        let read = blocking(move || {
            std::fs::read_to_string(&read_path).map(|text| {
                let fingerprint = FileFingerprint::read_from_path(&read_path).ok();
                (text, fingerprint, std::fs::canonicalize(&read_path).ok())
            })
        })
        .await;
        match read {
            Ok((text, fingerprint, canonical)) => {
                // Opened by another name, through a symbolic link, the file can be open
                // already.
                if let Some(other) = canonical
                    .as_deref()
                    .and_then(|path| self.document_elsewhere(None, Some(path)))
                {
                    other.present();
                    return false;
                }
                let imp = self.imp();
                imp.edit_buffer.set_text(&text);
                imp.edit_buffer.set_modified(false);
                {
                    let mut document = imp.document.borrow_mut();
                    document.fingerprint = fingerprint;
                    document.canonical = canonical;
                    document.file = Some(file);
                }
                self.update_title();
                self.adopt_file(&path).await;
                self.add_recent(&path);
                imp.preview.dirty.set(true);
                self.set_view_mode(ViewMode::Preview);
                true
            }
            Err(err) => {
                self.present_error(
                    gettext("Error Opening File"),
                    format!(
                        "{}\n\n{}",
                        gettext("Could not open the file"),
                        describe_io_error(&err)
                    ),
                );
                false
            }
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
            conflict::write_text_atomically(&write_path, &write_text).map(|()| {
                (
                    FileFingerprint::read_from_path(&write_path).ok(),
                    std::fs::canonicalize(&write_path).ok(),
                )
            })
        })
        .await;
        match written {
            Ok((fingerprint, canonical)) => {
                // Typing goes on while the write runs; the document is only saved if what
                // reached the disk is still what it holds.
                if buffer_text(&*imp.edit_buffer) == text {
                    imp.edit_buffer.set_modified(false);
                }
                {
                    let mut document = imp.document.borrow_mut();
                    document.fingerprint = fingerprint;
                    document.canonical = canonical;
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
        let Ok(file) = dialog.save_future(self.dialog_parent().as_ref()).await else {
            return false;
        };
        // Two documents saving to one file would each take the other's writes for a change
        // made by another program, and share one backup.
        let path = file.path();
        let canonical =
            blocking(move || path.and_then(|path| std::fs::canonicalize(path).ok())).await;
        if self
            .document_elsewhere(Some(&file), canonical.as_deref())
            .is_some()
        {
            self.present_error(
                gettext("Error Saving File"),
                gettext("\"{}\" is open in another tab or window. Close it there first, or save under another name.")
                    .replacen("{}", &file_title(&file), 1),
            );
            return false;
        }
        self.save_to(file).await
    }

    /// The document other than this one that holds `file`, or the file at the resolved
    /// path `canonical`.
    fn document_elsewhere(
        &self,
        file: Option<&gio::File>,
        canonical: Option<&Path>,
    ) -> Option<BlinkDocument> {
        gio::Application::default()
            .and_downcast::<BlinkApplication>()?
            .documents()
            .find(|document| document != self && document.holds_either(file, canonical))
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
        dialog
            .save_future(self.dialog_parent().as_ref())
            .await
            .ok()?
            .path()
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

    /// What an export of `text` takes from the document: the title, the folder of images,
    /// the fonts, the width and the colours of the code in the light style.
    async fn export_options(&self, text: &str) -> export::Options {
        let settings = self.settings();
        let (text_font, monospace_font) = (
            config::font_family(settings, false),
            config::font_family(settings, true),
        );
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

    async fn confirm_close(&self) -> bool {
        if self.imp().edit_buffer.is_modified() {
            self.present();
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
                return false;
            }
        }
        self.clear_backup().await;
        true
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
                    #[weak(rename_to = document)]
                    self,
                    move |_, _, _, event| match event {
                        gio::FileMonitorEvent::Changed
                        | gio::FileMonitorEvent::ChangesDoneHint
                        | gio::FileMonitorEvent::Created
                        | gio::FileMonitorEvent::AttributeChanged => {
                            document.enqueue(Command::DiskChanged)
                        }
                        gio::FileMonitorEvent::Deleted | gio::FileMonitorEvent::MovedOut => {
                            document.enqueue(Command::DiskDeleted)
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
        self.present();
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
        self.present();
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
pub fn markdown_filters() -> (gio::ListStore, gtk::FileFilter) {
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
