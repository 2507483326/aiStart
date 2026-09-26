mod autostart;
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
        // 必须第一个注册：第二次启动时本进程直接退出，回调跑在**已有实例**里，
        // 把它的主面板唤到前台 —— 等价于点托盘菜单的「打开主界面」。
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            show_main_window(app);
        }))
        .plugin(tauri_plugin_opener::init())
        .setup(|app| {
            let config_dir = app.path().app_config_dir()?;
            settings::init(&config_dir)?;
            // 开机启动由设置里的开关驱动（默认开启）：启动时同步一次注册表，
            // 既让默认值在首次运行就落地，也让「安装位置变了」自愈。失败不拦启动。
            let _ = autostart::apply(settings::snapshot().launch_at_login);
            filters::load()?;
            gateway::hydrate();
            gateway::attach(app.handle().clone());
            let _ = gateway::start();

            // 报文保留策略：启动先清一次过期报文，之后每天一次（按设置里的「请求保存时间」）。
            usage::spawn_retention_task();

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
            commands::system::open_data_dir,
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
