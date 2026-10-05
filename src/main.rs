#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    if let Err(err) = amber::start() {
        eprintln!("{err:#}");
        std::process::exit(1);
    }
}
