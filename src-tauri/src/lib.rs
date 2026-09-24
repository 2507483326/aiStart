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

use tauri::{
    menu::{Menu, MenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    App, AppHandle, Manager, WindowEvent,
};

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
            let _ = gateway::start();

            init_tray(app)?;

            Ok(())
        })
        .on_window_event(|window, event| {
            if let WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                let _ = window.hide();
            }
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

fn init_tray(app: &App) -> tauri::Result<()> {
    let open_item = MenuItem::with_id(app, "open", "打开主界面", true, None::<&str>)?;
    let quit_item = MenuItem::with_id(app, "quit", "退出应用", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&open_item, &quit_item])?;

    let mut builder = TrayIconBuilder::with_id("main-tray")
        .tooltip("AI Start")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id.as_ref() {
            "open" => show_main_window(app),
            "quit" => app.exit(0),
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                show_main_window(tray.app_handle());
            }
        });

    if let Some(icon) = app.default_window_icon().cloned() {
        builder = builder.icon(icon);
    }

    builder.build(app)?;
    Ok(())
}

fn show_main_window(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
    }
}
