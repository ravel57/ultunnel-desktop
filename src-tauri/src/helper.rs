//! macOS: привилегированный helper (SMJobBless) и путь до бинарника sing-box.

use std::path::Path;
use std::path::PathBuf;

use tauri::AppHandle;
use tauri::Manager;
use tracing::error;
use tracing::info;

#[cfg(target_os = "macos")]
use crate::macos_smjobbless;

#[cfg(target_os = "macos")]
pub(crate) const HELPER_LABEL: &str = "ru.ravel.ultunnel-macos.helper";

#[cfg(target_os = "macos")]
pub(crate) const HELPER_DST: &str = "/Library/PrivilegedHelperTools/ru.ravel.ultunnel-macos.helper";

/// Подсказка, когда helper установлен, но launchd его так и не запустил.
#[cfg(target_os = "macos")]
const HELPER_NOT_RUNNING_HINT: &str = "Служба ultunnel установлена, но macOS её не запускает. \
Проверьте, что она не отключена (`launchctl print-disabled system`), \
а на macOS 13+ — что ultunnel включён в «Системные настройки → Основные → Объекты входа → \
Разрешить в фоне», затем перезапустите приложение.";

/// Попытка включить отключённую службу делается не больше одного раза за запуск.
#[cfg(target_os = "macos")]
static ENABLE_ATTEMPTED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// `launchctl disable` запоминается в базе launchd отдельно от файлов: SMJobBless и
/// переустановка его не сбрасывают, `bootstrap` падает с "5: Input/output error", и
/// helper никогда не стартует. Проверка не требует root.
#[cfg(target_os = "macos")]
fn helper_disabled_in_launchd() -> bool {
    let out = match std::process::Command::new("/bin/launchctl")
        .args(["print-disabled", "system"])
        .output()
    {
        Ok(o) => o,
        Err(_) => return false,
    };
    let needle = format!("\"{}\"", HELPER_LABEL);
    String::from_utf8_lossy(&out.stdout).lines().any(|l| {
        let l = l.trim();
        l.starts_with(&needle) && (l.ends_with("=> true") || l.ends_with("=> disabled"))
    })
}

/// Включает и загружает службу с правами администратора (запрос пароля через osascript).
#[cfg(target_os = "macos")]
fn enable_helper_in_launchd() -> Result<(), String> {
    let plist = format!("/Library/LaunchDaemons/{}.plist", HELPER_LABEL);
    let script = format!(
        "do shell script \"/bin/launchctl enable system/{label}; /bin/launchctl bootstrap system {plist} || true\" with administrator privileges",
        label = HELPER_LABEL,
        plist = plist
    );
    let out = std::process::Command::new("/usr/bin/osascript")
        .args(["-e", &script])
        .output()
        .map_err(|e| format!("osascript: {}", e))?;
    if out.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
    }
}

/// Если helper не отвечает из-за отключённой службы — включает её и проверяет снова.
/// `None` — служба не отключена (или попытка уже была), причина в другом.
#[cfg(target_os = "macos")]
fn try_enable_disabled_helper() -> Option<Result<(), String>> {
    use std::sync::atomic::Ordering;
    if !helper_disabled_in_launchd() || ENABLE_ATTEMPTED.swap(true, Ordering::SeqCst) {
        return None;
    }
    info!("Служба helper отключена в launchd, включаем (launchctl enable + bootstrap)");
    if let Err(e) = enable_helper_in_launchd() {
        error!("Не удалось включить службу helper: {}", e);
        return Some(Err(HELPER_NOT_RUNNING_HINT.to_string()));
    }
    Some(ping_helper_with_retry(6).map_err(|e| {
        error!("Служба helper включена, но не отвечает по XPC: {}", e);
        HELPER_NOT_RUNNING_HINT.to_string()
    }).map(|()| info!("Служба helper включена и отвечает")))
}

/// SMJobBless (запрос пароля) делаем не больше одного раза за запуск приложения:
/// если после установки helper всё равно не отвечает, повторная установка ничего не
/// изменит, а пользователь получит бесконечные запросы пароля.
#[cfg(target_os = "macos")]
static BLESS_ATTEMPTED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Сериализует проверку/установку: параллельные вызовы не должны показывать
/// несколько окон с паролем одновременно.
#[cfg(target_os = "macos")]
static INSTALL_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[cfg(target_os = "macos")]
fn ping_helper_with_retry(attempts: u32) -> Result<(), String> {
    let mut last_err = String::new();
    for i in 0..attempts {
        match macos_smjobbless::helper_ping(HELPER_LABEL) {
            Ok(()) => return Ok(()),
            Err(e) => last_err = e,
        }
        if i + 1 < attempts {
            std::thread::sleep(std::time::Duration::from_millis(500));
        }
    }
    Err(last_err)
}

#[cfg(target_os = "macos")]
#[tauri::command]
pub fn install_helper_if_needed() -> Result<(), String> {
    use std::sync::atomic::Ordering;

    let _guard = INSTALL_LOCK.lock().unwrap_or_else(|e| e.into_inner());

    let reason = if !Path::new(HELPER_DST).exists() {
        "не установлен".to_string()
    } else if installed_helper_is_stale() {
        "отличается от helper в бандле (старая версия/другая архитектура)".to_string()
    } else {
        // Файл на месте ещё не значит, что LaunchDaemon загружен: без него XPC сразу
        // инвалидируется. Проверяем живым вызовом.
        match macos_smjobbless::helper_ping(HELPER_LABEL) {
            Ok(()) => return Ok(()),
            Err(e) => {
                if let Some(r) = try_enable_disabled_helper() {
                    return r;
                }
                format!("установлен, но не отвечает по XPC ({})", e)
            }
        }
    };

    if BLESS_ATTEMPTED.swap(true, Ordering::SeqCst) {
        error!("Привилегированный helper {}; повторную установку в этом запуске не делаем", reason);
        return Err(HELPER_NOT_RUNNING_HINT.to_string());
    }

    info!("Привилегированный helper {}, запускаем установку (SMJobBless)", reason);
    macos_smjobbless::install_helper(HELPER_LABEL).map_err(|e| {
        error!("Установка привилегированного helper не удалась: {}", e);
        e
    })?;
    if !Path::new(HELPER_DST).exists() {
        let msg = format!(
            "SMJobBless reported success but helper not found at {}",
            HELPER_DST
        );
        error!("{}", msg);
        return Err(msg);
    }
    match ping_helper_with_retry(6) {
        Ok(()) => {
            info!("Привилегированный helper установлен и отвечает: {}", HELPER_DST);
            Ok(())
        }
        Err(e) => {
            error!("Helper установлен, но не отвечает по XPC после установки: {}", e);
            try_enable_disabled_helper().unwrap_or_else(|| Err(HELPER_NOT_RUNNING_HINT.to_string()))
        }
    }
}

/// SMJobBless копирует helper из бандла побайтно, поэтому любое отличие от
/// `Contents/Library/LaunchServices/<label>` значит, что установлен helper от другой
/// сборки (например arm64-only на Intel — тогда launchd не может его запустить и XPC
/// сразу инвалидируется). Если сравнить не получается — считаем helper актуальным,
/// чтобы не запрашивать пароль при каждом запуске.
#[cfg(target_os = "macos")]
fn installed_helper_is_stale() -> bool {
    let bundled = match std::env::current_exe() {
        Ok(exe) => match exe.parent().and_then(|d| d.parent()) {
            Some(contents) => contents.join("Library/LaunchServices").join(HELPER_LABEL),
            None => return false,
        },
        Err(_) => return false,
    };
    let bundled_bytes = match std::fs::read(&bundled) {
        Ok(b) => b,
        Err(_) => return false, // запуск не из .app (cargo run) — сравнивать не с чем
    };
    match std::fs::read(HELPER_DST) {
        Ok(installed) => installed != bundled_bytes,
        Err(_) => std::fs::metadata(HELPER_DST)
            .map(|m| m.len() != bundled_bytes.len() as u64)
            .unwrap_or(false),
    }
}

#[tauri::command]
pub fn install_privileged_helper() -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        return install_helper_if_needed();
    }

    #[cfg(not(target_os = "macos"))]
    {
        Err("install_privileged_helper поддержан только на macOS".to_string())
    }
}

#[cfg(target_os = "macos")]
pub(crate) fn singbox_bin_path(app: &AppHandle) -> Result<PathBuf, String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let dir = exe.parent().ok_or("no parent dir".to_string())?;
    let p = dir.join("sing-box");
    if p.exists() {
        return Ok(p);
    }
    let res = app.path().resource_dir().map_err(|e| e.to_string())?;
    let p2 = res.join("sing-box");
    if p2.exists() {
        return Ok(p2);
    }
    Err(format!("sing-box not found: {:?} / {:?}", p, p2))
}
