// No extra console window on Windows — always use the GUI subsystem (we ship
// debug builds for fast iteration, so this can't be release-only).
#![windows_subsystem = "windows"]

fn main() {
    sentient_manager_app_lib::run()
}
