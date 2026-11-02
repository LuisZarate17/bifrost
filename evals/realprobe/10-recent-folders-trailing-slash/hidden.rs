#[cfg(test)]
mod realprobe_hidden {
    use super::*;

    fn list(entries: &[&str]) -> config::Config {
        config::Config { recent_workspaces: entries.iter().map(|s| s.to_string()).collect(), ..Default::default() }
    }

    fn recent(cfg: &config::Config) -> Vec<&str> {
        cfg.recent_workspaces.iter().map(String::as_str).collect()
    }

    #[test]
    fn realprobe_hidden_a_new_spelling_with_a_trailing_backslash_replaces_the_old_one() {
        let mut cfg = list(&[r"C:\other", r"C:\x"]);
        remember(&mut cfg, r"C:\x\");
        assert_eq!(recent(&cfg), [r"C:\x\", r"C:\other"]);
    }

    #[test]
    fn realprobe_hidden_an_old_spelling_with_a_trailing_backslash_is_replaced_too() {
        let mut cfg = list(&[r"C:\other", r"C:\x\"]);
        remember(&mut cfg, r"C:\x");
        assert_eq!(recent(&cfg), [r"C:\x", r"C:\other"]);
    }

    #[test]
    fn realprobe_hidden_forward_slashes_and_case_and_several_slashes_are_the_same_folder() {
        let mut cfg = list(&[r"D:\keep", r"c:\proj\app"]);
        remember(&mut cfg, r"C:\Proj\App/");
        assert_eq!(recent(&cfg), [r"C:\Proj\App/", r"D:\keep"]);
        remember(&mut cfg, r"C:\PROJ\app\\");
        assert_eq!(recent(&cfg), [r"C:\PROJ\app\\", r"D:\keep"]);
        remember(&mut cfg, r"c:\proj\app");
        assert_eq!(recent(&cfg), [r"c:\proj\app", r"D:\keep"]);
    }

    #[test]
    fn realprobe_hidden_different_folders_are_not_merged() {
        let mut cfg = list(&[]);
        for folder in [r"C:\x", r"C:\xy", r"C:\x\y", r"D:\x\"] {
            remember(&mut cfg, folder);
        }
        assert_eq!(recent(&cfg), [r"D:\x\", r"C:\x\y", r"C:\xy", r"C:\x"]);
        // And the same folder again, in another spelling, is still just one place.
        remember(&mut cfg, r"C:\xy\");
        assert_eq!(recent(&cfg), [r"C:\xy\", r"D:\x\", r"C:\x\y", r"C:\x"]);
    }

    #[test]
    fn realprobe_hidden_newest_first_and_the_cap_still_hold() {
        let mut cfg = list(&[]);
        for i in 0..RECENT_FOLDERS + 3 {
            remember(&mut cfg, &format!(r"C:\f{i}\"));
        }
        assert_eq!(cfg.recent_workspaces.len(), RECENT_FOLDERS);
        assert_eq!(cfg.recent_workspaces[0], format!(r"C:\f{}\", RECENT_FOLDERS + 2));
        assert_eq!(cfg.recent_workspaces[RECENT_FOLDERS - 1], format!(r"C:\f{}\", 3));
        // A repeat with another spelling moves to the front and drops nobody.
        let before = cfg.recent_workspaces.len();
        remember(&mut cfg, r"C:\f4");
        assert_eq!(cfg.recent_workspaces.len(), before);
        assert_eq!(cfg.recent_workspaces[0], r"C:\f4");
        assert_eq!(cfg.recent_workspaces.iter().filter(|p| p.starts_with(r"C:\f4")).count(), 1);
        assert_eq!(cfg.recent_workspaces[RECENT_FOLDERS - 1], format!(r"C:\f{}\", 3));
    }
}
