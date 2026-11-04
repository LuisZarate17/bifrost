//! Where Bifrost keeps its own data: `%APPDATA%\bifrost`. The config,
//! conversations, attachments and undo backups all live there, shared by every
//! app and the CLI.
//!
//! Until Stage I the folder was `%APPDATA%\odin`. [`migrate`] moves it once,
//! whole, under the new name. Nothing stored inside it points back into it (undo
//! records find their backups by id, conversations name attachments by file
//! name), so a folder moved intact keeps working. Until the move happens,
//! [`data_dir`] keeps answering with the old folder, so nothing reads a
//! half-empty new one.
//!
//! Only the apps and the CLI migrate, first thing at start. The probes never do:
//! they read whichever folder [`data_dir`] resolves to, and some of them point
//! `APPDATA` somewhere else partway through a run — which is why nothing here
//! caches the answer.
//!
//! Every function has an `_in(base)` form that takes the folder `%APPDATA%`
//! would name, so tests run on a scratch folder and never touch the environment.

use std::path::{Path, PathBuf};
use std::time::Duration;

/// The folder's name under `%APPDATA%`.
pub const DIR: &str = "bifrost";

/// Its name before Stage I.
pub const LEGACY_DIR: &str = "odin";

/// Left in the old folder once its contents have moved, so the old folder no
/// longer counts as the live one.
pub const MOVED_MARKER: &str = "MOVED.txt";

const MOVED_NOTE: &str = "This folder moved to %APPDATA%\\bifrost when ODIN became Bifrost.\r\n\
Everything that was here is there now. This note stops the apps from treating\r\n\
this folder as the live one; it is safe to delete the folder.\r\n";

/// How the start-up move went, for the caller to log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Migration {
    /// Nothing to move: a fresh machine, or the move already happened.
    Nothing,
    /// `odin` became `bifrost` just now.
    Moved,
    /// The old folder still holds files and has no marker, but `bifrost` already
    /// exists. Nothing is touched and `bifrost` is used; someone has to decide
    /// which one is right.
    BothExist,
    /// The rename kept failing (something held a file open). Nothing moved, and
    /// [`data_dir`] keeps using the old folder until a later start succeeds.
    Blocked(String),
}

/// `%APPDATA%\bifrost`, or the old `%APPDATA%\odin` while that still holds the
/// live data. `None` if `%APPDATA%` is not set.
pub fn data_dir() -> Option<PathBuf> {
    appdata().map(|base| data_dir_in(&base))
}

/// [`data_dir`] under `base`:
/// - `base\bifrost` if it exists;
/// - else `base\odin` if it still holds the data (files, and no marker);
/// - else `base\bifrost`, which the first save creates.
pub fn data_dir_in(base: &Path) -> PathBuf {
    let new = base.join(DIR);
    if new.exists() {
        return new;
    }
    let old = base.join(LEGACY_DIR);
    if legacy_is_live(&old) {
        return old;
    }
    new
}

/// Move `%APPDATA%\odin` to `%APPDATA%\bifrost`, once. Safe to call on every
/// start: it does nothing unless there is an unmoved old folder and no new one.
pub fn migrate() -> Migration {
    match appdata() {
        Some(base) => migrate_in(&base),
        None => Migration::Nothing,
    }
}

/// [`migrate`] under `base`. Never copies, never overwrites: it is one rename of
/// the whole folder, or nothing.
pub fn migrate_in(base: &Path) -> Migration {
    let old = base.join(LEGACY_DIR);
    let new = base.join(DIR);
    if !legacy_is_live(&old) {
        return Migration::Nothing;
    }
    if new.exists() {
        return Migration::BothExist;
    }
    match rename_patiently(&old, &new) {
        Ok(()) => {
            // Best effort: the move itself already succeeded, and a missing note
            // only means the old folder is gone entirely, which is also fine.
            if std::fs::create_dir(&old).is_ok() {
                let _ = std::fs::write(old.join(MOVED_MARKER), MOVED_NOTE);
            }
            Migration::Moved
        }
        // Another app started at the same moment and moved it first. It re-creates
        // the old folder for its note an instant later, so the old folder may be
        // back already, empty or marked; either way it no longer holds the data.
        Err(_) if new.exists() && !legacy_is_live(&old) => Migration::Nothing,
        Err(e) => Migration::Blocked(e.to_string()),
    }
}

fn appdata() -> Option<PathBuf> {
    std::env::var_os("APPDATA").map(PathBuf::from)
}

/// The old folder still holds the live data: it holds files, and nothing has
/// marked it as moved. An empty one is not live: that is what is left when the
/// note could not be written, or was deleted without the folder.
fn legacy_is_live(old: &Path) -> bool {
    old.is_dir() && !old.join(MOVED_MARKER).exists() && holds_files(old)
}

/// Whether `dir` holds anything but folders, at any depth. Read-only. Links are
/// never followed; a link counts as something, and so does a folder that cannot
/// be read. A folder that is not there holds nothing.
fn holds_files(dir: &Path) -> bool {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) => return e.kind() != std::io::ErrorKind::NotFound,
    };
    for entry in entries {
        let Ok(entry) = entry else { return true };
        match entry.file_type() {
            Ok(t) if t.is_dir() && !t.is_symlink() => {
                if holds_files(&entry.path()) {
                    return true;
                }
            }
            _ => return true,
        }
    }
    false
}

/// Rename a folder, retrying for about two seconds while something holds a file
/// inside it open — Vidar checks the config's timestamp every five seconds, and
/// a scanner may be looking at a file just written. A folder that is not there,
/// or a target that already exists, fails at once.
fn rename_patiently(from: &Path, to: &Path) -> std::io::Result<()> {
    let mut tries = 0;
    loop {
        match std::fs::rename(from, to) {
            Err(e)
                if tries < 40
                    && !matches!(
                        e.kind(),
                        std::io::ErrorKind::NotFound | std::io::ErrorKind::AlreadyExists
                    )
                    && !to.exists() =>
            {
                tries += 1;
                std::thread::sleep(Duration::from_millis(50));
            }
            done => return done,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    /// A scratch folder standing in for `%APPDATA%`, removed when dropped.
    struct Base(PathBuf);

    impl Base {
        fn new(tag: &str) -> Self {
            static N: AtomicU32 = AtomicU32::new(0);
            let n = N.fetch_add(1, Ordering::Relaxed);
            let dir = std::env::temp_dir().join(format!("bifrost-paths-{tag}-{}-{n}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            Base(dir)
        }

        fn write(&self, rel: &str, body: &str) {
            let p = self.0.join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, body).unwrap();
        }

        fn read(&self, rel: &str) -> String {
            std::fs::read_to_string(self.0.join(rel)).unwrap()
        }
    }

    impl Drop for Base {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn a_fresh_machine_uses_the_new_folder_and_moves_nothing() {
        let b = Base::new("fresh");
        assert_eq!(data_dir_in(&b.0), b.0.join("bifrost"));
        assert_eq!(migrate_in(&b.0), Migration::Nothing);
        assert!(!b.0.join("bifrost").exists(), "nothing is created before a save");
        assert!(!b.0.join("odin").exists());
    }

    #[test]
    fn an_unmoved_old_folder_is_used_until_it_moves() {
        let b = Base::new("old-only");
        b.write("odin/config.json", "{\"model\":\"m\"}");
        assert_eq!(data_dir_in(&b.0), b.0.join("odin"));
    }

    #[test]
    fn the_move_keeps_every_file_and_marks_the_old_folder() {
        let b = Base::new("move");
        b.write("odin/config.json", "{\"model\":\"m\"}");
        b.write("odin/conversations/a.json", "A");
        b.write("odin/attachments/x.png", "X");
        b.write("odin/backups/1.bak", "B");
        b.write("odin/backups/1.json", "R");

        assert_eq!(migrate_in(&b.0), Migration::Moved);

        assert_eq!(b.read("bifrost/config.json"), "{\"model\":\"m\"}");
        assert_eq!(b.read("bifrost/conversations/a.json"), "A");
        assert_eq!(b.read("bifrost/attachments/x.png"), "X");
        assert_eq!(b.read("bifrost/backups/1.bak"), "B");
        assert_eq!(b.read("bifrost/backups/1.json"), "R");
        assert!(b.0.join("odin").join(MOVED_MARKER).exists());
        assert_eq!(data_dir_in(&b.0), b.0.join("bifrost"));

        // A second start finds nothing left to do.
        assert_eq!(migrate_in(&b.0), Migration::Nothing);
    }

    #[test]
    fn only_the_new_folder_is_used_as_it_is() {
        let b = Base::new("new-only");
        b.write("bifrost/config.json", "{}");
        assert_eq!(data_dir_in(&b.0), b.0.join("bifrost"));
        assert_eq!(migrate_in(&b.0), Migration::Nothing);
    }

    #[test]
    fn two_live_folders_are_left_alone() {
        let b = Base::new("both");
        b.write("odin/config.json", "old");
        b.write("bifrost/config.json", "new");
        assert_eq!(migrate_in(&b.0), Migration::BothExist);
        assert_eq!(b.read("odin/config.json"), "old", "the old folder is untouched");
        assert_eq!(b.read("bifrost/config.json"), "new", "the new folder is untouched");
        assert!(!b.0.join("odin").join(MOVED_MARKER).exists());
    }

    #[test]
    fn a_marked_old_folder_is_not_live() {
        let b = Base::new("marked");
        b.write(&format!("odin/{MOVED_MARKER}"), "moved");
        b.write("bifrost/config.json", "{}");
        assert_eq!(migrate_in(&b.0), Migration::Nothing);
        assert_eq!(data_dir_in(&b.0), b.0.join("bifrost"));
    }

    #[test]
    fn an_empty_old_folder_is_not_live() {
        // What is left when the note was deleted without the folder, or could not
        // be written, or when a second app lost the race to move it: no warning,
        // and nothing to move.
        let b = Base::new("empty-old");
        std::fs::create_dir_all(b.0.join("odin/conversations")).unwrap();
        b.write("bifrost/config.json", "{}");
        assert_eq!(migrate_in(&b.0), Migration::Nothing);
        assert_eq!(data_dir_in(&b.0), b.0.join("bifrost"));

        // With nothing else there, it is a fresh machine.
        std::fs::remove_dir_all(b.0.join("bifrost")).unwrap();
        assert_eq!(migrate_in(&b.0), Migration::Nothing);
        assert_eq!(data_dir_in(&b.0), b.0.join("bifrost"));
        assert!(!b.0.join("bifrost").exists(), "nothing is created before a save");
    }

    #[test]
    fn one_file_deep_in_the_old_folder_keeps_it_live() {
        let b = Base::new("deep");
        b.write("odin/backups/1.bak", "B");
        assert_eq!(data_dir_in(&b.0), b.0.join("odin"));
        assert_eq!(migrate_in(&b.0), Migration::Moved);
        assert_eq!(b.read("bifrost/backups/1.bak"), "B");
    }

    #[cfg(windows)]
    #[test]
    fn a_held_file_blocks_the_move_and_nothing_is_lost() {
        use std::os::windows::fs::OpenOptionsExt;
        let b = Base::new("held");
        b.write("odin/config.json", "{\"model\":\"m\"}");
        // Opened with no sharing at all: Windows cannot rename the folder around it.
        let held = std::fs::OpenOptions::new()
            .read(true)
            .share_mode(0)
            .open(b.0.join("odin/config.json"))
            .unwrap();

        let result = migrate_in(&b.0);
        assert!(matches!(result, Migration::Blocked(_)), "{result:?}");
        drop(held);

        assert_eq!(b.read("odin/config.json"), "{\"model\":\"m\"}");
        assert!(!b.0.join("bifrost").exists(), "nothing was half-made");
        assert_eq!(
            data_dir_in(&b.0),
            b.0.join("odin"),
            "the old folder is still the live one"
        );

        // Released, the next start moves it.
        assert_eq!(migrate_in(&b.0), Migration::Moved);
        assert_eq!(b.read("bifrost/config.json"), "{\"model\":\"m\"}");
    }
}
