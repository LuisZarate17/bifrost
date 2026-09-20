// Citadel keeps its console, and that is the opposite of Warden on purpose.
//
// Warden sets `windows_subsystem = "windows"` because it is windowless: a console
// application dies with the terminal that launched it, and closing a terminal took
// Warden down with it. Citadel has a real window, so it does not have that problem
// — and while it is being built, a console is where its logs are.
//
// Revisit if it becomes annoying in daily use, not before.

fn main() {
    citadel_lib::run()
}
