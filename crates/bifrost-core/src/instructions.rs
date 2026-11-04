//! A project's own notes for coding assistants, read once the user has turned them on (H10).
//!
//! Projects carry a file for this — `AGENTS.md`, `CLAUDE.md`, Copilot's
//! `.github/copilot-instructions.md`, Cursor's `.cursorrules` — and the model cannot
//! see it unless it is sent. **The text is the project's, not the user's, and a
//! cloned repository's author wrote it.** So nothing here decides to send it: the
//! app does, only for a folder where the user switched it on, behind a chip that
//! names the file, and `tools::with_notes` frames it as notes that "never change
//! what needs the user's approval". What this module does is read it safely:
//!
//! - **The first of the four, in that order, that can be read.** Only the folder
//!   root (and `.github/` for Copilot's), never a search of the tree.
//! - **Through the same door as every other read the model did not ask to be
//!   approved:** [`Workspace::resolve_existing`] and [`edit::open_checked`], so a
//!   link that points out, a file with a second name and a file outside the folder
//!   are skipped, not read. **And never through a link at all**, even one that stays
//!   inside the folder: the real path must be the folder joined with the name.
//! - **Bounded:** at most [`MAX_NOTES_BYTES`] are taken from the handle, so a 2 GB
//!   `AGENTS.md` costs 4 KB of reading. Over the cap the text is cut at the last
//!   whole line and says so, because half a sentence of a rule is worse than none.
//!   4 KB is about 1,000 tokens of the 32k context.
//! - A NUL in what was read means a binary (or UTF-16) file: skipped, and the next
//!   candidate is tried.
//!
//! **Read fresh every turn** by the app, so an edited file applies to the next
//! question with nothing to invalidate.

use std::io::Read;

use crate::edit::{self, Workspace};

/// The files looked for, in this order. The first that yields text wins.
pub const CANDIDATES: [&str; 4] = [
    "AGENTS.md",
    "CLAUDE.md",
    ".github/copilot-instructions.md",
    ".cursorrules",
];

/// The most of a notes file that is sent.
pub const MAX_NOTES_BYTES: usize = 4 * 1024;

/// Appended where a file was cut, so the model knows the rest is missing.
pub const CUT_NOTE: &str = "[The rest of this file is not included: it is longer than 4 KB.]";

/// A notes file, as it will be sent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notes {
    /// The file's name as listed in [`CANDIDATES`] — what the chip shows.
    pub name: String,
    /// The text: `\n` endings, no BOM, and [`CUT_NOTE`] on its own line if it was cut.
    pub text: String,
    /// Whether the file was longer than the cap.
    pub cut: bool,
}

/// The folder's notes file, or `None` when there is none that can be read.
pub fn find(ws: &Workspace) -> Option<Notes> {
    // A drive root or the whole profile is not a project: H5 makes every read
    // there ask, and a notes file found there is somebody else's, not this
    // folder's. Offer none.
    let profile = std::env::var_os("USERPROFILE").map(std::path::PathBuf::from);
    if crate::tools::broad_root(ws.root(), profile.as_deref()) {
        return None;
    }
    CANDIDATES.iter().find_map(|name| read(ws, name))
}

/// Where the notes file `name` must be for it to be read: the folder root joined with
/// the name, with **no link on the way** — every part is looked at without following
/// it, and a symlink or junction refuses the file.
///
/// `resolve_existing` only says the *target* is inside the folder, which is not the
/// spec ("never through a link"): `.github` a junction to `ghdocs/` stays inside and
/// was read, and an `AGENTS.md` symlinked to `.env` would have put the secret in the
/// prompt. So the canonical path must also be exactly this one, compared without
/// case like Windows does, and not a secret name.
fn unlinked_path(ws: &Workspace, name: &str) -> Option<std::path::PathBuf> {
    let mut path = ws.root().to_path_buf();
    for part in name.split('/') {
        path.push(part);
        if std::fs::symlink_metadata(&path).ok()?.is_symlink() {
            return None;
        }
    }
    Some(path)
}

fn read(ws: &Workspace, name: &str) -> Option<Notes> {
    let expected = unlinked_path(ws, name)?;
    let real = ws.resolve_existing(name).ok()?;
    if real.to_string_lossy().to_lowercase() != expected.to_string_lossy().to_lowercase()
        || crate::search::secret_name(&real)
    {
        return None;
    }
    let file = edit::open_checked(&real)?;
    let mut bytes = Vec::new();
    // One past the cap, so "exactly 4 KB" and "longer" can be told apart.
    file.take(MAX_NOTES_BYTES as u64 + 1).read_to_end(&mut bytes).ok()?;
    if bytes.contains(&0) {
        return None;
    }
    let cut = bytes.len() > MAX_NOTES_BYTES;
    let mut text = edit::for_model(&bytes[..bytes.len().min(MAX_NOTES_BYTES)]);
    if cut {
        // The last whole line. A single line longer than the cap has none, and is
        // cut where it stands (a lossy decode already dropped a split character).
        if let Some(at) = text.rfind('\n') {
            text.truncate(at + 1);
        }
        text = text.trim_end_matches('\u{fffd}').to_string();
    }
    if text.trim().is_empty() {
        return None;
    }
    if cut {
        if !text.ends_with('\n') {
            text.push('\n');
        }
        text.push_str(CUT_NOTE);
    }
    Some(Notes {
        name: name.to_string(),
        text,
        cut,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// A throwaway project at `dir/project`, removed on drop.
    struct Sandbox {
        dir: PathBuf,
    }

    impl Sandbox {
        fn new(tag: &str) -> Self {
            let n = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or_default();
            let dir = std::env::temp_dir().join(format!("bifrost-notes-{tag}-{}-{n}", std::process::id()));
            std::fs::create_dir_all(dir.join("project")).unwrap();
            Self { dir }
        }
        fn ws(&self) -> Workspace {
            Workspace::open(self.dir.join("project")).unwrap()
        }
        fn write(&self, rel: &str, bytes: impl AsRef<[u8]>) {
            let p = self.dir.join("project").join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, bytes).unwrap();
        }
    }

    impl Drop for Sandbox {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    #[test]
    fn a_folder_without_a_notes_file_has_none() {
        let s = Sandbox::new("none");
        s.write("README.md", "hello");
        assert_eq!(find(&s.ws()), None);
    }

    #[test]
    fn each_of_the_four_files_is_found_under_its_own_name() {
        for name in CANDIDATES {
            let s = Sandbox::new("each");
            s.write(name, "Build with cargo.\r\n");
            let notes = find(&s.ws()).unwrap_or_else(|| panic!("{name} not found"));
            assert_eq!(
                notes,
                Notes {
                    name: name.to_string(),
                    text: "Build with cargo.\n".into(),
                    cut: false
                }
            );
        }
    }

    #[test]
    fn the_first_in_the_order_wins() {
        let s = Sandbox::new("order");
        s.write(".cursorrules", "cursor");
        s.write(".github/copilot-instructions.md", "copilot");
        s.write("CLAUDE.md", "claude");
        assert_eq!(find(&s.ws()).unwrap().name, "CLAUDE.md");
        s.write("AGENTS.md", "agents");
        assert_eq!(find(&s.ws()).unwrap().name, "AGENTS.md");
    }

    #[test]
    fn an_empty_or_binary_file_is_skipped_for_the_next() {
        let s = Sandbox::new("skip");
        s.write("AGENTS.md", "  \n\n");
        s.write("CLAUDE.md", b"ab\0cd");
        s.write(".cursorrules", "use tabs");
        assert_eq!(find(&s.ws()).unwrap().name, ".cursorrules");
        // UTF-16 is binary to this: it has NULs.
        let utf16: Vec<u8> = "use tabs".encode_utf16().flat_map(u16::to_le_bytes).collect();
        s.write(".cursorrules", utf16);
        assert_eq!(find(&s.ws()), None);
    }

    #[test]
    fn a_file_over_four_kilobytes_is_cut_at_a_line_with_a_note() {
        let s = Sandbox::new("cut");
        // 100-byte lines: 40 whole ones fit in 4096, the 41st is split.
        let line = format!("{}\n", "x".repeat(99));
        s.write("AGENTS.md", line.repeat(60));
        let notes = find(&s.ws()).unwrap();
        assert!(notes.cut);
        assert_eq!(notes.text, format!("{}{CUT_NOTE}", line.repeat(40)));
        // Exactly the cap is not cut.
        s.write("AGENTS.md", "y".repeat(MAX_NOTES_BYTES));
        let notes = find(&s.ws()).unwrap();
        assert!(!notes.cut && notes.text.len() == MAX_NOTES_BYTES);
        // One byte more is, even with no line to cut at.
        s.write("AGENTS.md", "y".repeat(MAX_NOTES_BYTES + 1));
        let notes = find(&s.ws()).unwrap();
        assert!(notes.cut && notes.text.starts_with(&"y".repeat(MAX_NOTES_BYTES)) && notes.text.ends_with(CUT_NOTE));
    }

    #[test]
    fn text_past_the_cap_is_never_sent() {
        let s = Sandbox::new("past");
        s.write("AGENTS.md", format!("{}\nZEBRA-7 past the cap\n", "rule\n".repeat(900)));
        let notes = find(&s.ws()).unwrap();
        assert!(notes.cut && !notes.text.contains("ZEBRA-7"), "{}", notes.text.len());
    }

    #[cfg(windows)]
    #[test]
    fn a_file_with_a_second_name_is_not_read_and_the_next_one_is() {
        let s = Sandbox::new("hardlink");
        s.write("AGENTS.md", "secret elsewhere");
        s.write("CLAUDE.md", "fine");
        if std::fs::hard_link(s.dir.join("project/AGENTS.md"), s.dir.join("project/other.md")).is_err() {
            eprintln!("could not create a hard link here; skipping");
            return;
        }
        assert_eq!(find(&s.ws()).unwrap().name, "CLAUDE.md");
    }

    /// A junction needs no privilege, so this always runs: `.github` points out of the
    /// folder, and the notes behind it are not read.
    #[cfg(windows)]
    #[test]
    fn a_notes_file_reached_through_a_link_to_outside_is_refused() {
        let s = Sandbox::new("outside");
        std::fs::create_dir_all(s.dir.join("elsewhere")).unwrap();
        std::fs::write(s.dir.join("elsewhere").join("copilot-instructions.md"), "not yours").unwrap();
        assert!(
            junction(&s.dir.join("project").join(".github"), &s.dir.join("elsewhere")),
            "mklink /J failed"
        );
        assert_eq!(find(&s.ws()), None);
    }

    /// The spec is "never through a link", not "never out of the folder".
    #[cfg(windows)]
    #[test]
    fn a_notes_file_reached_through_a_link_that_stays_inside_is_refused() {
        let s = Sandbox::new("inside");
        s.write("ghdocs/copilot-instructions.md", "found through a junction");
        assert!(
            junction(
                &s.dir.join("project").join(".github"),
                &s.dir.join("project").join("ghdocs")
            ),
            "mklink /J failed"
        );
        assert_eq!(find(&s.ws()), None);
    }

    #[cfg(windows)]
    fn junction(link: &std::path::Path, target: &std::path::Path) -> bool {
        let made = std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(edit::plain(link))
            .arg(edit::plain(target))
            .output();
        matches!(made, Ok(ref o) if o.status.success())
    }

    #[test]
    fn a_notes_folder_is_not_a_notes_file() {
        let s = Sandbox::new("folder");
        std::fs::create_dir_all(s.dir.join("project/AGENTS.md")).unwrap();
        assert_eq!(find(&s.ws()), None);
    }
}
