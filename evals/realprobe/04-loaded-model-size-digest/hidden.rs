#[cfg(test)]
mod realprobe_hidden {
    use super::*;

    const FULL: &str = r#"{"models":[{"name":"gpt-oss:20b","model":"gpt-oss:20b","size":14022332416,"digest":"aa4295ac10c3afd2c2b6a2d1b1f0b3e7c0f6d9c1b5f0e3f4a2b1c0d9e8f7a6b5","details":{"parent_model":"","format":"gguf","family":"gptoss","families":["gptoss"],"parameter_size":"20.9B","quantization_level":"MXFP4"},"expires_at":"2318-01-06T22:13:05.123-08:00","size_vram":12884901888,"context_length":32768}]}"#;

    const BARE: &str = r#"{"models":[{"name":"qwen3:8b","expires_at":"2026-11-01T10:00:00Z","size_vram":5333539264}]}"#;

    #[test]
    fn realprobe_hidden_a_reply_with_size_and_digest_is_read() {
        let ps: PsResponse = serde_json::from_str(FULL).unwrap();
        assert_eq!(ps.models.len(), 1);
        let m = &ps.models[0];
        assert_eq!(m.size, Some(14_022_332_416));
        assert_eq!(m.digest.as_deref(), Some("aa4295ac10c3afd2c2b6a2d1b1f0b3e7c0f6d9c1b5f0e3f4a2b1c0d9e8f7a6b5"));
    }

    #[test]
    fn realprobe_hidden_size_is_not_confused_with_size_vram() {
        let ps: PsResponse = serde_json::from_str(FULL).unwrap();
        let m = &ps.models[0];
        assert_eq!(m.size_vram, Some(12_884_901_888));
        assert_ne!(m.size, m.size_vram);
        assert_eq!(m.size_vram_mib(), Some(12288));
    }

    #[test]
    fn realprobe_hidden_a_reply_without_them_still_parses_with_none() {
        let ps: PsResponse = serde_json::from_str(BARE).unwrap();
        assert_eq!(ps.models.len(), 1);
        let m = &ps.models[0];
        assert!(m.size.is_none());
        assert!(m.digest.is_none());
        assert_eq!(m.size_vram, Some(5_333_539_264));
        assert!(m.is("qwen3:8b"));
    }

    #[test]
    fn realprobe_hidden_each_entry_is_read_on_its_own_and_old_fields_still_work() {
        let both = format!(
            r#"{{"models":[{},{}]}}"#,
            FULL.trim_start_matches(r#"{"models":["#).trim_end_matches("]}"),
            BARE.trim_start_matches(r#"{"models":["#).trim_end_matches("]}"),
        );
        let ps: PsResponse = serde_json::from_str(&both).unwrap();
        assert_eq!(ps.models.len(), 2);
        assert!(ps.models[0].size.is_some() && ps.models[0].digest.is_some());
        assert!(ps.models[1].size.is_none() && ps.models[1].digest.is_none());
        assert!(ps.models[0].pinned());
        assert!(!ps.models[1].pinned());
        assert!(find(&ps.models, "gpt-oss:20b").is_some());
        assert!(find(&ps.models, "nope").is_none());
    }
}
