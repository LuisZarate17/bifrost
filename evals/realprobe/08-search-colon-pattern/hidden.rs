#[cfg(test)]
mod realprobe_hidden {
    use super::*;

    const OUTSIDE: &str = "not inside the project folder";

    /// A project folder of its own, removed on drop.
    struct Project {
        dir: PathBuf,
    }

    impl Project {
        fn new(tag: &str) -> Self {
            let n = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or_default();
            let dir = std::env::temp_dir().join(format!("realprobe-colon-{tag}-{}-{n}", std::process::id()));
            std::fs::create_dir_all(dir.join("src")).unwrap();
            std::fs::write(dir.join("src").join("main.rs"), "fn main() {}\n").unwrap();
            Self { dir }
        }

        fn ws(&self) -> Workspace {
            Workspace::open(&self.dir).unwrap()
        }
    }

    impl Drop for Project {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    fn refusal(ws: &Workspace, pattern: &str) -> String {
        list(ws, Some(pattern), || false).unwrap_err()
    }

    #[test]
    fn realprobe_hidden_a_drive_path_is_still_outside_the_folder() {
        let p = Project::new("drive");
        let ws = p.ws();
        for bad in ["C:", "C:/**", r"C:\Users\**", "c:src/*.rs", r"\\?\C:\**", "{C:/,x}/**"] {
            let err = refusal(&ws, bad);
            assert!(err.contains(OUTSIDE), "{bad}: {err}");
            assert!(!err.contains("colon"), "{bad}: {err}");
        }
    }

    #[test]
    fn realprobe_hidden_an_absolute_or_escaping_path_is_still_outside_the_folder() {
        let p = Project::new("outside");
        let ws = p.ws();
        for bad in ["/etc/*", r"\\server\share\*", "../**", "src/../../x", "{..,x}/**", "src/{a,..}/*", "[.][.]/x"] {
            let err = refusal(&ws, bad);
            assert!(err.contains(OUTSIDE), "{bad}: {err}");
        }
    }

    #[test]
    fn realprobe_hidden_a_colon_elsewhere_is_refused_as_a_colon() {
        let p = Project::new("colon");
        let ws = p.ws();
        for bad in ["src/a:b/*.rs", "serde_json::from_str", "undo(id:", "*.rs:12", "src/{a:b,c}/*"] {
            let err = refusal(&ws, bad);
            assert_eq!(
                err,
                format!("{bad} has a colon in it, which Windows does not allow in a file name — nothing could match it"),
                "{bad}"
            );
            assert!(!err.contains(OUTSIDE), "{bad}: {err}");
        }
    }

    #[test]
    fn realprobe_hidden_the_way_out_is_named_first_when_a_pattern_has_a_colon_too() {
        let p = Project::new("both");
        let ws = p.ws();
        for bad in ["C:/a:b", "/a:b", "src/../a:b", "src/{a:b,..}/*", "x/{//server,a:b}/*"] {
            let err = refusal(&ws, bad);
            assert!(err.contains(OUTSIDE), "{bad}: {err}");
            assert!(!err.contains("colon"), "{bad}: {err}");
        }
    }

    #[test]
    fn realprobe_hidden_a_colon_is_refused_before_the_card_and_in_a_search_too() {
        let p = Project::new("validate");
        let ws = p.ws();
        let err = validate(&ws, Some("greet"), Some("src/a:b/*.rs")).unwrap_err();
        assert!(err.contains("colon") && !err.contains(OUTSIDE), "{err}");
        assert!(validate(&ws, None, Some("a:b/**")).is_err());
        let err = search(&ws, "main", Some("src/a:b/*.rs"), || false).unwrap_err();
        assert!(err.contains("colon") && !err.contains(OUTSIDE), "{err}");
        assert!(validate(&ws, Some("greet"), Some("C:/x")).unwrap_err().contains(OUTSIDE));
    }

    #[test]
    fn realprobe_hidden_ordinary_patterns_are_read_as_before() {
        let p = Project::new("fine");
        let ws = p.ws();
        for good in ["src/**/*.rs", "*.rs", "./src", "src", "src/", "\"src/*.rs\"", "{src,docs}/*.md", "src/ma?n.rs"] {
            assert!(validate(&ws, None, Some(good)).is_ok(), "{good}");
        }
        let found = list(&ws, Some("*.rs"), || false).unwrap();
        assert!(found.contains("src/main.rs"), "{found}");
        let hits = search(&ws, "fn main", Some("src/*.rs"), || false).unwrap();
        assert!(hits.contains("src/main.rs"), "{hits}");
    }
}
