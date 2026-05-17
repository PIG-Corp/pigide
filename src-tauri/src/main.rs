// Prevents additional console window on Windows in release; ignored on other targets.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    pigide_lib::run();
}
