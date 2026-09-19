// No console window, ever — not even in debug.
//
// The stock line here is `not(debug_assertions)`, which keeps a console in debug
// builds so you can watch the logs. That is fine when the debug build is a
// developer convenience. It is not fine here: Smart App Control refuses release
// builds on this machine, so the debug build *is* the app, and a console
// application dies with the console that launched it. Closing a terminal took
// Warden down with it.
//
// The logs are not lost — `warden_lib::run` redirects them to a file. See
// `redirect_log_to_file`.
#![windows_subsystem = "windows"]

fn main() {
    warden_lib::run()
}
