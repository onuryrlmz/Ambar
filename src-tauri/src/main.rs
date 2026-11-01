// Prevents additional console window on Windows in release, DO NOT REMOVE!!
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    let args: Vec<std::ffi::OsString> = std::env::args_os().skip(1).collect();
    // Only the exact command words and our own options go to the command line: anything the
    // OS passes at launch, such as macOS's `-psn_*`, still opens the window.
    if ambar_lib::cli::wants_cli(args.first().map(|a| a.as_os_str())) {
        std::process::exit(ambar_lib::cli::run(&args));
    }
    ambar_lib::run()
}
