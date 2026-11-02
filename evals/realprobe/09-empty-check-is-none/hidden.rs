#[cfg(test)]
mod realprobe_hidden {
    use super::*;

    fn has_entry(c: &Config, root: &str) -> bool {
        c.checks.keys().any(|k| k.eq_ignore_ascii_case(root))
    }

    #[test]
    fn realprobe_hidden_setting_an_empty_check_stores_nothing() {
        let mut c = Config::default();
        c.set_check(r"C:\proj\app", Some(vec![]));
        assert!(!has_entry(&c, r"C:\proj\app"), "{:?}", c.checks);
        assert!(c.checks.is_empty());
        assert_eq!(c.check_for(r"C:\proj\app"), None);
    }

    #[test]
    fn realprobe_hidden_setting_an_empty_check_clears_the_one_there_in_any_case() {
        let mut c = Config::default();
        c.set_check(r"C:\Proj\App", Some(vec!["cargo".into(), "test".into()]));
        c.set_check(r"C:\Other", Some(vec!["npm.cmd".into(), "test".into()]));
        c.set_check(r"c:\proj\APP", Some(vec![]));
        assert!(!has_entry(&c, r"C:\Proj\App"), "{:?}", c.checks);
        assert_eq!(c.check_for(r"C:\Proj\App"), None);
        // Another folder's check is not touched.
        assert_eq!(c.check_for(r"C:\Other"), Some(&vec!["npm.cmd".to_string(), "test".to_string()]));
        // None still clears, and a real command still sets.
        c.set_check(r"C:\Other", None);
        assert!(c.checks.is_empty());
        c.set_check(r"C:\Proj\App", Some(vec!["cargo".into(), "check".into()]));
        assert_eq!(c.check_for(r"C:\PROJ\APP"), Some(&vec!["cargo".to_string(), "check".to_string()]));
    }

    #[test]
    fn realprobe_hidden_an_empty_check_in_the_file_is_dropped_on_load() {
        let c: Config = serde_json::from_str(r#"{"model":"m","checks":{"C:\\x":[]}}"#).unwrap();
        assert!(c.checks.is_empty(), "{:?}", c.checks);
        assert_eq!(c.model, "m");
    }

    #[test]
    fn realprobe_hidden_a_real_check_survives_beside_an_empty_one() {
        let c: Config = serde_json::from_str(
            r#"{"checks":{"C:\\a":[],"C:\\b":["cargo","test"],"C:\\c":"oops","D:\\d":["npm.cmd","test"],"D:\\e":[]}}"#,
        )
        .unwrap();
        let folders: Vec<&str> = c.checks.keys().map(String::as_str).collect();
        assert_eq!(folders, [r"C:\b", r"D:\d"]);
        assert_eq!(c.check_for(r"C:\b"), Some(&vec!["cargo".to_string(), "test".to_string()]));
    }

    #[test]
    fn realprobe_hidden_an_empty_check_does_not_come_back_on_the_next_save() {
        let dir = std::env::temp_dir().join(format!("realprobe-chk-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("config.json");
        std::fs::write(&file, r#"{"checks":{"C:\\x":[],"C:\\y":["cargo","check"]}}"#).unwrap();
        let loaded = load_from(&file);
        save_to(&file, &loaded).unwrap();
        let saved: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        let checks = saved["checks"].as_object().unwrap();
        assert_eq!(checks.len(), 1, "{saved}");
        assert!(checks.contains_key(r"C:\y"), "{saved}");
        assert!(!checks.contains_key(r"C:\x"), "{saved}");
    }
}
