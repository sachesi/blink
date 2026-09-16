//! Crash recovery: unsaved changes are copied to a backup a few seconds after typing stops
//! and when the window loses focus, and a backup left by a session that is gone is put
//! back into a document when the user asks for it. The application finds those backups
//! and holds the lock that tells this session's apart.

use adw::prelude::*;
use adw::subclass::prelude::*;
use gettextrs::gettext;
use gtk::{gio, glib};
use std::path::{Path, PathBuf};

use super::file::{Command, blocking, describe_io_error, file_title};
use super::{BlinkDocument, ViewMode, buffer_text};
use crate::backup::{self, BackupRecord};
use crate::conflict::FileFingerprint;

/// How long typing has to pause before unsaved changes are backed up.
const BACKUP_DELAY_SECS: u32 = 5;

pub struct State {
    pid: u32,
    /// Names the backup of an untitled document, unique to this run.
    instance_id: u64,
    backups_dir: PathBuf,
    /// The current document's backup.
    backup_id: String,
    /// Hash of what the backup holds, so an unchanged text is not written again.
    last_hash: Option<u64>,
    pub timer: Option<glib::SourceId>,
}

impl Default for State {
    fn default() -> Self {
        let instance_id = backup::new_instance_id();
        Self {
            pid: std::process::id(),
            instance_id,
            backups_dir: backup::backups_dir(),
            backup_id: backup::untitled_backup_id(instance_id),
            last_hash: None,
            timer: None,
        }
    }
}

impl BlinkDocument {
    pub(super) fn schedule_backup(&self) {
        let imp = self.imp();
        if !imp.edit_buffer.is_modified() {
            return;
        }
        let mut state = imp.recovery.borrow_mut();
        if let Some(id) = state.timer.take() {
            id.remove();
        }
        state.timer = Some(glib::timeout_add_seconds_local_once(
            BACKUP_DELAY_SECS,
            glib::clone!(
                #[weak(rename_to = document)]
                self,
                move || {
                    document.imp().recovery.borrow_mut().timer.take();
                    document.enqueue(Command::Backup);
                }
            ),
        ));
    }

    fn cancel_backup_timer(&self) {
        if let Some(id) = self.imp().recovery.borrow_mut().timer.take() {
            id.remove();
        }
    }

    pub(super) async fn write_backup_now(&self) {
        self.cancel_backup_timer();
        let imp = self.imp();
        if !imp.edit_buffer.is_modified() {
            return;
        }
        let text = buffer_text(&*imp.edit_buffer);
        let hash = backup::content_hash(&text);
        let (file, fingerprint) = {
            let document = imp.document.borrow();
            (document.file.clone(), document.fingerprint)
        };
        let (record, backups_dir) = {
            let state = imp.recovery.borrow();
            if state.last_hash == Some(hash) {
                return;
            }
            let record = BackupRecord {
                version: BackupRecord::CURRENT_VERSION,
                backup_id: state.backup_id.clone(),
                original_path: file.as_ref().and_then(|file| file.path()),
                original_mtime_secs: fingerprint.map(|fp| backup::system_time_to_secs(fp.mtime)),
                original_size: fingerprint.map(|fp| fp.size),
                backup_timestamp_secs: backup::now_secs(),
                owner_pid: state.pid,
                display_name: file
                    .as_ref()
                    .map(file_title)
                    .unwrap_or_else(|| gettext("Untitled Document")),
            };
            (record, state.backups_dir.clone())
        };
        match blocking(move || backup::write_backup(&backups_dir, &record, &text)).await {
            Ok(()) => imp.recovery.borrow_mut().last_hash = Some(hash),
            Err(err) => self.toast(&format!(
                "{}: {}",
                gettext("Backup failed"),
                describe_io_error(&err)
            )),
        }
    }

    /// Remove the current document's backup; its changes were saved or discarded.
    pub(super) async fn clear_backup(&self) {
        self.cancel_backup_timer();
        let (backups_dir, backup_id) = {
            let mut state = self.imp().recovery.borrow_mut();
            state.last_hash = None;
            (state.backups_dir.clone(), state.backup_id.clone())
        };
        blocking(move || backup::delete_backup(&backups_dir, &backup_id)).await;
    }

    pub(super) fn use_untitled_backup(&self) {
        let mut state = self.imp().recovery.borrow_mut();
        state.backup_id = backup::untitled_backup_id(state.instance_id);
        state.last_hash = None;
    }

    /// Switch to the backup of a file that was just opened or saved. A backup already there
    /// for that file is left over from a crash, and the user has just chosen the file's
    /// contents over it.
    pub(super) async fn use_backup_for(&self, path: &Path) {
        self.clear_backup().await;
        let backups_dir = self.imp().recovery.borrow().backups_dir.clone();
        let path = path.to_path_buf();
        let backup_id = blocking(move || {
            let backup_id = backup::backup_id_for_path(&path);
            backup::delete_backup(&backups_dir, &backup_id);
            backup_id
        })
        .await;
        let mut state = self.imp().recovery.borrow_mut();
        state.backup_id = backup_id;
        state.last_hash = None;
    }

    pub(super) async fn restore_backup(&self, record: BackupRecord) {
        let imp = self.imp();
        let backups_dir = imp.recovery.borrow().backups_dir.clone();
        // The backup, and the file it was of as it is now, if it is still there.
        let (read_dir, backup_id, original_path) = (
            backups_dir.clone(),
            record.backup_id.clone(),
            record.original_path.clone(),
        );
        let read = blocking(move || {
            backup::read_content(&read_dir, &backup_id).map(|content| {
                let original = original_path.filter(|path| path.exists()).map(|path| {
                    let fingerprint = FileFingerprint::read_from_path(&path).ok();
                    let canonical = std::fs::canonicalize(&path).ok();
                    let backup_id = backup::backup_id_for_path(&path);
                    (path, fingerprint, canonical, backup_id)
                });
                (content, original)
            })
        })
        .await;
        let (content, original) = match read {
            Ok(read) => read,
            Err(err) => {
                self.present_error(
                    gettext("Recovery Failed"),
                    format!(
                        "{}\n\n{}",
                        gettext("Could not read the recovered document"),
                        describe_io_error(&err)
                    ),
                );
                return;
            }
        };
        imp.edit_buffer.set_text(&content);
        imp.edit_buffer.set_modified(true);

        match original {
            Some((path, fingerprint, canonical, backup_id)) => {
                {
                    let mut document = imp.document.borrow_mut();
                    document.fingerprint = fingerprint;
                    document.canonical = canonical;
                    document.file = Some(gio::File::for_path(&path));
                }
                imp.recovery.borrow_mut().backup_id = backup_id;
                self.watch_file(&path);
            }
            None => {
                {
                    let mut document = imp.document.borrow_mut();
                    document.file = None;
                    document.fingerprint = None;
                    document.canonical = None;
                }
                self.use_untitled_backup();
            }
        }
        self.update_title();
        // The recovered text belongs to this session's backup now. Setting it in the editor
        // is not an edit, which would back it up, so it is backed up here, before the old
        // backup goes: a crash in between would lose it. A backup of the same file has the
        // same id, and has just been written over.
        imp.recovery.borrow_mut().last_hash = None;
        self.write_backup_now().await;
        let replaced = {
            let state = imp.recovery.borrow();
            state.last_hash.is_some() && state.backup_id != record.backup_id
        };
        if replaced {
            blocking(move || backup::delete_backup(&backups_dir, &record.backup_id)).await;
        }

        // Recovered work is unsaved: show it in the editor.
        imp.last_single_mode.set(ViewMode::Edit);
        imp.preview.dirty.set(true);
        self.set_view_mode(ViewMode::Edit);
    }
}
