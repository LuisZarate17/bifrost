fn main() {
    // The exe embeds icons/icon.ico (the taskbar, the shortcuts and the tray all
    // show it), but tauri-build only reruns when tauri.conf.json or the
    // capabilities change, so a redrawn icon would otherwise never reach the exe.
    println!("cargo:rerun-if-changed=icons");
    tauri_build::build()
}
