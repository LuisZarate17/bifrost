//! Existing tests that a patch changes: a test bent to fit the code (`hugin -p`).
//!
//! A check that passes says the tests pass, not that the work is right. A model that cannot make
//! the code fit a test can make the test fit the code: it edits the assertion that was in the way
//! (`npm.cmd` expected where the code now gives `NPM.cmd`), and the check goes green. This reads
//! that off the patch, with no model and no run, so that it can be told, and so that the person
//! reading the patch is.
//!
//! **What counts is a line the patch removes** — a changed line is a removed one and an added one —
//! because a test that is only added, or only renamed, has bent nothing. A removed line counts when
//! it is in a test file ([`is_test_path`]), whatever it says, or when it asserts ([`asserts`]) in
//! any other file: an inline `#[cfg(test)]` module, a `.py` with its tests beside the code.
//!
//! **A patch that still changes them after the model was asked to put them back is not done**:
//! the report's status is `review` until someone confirms the change was asked for, unless the
//! check failed, which is the status then.
//!
//! - [`changed_tests`] — the files of a patch and the test lines it removes from each
//! - [`follow_up`] — what the model is told, in the conversation it is in
//! - [`review_line`], [`warning_line`] — the first line of the plain-text report, with or without
//!   a check that failed
//! - [`listing`] — the files and lines, for the model and for the reader

/// One file whose existing tests a patch changes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bent {
    /// Relative to the repository's top folder, as the patch names it.
    pub path: String,
    /// The test lines the patch removes from it: every line of a test file, and in any other file
    /// the lines that assert. Without the `-` that marks them.
    pub removed: Vec<String>,
}

impl Bent {
    /// How the report's `warnings` say it: `changes existing tests: tests/a.rs (-3)`.
    pub fn warning(&self) -> String {
        format!("changes existing tests: {} (-{})", self.path, self.removed.len())
    }
}

/// The most files and lines the model is shown, and the most characters of a line.
const SHOWN_FILES: usize = 5;
const SHOWN_LINES: usize = 6;
const SHOWN_WIDTH: usize = 160;

/// The most files the plain-text warning names.
const NAMED_FILES: usize = 4;

/// Whether `path` is a test file: in a `tests`, `test` or `__tests__` folder, or named as tests are
/// — `x_test.rs`, `x_tests.rs`, `tests.rs`, `x_test.go`, `test_x.py`, `x_test.py`, `conftest.py`,
/// `x.test.ts`, `x.spec.js`. Case and the kind of slash do not matter.
pub fn is_test_path(path: &str) -> bool {
    let lower = path.replace('\\', "/").to_ascii_lowercase();
    let mut parts: Vec<&str> = lower.split('/').collect();
    let file = parts.pop().unwrap_or_default();
    if parts.iter().any(|dir| matches!(*dir, "tests" | "test" | "__tests__")) {
        return true;
    }
    let stem = file.rsplit_once('.').map_or(file, |(stem, _)| stem);
    matches!(stem, "tests" | "test" | "conftest")
        || stem.ends_with("_test")
        || stem.ends_with("_tests")
        || stem.starts_with("test_")
        || stem.ends_with(".test")
        || stem.ends_with(".spec")
}

/// Files that are prose: a line in them that mentions `assert` is not an assertion.
fn is_prose(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    [".md", ".markdown", ".txt", ".rst", ".adoc"].iter().any(|ext| lower.ends_with(ext))
}

/// Whether a removed line is an assertion or marks a test: `assert…` in any spelling
/// (`assert!`, `assert_eq!`, `assert.equal(`, `assertEquals(`, Python's `assert x`), `expect(…)`,
/// `.should`, or a `#[test]` attribute (`#[tokio::test]` too). A comment is not one, nor is
/// `debug_assert!`, which is the program checking itself, nor a `.expect("…")` on a `Result`.
pub fn asserts(line: &str) -> bool {
    let code = line.trim_start();
    let comment = code.starts_with("//")
        || code.starts_with("/*")
        || code.starts_with('*')
        || code.starts_with("--")
        || (code.starts_with('#') && !code.starts_with("#["));
    !comment && (is_test_attribute(code) || code.contains(".should") || has_expect(code) || has_assert(code))
}

/// `#[test]`, `#[tokio::test]`, `#[tokio::test(flavor = "multi_thread")]`: an attribute whose
/// last word is `test`. `#[cfg(test)]` is not one.
fn is_test_attribute(code: &str) -> bool {
    let Some(at) = code.find("#[") else { return false };
    let name: String = code[at + 2..].chars().take_while(|c| c.is_alphanumeric() || *c == '_' || *c == ':').collect();
    name.rsplit("::").next() == Some("test")
}

/// `expect(` that begins a call — jest's — and not a method called on something.
fn has_expect(code: &str) -> bool {
    let bytes = code.as_bytes();
    code.match_indices("expect(").any(|(i, _)| i == 0 || !(bytes[i - 1].is_ascii_alphanumeric() || matches!(bytes[i - 1], b'_' | b'.')))
}

/// `assert` that begins a word and is followed by what an assertion is followed by: `!`, `(`, `_`,
/// `.`, a capital letter, or — only at the start of the line — a space, which is Python's.
fn has_assert(code: &str) -> bool {
    let bytes = code.as_bytes();
    code.match_indices("assert").any(|(at, _)| {
        let begins = at == 0 || !(bytes[at - 1].is_ascii_alphanumeric() || bytes[at - 1] == b'_');
        let follows = match bytes.get(at + 6) {
            Some(b'!' | b'(' | b'_' | b'.') => true,
            Some(c) if c.is_ascii_uppercase() => true,
            Some(c) if c.is_ascii_whitespace() => at == 0,
            _ => false,
        };
        begins && follows
    })
}

/// The path out of a `--- a/x` or `+++ b/x` line: without the `a/` or `b/`, the tab git adds after
/// a name with a space in it, or the quotes it puts round a name it escapes. `/dev/null` as it is.
fn named(rest: &str) -> String {
    let name = rest.split('\t').next().unwrap_or(rest).trim_matches('"');
    name.strip_prefix("a/").or_else(|| name.strip_prefix("b/")).unwrap_or(name).to_string()
}

/// The existing test lines `patch` removes or changes, file by file in the order the patch has
/// them. A binary file has no lines, and a file that is only renamed or only added has no removed
/// ones, so neither is named; a test file that is deleted is named with every line it had.
pub fn changed_tests(patch: &[u8]) -> Vec<Bent> {
    let text = String::from_utf8_lossy(patch);
    let mut out: Vec<Bent> = Vec::new();
    let (mut old, mut new, mut in_hunk) = (String::new(), String::new(), false);
    for line in text.lines() {
        if line.starts_with("diff --git ") {
            (old, new, in_hunk) = (String::new(), String::new(), false);
        } else if !in_hunk {
            // The header: the names, then the first hunk.
            if let Some(rest) = line.strip_prefix("--- ") {
                old = named(rest);
            } else if let Some(rest) = line.strip_prefix("+++ ") {
                new = named(rest);
            } else if line.starts_with("@@ ") {
                in_hunk = true;
            }
        } else if let Some(removed) = line.strip_prefix('-') {
            let path = if new == "/dev/null" { &old } else { &new };
            if removed.trim().is_empty() || !(is_test_path(path) || (!is_prose(path) && asserts(removed))) {
                continue;
            }
            match out.last_mut() {
                Some(last) if last.path == *path => last.removed.push(removed.to_string()),
                _ => out.push(Bent { path: path.clone(), removed: vec![removed.to_string()] }),
            }
        }
    }
    out
}

/// A line cut to `SHOWN_WIDTH` characters, with an ellipsis where it was cut.
fn cut(line: &str) -> String {
    if line.chars().count() <= SHOWN_WIDTH {
        return line.to_string();
    }
    let kept: String = line.chars().take(SHOWN_WIDTH).collect();
    format!("{kept}…")
}

/// The files and the lines removed from each, cut to what fits: `tests/a.rs (-2):`, then the
/// lines, four spaces in. At most [`SHOWN_FILES`] files and [`SHOWN_LINES`] lines of each, with
/// what is left out counted. No newline at the end.
pub fn listing(bent: &[Bent]) -> String {
    let mut lines: Vec<String> = Vec::new();
    for b in bent.iter().take(SHOWN_FILES) {
        lines.push(format!("{} (-{}):", b.path, b.removed.len()));
        lines.extend(b.removed.iter().take(SHOWN_LINES).map(|line| format!("    {}", cut(line.trim()))));
        if b.removed.len() > SHOWN_LINES {
            lines.push(format!("    … and {} more", b.removed.len() - SHOWN_LINES));
        }
    }
    if bent.len() > SHOWN_FILES {
        lines.push(format!("… and {} more files", bent.len() - SHOWN_FILES));
    }
    lines.join("\n")
}

/// What the model is told when its patch changes existing tests, in the conversation it is in:
/// the files and the lines it removed, so that it can put them back exactly.
pub fn follow_up(bent: &[Bent]) -> String {
    format!(
        "Your patch changes existing tests:\n{}\nThe task did not ask to change them. Restore them exactly \
         unless the task requires the change, then run the check again.",
        listing(bent)
    )
}

/// The files named in the first line of the report: the first few, and how many more.
fn files_named(bent: &[Bent]) -> String {
    let mut files: Vec<String> = bent.iter().take(NAMED_FILES).map(|b| b.path.clone()).collect();
    if bent.len() > NAMED_FILES {
        files.push(format!("and {} more", bent.len() - NAMED_FILES));
    }
    files.join(", ")
}

/// The first line of the plain-text report when the final patch still changes existing tests and
/// the check did not fail: **not done until someone confirms the change was asked for.**
/// `check_passed` says whether there was a check and it passed; without one the line does not
/// claim it.
pub fn review_line(bent: &[Bent], check_passed: bool) -> String {
    let because = if check_passed { "the check passed, but " } else { "" };
    format!(
        "hugin: REVIEW — {because}the patch changes existing tests ({}); not done until someone confirms the change was asked for.",
        files_named(bent)
    )
}

/// The line above the rest when the check failed and the patch changes existing tests as well:
/// the failure is the status, and this says the tests were touched too.
pub fn warning_line(bent: &[Bent]) -> String {
    format!("hugin: WARNING — the patch changes existing tests ({}); check they were not bent to fit.", files_named(bent))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A file's diff, as git writes it: `old` is what the lines before the change look like.
    fn modified(path: &str, hunk: &str) -> String {
        format!("diff --git a/{path} b/{path}\nindex 111..222 100644\n--- a/{path}\n+++ b/{path}\n@@ -1,3 +1,3 @@\n{hunk}")
    }

    fn found(patch: &str) -> Vec<(String, usize)> {
        changed_tests(patch.as_bytes()).into_iter().map(|b| (b.path, b.removed.len())).collect()
    }

    #[test]
    fn a_changed_assertion_is_a_bent_test_and_the_lines_come_back_whole() {
        // The change that was really made: the expected value edited to fit the new code.
        let patch = modified(
            "apps/hugin/src/print_tests.rs",
            " fn a_check_is_one_program() {\n-    assert_eq!(parse_check(\"NPM run build\"), Ok(words(&[\"npm.cmd\", \"run\", \"build\"])));\n+    assert_eq!(parse_check(\"NPM run build\"), Ok(words(&[\"NPM.cmd\", \"run\", \"build\"])));\n }\n",
        );
        let bent = changed_tests(patch.as_bytes());
        assert_eq!(bent.len(), 1);
        assert_eq!(bent[0].path, "apps/hugin/src/print_tests.rs");
        assert_eq!(bent[0].removed, ["    assert_eq!(parse_check(\"NPM run build\"), Ok(words(&[\"npm.cmd\", \"run\", \"build\"])));"]);
        assert_eq!(bent[0].warning(), "changes existing tests: apps/hugin/src/print_tests.rs (-1)");
    }

    #[test]
    fn tests_that_are_only_added_are_not_bent() {
        let added = modified("apps/hugin/src/print_tests.rs", " fn a() {\n+    assert_eq!(1, 1);\n+    assert!(true);\n }\n");
        assert!(changed_tests(added.as_bytes()).is_empty());
        let new_file = "diff --git a/tests/new.rs b/tests/new.rs\nnew file mode 100644\nindex 0000000..222\n--- /dev/null\n+++ b/tests/new.rs\n@@ -0,0 +1,2 @@\n+#[test]\n+fn t() { assert!(true); }\n";
        assert!(changed_tests(new_file.as_bytes()).is_empty());
    }

    #[test]
    fn a_rename_with_nothing_changed_is_not_bent_but_a_rename_that_changed_an_assertion_is() {
        let pure = "diff --git a/tests/old.rs b/tests/new.rs\nsimilarity index 100%\nrename from tests/old.rs\nrename to tests/new.rs\n";
        assert!(changed_tests(pure.as_bytes()).is_empty());
        let changed = "diff --git a/tests/old.rs b/tests/new.rs\nsimilarity index 80%\nrename from tests/old.rs\nrename to tests/new.rs\nindex 111..222 100644\n--- a/tests/old.rs\n+++ b/tests/new.rs\n@@ -1,2 +1,2 @@\n-    assert_eq!(a(), 1);\n+    assert_eq!(a(), 2);\n fn b() {}\n";
        assert_eq!(found(changed), [("tests/new.rs".to_string(), 1)]);
    }

    #[test]
    fn deleting_a_test_file_is_bent_with_every_line_it_had() {
        let gone = "diff --git a/tests/it.rs b/tests/it.rs\ndeleted file mode 100644\nindex 111..0000000\n--- a/tests/it.rs\n+++ /dev/null\n@@ -1,4 +0,0 @@\n-use sample::add;\n-\n-#[test]\n-fn adds() { assert_eq!(add(1, 2), 3); }\n";
        let bent = changed_tests(gone.as_bytes());
        assert_eq!(bent.len(), 1);
        assert_eq!(bent[0].path, "tests/it.rs");
        assert_eq!(bent[0].removed.len(), 3, "the blank line is not counted: {:?}", bent[0].removed);
    }

    #[test]
    fn any_removed_line_of_a_test_file_counts_and_any_other_file_only_when_it_asserts() {
        // A test file: a removed line that asserts nothing, such as a helper's body.
        let helper = modified("crates/x/tests/common.rs", " fn words(list: &[&str]) -> Vec<String> {\n-    list.iter().map(|s| s.to_string()).collect()\n+    list.iter().map(ToString::to_string).collect()\n }\n");
        assert_eq!(found(&helper), [("crates/x/tests/common.rs".to_string(), 1)]);
        // Not a test file: ordinary code changes, and an inline test's assertion or marker does.
        let code = modified("src/lib.rs", "-pub fn add(a: i32, b: i32) -> i32 { a + b }\n+pub fn add(a: i32, b: i32) -> i32 { b + a }\n fn x() {}\n");
        assert!(changed_tests(code.as_bytes()).is_empty());
        let inline = modified("src/lib.rs", " mod tests {\n-    #[test] fn adds() { assert_eq!(add(1, 2), 3); }\n+    #[test] fn adds() { assert_eq!(add(1, 2), 4); }\n }\n");
        assert_eq!(found(&inline), [("src/lib.rs".to_string(), 1)]);
    }

    #[test]
    fn each_file_is_named_once_with_its_own_count_in_the_order_of_the_patch() {
        let patch = format!(
            "{}{}{}",
            modified("tests/a.rs", "-    assert_eq!(a(), 1);\n-    assert_eq!(b(), 2);\n+    assert_eq!(a(), 2);\n fn c() {}\n"),
            modified("src/lib.rs", "-pub fn f() {}\n+pub fn f() { g() }\n fn x() {}\n"),
            modified("web/app.test.ts", "-  expect(f()).toBe(1);\n+  expect(f()).toBe(2);\n it('x', () => {});\n"),
        );
        assert_eq!(found(&patch), [("tests/a.rs".to_string(), 2), ("web/app.test.ts".to_string(), 1)]);
    }

    #[test]
    fn a_second_hunk_of_the_same_file_adds_to_it() {
        let patch = format!(
            "{}@@ -40,2 +40,2 @@\n-    assert_eq!(b(), 2);\n+    assert_eq!(b(), 3);\n fn d() {{}}\n",
            modified("tests/a.rs", "-    assert_eq!(a(), 1);\n+    assert_eq!(a(), 2);\n fn c() {}\n"),
        );
        assert_eq!(found(&patch), [("tests/a.rs".to_string(), 2)]);
    }

    #[test]
    fn a_line_that_looks_like_a_header_inside_a_hunk_is_a_removed_line() {
        // `--- a/x` as a removed line `-- a/x`, in a hunk, and a path with a space (git adds a tab).
        let patch = "diff --git a/tests/my file.rs b/tests/my file.rs\nindex 111..222 100644\n--- a/tests/my file.rs\t\n+++ b/tests/my file.rs\t\n@@ -1,2 +1,2 @@\n--- a/not a header\n+++ b/not a header either\n fn a() {}\n";
        assert_eq!(found(patch), [("tests/my file.rs".to_string(), 1)]);
    }

    #[test]
    fn a_binary_change_and_an_empty_patch_name_nothing() {
        let binary = "diff --git a/tests/data.bin b/tests/data.bin\nindex 111..222 100644\nGIT binary patch\nliteral 3\nKcmZPj\n\nliteral 2\nJcmZ\n\n";
        assert!(changed_tests(binary.as_bytes()).is_empty());
        assert!(changed_tests(b"").is_empty());
    }

    #[test]
    fn test_files_are_known_by_their_folder_and_by_their_name() {
        for yes in [
            "tests/a.rs",
            "crates/x/tests/it/main.rs",
            "test/helper.js",
            "pkg/__tests__/a.js",
            "src/foo_test.rs",
            "src/foo_tests.rs",
            "apps/hugin/src/tests.rs",
            "apps/hugin/src/print_tests.rs",
            "pkg/test_math.py",
            "pkg/math_test.py",
            "tests/conftest.py",
            "conftest.py",
            "web/app.test.ts",
            "web/app.spec.js",
            "pkg/thing_test.go",
            "test.js",
            r"C:\work\Tests\A.rs",
        ] {
            assert!(is_test_path(yes), "{yes}");
        }
        for no in [
            "src/lib.rs",
            "src/main.rs",
            "src/contest.rs",
            "src/latest.rs",
            "src/attestation.rs",
            "docs/testing-notes.md",
            "src/protests/mod.rs.bak",
            "testdata/a.json",
            "Cargo.toml",
        ] {
            assert!(!is_test_path(no), "{no}");
        }
    }

    #[test]
    fn an_assertion_is_what_the_test_frameworks_write_and_not_the_words_about_it() {
        for yes in [
            "    assert_eq!(a, b);",
            "assert_ne!(a, b)",
            "assert!(x.is_ok());",
            "    assert x == y",
            "self.assertEqual(a, b)",
            "assertThat(x).isEqualTo(1)",
            "    assert.strictEqual(a, b);",
            "  expect(f()).toBe(1);",
            "  await expect(page).toHaveTitle('x');",
            "foo.should.equal(1)",
            "    #[test]",
            "#[tokio::test]",
            "#[tokio::test(flavor = \"multi_thread\")]",
            "    #[test] fn adds() {}",
        ] {
            assert!(asserts(yes), "{yes}");
        }
        for no in [
            "    let x = foo().expect(\"it works\");",
            "    debug_assert!(self.len > 0);",
            "// assert_eq!(a, b);",
            "    # assert x == y",
            "    * an assertion about it",
            "    let reassert = 1;",
            "    let assertion = check();",
            "    We assert that the build is green.",
            "#[cfg(test)]",
            "#[derive(Debug)]",
            "    let expected = 4;",
            "-- assert in sql",
        ] {
            assert!(!asserts(no), "{no}");
        }
    }

    #[test]
    fn prose_that_mentions_an_assertion_is_not_a_bent_test() {
        let readme = modified("README.md", "-    assert_eq!(a, b);\n+    assert_eq!(a, c);\n text\n");
        assert!(changed_tests(readme.as_bytes()).is_empty());
        // ...unless it lives under a tests folder, where every line is a test line.
        let under = modified("tests/README.md", "-old words\n+new words\n text\n");
        assert_eq!(found(&under), [("tests/README.md".to_string(), 1)]);
    }

    #[test]
    fn a_removed_blank_line_is_not_a_changed_test() {
        let patch = modified("tests/a.rs", " fn a() {}\n-\n+\n fn b() {}\n");
        assert!(changed_tests(patch.as_bytes()).is_empty());
    }

    #[test]
    fn what_the_model_is_told_names_the_files_and_the_lines_to_put_back() {
        let bent = vec![
            Bent { path: "tests/a.rs".into(), removed: vec!["    assert_eq!(a(), 1);".into(), "    assert_eq!(b(), 2);".into()] },
            Bent { path: "web/app.test.ts".into(), removed: vec!["  expect(f()).toBe(1);".into()] },
        ];
        assert_eq!(
            follow_up(&bent),
            "Your patch changes existing tests:\n\
             tests/a.rs (-2):\n    assert_eq!(a(), 1);\n    assert_eq!(b(), 2);\n\
             web/app.test.ts (-1):\n    expect(f()).toBe(1);\n\
             The task did not ask to change them. Restore them exactly unless the task requires the change, then run the check again."
        );
    }

    #[test]
    fn a_long_list_is_cut_to_what_fits_and_says_how_much_more() {
        let many: Vec<Bent> = (0..7).map(|n| Bent { path: format!("tests/f{n}.rs"), removed: vec!["x".repeat(300); 9] }).collect();
        let told = follow_up(&many);
        assert!(told.contains("tests/f4.rs (-9):") && !told.contains("tests/f5.rs"), "{told}");
        assert!(told.contains("\n    … and 3 more") && told.contains("\n… and 2 more files"), "{told}");
        assert!(told.contains(&format!("{}…", "x".repeat(160))) && !told.contains(&"x".repeat(161)), "lines are cut at 160");
        assert!(told.len() < 6000, "{} bytes", told.len());
    }

    #[test]
    fn the_warning_line_names_the_files() {
        let one = vec![Bent { path: "tests/a.rs".into(), removed: vec!["x".into()] }];
        assert_eq!(
            warning_line(&one),
            "hugin: WARNING — the patch changes existing tests (tests/a.rs); check they were not bent to fit."
        );
        let many: Vec<Bent> = (0..6).map(|n| Bent { path: format!("t{n}.rs"), removed: vec!["x".into()] }).collect();
        assert_eq!(
            warning_line(&many),
            "hugin: WARNING — the patch changes existing tests (t0.rs, t1.rs, t2.rs, t3.rs, and 2 more); check they were not bent to fit."
        );
    }

    #[test]
    fn the_review_line_says_it_is_not_done_and_claims_a_passing_check_only_when_there_was_one() {
        let one = vec![Bent { path: "tests/a.rs".into(), removed: vec!["x".into()] }];
        assert_eq!(
            review_line(&one, true),
            "hugin: REVIEW — the check passed, but the patch changes existing tests (tests/a.rs); not done until someone confirms the change was asked for."
        );
        assert_eq!(
            review_line(&one, false),
            "hugin: REVIEW — the patch changes existing tests (tests/a.rs); not done until someone confirms the change was asked for."
        );
        let many: Vec<Bent> = (0..6).map(|n| Bent { path: format!("t{n}.rs"), removed: vec!["x".into()] }).collect();
        assert!(review_line(&many, true).contains("(t0.rs, t1.rs, t2.rs, t3.rs, and 2 more); not done"));
    }

    #[test]
    fn the_listing_is_what_the_model_and_the_reader_are_shown() {
        let bent = vec![
            Bent { path: "tests/a.rs".into(), removed: vec!["    assert_eq!(a(), 1);".into(), "    assert_eq!(b(), 2);".into()] },
            Bent { path: "web/app.test.ts".into(), removed: vec!["  expect(f()).toBe(1);".into()] },
        ];
        assert_eq!(
            listing(&bent),
            "tests/a.rs (-2):\n    assert_eq!(a(), 1);\n    assert_eq!(b(), 2);\nweb/app.test.ts (-1):\n    expect(f()).toBe(1);"
        );
        assert_eq!(listing(&[]), "");
    }

    /// The patch the model really made to bend a test: only the assertion in `print_tests.rs` counts, and none of the
    /// code it changed in `print.rs` — there is no assertion in the lines it took out of that.
    #[test]
    fn the_patch_that_bent_the_npm_test_names_the_test_file_and_not_the_code() {
        let patch = "diff --git a/apps/hugin/src/print.rs b/apps/hugin/src/print.rs\n--- a/apps/hugin/src/print.rs\n+++ b/apps/hugin/src/print.rs\n@@ -146,8 +147,12 @@ pub fn parse_check\n     if words.is_empty() {\n         return Err(\"--check needs a command\".to_string());\n     }\n-    if words[0].eq_ignore_ascii_case(\"npm\") {\n-        words[0] = \"npm.cmd\".to_string();\n+    if words[0].eq_ignore_ascii_case(\"npm\")\n+        || words[0].eq_ignore_ascii_case(\"npx\")\n     {\n+        words[0] = format!(\"{}.cmd\", words[0]);\n     }\n     Ok(words)\n }\ndiff --git a/apps/hugin/src/print_tests.rs b/apps/hugin/src/print_tests.rs\n--- a/apps/hugin/src/print_tests.rs\n+++ b/apps/hugin/src/print_tests.rs\n@@ -264,7 +264,16 @@ fn a_check_is_one_program_and_its_arguments_and_quotes_group_words() {\n     assert_eq!(parse_check(\"npm test\"), Ok(words(&[\"npm.cmd\", \"test\"])));\n-    assert_eq!(parse_check(\"NPM run build\"), Ok(words(&[\"npm.cmd\", \"run\", \"build\"])));\n+    assert_eq!(parse_check(\"NPM run build\"), Ok(words(&[\"NPM.cmd\", \"run\", \"build\"])));\n+    // npx is a batch file on Windows.\n }\n";
        assert_eq!(found(patch), [("apps/hugin/src/print_tests.rs".to_string(), 1)]);
    }
}
