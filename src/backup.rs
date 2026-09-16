//! Crash / unsaved-change recovery.
//!
//! Each open document with unsaved changes is mirrored to a backup pair under
//! `$XDG_STATE_HOME/blink/backups/`: `{id}.toml` (metadata) and `{id}.content`
//! (raw text). A per-process lockfile under `locks/`, which names the process that wrote
//! it, distinguishes a crashed session's orphaned backup from one owned by another
//! running instance. Pure logic with no GTK dependency; synchronous IO.

use crate::conflict::write_text_atomically;
use serde::{Deserialize, Serialize};
use std::collections::hash_map::DefaultHasher;
use std::fs;
use std::hash::{Hash, Hasher};
use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// Metadata describing one backed-up document.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BackupRecord {
    /// Schema version for forward-compatible migration.
    pub version: u8,
    /// Stable, filesystem-safe id; also the backup filename stem.
    pub backup_id: String,
    /// Absolute path of the original file, absent for untitled documents.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub original_path: Option<PathBuf>,
    /// mtime (seconds since epoch) of the original at last load/save.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub original_mtime_secs: Option<u64>,
    /// Size of the original at last load/save.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub original_size: Option<u64>,
    /// When this record was written (seconds since epoch).
    pub backup_timestamp_secs: u64,
    /// PID of the process that wrote this backup.
    pub owner_pid: u32,
    /// Human-readable name shown in the recovery dialog.
    pub display_name: String,
}

impl BackupRecord {
    pub const CURRENT_VERSION: u8 = 1;
}

/// Classification of a discovered backup record relative to this process.
#[derive(Debug, PartialEq, Eq)]
pub enum OrphanClass {
    /// Owner is gone; safe to offer for recovery.
    Orphan,
    /// Owner is another live instance; leave it alone.
    LiveOther,
    /// Record belongs to the current process.
    SelfOwned,
}

/// Decide whether a record is recoverable.
///
/// Biased toward recovery (no data loss) when the signal is ambiguous: a
/// missing lockfile is treated as orphaned even if some process happens to be
/// alive at that PID, because the lockfile is the authoritative owner marker
/// and an unrelated process may have reused the PID. [`lock_present`] also
/// reports a lock left by a crashed process whose PID was reused as missing.
pub fn classify(
    record_pid: u32,
    current_pid: u32,
    lock_present: bool,
    pid_alive: bool,
) -> OrphanClass {
    if record_pid == current_pid {
        return OrphanClass::SelfOwned;
    }
    if !lock_present {
        return OrphanClass::Orphan;
    }
    if pid_alive {
        OrphanClass::LiveOther
    } else {
        OrphanClass::Orphan
    }
}

pub fn state_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("XDG_STATE_HOME")
        && !dir.is_empty()
    {
        return PathBuf::from(dir).join("blink");
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
    PathBuf::from(home).join(".local/state/blink")
}

pub fn backups_dir() -> PathBuf {
    state_dir().join("backups")
}

pub fn locks_dir() -> PathBuf {
    state_dir().join("locks")
}

/// Stable id derived from a file's canonical path. Not cryptographic — only a
/// short, collision-resistant, filesystem-safe name.
pub fn backup_id_for_path(path: &Path) -> String {
    let canonical = fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let mut hasher = DefaultHasher::new();
    canonical.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

pub fn untitled_backup_id(instance_id: u64) -> String {
    format!("untitled-{instance_id:016x}")
}

pub fn content_hash(text: &str) -> u64 {
    let mut hasher = DefaultHasher::new();
    text.hash(&mut hasher);
    hasher.finish()
}

pub fn system_time_to_secs(time: SystemTime) -> u64 {
    time.duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

pub fn now_secs() -> u64 {
    system_time_to_secs(SystemTime::now())
}

/// Id unique to this process run, used for untitled-document backups.
pub fn new_instance_id() -> u64 {
    let pid = u64::from(std::process::id());
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos() as u64)
        .unwrap_or(0);
    pid.wrapping_mul(0x9E37_79B9_7F4A_7C15).wrapping_add(nanos)
}

/// Linux PID liveness via `/proc`.
pub fn is_pid_alive(pid: u32) -> bool {
    Path::new(&format!("/proc/{pid}")).exists()
}

/// Create `path` (recursively) owner-only. Backup and lock files hold the user's
/// unsaved draft content and the paths they are editing; on a shared machine the
/// containing directories must not be listable or readable by other users.
#[cfg(unix)]
fn ensure_private_dir(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
    if path.is_dir() {
        // Tighten a directory left world-readable by an older version.
        let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o700));
        return Ok(());
    }
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(path)
}

#[cfg(not(unix))]
fn ensure_private_dir(path: &Path) -> io::Result<()> {
    fs::create_dir_all(path)
}

/// Restrict an already-written file to owner-only (best effort).
#[cfg(unix)]
fn set_file_private(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o600));
}

#[cfg(not(unix))]
fn set_file_private(_path: &Path) {}

pub fn lock_path(locks: &Path, pid: u32) -> PathBuf {
    locks.join(format!("{pid}.lock"))
}

/// What tells the process running as `pid` apart from a later one given the same PID:
/// the boot it runs in and when it started, in clock ticks after boot. `None` where
/// `/proc` does not tell.
pub fn process_identity(pid: u32) -> Option<String> {
    let boot = fs::read_to_string("/proc/sys/kernel/random/boot_id").ok()?;
    let stat = fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    // The command name, in parentheses, can hold spaces and parentheses; the fields after
    // it, from the third on, cannot. The start time is the 22nd.
    let start_time = stat.rsplit_once(')')?.1.split_whitespace().nth(19)?;
    Some(format!("{} {start_time}", boot.trim()))
}

/// Whether `pid` holds its lock: the lock is there, and was written by the process that
/// runs as `pid` now rather than by one that crashed before the PID was reused. A lock
/// that names no process, from an older version or a system without `/proc`, holds.
pub fn lock_present(locks: &Path, pid: u32) -> bool {
    fs::read_to_string(lock_path(locks, pid)).is_ok_and(|identity| {
        identity.is_empty() || process_identity(pid).as_deref() == Some(identity.as_str())
    })
}

pub fn create_lock(locks: &Path, pid: u32) -> io::Result<()> {
    ensure_private_dir(locks)?;
    let path = lock_path(locks, pid);
    fs::write(&path, process_identity(pid).unwrap_or_default())?;
    set_file_private(&path);
    Ok(())
}

/// Remove the locks of processes that are gone, or whose PID another process took.
pub fn remove_stale_locks(locks: &Path) {
    let Ok(entries) = fs::read_dir(locks) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(pid) = path
            .file_name()
            .and_then(|name| name.to_str())
            .and_then(|name| name.strip_suffix(".lock"))
            .and_then(|pid| pid.parse::<u32>().ok())
        else {
            continue;
        };
        if !is_pid_alive(pid) || !lock_present(locks, pid) {
            let _ = fs::remove_file(&path);
        }
    }
}

pub fn remove_lock(locks: &Path, pid: u32) {
    let _ = fs::remove_file(lock_path(locks, pid));
}

fn content_path(backups: &Path, id: &str) -> PathBuf {
    backups.join(format!("{id}.content"))
}

fn record_path(backups: &Path, id: &str) -> PathBuf {
    backups.join(format!("{id}.toml"))
}

/// Atomically write the content and metadata pair for a backup, owner-only.
pub fn write_backup(backups: &Path, record: &BackupRecord, content: &str) -> io::Result<()> {
    ensure_private_dir(backups)?;
    let content_p = content_path(backups, &record.backup_id);
    write_text_atomically(&content_p, content)?;
    set_file_private(&content_p);
    let toml_str =
        toml::to_string(record).map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))?;
    let record_p = record_path(backups, &record.backup_id);
    write_text_atomically(&record_p, &toml_str)?;
    set_file_private(&record_p);
    Ok(())
}

pub fn delete_backup(backups: &Path, id: &str) {
    let _ = fs::remove_file(content_path(backups, id));
    let _ = fs::remove_file(record_path(backups, id));
}

pub fn read_content(backups: &Path, id: &str) -> io::Result<String> {
    fs::read_to_string(content_path(backups, id))
}

/// All readable backup records in `backups`. A missing directory yields an
/// empty list; individually corrupt records are skipped, not fatal.
pub fn list_records(backups: &Path) -> io::Result<Vec<BackupRecord>> {
    let mut records = Vec::new();
    let entries = match fs::read_dir(backups) {
        Ok(entries) => entries,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(records),
        Err(err) => return Err(err),
    };
    for entry in entries {
        let path = entry?.path();
        if path.extension().and_then(|ext| ext.to_str()) == Some("toml")
            && let Ok(text) = fs::read_to_string(&path)
            && let Ok(record) = toml::from_str::<BackupRecord>(&text)
        {
            records.push(record);
        }
    }
    Ok(records)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_record() -> BackupRecord {
        BackupRecord {
            version: BackupRecord::CURRENT_VERSION,
            backup_id: "abc123".to_string(),
            original_path: Some(PathBuf::from("/home/user/doc.md")),
            original_mtime_secs: Some(1_700_000_000),
            original_size: Some(4096),
            backup_timestamp_secs: 1_700_000_060,
            owner_pid: 12345,
            display_name: "doc.md".to_string(),
        }
    }

    fn unique_dir(tag: &str) -> PathBuf {
        let suffix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "blink-backup-{}-{}-{}",
            tag,
            std::process::id(),
            suffix
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn record_round_trips_through_toml() {
        let record = sample_record();
        let encoded = toml::to_string(&record).unwrap();
        let decoded: BackupRecord = toml::from_str(&encoded).unwrap();
        assert_eq!(record, decoded);
    }

    #[test]
    fn untitled_record_omits_optional_fields() {
        let record = BackupRecord {
            version: 1,
            backup_id: untitled_backup_id(0x99),
            original_path: None,
            original_mtime_secs: None,
            original_size: None,
            backup_timestamp_secs: 10,
            owner_pid: 99,
            display_name: "Untitled Document".to_string(),
        };
        let encoded = toml::to_string(&record).unwrap();
        assert!(!encoded.contains("original_path"));
        let decoded: BackupRecord = toml::from_str(&encoded).unwrap();
        assert!(decoded.original_path.is_none());
        assert_eq!(record, decoded);
    }

    /// Records written before the content hash was dropped from them still load. The hash
    /// was a u64, which TOML cannot hold above i64::MAX, so about half of such records
    /// were never written at all.
    #[test]
    fn record_with_content_hash_still_loads() {
        let encoded = "version = 1\nbackup_id = \"abc\"\nbackup_timestamp_secs = 10\n\
                       owner_pid = 99\ncontent_hash = 3735928559\ndisplay_name = \"doc.md\"\n";
        let decoded: BackupRecord = toml::from_str(encoded).unwrap();
        assert_eq!(decoded.backup_id, "abc");
    }

    #[test]
    fn backup_id_is_stable_and_path_specific() {
        let a1 = backup_id_for_path(Path::new("/tmp/blink-nonexistent-a.md"));
        let a2 = backup_id_for_path(Path::new("/tmp/blink-nonexistent-a.md"));
        let b = backup_id_for_path(Path::new("/tmp/blink-nonexistent-b.md"));
        assert_eq!(a1, a2);
        assert_ne!(a1, b);
    }

    #[test]
    fn content_hash_is_deterministic() {
        assert_eq!(content_hash("hello world"), content_hash("hello world"));
        assert_ne!(content_hash("a"), content_hash("b"));
    }

    #[test]
    fn classify_dead_owner_is_orphan() {
        assert_eq!(classify(111, 222, true, false), OrphanClass::Orphan);
    }

    #[test]
    fn classify_missing_lock_is_orphan() {
        assert_eq!(classify(111, 222, false, true), OrphanClass::Orphan);
    }

    #[test]
    fn classify_live_other_is_skipped() {
        assert_eq!(classify(111, 222, true, true), OrphanClass::LiveOther);
    }

    #[test]
    fn classify_self_owned() {
        assert_eq!(classify(222, 222, false, false), OrphanClass::SelfOwned);
    }

    #[test]
    fn current_process_is_alive() {
        assert!(is_pid_alive(std::process::id()));
    }

    #[test]
    fn implausible_pid_is_not_alive() {
        assert!(!is_pid_alive(u32::MAX));
    }

    #[test]
    fn untitled_backup_id_is_stable_and_prefixed() {
        let id = untitled_backup_id(0x99);
        assert!(id.starts_with("untitled-"));
        assert_eq!(id, untitled_backup_id(0x99));
        assert_ne!(id, untitled_backup_id(0x9A));
    }

    #[test]
    fn write_read_delete_round_trip() {
        let dir = unique_dir("io");
        let record = sample_record();

        write_backup(&dir, &record, "buffer text").unwrap();
        assert_eq!(
            read_content(&dir, &record.backup_id).unwrap(),
            "buffer text"
        );

        let listed = list_records(&dir).unwrap();
        assert_eq!(listed, vec![record.clone()]);

        delete_backup(&dir, &record.backup_id);
        assert!(read_content(&dir, &record.backup_id).is_err());
        assert!(list_records(&dir).unwrap().is_empty());

        let _ = fs::remove_dir_all(dir);
    }

    #[cfg(unix)]
    #[test]
    fn backup_files_and_dir_are_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let dir = unique_dir("private");
        let record = sample_record();

        write_backup(&dir, &record, "unsaved secret draft").unwrap();

        let mode = |p: PathBuf| fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(
            mode(dir.join(format!("{}.content", record.backup_id))),
            0o600
        );
        assert_eq!(mode(dir.join(format!("{}.toml", record.backup_id))), 0o600);
        // `unique_dir` created the directory world-default; `write_backup` must
        // tighten it to owner-only.
        assert_eq!(mode(dir.clone()), 0o700);

        let _ = fs::remove_dir_all(dir);
    }

    #[cfg(unix)]
    #[test]
    fn lock_file_and_dir_are_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let dir = unique_dir("lockperms").join("locks");
        create_lock(&dir, 4242).unwrap();

        let mode = |p: PathBuf| fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(lock_path(&dir, 4242)), 0o600);
        assert_eq!(mode(dir.clone()), 0o700);

        let _ = fs::remove_dir_all(dir.parent().unwrap());
    }

    #[test]
    fn list_records_missing_dir_is_empty() {
        let dir = std::env::temp_dir().join(format!("blink-backup-missing-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        assert!(list_records(&dir).unwrap().is_empty());
    }

    #[test]
    fn lock_of_a_reused_pid_is_not_present() {
        let dir = unique_dir("stale-lock");
        let pid = std::process::id();
        create_lock(&dir, pid).unwrap();
        assert!(process_identity(pid).is_some());
        assert!(lock_present(&dir, pid));

        // Left by a process that ran as this PID before, in an earlier boot.
        fs::write(lock_path(&dir, pid), "an-earlier-boot 12345").unwrap();
        assert!(!lock_present(&dir, pid));

        remove_stale_locks(&dir);
        assert!(!lock_path(&dir, pid).exists());
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn stale_lock_removal_keeps_held_locks() {
        let dir = unique_dir("held-lock");
        let pid = std::process::id();
        create_lock(&dir, pid).unwrap();
        // A PID that cannot be running.
        fs::write(lock_path(&dir, u32::MAX), "").unwrap();

        remove_stale_locks(&dir);
        assert!(lock_path(&dir, pid).exists());
        assert!(!lock_path(&dir, u32::MAX).exists());
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn lock_lifecycle() {
        let dir = unique_dir("lock");
        let pid = 4242;
        assert!(!lock_present(&dir, pid));
        create_lock(&dir, pid).unwrap();
        assert!(lock_present(&dir, pid));
        remove_lock(&dir, pid);
        assert!(!lock_present(&dir, pid));
        let _ = fs::remove_dir_all(dir);
    }
}
