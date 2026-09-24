mod commands;
mod db;
mod domain;
mod error;
mod events;
mod filters;
mod gateway;
mod install;
mod platform;
mod providers;
mod settings;
mod updates;
mod usage;

#[cfg(test)]
mod tests;

use tauri::Manager;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .setup(|app| {
            let config_dir = app.path().app_config_dir()?;
            settings::init(&config_dir)?;
            filters::load()?;
            gateway::hydrate();
            gateway::attach(app.handle().clone());

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
            commands::apps::check_app_updates,
            commands::apps::open_download_dir,
            commands::apps::installer_url,
            commands::models::list_model_formats,
            commands::models::list_models,
            commands::models::save_model,
            commands::models::delete_model,
            commands::models::activate_model,
            commands::models::test_model,
            commands::models::test_model_config,
            commands::models::fetch_upstream_models,
            commands::filters::list_filters,
            commands::filters::save_filter,
            commands::filters::set_filter_enabled,
            commands::filters::delete_filter,
            commands::gateway::gateway_status,
            commands::gateway::restart_gateway,
            commands::system::get_settings,
            commands::system::update_settings,
            commands::system::app_info,
            commands::events::list_events,
            commands::usage::usage_summary,
            commands::usage::usage_records,
            commands::usage::usage_page,
            commands::usage::usage_detail,
            commands::translate::translate_text,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
