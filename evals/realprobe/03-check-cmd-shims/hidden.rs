#[cfg(test)]
mod realprobe_hidden {
    use super::*;

    fn words(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn realprobe_hidden_npx_is_run_as_npx_cmd_in_any_case() {
        assert_eq!(parse_check("npx tsc --noEmit"), Ok(words(&["npx.cmd", "tsc", "--noEmit"])));
        assert_eq!(parse_check("NPX tsc"), Ok(words(&["npx.cmd", "tsc"])));
        assert_eq!(parse_check("Npx"), Ok(words(&["npx.cmd"])));
    }

    #[test]
    fn realprobe_hidden_pnpm_is_run_as_pnpm_cmd_in_any_case() {
        assert_eq!(parse_check("pnpm test"), Ok(words(&["pnpm.cmd", "test"])));
        assert_eq!(parse_check("PNPM run build"), Ok(words(&["pnpm.cmd", "run", "build"])));
        assert_eq!(parse_check("pNpM -r test"), Ok(words(&["pnpm.cmd", "-r", "test"])));
    }

    #[test]
    fn realprobe_hidden_yarn_is_run_as_yarn_cmd_in_any_case() {
        assert_eq!(parse_check("yarn build"), Ok(words(&["yarn.cmd", "build"])));
        assert_eq!(parse_check("Yarn test --watch=false"), Ok(words(&["yarn.cmd", "test", "--watch=false"])));
        assert_eq!(parse_check("YARN"), Ok(words(&["yarn.cmd"])));
    }

    #[test]
    fn realprobe_hidden_npm_is_unchanged() {
        assert_eq!(parse_check("npm test"), Ok(words(&["npm.cmd", "test"])));
        assert_eq!(parse_check("NPM run build"), Ok(words(&["npm.cmd", "run", "build"])));
        assert_eq!(parse_check("Npm ci"), Ok(words(&["npm.cmd", "ci"])));
    }

    #[test]
    fn realprobe_hidden_other_programs_and_lookalikes_are_not_touched() {
        assert_eq!(parse_check("cargo test -p hugin"), Ok(words(&["cargo", "test", "-p", "hugin"])));
        assert_eq!(parse_check("npxx test"), Ok(words(&["npxx", "test"])));
        assert_eq!(parse_check("yarnpkg install"), Ok(words(&["yarnpkg", "install"])));
        assert_eq!(parse_check("npm.cmd test"), Ok(words(&["npm.cmd", "test"])));
        assert_eq!(parse_check("node -e npx"), Ok(words(&["node", "-e", "npx"])), "only the program is spelled, not its arguments");
        assert_eq!(parse_check("cargo yarn"), Ok(words(&["cargo", "yarn"])));
    }

    #[test]
    fn realprobe_hidden_quoted_and_refused_checks_behave_as_before() {
        assert_eq!(parse_check(r#"npx "two words" 'and one'"#), Ok(words(&["npx.cmd", "two words", "and one"])));
        assert!(parse_check("npx a | b").unwrap_err().contains("not a shell command line"));
        assert!(parse_check("   ").unwrap_err().contains("needs a command"));
    }
}
