use anyhow::{Context, Result};
use fs2::FileExt;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

/// RAII exclusive file lock for serializing read-modify-write operations on
/// shared on-disk state. Released on drop.
pub struct LockGuard {
    file: File,
    #[allow(dead_code)]
    path: PathBuf,
}

impl Drop for LockGuard {
    fn drop(&mut self) {
        let _ = self.file.unlock();
    }
}

/// Acquire an exclusive `flock(2)` on `<dir>/<name>.lock`, creating the file
/// if needed. Blocks until the lock is available.
pub fn lock_file_in(dir: &Path, name: &str) -> Result<LockGuard> {
    fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    let path = dir.join(format!("{name}.lock"));
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&path)
        .with_context(|| format!("opening lock file {}", path.display()))?;
    file.lock_exclusive()
        .with_context(|| format!("acquiring lock on {}", path.display()))?;
    Ok(LockGuard { file, path })
}

/// Atomic-ish write: write to a sibling tempfile, then rename. SIGKILL between
/// write and rename leaves the tempfile but never a half-written `path`.
pub fn atomic_write(path: &Path, contents: &[u8]) -> Result<()> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let file_name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "out".into());
    let tmp = parent.join(format!(".{}.tmp.{}", file_name, std::process::id()));
    {
        let mut f = File::create(&tmp)
            .with_context(|| format!("creating temp file {}", tmp.display()))?;
        f.write_all(contents)
            .with_context(|| format!("writing temp file {}", tmp.display()))?;
        f.sync_all().ok();
    }
    fs::rename(&tmp, path)
        .with_context(|| format!("renaming {} -> {}", tmp.display(), path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::thread;
    use std::time::Duration;

    #[test]
    fn lock_serializes_concurrent_acquires() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().to_path_buf();

        let first = lock_file_in(&path, "test").expect("first lock");

        let (tx, rx) = mpsc::channel();
        let path_clone = path.clone();
        let handle = thread::spawn(move || {
            let _second = lock_file_in(&path_clone, "test").expect("second lock");
            tx.send(()).ok();
        });

        assert!(
            rx.recv_timeout(Duration::from_millis(150)).is_err(),
            "second lock acquired while first was still held"
        );

        drop(first);
        rx.recv_timeout(Duration::from_secs(2))
            .expect("second lock should acquire after first dropped");
        handle.join().unwrap();
    }
}
