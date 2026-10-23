//! Replacing a file's contents safely, the way editors do.
//!
//! POSIX can't replace a file's contents atomically and keep its inode:
//! `rename` is atomic, but it puts a new inode under the name, which splits
//! hard links and loses an owner this process can't set. So, like Vim's
//! `backupcopy=auto`, the file is normally replaced by renaming a new file
//! over it, and rewritten in place, with a backup copy beside it, when
//! renaming would lose one of those.

use std::fs::{self, File, Metadata, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// Give `path` the contents `data`, creating it if it doesn't exist.
///
/// `path` must not be a symlink; the caller resolves it first, so the file
/// replaced is the real one rather than the link. A file this process can't
/// open for writing is refused, as an editor would. The temporary files this
/// makes go in the same directory, under names nobody else can predict or
/// have taken. A failure leaves the original as it was; if it struck after
/// the in-place write began, the message names the backup, which is kept.
pub fn replace(path: &Path, data: &[u8]) -> Result<(), String> {
    let shown = path.display();
    let existing = match fs::metadata(path) {
        Ok(meta) => Some(meta),
        Err(err) if err.kind() == io::ErrorKind::NotFound => None,
        Err(err) => return Err(format!("cannot read {shown}: {err}")),
    };
    // A file this process may not write is left alone, whichever way it
    // would be replaced: renaming over a read-only file would succeed, but
    // the mode says not to change it. Opening without truncating changes
    // nothing.
    if existing.is_some() {
        OpenOptions::new()
            .write(true)
            .open(path)
            .map_err(|err| format!("cannot write {shown}: {err}"))?;
    }
    let dir = match path.parent() {
        Some(dir) if !dir.as_os_str().is_empty() => dir,
        _ => Path::new("."),
    };
    let name = path
        .file_name()
        .map_or("file".into(), |n| n.to_string_lossy());
    // The new contents in a file of their own. A new file gets the usual
    // permissions (0666 less the umask); a replacement gets the original's.
    let mode = existing
        .as_ref()
        .map_or(0o666, |meta| meta.permissions().mode() & 0o7777);
    let (temp, mut file) = create_unique(dir, &format!(".{name}.rea-new-"), mode)
        .map_err(|err| format!("cannot create a temporary file in {}: {err}", dir.display()))?;
    let written = (|| {
        file.write_all(data)?;
        if let Some(meta) = &existing {
            // The mode passed at creation went through the umask; set it exactly.
            file.set_permissions(meta.permissions())?;
        }
        file.sync_all()?;
        file.metadata()
    })();
    let new_meta = match written {
        Ok(meta) => meta,
        Err(err) => {
            let _ = fs::remove_file(&temp);
            return Err(format!("cannot write {}: {err}", temp.display()));
        }
    };
    match existing {
        Some(meta) if !renaming_keeps(&meta, &new_meta) => {
            // The new file was only a way to find that out.
            let _ = fs::remove_file(&temp);
            rewrite_in_place(path, dir, &name, data)
        }
        _ => {
            if let Err(err) = fs::rename(&temp, path) {
                let _ = fs::remove_file(&temp);
                return Err(format!("cannot replace {shown}: {err}"));
            }
            // Make the rename itself durable; failing that loses nothing now.
            if let Ok(dir) = File::open(dir) {
                let _ = dir.sync_all();
            }
            Ok(())
        }
    }
}

/// Whether renaming `new` over the file described by `old` keeps everything
/// that matters about it: no other hard links, and the same owner and group
/// (which a process can't always give a file it creates).
fn renaming_keeps(old: &Metadata, new: &Metadata) -> bool {
    old.nlink() == 1 && old.uid() == new.uid() && old.gid() == new.gid()
}

/// Overwrite `path` in place, keeping its inode, with a backup of the
/// original beside it until the new contents are safely on disk.
fn rewrite_in_place(path: &Path, dir: &Path, name: &str, data: &[u8]) -> Result<(), String> {
    let shown = path.display();
    // Open it before anything else: if it can't be written (another owner,
    // no write permission), nothing has changed and no backup is needed.
    // Reading through the same handle backs up exactly the file overwritten.
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .map_err(|err| format!("cannot write {shown}: {err}"))?;
    let mut original = Vec::new();
    file.read_to_end(&mut original)
        .map_err(|err| format!("cannot read {shown}: {err}"))?;
    // Without a backup a crash mid-write could lose the file, so no backup
    // means no write.
    let (backup, mut copy) = create_unique(dir, &format!(".{name}.rea-backup-"), 0o600)
        .map_err(|err| format!("cannot create a backup in {}: {err}", dir.display()))?;
    if let Err(err) = copy
        .write_all(&original)
        .and_then(|()| copy.sync_all())
        // The backup's name must survive a crash too, not just its contents.
        .and_then(|()| File::open(dir)?.sync_all())
    {
        let _ = fs::remove_file(&backup);
        return Err(format!(
            "cannot write the backup {}: {err}",
            backup.display()
        ));
    }
    // Write over the old contents, then cut off whatever is left of them.
    // From here on a failure may leave the file half-written, so the backup
    // is kept and named.
    let rewritten = (|| {
        file.seek(SeekFrom::Start(0))?;
        file.write_all(data)?;
        file.set_len(data.len() as u64)?;
        file.sync_all()
    })();
    match rewritten {
        Ok(()) => {
            let _ = fs::remove_file(&backup);
            Ok(())
        }
        Err(err) => Err(format!(
            "cannot write {shown}: {err}; its previous contents are in {}",
            backup.display()
        )),
    }
}

/// Create a new file in `dir` named `prefix` plus a random suffix, failing
/// rather than opening anything that already exists.
///
/// `create_new` is `O_CREAT | O_EXCL`: it refuses an existing name,
/// symlinks included, so nothing planted under the name can redirect the
/// write. A taken name just means trying another suffix.
fn create_unique(dir: &Path, prefix: &str, mode: u32) -> io::Result<(PathBuf, File)> {
    // Arbitrary: enough attempts that only a broken directory exhausts them.
    const ATTEMPTS: u32 = 100;
    for attempt in 0..ATTEMPTS {
        let path = dir.join(format!("{prefix}{}", suffix(attempt)));
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(mode)
            .open(&path)
        {
            Ok(file) => return Ok((path, file)),
            Err(err) if err.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(err) => return Err(err),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "every temporary name tried was taken",
    ))
}

/// Eight hex digits that differ between calls and between processes.
///
/// Not cryptographic: `create_new` already makes a guessed name harmless, so
/// the suffix only has to avoid collisions.
fn suffix(attempt: u32) -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    // Mix the time, process and attempt so neighbours don't step in line.
    let mixed = (nanos as u64) ^ (u64::from(std::process::id()) << 32) ^ u64::from(attempt);
    let hashed = mixed.wrapping_mul(0x9E37_79B9_7F4A_7C15);
    format!("{:08x}", (hashed >> 32) as u32)
}
