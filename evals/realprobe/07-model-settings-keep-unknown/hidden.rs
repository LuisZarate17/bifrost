#[cfg(test)]
mod realprobe_hidden {
    use super::*;

    /// A config.json of its own in a folder of its own, removed when the test ends.
    struct Dir(PathBuf);

    impl Dir {
        fn new(name: &str) -> Dir {
            let dir = std::env::temp_dir().join(format!("realprobe-ms-{}-{name}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            Dir(dir)
        }
        fn file(&self) -> PathBuf {
            self.0.join("config.json")
        }
    }

    impl Drop for Dir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// Write `json` as a config, load it, save it to a second file, and read that back as JSON.
    fn round_trip(name: &str, json: &str) -> (Config, serde_json::Value) {
        let dir = Dir::new(name);
        std::fs::write(dir.file(), json).unwrap();
        let loaded = load_from(&dir.file());
        let out = dir.0.join("saved.json");
        save_to(&out, &loaded).unwrap();
        let saved = serde_json::from_str(&std::fs::read_to_string(&out).unwrap()).unwrap();
        (loaded, saved)
    }

    #[test]
    fn realprobe_hidden_an_unknown_key_in_an_entry_survives_load_and_save() {
        let (c, saved) = round_trip(
            "keep",
            r#"{"model":"m","model_settings":{"gpt-oss:20b":{"think":"high","temperature":0.3}}}"#,
        );
        assert_eq!(c.settings_for("gpt-oss:20b").and_then(|s| s.think), Some(Think::High));
        let entry = &saved["model_settings"]["gpt-oss:20b"];
        assert_eq!(entry["temperature"], serde_json::json!(0.3), "{saved}");
        assert_eq!(entry["think"], serde_json::json!("high"), "{saved}");
    }

    #[test]
    fn realprobe_hidden_every_kind_of_value_is_kept_as_it_was() {
        let (_, saved) = round_trip(
            "kinds",
            r#"{"model_settings":{"a":{"note":"hand typed","top_k":40,"flag":true,"nested":{"x":[1,2,{"y":null}]}}}}"#,
        );
        let entry = &saved["model_settings"]["a"];
        assert_eq!(
            *entry,
            serde_json::json!({"note":"hand typed","top_k":40,"flag":true,"nested":{"x":[1,2,{"y":null}]}}),
            "{saved}"
        );
    }

    #[test]
    fn realprobe_hidden_a_bad_think_is_still_unset_and_is_not_written_back() {
        let (c, saved) = round_trip(
            "bad",
            r#"{"model_settings":{"d":{"think":"extreme","temperature":0.7},"e":{"think":3},"f":{"think":"medium","seed":7}}}"#,
        );
        assert_eq!(c.settings_for("d").and_then(|s| s.think), None);
        assert_eq!(c.settings_for("e").and_then(|s| s.think), None);
        assert_eq!(c.settings_for("f").and_then(|s| s.think), Some(Think::Medium));
        let ms = &saved["model_settings"];
        assert!(ms["d"].get("think").is_none(), "{saved}");
        assert_eq!(ms["d"]["temperature"], serde_json::json!(0.7));
        assert!(ms["e"].get("think").is_none(), "{saved}");
        assert_eq!(ms["f"], serde_json::json!({"think":"medium","seed":7}));
    }

    #[test]
    fn realprobe_hidden_think_is_written_once_and_normalised() {
        let dir = Dir::new("once");
        std::fs::write(dir.file(), r#"{"model_settings":{"x":{"think":"High","temperature":0.3}}}"#).unwrap();
        let out = dir.0.join("saved.json");
        save_to(&out, &load_from(&dir.file())).unwrap();
        let text = std::fs::read_to_string(&out).unwrap();
        assert_eq!(text.matches("\"think\"").count(), 1, "{text}");
        assert!(text.contains("\"high\"") && !text.contains("\"High\""), "{text}");
    }

    #[test]
    fn realprobe_hidden_entries_that_are_not_objects_are_still_dropped_and_the_rest_kept() {
        let (c, saved) = round_trip(
            "drop",
            r#"{"model":"mine","workspace":"C:\\w","model_settings":{"a":"high","b":["think"],"c":null,"g":{"think":"low","k":1}}}"#,
        );
        assert_eq!((c.model.as_str(), c.workspace.as_deref()), ("mine", Some(r"C:\w")));
        let tags: Vec<&str> = c.model_settings.keys().map(String::as_str).collect();
        assert_eq!(tags, ["g"]);
        assert_eq!(saved["model_settings"], serde_json::json!({"g":{"think":"low","k":1}}));
    }

    #[test]
    fn realprobe_hidden_top_level_unknown_keys_and_plain_entries_are_unchanged() {
        let (_, saved) = round_trip(
            "plain",
            r#"{"model":"m","future_field":{"a":1},"model_settings":{"p":{"think":"off"},"q":{}}}"#,
        );
        assert_eq!(saved["future_field"], serde_json::json!({"a":1}));
        assert_eq!(saved["model_settings"]["p"], serde_json::json!({"think":"off"}));
        assert_eq!(saved["model_settings"]["q"], serde_json::json!({}));
    }
}
