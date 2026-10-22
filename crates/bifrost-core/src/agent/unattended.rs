//! The rule for a turn with nobody to ask (I5).
//!
//! `hugin -p` and the task probe's propose mode both run the app's turn loop with no person
//! to approve a card, so a rule answers in their place. It is **one function, here**, so the
//! two cannot disagree about what a model may do unattended — and the probe measures the
//! rule that ships, as it does for every other rule.
//!
//! The model works in a copy of the folder ([`crate::scratch`]), and what it does comes back
//! as a patch for somebody to review, so a change to a file is safe to allow: [`crate::edit`]
//! already refuses anything outside the folder, and `.git`, `.cargo` and Bifrost's own data.
//! Everything that would run something, or reach beyond the folder, is not.
//!
//! **The check is the exception that needs care.** It runs code the model wrote — a test it
//! added, a build script it changed — with the user's rights, and nobody has read that code
//! yet. Mimir asks first, and its card names the build files that changed. Here the answer
//! is no for a wider list of files ([`runs_code`]): those that a check reads as instructions
//! or runs on start, in any language the user's repositories are likely to be in.

use super::ticket::PendingCall;
use super::turn::Decision;
use crate::tools;

/// The most rounds an unattended turn takes. The app's own bound is
/// [`crate::tools::MAX_ROUNDS`]; a turn nobody watches is allowed the longer one the task
/// probe records tasks finishing in (`late` there), since the time limit is what ends it.
pub const ROUNDS: usize = 20;

/// What to answer a card that asks, when there is nobody to ask. `edited` is every file the
/// turn has changed so far, as the cards named them (relative to the folder).
///
/// - **An edit, a create, a delete or a move: allowed.** It lands in the copy.
/// - **The folder's check: allowed, unless the turn changed a file that makes a check run
///   code** ([`runs_code`]), or Mimir's own list of build files says so
///   ([`PendingCall::changed_build_files`]). The card's warning about it is for a person to read.
/// - **Anything else: denied** — a shell command (`hugin -p` offers none), a read outside the
///   folder or of a file that is held back, and any call this does not know.
pub fn decide(call: &PendingCall, edited: &[String]) -> Decision {
    match call.name.as_str() {
        "edit_file" | "create_file" | "delete_file" | "move_file" => Decision::Allow,
        "run_check" if call.changed_build_files.is_empty() && !edited.iter().any(|p| runs_code(p)) => {
            Decision::Allow
        }
        _ => Decision::Deny,
    }
}

/// Names (lower case) of files that make a check run what they hold, beyond the three
/// [`tools::is_build_file`] knows.
const NAMES: [&str; 18] = [
    // Python: run by pytest, tox, nox and pip.
    "conftest.py",
    "setup.py",
    "setup.cfg",
    "pyproject.toml",
    "pytest.ini",
    "tox.ini",
    "noxfile.py",
    // Make, CMake and the task runners.
    "makefile",
    "gnumakefile",
    "cmakelists.txt",
    "justfile",
    "taskfile.yml",
    "taskfile.yaml",
    // JavaScript package managers' own settings, which can name a program to run.
    ".yarnrc.yml",
    ".npmrc",
    // Java.
    "pom.xml",
    // Ruby, which is code.
    "rakefile",
    "gemfile",
];

/// Endings (lower case) of such files: makefile fragments, .NET projects and their shared
/// property files, CMake modules.
const SUFFIXES: [&str; 8] = [".mk", ".csproj", ".vbproj", ".fsproj", ".props", ".targets", ".cmake", ".gradle"];

/// Beginnings (lower case): Gradle's, .NET's `Directory.Build.*`, and the Babel and gulp files.
const PREFIXES: [&str; 6] = ["build.gradle", "settings.gradle", "directory.build.", ".babelrc", "gulpfile.", "gruntfile."];

/// Whether editing `path` makes a check run code that the editor chose: `build.rs`,
/// `Cargo.toml` and `package.json` (Mimir's own list), the Python files pytest, tox, nox and
/// pip run, makefiles and CMake, Gradle and Maven, .NET projects and `Directory.Build.*`,
/// `justfile` and `Taskfile`, the package managers' `.npmrc` and `.yarnrc.yml`, and any
/// JavaScript tool's `*.config.js` (jest, vitest, vite, webpack, rollup, eslint and the rest).
/// By the file's name, in any folder, in any case.
pub fn runs_code(path: &str) -> bool {
    if tools::is_build_file(path) {
        return true;
    }
    let file = path.rsplit(['/', '\\']).next().unwrap_or_default().to_ascii_lowercase();
    let script_config = ["js", "cjs", "mjs", "ts", "cts", "mts"].iter().any(|ext| file.ends_with(&format!(".config.{ext}")));
    NAMES.contains(&file.as_str())
        || SUFFIXES.iter().any(|s| file.ends_with(s))
        || PREFIXES.iter().any(|p| file.starts_with(p))
        || script_config
}

#[cfg(test)]
mod tests {
    use super::*;

    fn card(name: &str, changed_build_files: &[&str]) -> PendingCall {
        PendingCall {
            name: name.into(),
            path: "x".into(),
            token: "t".into(),
            diff: None,
            range: None,
            changed_build_files: changed_build_files.iter().map(|s| s.to_string()).collect(),
            to: None,
            folders: Vec::new(),
            size: None,
        }
    }

    /// The whole policy, one row per kind of call.
    #[test]
    fn the_policy_table() {
        let cases = [
            ("edit_file", &[][..], Decision::Allow),
            ("create_file", &[], Decision::Allow),
            ("delete_file", &[], Decision::Allow),
            ("move_file", &[], Decision::Allow),
            // An edit is allowed whatever it touched: it is in the patch to be read.
            ("edit_file", &["Cargo.toml"], Decision::Allow),
            ("run_check", &[], Decision::Allow),
            ("run_check", &["Cargo.toml"], Decision::Deny),
            ("run_check", &["build.rs", "package.json"], Decision::Deny),
            ("run_command", &[], Decision::Deny),
            ("read_file", &[], Decision::Deny),
            ("list_files", &[], Decision::Deny),
            ("search", &[], Decision::Deny),
            ("update_plan", &[], Decision::Deny),
            ("write_file", &[], Decision::Deny),
            ("no_such_tool", &[], Decision::Deny),
            ("", &[], Decision::Deny),
        ];
        for (name, build_files, want) in cases {
            assert_eq!(decide(&card(name, build_files), &[]), want, "{name} with {build_files:?}");
        }
    }

    /// **The check is turned away after a runner or config file was edited**, which Mimir's own
    /// list of build files does not name; ordinary source files do not stop it.
    #[test]
    fn the_check_is_denied_after_a_file_it_runs_was_edited() {
        let check = card("run_check", &[]);
        let edited = |list: &[&str]| list.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(decide(&check, &edited(&["src/lib.rs", "tests/a.rs", "README.md"])), Decision::Allow);
        for file in ["conftest.py", "tests/conftest.py", "Makefile", "web/vite.config.ts", "App.csproj"] {
            assert_eq!(decide(&check, &edited(&["src/lib.rs", file])), Decision::Deny, "{file}");
        }
        // Edits themselves are never turned away for it.
        assert_eq!(decide(&card("edit_file", &[]), &edited(&["conftest.py"])), Decision::Allow);
    }

    /// The list, kind by kind.
    #[test]
    fn the_files_that_make_a_check_run_code() {
        for yes in [
            "build.rs", "Cargo.toml", "package.json", "crates/x/Cargo.toml",
            "conftest.py", "pkg/tests/conftest.py", "setup.py", "setup.cfg", "pyproject.toml", "pytest.ini", "tox.ini", "noxfile.py",
            "Makefile", "GNUmakefile", "rules.mk", "CMakeLists.txt", "cmake/Find.cmake", "justfile", "Taskfile.yml", "Taskfile.yaml",
            ".yarnrc.yml", ".npmrc", "web/.npmrc",
            "build.gradle", "build.gradle.kts", "settings.gradle", "app/build.gradle", "pom.xml",
            "App.csproj", "Lib.fsproj", "Common.props", "Common.targets", "Directory.Build.props", "Directory.Build.targets",
            "jest.config.js", "vitest.config.ts", "vite.config.mts", "webpack.config.cjs", "babel.config.js", "eslint.config.mjs", ".babelrc", ".babelrc.json",
            "gulpfile.js", "Gruntfile.js", "Rakefile", "Gemfile", r"web\Vite.Config.TS", "SETUP.PY",
        ] {
            assert!(runs_code(yes), "{yes}");
        }
        for no in [
            "src/main.rs", "lib.rs", "Cargo.lock", "package-lock.json", "yarn.lock", "README.md", "config.toml", "app.config",
            "tests/test_a.py", "conftest.pyc", "notmakefile", "build.rs.bak", "setup.py.orig", "vite.config.json", "Makefile.md",
            "src/config.js", "x.csproj.user", "", "docs/pom.xml.txt",
        ] {
            assert!(!runs_code(no), "{no}");
        }
    }

    /// Nothing but the five answers above is ever allowed, whatever the call's path says.
    #[test]
    fn the_path_does_not_change_the_answer() {
        for path in ["src/a.rs", "../outside.txt", r"C:\Windows\system.ini", ".git/config", ""] {
            let mut read = card("read_file", &[]);
            read.path = path.into();
            assert_eq!(decide(&read, &[]), Decision::Deny, "{path}");
            let mut command = card("run_command", &[]);
            command.path = path.into();
            assert_eq!(decide(&command, &[]), Decision::Deny, "{path}");
        }
    }

    #[test]
    fn an_unattended_turn_is_allowed_more_rounds_than_the_apps() {
        const { assert!(ROUNDS > crate::tools::MAX_ROUNDS) };
    }
}
