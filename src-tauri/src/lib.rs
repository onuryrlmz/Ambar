pub mod batch;
pub mod cli;
pub mod engine;
pub mod commands;
pub mod error;
pub mod jobs;
pub mod local_fs;
pub mod menu;
pub mod open_guard;
pub mod session;

/// Cancels running jobs, waits briefly so they remove their partial files, then
/// deletes this run's decrypted temp files.
fn shutdown(app: &tauri::AppHandle) {
    use tauri::Manager;
    if let Some(jobs) = app.try_state::<jobs::Jobs>() {
        jobs.cancel_all();
        jobs.wait_idle(std::time::Duration::from_secs(2));
    }
    if let Some(session) = app.try_state::<session::Session>() {
        session.cleanup();
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    // Release builds: a panic payload could hold archive-derived data, so print nothing.
    #[cfg(not(any(debug_assertions, test)))]
    std::panic::set_hook(Box::new(|_| {}));
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
            let handle = app.handle();
            app.set_menu(menu::build(handle, "tr", false)?)?;
            app.on_menu_event(|app, event| menu::forward_event(app, event.id().as_ref()));
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::open_archive,
            commands::start_extract,
            commands::start_test,
            commands::plan_batch,
            commands::archive_needs_password,
            commands::cancel_job,
            commands::answer_conflict,
            commands::list_local,
            commands::home_dir,
            commands::parent_dir,
            commands::open_entry,
            commands::open_local,
            commands::quick_look_local,
            menu::set_menu_language,
        ])
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        .run(|app, event| {
            // Covers window close and Cmd+Q alike. Idempotent.
            if let tauri::RunEvent::Exit = event {
                shutdown(app);
            }
        });
}
