mod api;
mod autostart;
mod browser_api;
mod configs;
mod helper;
#[cfg(target_os = "linux")]
mod linux;
mod logging;
#[cfg(target_os = "macos")]
mod macos_smjobbless;
mod settings;
mod singbox;
mod state;
mod tray;
mod updater;
mod vpn;

use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;

use tauri::Manager;
use tauri::WindowEvent;
use tauri_plugin_single_instance::init as single_instance_init;
use tracing::info;

use crate::configs::configs_path_from_settings;
use crate::configs::load_configs_from_file;
#[cfg(target_os = "linux")]
use crate::linux::relaunch_as_root_if_needed;
use crate::logging::init_file_logger;
use crate::logging::log_startup_info;
use crate::settings::LocalSettings;
use crate::singbox::diagnostics::spawn_singbox_watchdog;
use crate::singbox::process::stop_singbox_before_exit;
use crate::state::APP_HANDLE;
use crate::state::AppState;
use crate::state::EXITING;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    #[cfg(target_os = "linux")]
    if let Err(e) = relaunch_as_root_if_needed() {
        eprintln!("{e}");
    }

    let mut builder = tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(single_instance_init(|app, _argv, _cwd| {
            info!("Повторный запуск приложения — показываем существующее окно");
            if let Some(win) = app.get_webview_window("main") {
                let _ = win.show();
                let _ = win.unminimize();
                let _ = win.set_focus();
            }
        }));
    #[cfg(target_os = "macos")]
    {
        builder = builder.plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            Some(vec![]),
        ));
    }
    #[cfg(any(target_os ="windows", target_os = "linux"))]
    {
        builder = builder.plugin(tauri_plugin_autostart::init(
            Default::default(),
            Some(vec![]),
        ));
    }

    let app = builder
        .setup(|app| {
            // Логгер поднимаем первым, чтобы в app.log попало всё, что происходит при старте
            // (в том числе загрузка настроек и конфигов).
            let handle = app.handle();
            let log_guard = match init_file_logger(&handle) {
                Ok(guard) => {
                    info!("Логгер инициализирован");
                    Some(guard)
                }
                Err(e) => {
                    eprintln!("Не удалось инициализировать app.log: {e}");
                    None
                }
            };
            let _ = APP_HANDLE.set(handle.clone());
            log_startup_info(&handle);

            tray::create_tray(app)?;
            let settings_path = handle
                .path()
                .app_data_dir()
                .expect("cannot get app data dir")
                .join("config.json");

            let settings = LocalSettings::load(&settings_path);
            let configs_path = configs_path_from_settings(&settings_path);
            let configs = load_configs_from_file(&configs_path);
            info!(
                "Состояние загружено: профилей={}, выбран профиль={:?}, ключ доступа задан={}, split_routing={}, socks5_inbound={}, автозапуск={}",
                configs.len(),
                settings.selected_config,
                !settings.access_key.is_empty(),
                settings.split_routing.enabled,
                settings.socks5_inbound,
                settings.autostart_enabled
            );

            let state = Arc::new(AppState {
                settings_path,
                configs_path,
                settings: Mutex::new(settings),
                configs: Mutex::new(configs),
                running: AtomicBool::new(false),
                singbox: Mutex::new(None),
                log_guard: Mutex::new(log_guard),
                singbox_log_offset: AtomicU64::new(0),
                singbox_starting: AtomicBool::new(false),
            });

            app.manage(state.clone());
            browser_api::spawn_browser_api(state.clone());
            spawn_singbox_watchdog(state.clone());
            vpn::spawn_background_tasks(handle.clone(), state.clone());
            updater::spawn_update_loop(handle.clone(), state.clone());
            tray::refresh_tray();
            info!("Инициализация приложения завершена");
            Ok(())
        })
        .on_window_event(|window, event| {
            if let WindowEvent::CloseRequested { api, .. } = event {
                if EXITING.load(Ordering::SeqCst) {
                    return;
                }
                info!("Закрытие окна перехвачено — окно скрыто, приложение работает в трее");
                api.prevent_close();
                let _ = window.hide();
            }
        })
        .invoke_handler(tauri::generate_handler![
            configs::get_access_key,
            configs::set_access_key,
            configs::get_selected_profile,
            configs::set_selected_profile,
            singbox::process::get_state,
            configs::get_profiles,
            configs::load_configs,
            singbox::process::singbox_start_platform,
            singbox::process::singbox_stop_platform,
            logging::open_logs,
            singbox::routing::get_split_routing,
            singbox::routing::set_split_routing,
            singbox::routing::list_running_apps,
            singbox::routing::get_socks5_inbound,
            singbox::routing::set_socks5_inbound,
            singbox::dns::get_dns_servers,
            singbox::dns::set_dns_servers,
            #[cfg(target_os = "macos")]
            helper::install_helper_if_needed,
            singbox::routing::list_running_processes,
            singbox::routing::get_macos_tunneled_processes,
            singbox::routing::set_macos_tunneled_processes,
            singbox::routing::set_macos_process_tunnel_enabled,
            autostart::get_autostart_status,
            autostart::set_autostart_enabled,
            singbox::stats::get_dashboard_stats,
            singbox::profile_check::check_profiles,
            singbox::profile_check::cancel_profile_measurement,
            singbox::connections::get_connections,
            singbox::connections::close_connection,
            singbox::routing_io::export_routing_settings,
            singbox::routing_io::import_routing_settings,
            vpn::vpn_connect,
            vpn::vpn_disconnect,
            vpn::vpn_restart,
            vpn::get_vpn_status,
            vpn::get_app_options,
            vpn::set_app_options,
            vpn::set_profile_ranking,
            updater::get_update_state,
            updater::check_for_updates,
            updater::install_update,
        ])
        .build(tauri::generate_context!())
        .expect("error while running tauri application");
    app.run(|app_handle, event| {
        if let tauri::RunEvent::ExitRequested { api, .. } = event {
            if EXITING.swap(true, Ordering::SeqCst) {
                return;
            }
            info!("Завершение приложения: останавливаем sing-box перед выходом");
            api.prevent_exit();
            let app = app_handle.clone();
            tauri::async_runtime::spawn(async move {
                stop_singbox_before_exit(&app);
                info!("Приложение завершает работу");
                app.exit(0);
            });
        }
    });
}
