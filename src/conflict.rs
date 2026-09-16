//! Save durability and external-change detection.
//!
//! Pure logic with no GTK/GIO dependency so it can be unit tested. The GTK side
//! (file monitor, dialogs) lives in the window and drives these primitives. File
//! IO is synchronous; the window runs the writes on a worker thread.

use std::fs;
use std::io::{self, Write};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

/// Snapshot of a file's identity used to detect external modification.
///
/// `mtime` + `size` is a cheap fingerprint with no false positives. The inode
/// is intentionally excluded: an atomic rename by another program changes the
/// inode without changing content, which would otherwise read as a conflict.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileFingerprint {
    pub mtime: SystemTime,
    pub size: u64,
}

impl FileFingerprint {
    /// Reads the current fingerprint from disk. Follows symlinks (the same
    /// behaviour as a subsequent write through the symlink).
    pub fn read_from_path(path: &Path) -> io::Result<Self> {
        let meta = fs::metadata(path)?;
        Ok(Self {
            mtime: meta.modified()?,
            size: meta.len(),
        })
    }

    /// True when the on-disk file has diverged from this snapshot.
    pub fn conflicts_with(&self, other: &FileFingerprint) -> bool {
        self.mtime != other.mtime || self.size != other.size
    }
}

/// Writes `text` to `path` atomically and durably.
///
/// Temp-file + fsync + permission-copy + rename, then fsyncs the containing
/// directory so the rename itself survives a crash: on Linux a rename is not
/// crash-durable until the directory's metadata is flushed. The directory fsync
/// is best-effort — the rename has already taken effect, so a flush failure does
/// not roll it back.
pub fn write_text_atomically(path: &Path, text: impl AsRef<[u8]>) -> io::Result<()> {
    // Write through a symbolic link rather than over it: renaming onto the link would
    // turn it into a regular file and leave the file it points at unchanged.
    let resolved = fs::canonicalize(path).ok();
    let path = resolved.as_deref().unwrap_or(path);
    let directory = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("document");
    let suffix = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    let tmp_path = directory.join(format!(
        ".{}.blink-tmp-{}-{}",
        file_name,
        std::process::id(),
        suffix
    ));

    let write_result = (|| {
        // `create_new` refuses to open a pre-existing path or follow a planted
        // symlink at `tmp_path`; the pid+nanos suffix makes a legitimate
        // collision effectively impossible, so this only rejects an attack.
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp_path)?;
        file.write_all(text.as_ref())?;
        file.sync_all()
    })();
    if let Err(err) = write_result {
        let _ = fs::remove_file(&tmp_path);
        return Err(err);
    }

    if let Ok(metadata) = fs::metadata(path)
        && let Err(err) = fs::set_permissions(&tmp_path, metadata.permissions())
    {
        let _ = fs::remove_file(&tmp_path);
        return Err(err);
    }

    if let Err(err) = fs::rename(&tmp_path, path) {
        let _ = fs::remove_file(&tmp_path);
        return Err(err);
    }

    // Durably commit the rename. Best-effort; the rename is already applied.
    if let Ok(dir) = fs::File::open(directory) {
        let _ = dir.sync_all();
    }

    Ok(())
}

/// Result of an autosave that first checks for an external change.
#[derive(Debug)]
pub enum AutosaveOutcome {
    Saved,
    /// The disk file changed since `last_known`; the document was not written.
    ConflictDetected,
    /// The backing file is gone.
    FileDeleted,
    Failed(io::Error),
}

/// Saves only if the backing file still matches `last_known`, so a background
/// autosave can never silently clobber an external edit.
pub fn autosave_with_conflict_check(
    path: &Path,
    text: &str,
    last_known: &FileFingerprint,
) -> AutosaveOutcome {
    match FileFingerprint::read_from_path(path) {
        Ok(current) if current.conflicts_with(last_known) => AutosaveOutcome::ConflictDetected,
        Ok(_) => match write_text_atomically(path, text) {
            Ok(()) => AutosaveOutcome::Saved,
            Err(err) => AutosaveOutcome::Failed(err),
        },
        Err(err) if err.kind() == io::ErrorKind::NotFound => AutosaveOutcome::FileDeleted,
        Err(err) => AutosaveOutcome::Failed(err),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::time::Duration;

    fn unique_dir(tag: &str) -> PathBuf {
        let suffix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "blink-conflict-{}-{}-{}",
            tag,
            std::process::id(),
            suffix
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn fingerprint_detects_mtime_change() {
        let base = SystemTime::UNIX_EPOCH;
        let before = FileFingerprint {
            mtime: base,
            size: 100,
        };
        let after = FileFingerprint {
            mtime: base + Duration::from_secs(1),
            size: 100,
        };
        assert!(after.conflicts_with(&before));
    }

    #[test]
    fn fingerprint_detects_size_change() {
        let base = SystemTime::UNIX_EPOCH;
        let before = FileFingerprint {
            mtime: base,
            size: 100,
        };
        let after = FileFingerprint {
            mtime: base,
            size: 101,
        };
        assert!(after.conflicts_with(&before));
    }

    #[test]
    fn fingerprint_identical_is_not_conflict() {
        let fp = FileFingerprint {
            mtime: SystemTime::UNIX_EPOCH,
            size: 100,
        };
        assert!(!fp.conflicts_with(&fp));
    }

    #[test]
    fn atomic_write_replaces_existing_content() {
        let dir = unique_dir("replace");
        let path = dir.join("doc.md");
        fs::write(&path, "old").unwrap();

        write_text_atomically(&path, "new").unwrap();

        assert_eq!(fs::read_to_string(&path).unwrap(), "new");
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn atomic_write_leaves_no_temp_file() {
        let dir = unique_dir("notemp");
        let path = dir.join("doc.md");

        write_text_atomically(&path, "content").unwrap();

        let leftover: Vec<_> = fs::read_dir(&dir)
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| entry.file_name().to_string_lossy().contains("blink-tmp"))
            .collect();
        assert!(leftover.is_empty(), "temp file was left behind");
        let _ = fs::remove_dir_all(dir);
    }

    #[cfg(unix)]
    #[test]
    fn atomic_write_preserves_permissions() {
        use std::os::unix::fs::PermissionsExt;
        let dir = unique_dir("perms");
        let path = dir.join("doc.md");
        fs::write(&path, "old").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o640)).unwrap();

        write_text_atomically(&path, "new").unwrap();

        let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o640);
        let _ = fs::remove_dir_all(dir);
    }

    #[cfg(unix)]
    #[test]
    fn atomic_write_keeps_symlinks() {
        let dir = unique_dir("symlink");
        let target = dir.join("real.md");
        let link = dir.join("link.md");
        fs::write(&target, "old").unwrap();
        std::os::unix::fs::symlink(&target, &link).unwrap();

        write_text_atomically(&link, "new").unwrap();

        assert!(
            fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(fs::read_to_string(&target).unwrap(), "new");
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn autosave_detects_external_change() {
        let dir = unique_dir("autosave-conflict");
        let path = dir.join("doc.md");
        fs::write(&path, "original").unwrap();
        let snapshot = FileFingerprint::read_from_path(&path).unwrap();

        // External edit changes the size, forcing a conflict regardless of
        // filesystem mtime granularity.
        fs::write(&path, "externally edited content").unwrap();

        let outcome = autosave_with_conflict_check(&path, "our text", &snapshot);
        assert!(matches!(outcome, AutosaveOutcome::ConflictDetected));
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            "externally edited content"
        );
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn autosave_writes_when_unchanged() {
        let dir = unique_dir("autosave-ok");
        let path = dir.join("doc.md");
        fs::write(&path, "original").unwrap();
        let snapshot = FileFingerprint::read_from_path(&path).unwrap();

        let outcome = autosave_with_conflict_check(&path, "updated", &snapshot);
        assert!(matches!(outcome, AutosaveOutcome::Saved));
        assert_eq!(fs::read_to_string(&path).unwrap(), "updated");
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn fingerprint_reads_size_from_disk() {
        let dir = unique_dir("fingerprint");
        let path = dir.join("doc.md");
        fs::write(&path, "12345").unwrap();

        let fingerprint = FileFingerprint::read_from_path(&path).unwrap();
        assert_eq!(fingerprint.size, 5);

        let _ = fs::remove_dir_all(dir);
    }

    #[cfg(unix)]
    #[test]
    fn autosave_reports_write_failure() {
        use std::os::unix::fs::PermissionsExt;
        let dir = unique_dir("autosave-fail");
        let path = dir.join("doc.md");
        fs::write(&path, "original").unwrap();
        let snapshot = FileFingerprint::read_from_path(&path).unwrap();

        // Make the directory non-writable so the temp-file create fails, while
        // the fingerprint read still succeeds (file unchanged) -> Failed, not
        // ConflictDetected or FileDeleted.
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o500)).unwrap();
        // Root writes into a read-only directory regardless, as in a CI container;
        // there is no failure to provoke then.
        if fs::write(dir.join("probe"), "").is_ok() {
            fs::set_permissions(&dir, fs::Permissions::from_mode(0o700)).unwrap();
            let _ = fs::remove_dir_all(dir);
            return;
        }
        let outcome = autosave_with_conflict_check(&path, "new text", &snapshot);
        // Restore write permission so cleanup can remove the directory.
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o700)).unwrap();

        assert!(matches!(outcome, AutosaveOutcome::Failed(_)));
        assert_eq!(fs::read_to_string(&path).unwrap(), "original");

        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn autosave_reports_deleted_file() {
        let dir = unique_dir("autosave-deleted");
        let path = dir.join("doc.md");
        let snapshot = FileFingerprint {
            mtime: SystemTime::UNIX_EPOCH,
            size: 0,
        };

        let outcome = autosave_with_conflict_check(&path, "text", &snapshot);
        assert!(matches!(outcome, AutosaveOutcome::FileDeleted));
        let _ = fs::remove_dir_all(dir);
    }
}
