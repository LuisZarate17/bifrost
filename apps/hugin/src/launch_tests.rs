use crate::launch::{choose_folder, wt_args};

#[test]
fn the_wt_arguments_open_a_new_window_with_a_titled_tab_in_the_folder() {
    assert_eq!(
        wt_args(r"C:\work\demo", r"C:\bin\hugin.exe"),
        [
            "-w",
            "new",
            "new-tab",
            "--title",
            "Hugin",
            "-d",
            r"C:\work\demo",
            r"C:\bin\hugin.exe"
        ]
    );
}

#[test]
fn a_semicolon_in_a_path_is_escaped_and_spaces_are_left_to_quoting() {
    let args = wt_args(r"C:\a;b\my work", r"C:\Program Files;x\hugin.exe");
    assert_eq!(args[6], r"C:\a\;b\my work");
    assert_eq!(args[7], r"C:\Program Files\;x\hugin.exe");
    assert_eq!(args.len(), 8);
}

#[test]
fn the_folder_is_the_recorded_one_only_while_it_exists() {
    let exists = |f: &str| f == r"C:\work";
    let profile = || Some(r"C:\Users\me".to_string());
    assert_eq!(choose_folder(Some(r"C:\work".into()), profile(), exists), r"C:\work");
    assert_eq!(
        choose_folder(Some(r"C:\gone".into()), profile(), exists),
        r"C:\Users\me"
    );
    assert_eq!(choose_folder(None, profile(), exists), r"C:\Users\me");
    assert_eq!(choose_folder(None, None, exists), ".");
}
