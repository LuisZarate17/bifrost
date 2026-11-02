#[cfg(test)]
mod realprobe_hidden {
    use super::*;

    fn words(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn realprobe_hidden_every_known_flag_is_accepted() {
        let all = words(&["--model", "x", "--think", "high", "--recover", "--help", "-h"]);
        assert_eq!(unknown_flag(&all), None);
        assert_eq!(unknown_flag(&[]), None);
        for flag in ["--recover", "--help", "-h"] {
            assert_eq!(unknown_flag(&words(&[flag])), None, "{flag}");
        }
    }

    #[test]
    fn realprobe_hidden_the_first_unknown_flag_is_named() {
        assert_eq!(unknown_flag(&words(&["--bogus"])), Some("--bogus".to_string()));
        assert_eq!(unknown_flag(&words(&["--model", "x", "--bogus", "--nope"])), Some("--bogus".to_string()));
        assert_eq!(unknown_flag(&words(&["--recover", "-z"])), Some("-z".to_string()));
        assert_eq!(unknown_flag(&words(&["--recovr"])), Some("--recovr".to_string()));
    }

    #[test]
    fn realprobe_hidden_the_word_after_a_value_flag_is_its_value_whatever_it_starts_with() {
        assert_eq!(unknown_flag(&words(&["--model", "-x"])), None);
        assert_eq!(unknown_flag(&words(&["--model", "--bogus"])), None);
        assert_eq!(unknown_flag(&words(&["--think", "-1", "--recover"])), None);
        // A flag that takes no value does not swallow the next word.
        assert_eq!(unknown_flag(&words(&["--recover", "-x"])), Some("-x".to_string()));
        assert_eq!(unknown_flag(&words(&["--recover", "--bogus"])), Some("--bogus".to_string()));
        // A value flag at the very end, with nothing after it, is not an unknown flag.
        assert_eq!(unknown_flag(&words(&["--think"])), None);
    }

    #[test]
    fn realprobe_hidden_flags_are_matched_exactly_and_plain_words_are_not_flags() {
        assert_eq!(unknown_flag(&words(&["--MODEL", "x"])), Some("--MODEL".to_string()));
        assert_eq!(unknown_flag(&words(&["--model=x"])), Some("--model=x".to_string()));
        assert_eq!(unknown_flag(&words(&["extra", "words"])), None);
    }
}
