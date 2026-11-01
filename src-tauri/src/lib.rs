pub mod engine;
pub mod commands;
pub mod error;
pub mod jobs;
pub mod local_fs;
pub mod session;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .manage(jobs::Jobs::default())
        .setup(|app| {
            use tauri::Manager;
            let root = app
                .path()
                .app_cache_dir()?
                .join(session::TEMP_DIR_NAME);
            let sess = session::Session::new(root);
            sess.cleanup_stale();
            app.manage(sess);
            Ok(())
        })
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::Destroyed = event {
                use tauri::Manager;
                if let Some(j) = window.try_state::<jobs::Jobs>() {
                    j.cancel_all();
                }
                if let Some(s) = window.try_state::<session::Session>() {
                    s.cleanup();
                }
            }
        })
        .invoke_handler(tauri::generate_handler![
            commands::open_archive,
            commands::start_extract,
            commands::start_test,
            commands::cancel_job,
            commands::answer_conflict,
            commands::forget_password,
            commands::list_local,
            commands::home_dir,
            commands::parent_dir,
            commands::open_entry,
        ])
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
