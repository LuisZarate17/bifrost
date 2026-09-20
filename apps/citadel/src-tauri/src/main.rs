// No console window, ever — not even in debug.
//
// This started the other way round: Citadel has a real window, so a console
// seemed like a free place to watch logs while building. It is not free. A
// console application on Windows **opens a console** when you launch it from a
// shortcut, and it **dies with the terminal** that started it. That is a second
// window you did not ask for, sitting in the taskbar beside the one you did.
//
// Warden reached the same conclusion for a different reason — it is windowless,
// so closing a terminal took the whole app down with it. Same attribute, same
// fix, and the logs are not lost: `citadel_lib::run` redirects them to a file.
#![windows_subsystem = "windows"]

fn main() {
    citadel_lib::run()
}
