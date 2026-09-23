mod commands;
mod domain;
mod error;
mod gateway;
mod platform;
mod providers;
mod settings;

#[cfg(test)]
mod tests;

use tauri::Manager;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .setup(|app| {
            let config_dir = app.path().app_config_dir()?;
            settings::init(config_dir)?;

            if settings::snapshot().active_model().is_some() {
                let _ = gateway::start();
            }

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::apps::list_apps,
            commands::apps::apply_model,
            commands::apps::clear_app_model,
            commands::apps::install_app,
            commands::apps::update_app,
            commands::apps::app_apply_mode_label,
            commands::models::list_model_formats,
            commands::models::list_models,
            commands::models::list_model_presets,
            commands::models::save_model,
            commands::models::delete_model,
            commands::models::activate_model,
            commands::models::test_model,
            commands::models::fetch_upstream_models,
            commands::gateway::gateway_status,
            commands::gateway::start_gateway,
            commands::gateway::stop_gateway,
            commands::gateway::restart_gateway,
            commands::system::get_settings,
            commands::system::update_settings,
            commands::system::app_info,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
