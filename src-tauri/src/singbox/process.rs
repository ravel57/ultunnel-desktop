//! Запуск и остановка процесса sing-box на каждой платформе, ожидание Clash API.

#[cfg(windows)]
use std::os::windows::process::CommandExt;
use std::path::PathBuf;
use std::process::Command;
#[cfg(target_os = "macos")]
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::thread::sleep;
use std::time::Duration;
use std::time::Instant;

use serde::Deserialize;
use tauri::AppHandle;
use tauri::Manager;
#[cfg(all(not(target_os = "macos"), not(target_os = "windows")))]
use tauri_plugin_shell::ShellExt;
use tracing::error;
use tracing::info;
use tracing::warn;

#[cfg(target_os = "macos")]
use crate::helper::HELPER_LABEL;
#[cfg(target_os = "macos")]
use crate::helper::install_helper_if_needed;
use crate::helper::install_privileged_helper;
#[cfg(target_os = "macos")]
use crate::macos_smjobbless;
use crate::singbox::CLASH_API_SECRET;
use crate::singbox::CLASH_API_VERSION_URL;
#[cfg(windows)]
use crate::singbox::CREATE_NO_WINDOW;
use crate::singbox::config::write_singbox_config;
use crate::singbox::diagnostics::begin_singbox_run;
use crate::singbox::diagnostics::describe_singbox_failure;
use crate::singbox::diagnostics::first_line;
use crate::singbox::diagnostics::singbox_extra_args;
use crate::state::AppState;
use crate::state::SharedState;

#[tauri::command]
pub fn get_state(state: SharedState) -> bool {
    #[cfg(target_os = "windows")]
    {
        return is_singbox_running_windows();
    }
    #[cfg(not(target_os = "windows"))]
    {
        state.running.load(Ordering::Relaxed)
    }
}

// #[tauri::command]
pub async fn singbox_start_root(
    config_path: String,
    args: Option<Vec<String>>,
) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        install_privileged_helper().map_err(|e| {
            error!("Запуск sing-box невозможен: helper недоступен: {}", e);
            e.to_string()
        })?;
        // 2) вычисляем путь до sing-box рядом с текущим exe
        // /Applications/ultunnel-desktop.app/Contents/MacOS/ultunnel-desktop
        let exe = std::env::current_exe().map_err(|e| e.to_string())?;
        let macos_dir = exe.parent().ok_or("Cannot get Contents/MacOS dir")?;
        let singbox_path = macos_dir.join("sing-box");

        if !singbox_path.exists() {
            let msg = format!("sing-box not found at: {}", singbox_path.display());
            error!("{}", msg);
            return Err(msg);
        }

        let cfg = PathBuf::from(config_path);
        if !cfg.exists() {
            let msg = format!("config not found at: {}", cfg.display());
            error!("{}", msg);
            return Err(msg);
        }

        let args_vec = args.unwrap_or_default();

        info!(
            "Запуск sing-box через привилегированный helper: bin={}, config={}, extra_args={:?}",
            singbox_path.display(),
            cfg.display(),
            args_vec
        );
        let started = Instant::now();
        macos_smjobbless::helper_start_singbox(HELPER_LABEL, &singbox_path, &cfg, &args_vec)
            .map_err(|e| {
                error!("Helper не смог запустить sing-box: {}", e);
                e
            })?;
        info!(
            "Helper принял запуск sing-box за {} мс",
            started.elapsed().as_millis()
        );
        Ok(())
    }
    #[cfg(not(target_os = "macos"))]
    {
        Err("not supported on non-macOS".to_string())
    }
}

// #[tauri::command]
pub(crate) async fn singbox_stop_root(app: AppHandle) -> Result<(), String> {
    #[cfg(not(target_os = "macos"))]
    {
        return Err("macOS only".into());
    }

    #[cfg(target_os = "macos")]
    {
        info!("Остановка sing-box через привилегированный helper");
        install_helper_if_needed().map_err(|e| {
            error!("Остановка sing-box: helper недоступен: {}", e);
            e
        })?;
        let _ = install_helper_if_needed();
        macos_smjobbless::helper_stop_singbox(HELPER_LABEL).map_err(|e| {
            error!("Helper не смог остановить sing-box: {}", e);
            e
        })?;
        info!("Helper остановил sing-box, добиваем возможные оставшиеся процессы (pkill/killall)");
        let _ = Command::new("/usr/bin/pkill")
            .args(["-x", "sing-box"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        let _ = Command::new("/usr/bin/killall")
            .args(["sing-box"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        let _ = Command::new("/usr/bin/pkill")
            .args(["-f", "sing-box"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        Ok(())
    }
}

#[tauri::command]
pub fn singbox_start_admin(app: AppHandle, cfg_path: String) -> Result<(), String> {
    #[cfg(not(target_os = "windows"))]
    {
        return Err("Windows only".into());
    }

    #[cfg(target_os = "windows")]
    {
        let exe_dir = std::env::current_exe()
            .map_err(|e| e.to_string())?
            .parent()
            .ok_or("no exe dir".to_string())?
            .to_path_buf();
        let singbox = exe_dir.join("sing-box.exe");
        if !singbox.exists() {
            let msg = format!("sing-box not found: {}", singbox.display());
            error!("{}", msg);
            return Err(msg);
        }
        info!(
            "Запуск sing-box с правами администратора: bin={}, config={}",
            singbox.display(),
            cfg_path
        );
        let status = runas::Command::new(singbox)
            .arg("run")
            .arg("-c")
            .arg(&cfg_path)
            .args(&singbox_extra_args(&app))
            .show(false)
            .status()
            .map_err(|e| {
                error!("Не удалось запустить sing-box через runas: {}", e);
                e.to_string()
            })?;
        info!("runas для sing-box завершился: {}", status);
        Ok(())
    }
}

#[tauri::command]
pub fn singbox_stop_admin(app: AppHandle) -> Result<(), String> {
    #[cfg(not(target_os = "windows"))]
    {
        return Err("Windows only".into());
    }
    #[cfg(target_os = "windows")]
    {
        info!("Остановка sing-box (taskkill /F)");
        let status = runas::Command::new("taskkill")
            .arg("/IM")
            .arg("sing-box.exe")
            .arg("/F")
            .show(false)
            .status()
            .map_err(|e| {
                error!("taskkill для sing-box не выполнился: {}", e);
                e.to_string()
            })?;
        info!("taskkill для sing-box завершился: {}", status);
        Ok(())
    }
}

#[cfg(all(not(target_os = "macos"), not(target_os = "windows")))]
pub(crate) async fn singbox_start(
    app: AppHandle,
    state: SharedState<'_>,
    cfg_path: String,
) -> Result<(), String> {
    {
        let guard = state.singbox.lock().unwrap();
        if guard.is_some() {
            info!("sing-box уже запущен (sidecar), повторный запуск пропущен");
            return Ok(());
        }
    }

    let sidecar = app.shell().sidecar("sing-box").map_err(|e| {
        let msg = format!("Не удалось найти sidecar sing-box: {e}");
        error!("{}", msg);
        msg
    })?;

    info!("Запуск sing-box (sidecar), config={}", cfg_path);
    let mut run_args = vec!["run".to_string(), "-c".to_string(), cfg_path.clone()];
    run_args.extend(singbox_extra_args(&app));
    let (mut rx, child) = sidecar
        .args(run_args)
        .spawn()
        .map_err(|e| {
            let msg = format!("Не удалось запустить sing-box: {e}");
            error!("{}", msg);
            msg
        })?;
    info!("sing-box (sidecar) запущен, pid={}", child.pid());

    // Вывод ядра пишем в app.log, а канал обязательно вычитываем, иначе при
    // переполнении буфера процесс может зависнуть на записи.
    tauri::async_runtime::spawn(async move {
        use tauri_plugin_shell::process::CommandEvent;
        while let Some(event) = rx.recv().await {
            match event {
                CommandEvent::Stdout(line) | CommandEvent::Stderr(line) => {
                    info!(target: "sing-box", "{}", String::from_utf8_lossy(&line).trim_end());
                }
                CommandEvent::Error(e) => error!("Ошибка процесса sing-box: {}", e),
                CommandEvent::Terminated(payload) => {
                    warn!(
                        "Процесс sing-box завершился: code={:?}, signal={:?}",
                        payload.code, payload.signal
                    );
                }
                _ => {}
            }
        }
    });

    *state.singbox.lock().unwrap() = Some(child);

    if !wait_for_clash_api(5000).await {
        error!("sing-box запущен, но Clash API не ответил за 5 с — останавливаем");
        kill_singbox(state.inner());
        return Err("sing-box запустился, но Clash API на 127.0.0.1:9090 не ответил".into());
    }

    Ok(())
}

#[cfg(all(not(target_os = "macos"), not(target_os = "windows")))]
pub(crate) async fn singbox_stop(
    _app: AppHandle,
    state: SharedState<'_>,
) -> Result<(), String> {
    kill_singbox(state.inner());
    Ok(())
}

#[cfg(not(target_os = "windows"))]
pub(crate) fn kill_singbox(state: &Arc<AppState>) {
    if let Some(child) = state.singbox.lock().unwrap().take() {
        let pid = child.pid();
        match child.kill() {
            Ok(()) => info!("Процесс sing-box (pid={}) завершён", pid),
            Err(e) => error!("Не удалось завершить процесс sing-box (pid={}): {}", pid, e),
        }
    } else {
        info!("kill_singbox: управляемого процесса sing-box нет");
    }
    state.running.store(false, Ordering::Relaxed);
}

pub(crate) fn stop_singbox_before_exit(app: &tauri::AppHandle) {
    // Всегда помечаем как "не запущено" в состоянии
    if let Some(state) = app.try_state::<Arc<AppState>>() {
        state.running.store(false, Ordering::Relaxed);
    }
    // macOS: у вас stop идет через osascript (потребует прав)
    #[cfg(target_os = "macos")]
    {
        if let Err(e) = tauri::async_runtime::block_on(singbox_stop_root(app.clone())) {
            error!("Не удалось остановить sing-box при выходе: {}", e);
        }
        return;
    }
    // Windows: у вас stop идет через taskkill, запускаемый runas (может показать UAC)
    #[cfg(target_os = "windows")]
    {
        if let Err(e) = singbox_stop_admin(app.clone()) {
            error!("Не удалось остановить sing-box при выходе: {}", e);
        }
        return;
    }
    // Linux/прочие: sing-box у вас живет как CommandChild в state.singbox
    #[cfg(all(not(target_os = "macos"), not(target_os = "windows")))]
    {
        if let Some(state) = app.try_state::<Arc<AppState>>() {
            kill_singbox(&state);
        }
    }
}

#[cfg(target_os = "windows")]
pub(crate) fn is_singbox_running_windows() -> bool {
    let mut cmd = Command::new("tasklist");
    cmd.creation_flags(CREATE_NO_WINDOW)
        .args(["/FI", "IMAGENAME eq sing-box.exe"]);
    let out = cmd.output();
    match out {
        Ok(o) if o.status.success() => {
            let s = String::from_utf8_lossy(&o.stdout).to_ascii_lowercase();
            // tasklist выводит имя процесса, если он найден
            s.contains("sing-box.exe")
        }
        _ => false,
    }
}

#[cfg(target_os = "windows")]
pub(crate) fn wait_singbox_running_windows(timeout_ms: u64) -> bool {
    let deadline = Instant::now() + Duration::from_millis(timeout_ms);
    while Instant::now() < deadline {
        if is_singbox_running_windows() {
            return true;
        }
        sleep(Duration::from_millis(120));
    }
    false
}

/// RAII-guard: снимает флаг "запуск выполняется" при выходе из функции
/// через любой return (успех, ошибка, ранний выход).
struct StartGuard<'a>(&'a std::sync::atomic::AtomicBool);

impl Drop for StartGuard<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst);
    }
}

#[tauri::command]
pub async fn singbox_start_platform(app: AppHandle, state: SharedState<'_>) -> Result<(), String> {
    // Защита от параллельного запуска: если singbox_start_platform уже выполняется
    // (например, из-за повторного клика до ответа первого вызова), второй вызов не
    // должен поднимать ещё один процесс sing-box — иначе оба runas-запуска дерутся
    // за один и тот же TUN-адаптер и оба падают.
    if state
        .singbox_starting
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_err()
    {
        warn!("Запуск VPN пропущен: запуск уже выполняется (повторный вызов)");
        return Err("Запуск VPN уже выполняется, подождите".into());
    }
    let _start_guard = StartGuard(&state.singbox_starting);

    // уже запущено — считаем успехом
    #[cfg(target_os = "windows")]
    {
        if is_singbox_running_windows() {
            info!("Запуск VPN пропущен: sing-box уже запущен");
            state.running.store(true, Ordering::Relaxed);
            return Ok(());
        }
    }

    // выбрать профиль
    let selected = state
        .settings
        .lock()
        .unwrap()
        .selected_config
        .clone()
        .ok_or_else(|| {
            warn!("Запуск VPN невозможен: профиль не выбран");
            "Не выбран конфиг"
        })?;

    // найти конфиг
    let cfg = {
        let list = state.configs.lock().unwrap();
        list.iter()
            .find(|c| c.name == selected)
            .cloned()
            .ok_or_else(|| {
                warn!("Запуск VPN невозможен: выбранный профиль {:?} не найден в списке", selected);
                "Выбранный конфиг не найден (обновите список)"
            })?
    };

    info!("Запуск VPN: профиль {:?}", selected);
    let started = Instant::now();
    let state_arc = state.inner().clone();
    let settings = { state.settings.lock().unwrap().clone() };
    let cfg_path = write_singbox_config(&app, &cfg.config, &settings).map_err(|e| {
        error!("Запуск VPN не удался: не удалось подготовить конфиг профиля {:?}: {}", selected, e);
        e
    })?;
    let cfg_path_str = cfg_path.to_string_lossy().to_string();
    begin_singbox_run(&app, &state_arc, &cfg_path);

    #[cfg(target_os = "macos")]
    {
        let r = singbox_start_root(cfg_path_str, Some(singbox_extra_args(&app))).await;
        if let Err(e) = r {
            state.running.store(false, Ordering::Relaxed);
            error!("Запуск sing-box не удался: {}", e);
            return Err(describe_singbox_failure(&app, &state_arc, &format!("Не удалось запустить sing-box ({})", first_line(&e))));
        }

        if !wait_for_clash_api(5000).await {
            state.running.store(false, Ordering::Relaxed);
            return Err(describe_singbox_failure(
                &app,
                &state_arc,
                "sing-box запустился, но Clash API на 127.0.0.1:9090 не ответил",
            ));
        }

        state.running.store(true, Ordering::Relaxed);
        info!("VPN запущен (профиль {:?}) за {} мс", selected, started.elapsed().as_millis());
        return Ok(());
    }

    #[cfg(target_os = "windows")]
    {
        // runas::Command::status() вызывает ShellExecuteExW и затем
        // WaitForSingleObject(hProcess, INFINITE) — то есть блокируется до тех пор,
        // пока сам sing-box.exe не завершится, а это происходит только при отключении
        // VPN. Если ждать этот вызов прямо здесь, singbox_start_platform не вернёт
        // ответ фронтенду, пока пользователь не нажмёт "Отключиться" — поэтому
        // запускаем его в отдельном потоке и сами проверяем результат через
        // tasklist/Clash API ниже.
        let app_for_runas = app.clone();
        let cfg_path_for_runas = cfg_path_str.clone();
        std::thread::spawn(move || {
            if let Err(e) = singbox_start_admin(app_for_runas, cfg_path_for_runas) {
                error!("Запуск sing-box через runas завершился с ошибкой: {}", e);
            }
        });

        if !wait_singbox_running_windows(2500) {
            state.running.store(false, Ordering::Relaxed);
            return Err(describe_singbox_failure(
                &app,
                &state_arc,
                "sing-box не запустился (process not found)",
            ));
        }

        if !wait_for_clash_api(5000).await {
            state.running.store(false, Ordering::Relaxed);
            return Err(describe_singbox_failure(
                &app,
                &state_arc,
                "sing-box запустился, но Clash API на 127.0.0.1:9090 не ответил",
            ));
        }

        state.running.store(true, Ordering::Relaxed);
        info!("VPN запущен (профиль {:?}) за {} мс", selected, started.elapsed().as_millis());
        return Ok(());
    }

    #[cfg(all(not(target_os = "macos"), not(target_os = "windows")))]
    {
        let r = singbox_start(app.clone(), state.clone(), cfg_path_str).await;
        match &r {
            Ok(()) => {
                state.running.store(true, Ordering::Relaxed);
                info!("VPN запущен (профиль {:?}) за {} мс", selected, started.elapsed().as_millis());
            }
            Err(e) => {
                error!("Запуск sing-box не удался: {}", e);
                return Err(describe_singbox_failure(&app, &state_arc, &e.clone()));
            }
        }
        return r;
    }
}

#[tauri::command]
pub async fn singbox_stop_platform(
    app: tauri::AppHandle,
    state: SharedState<'_>,
) -> Result<(), String> {
    info!("Остановка VPN по запросу пользователя");
    #[cfg(target_os = "macos")]
    let r = singbox_stop_root(app).await;

    #[cfg(target_os = "windows")]
    let r = singbox_stop_admin(app);

    #[cfg(all(not(target_os = "macos"), not(target_os = "windows")))]
    let r = singbox_stop(app, state.clone()).await;

    state.running.store(false, Ordering::Relaxed);
    match &r {
        Ok(()) => info!("VPN остановлен"),
        Err(e) => error!("Остановка VPN завершилась с ошибкой: {}", e),
    }
    r
}

#[derive(Debug, Deserialize)]
pub(crate) struct VersionResponse {
    pub(crate) version: Option<String>,
}

pub(crate) async fn wait_for_clash_api(timeout_ms: u64) -> bool {
    let client = reqwest::Client::new();
    let started = Instant::now();
    let deadline = started + Duration::from_millis(timeout_ms);
    let mut attempts = 0u32;
    let mut last_problem = String::from("нет попыток");

    while Instant::now() < deadline {
        attempts += 1;
        match client
            .get(CLASH_API_VERSION_URL)
            .bearer_auth(CLASH_API_SECRET)
            .send()
            .await
        {
            Ok(r) if r.status().is_success() => {
                let version = r
                    .json::<VersionResponse>()
                    .await
                    .ok()
                    .and_then(|v| v.version);
                info!(
                    "Clash API ответил через {} мс (попыток: {}), версия ядра: {}",
                    started.elapsed().as_millis(),
                    attempts,
                    version.as_deref().unwrap_or("неизвестна")
                );
                return true;
            }
            Ok(r) => last_problem = format!("HTTP {}", r.status()),
            Err(e) => last_problem = e.to_string(),
        }

        sleep(Duration::from_millis(150));
    }

    error!(
        "Clash API не ответил за {} мс (попыток: {}), последняя ошибка: {}",
        timeout_ms, attempts, last_problem
    );
    false
}
