//! Иконка в трее: статус VPN, подключение/отключение, быстрый выбор сервера.

use std::sync::Arc;
use std::sync::OnceLock;
use std::sync::atomic::Ordering;

use tauri::AppHandle;
use tauri::Manager;
use tauri::Wry;
use tauri::image::Image;
use tauri::menu::CheckMenuItem;
use tauri::menu::IsMenuItem;
use tauri::menu::Menu;
use tauri::menu::MenuItem;
use tauri::menu::PredefinedMenuItem;
use tauri::menu::Submenu;
use tauri::tray::TrayIconBuilder;
use tracing::info;
use tracing::warn;

use crate::configs::select_profile_internal;
use crate::state::APP_HANDLE;
use crate::state::AppState;
use crate::state::EXITING;
use crate::vpn::Origin;
use crate::vpn::RECONNECTING;
use crate::vpn::USER_WANTS_RUNNING;

const TRAY_ID: &str = "main";
const PROFILE_PREFIX: &str = "profile:";
/// В меню трея показываем не больше этого числа серверов.
const MAX_TRAY_PROFILES: usize = 40;

static DISCONNECTED_ICON: OnceLock<Option<Image<'static>>> = OnceLock::new();

struct TrayView {
    running: bool,
    reconnecting: bool,
    selected: Option<String>,
    profiles: Vec<String>,
    auto_best: bool,
}

fn view(app: &AppHandle) -> Option<TrayView> {
    let state = app.try_state::<Arc<AppState>>()?;
    let (selected, auto_best) = {
        let s = state.settings.lock().unwrap();
        (s.selected_config.clone(), s.auto_best_server)
    };
    let profiles: Vec<String> = state.configs.lock().unwrap().iter().map(|c| c.name.clone()).collect();
    Some(TrayView {
        running: state.running.load(Ordering::Relaxed),
        reconnecting: RECONNECTING.load(Ordering::SeqCst),
        selected,
        profiles,
        auto_best,
    })
}

fn shorten(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        format!("{}…", s.chars().take(max - 1).collect::<String>())
    }
}

fn status_text(v: &TrayView) -> String {
    let profile = v.selected.as_deref().map(|p| shorten(p, 32)).unwrap_or_default();
    if v.reconnecting {
        "↻ Переподключение…".to_string()
    } else if v.running {
        format!("● Подключено: {profile}")
    } else {
        "○ Отключено".to_string()
    }
}

fn build_menu(app: &AppHandle, v: &TrayView) -> tauri::Result<Menu<Wry>> {
    let status = MenuItem::with_id(app, "status", status_text(v), false, None::<&str>)?;
    let toggle_text = if v.running || v.reconnecting { "Отключить" } else { "Подключить" };
    let can_toggle = v.running || v.reconnecting || v.selected.is_some() || (v.auto_best && !v.profiles.is_empty());
    let toggle = MenuItem::with_id(app, "toggle", toggle_text, can_toggle, None::<&str>)?;

    let auto_best = CheckMenuItem::with_id(app, "auto_best", "Автовыбор лучшего", !v.profiles.is_empty(), v.auto_best, None::<&str>)?;
    let mut profile_items: Vec<CheckMenuItem<Wry>> = Vec::new();
    for (i, name) in v.profiles.iter().take(MAX_TRAY_PROFILES).enumerate() {
        profile_items.push(CheckMenuItem::with_id(
            app,
            format!("{PROFILE_PREFIX}{i}"),
            shorten(name, 48),
            true,
            v.selected.as_deref() == Some(name.as_str()),
            None::<&str>,
        )?);
    }
    let sep_in = PredefinedMenuItem::separator(app)?;
    let mut server_items: Vec<&dyn IsMenuItem<Wry>> = vec![&auto_best];
    if !profile_items.is_empty() {
        server_items.push(&sep_in);
    }
    for item in &profile_items {
        server_items.push(item);
    }
    let servers = Submenu::with_items(app, "Сервер", !v.profiles.is_empty(), &server_items)?;

    let show = MenuItem::with_id(app, "show", "Показать/Скрыть окно", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "Выход", true, None::<&str>)?;
    let sep1 = PredefinedMenuItem::separator(app)?;
    let sep2 = PredefinedMenuItem::separator(app)?;
    Menu::with_items(app, &[&status, &sep1, &toggle, &servers, &sep2, &show, &quit])
}

/// Иконка «отключено»: обесцвеченная и полупрозрачная копия иконки приложения.
fn disconnected_icon(app: &AppHandle) -> Option<Image<'static>> {
    DISCONNECTED_ICON
        .get_or_init(|| {
            let icon = app.default_window_icon()?;
            let mut rgba = icon.rgba().to_vec();
            for px in rgba.chunks_exact_mut(4) {
                let gray = (0.299 * px[0] as f32 + 0.587 * px[1] as f32 + 0.114 * px[2] as f32) as u8;
                px[0] = gray;
                px[1] = gray;
                px[2] = gray;
                px[3] = (px[3] as f32 * 0.55) as u8;
            }
            Some(Image::new_owned(rgba, icon.width(), icon.height()))
        })
        .clone()
}

fn apply(app: &AppHandle) {
    let Some(tray) = app.tray_by_id(TRAY_ID) else {
        return;
    };
    let Some(v) = view(app) else {
        return;
    };
    match build_menu(app, &v) {
        Ok(menu) => {
            let _ = tray.set_menu(Some(menu));
        }
        Err(e) => warn!("Не удалось обновить меню трея: {}", e),
    }
    let tooltip = format!("ULtunnel — {}", status_text(&v).trim_start_matches(['●', '○', '↻', ' ']));
    let _ = tray.set_tooltip(Some(tooltip));
    let icon = if v.running && !v.reconnecting {
        app.default_window_icon().cloned()
    } else {
        disconnected_icon(app)
    };
    if let Some(icon) = icon {
        let _ = tray.set_icon(Some(icon));
    }
}

/// Перестраивает меню, подсказку и иконку трея под текущее состояние. Можно вызывать
/// из любого потока.
pub(crate) fn refresh_tray() {
    let Some(app) = APP_HANDLE.get().cloned() else {
        return;
    };
    let app2 = app.clone();
    let _ = app.run_on_main_thread(move || apply(&app2));
}

fn toggle_window(app: &AppHandle) {
    if let Some(win) = app.get_webview_window("main") {
        let visible = win.is_visible().unwrap_or(true);
        info!("Трей: переключение окна ({})", if visible { "скрыть" } else { "показать" });
        if visible {
            let _ = win.hide();
        } else {
            let _ = win.show();
            let _ = win.unminimize();
            let _ = win.set_focus();
        }
    } else {
        warn!("Трей: главное окно не найдено");
    }
}

fn on_menu_event(app: &AppHandle, id: &str) {
    let state = app.state::<Arc<AppState>>().inner().clone();
    match id {
        "show" => toggle_window(app),
        "quit" => {
            info!("Трей: запрошен выход из приложения");
            EXITING.store(true, Ordering::SeqCst);
            app.exit(0);
        }
        "toggle" => {
            let app = app.clone();
            let on = state.running.load(Ordering::Relaxed)
                || RECONNECTING.load(Ordering::SeqCst)
                || (USER_WANTS_RUNNING.load(Ordering::SeqCst) && state.singbox_starting.load(Ordering::SeqCst));
            info!("Трей: {}", if on { "отключение VPN" } else { "подключение VPN" });
            tauri::async_runtime::spawn(async move {
                let _ = if on {
                    crate::vpn::disconnect(&app, Origin::Tray).await
                } else {
                    crate::vpn::connect(&app, Origin::Tray).await.map(|_| ())
                };
            });
        }
        "auto_best" => {
            let enabled = !state.settings.lock().unwrap().auto_best_server;
            crate::vpn::set_auto_best_internal(&state, enabled);
        }
        other if other.starts_with(PROFILE_PREFIX) => {
            let Some(name) = other[PROFILE_PREFIX.len()..]
                .parse::<usize>()
                .ok()
                .and_then(|i| state.configs.lock().unwrap().get(i).map(|c| c.name.clone()))
            else {
                return;
            };
            info!("Трей: выбран сервер {:?}", name);
            // ручной выбор сервера отключает автовыбор
            if state.settings.lock().unwrap().auto_best_server {
                crate::vpn::set_auto_best_internal(&state, false);
            }
            let changed = state.settings.lock().unwrap().selected_config.as_deref() != Some(name.as_str());
            select_profile_internal(&state, &name);
            if changed && state.running.load(Ordering::Relaxed) {
                let app = app.clone();
                tauri::async_runtime::spawn(async move {
                    let _ = crate::vpn::restart(&app).await;
                });
            } else {
                refresh_tray();
            }
        }
        other => warn!("Трей: неизвестный пункт меню {:?}", other),
    }
}

pub(crate) fn create_tray(app: &tauri::App) -> tauri::Result<()> {
    let handle = app.handle();
    let initial = Menu::with_items(handle, &[&MenuItem::with_id(handle, "show", "Показать/Скрыть окно", true, None::<&str>)?])?;
    let mut builder = TrayIconBuilder::with_id(TRAY_ID)
        .tooltip("ULtunnel")
        .menu(&initial)
        .show_menu_on_left_click(true)
        .on_menu_event(|app, event| on_menu_event(app, event.id().as_ref()));
    if let Some(icon) = disconnected_icon(handle).or_else(|| app.default_window_icon().cloned()) {
        builder = builder.icon(icon);
    }
    builder.build(app)?;
    Ok(())
}
