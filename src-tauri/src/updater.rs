//! Обновление приложения: проверка версии на win/mac.ultunnel.ru, скачивание сборки
//! с проверкой sha256, установка (Windows — NSIS/MSI, macOS — замена .app из DMG)
//! и тихое автообновление в фоне.
//!
//! Версия приложения задаётся в `src-tauri/tauri.conf.json` (поле `version`).

use std::fs;
use std::io::Write;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use std::time::Duration;
use std::time::Instant;

use futures_util::StreamExt;
use serde::Deserialize;
use serde::Serialize;
use sha2::Digest;
use sha2::Sha256;
use tauri::AppHandle;
use tauri::Emitter;
use tauri::Manager;
use tracing::error;
use tracing::info;
use tracing::warn;

use crate::singbox::process::singbox_stop_platform;
use crate::state::AppState;
use crate::state::EXITING;
use crate::vpn::USER_WANTS_RUNNING;
use crate::vpn::notify;
use crate::vpn::unix_now;

/// Хост раздачи сборок для текущей платформы.
#[cfg(target_os = "windows")]
pub(crate) const UPDATE_HOST: Option<&str> = Some("win.ultunnel.ru");
#[cfg(target_os = "macos")]
pub(crate) const UPDATE_HOST: Option<&str> = Some("mac.ultunnel.ru");
#[cfg(not(any(target_os = "windows", target_os = "macos")))]
pub(crate) const UPDATE_HOST: Option<&str> = None;

const CHECK_INTERVAL_SECS: u64 = 6 * 3600;
const CHECK_RETRY_SECS: u64 = 3600;

// ---------------------------------------------------------------------------
// Сравнение версий (как AppVersions.kt на сервере)
// ---------------------------------------------------------------------------

fn split_version(v: &str) -> Vec<String> {
    let v = v.trim();
    let v = v.strip_prefix('v').unwrap_or(v);
    v.split(['.', '-', '_', '+'])
        .filter(|p| !p.is_empty())
        .map(|p| p.to_string())
        .collect()
}

fn is_numeric(p: &str) -> bool {
    !p.is_empty() && p.chars().all(|c| c.is_ascii_digit())
}

fn compare_parts(a: &str, b: &str) -> std::cmp::Ordering {
    match (a.parse::<u64>(), b.parse::<u64>()) {
        (Ok(x), Ok(y)) => x.cmp(&y),
        // число старше суффикса: 1.4.1 новее 1.4.rc1
        (Ok(_), Err(_)) => std::cmp::Ordering::Greater,
        (Err(_), Ok(_)) => std::cmp::Ordering::Less,
        _ => a.to_lowercase().cmp(&b.to_lowercase()),
    }
}

pub(crate) fn compare_versions(left: &str, right: &str) -> std::cmp::Ordering {
    use std::cmp::Ordering::*;
    let a = split_version(left);
    let b = split_version(right);
    for i in 0..a.len().max(b.len()) {
        let r = match (a.get(i), b.get(i)) {
            (None, None) => Equal,
            // «1.4» и «1.4.0» равны; релиз старше своей пре-сборки «1.4-rc1»
            (None, Some(pb)) => if is_numeric(pb) { compare_parts("0", pb) } else { Greater },
            (Some(pa), None) => if is_numeric(pa) { compare_parts(pa, "0") } else { Less },
            (Some(pa), Some(pb)) => compare_parts(pa, pb),
        };
        if r != Equal {
            return r;
        }
    }
    Equal
}

pub(crate) fn is_newer(candidate: &str, current: &str) -> bool {
    compare_versions(candidate, current) == std::cmp::Ordering::Greater
}

/// Версия годится для пути на сервере раздачи (`win.ultunnel.ru/1.4.2`).
fn valid_version(v: &str) -> bool {
    let mut chars = v.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_digit())
        && v.len() <= 64
        && v.chars().all(|c| c.is_ascii_alphanumeric() || "._+-".contains(c))
}

// ---------------------------------------------------------------------------
// Состояние
// ---------------------------------------------------------------------------

/// Ответ `/update` сервера раздачи.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ReleaseInfo {
    pub version: String,
    #[serde(default)]
    pub file_name: Option<String>,
    #[serde(default)]
    pub size_bytes: Option<u64>,
    #[serde(default)]
    pub sha256: Option<String>,
    #[serde(default)]
    pub release_notes: Option<String>,
    #[serde(default)]
    pub mandatory: bool,
    #[serde(default)]
    pub published_at: Option<String>,
    #[serde(default)]
    pub update_available: bool,
}

#[derive(Debug, Clone, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct UpdateState {
    pub current_version: String,
    /// Обновления поддерживаются на этой платформе.
    pub supported: bool,
    pub checking: bool,
    pub last_checked_at: Option<u64>,
    /// Более новая версия, если есть.
    pub available: Option<ReleaseInfo>,
    pub downloading: bool,
    pub downloaded_bytes: u64,
    pub total_bytes: Option<u64>,
    /// Сборка скачана и проверена — можно ставить.
    pub ready: bool,
    pub installing: bool,
    pub error: Option<String>,
}

static STATE: Mutex<Option<UpdateState>> = Mutex::new(None);
/// Скачивание/установка идёт — второй раз не запускаем.
static BUSY: AtomicBool = AtomicBool::new(false);
static NOTIFIED_VERSION: Mutex<Option<String>> = Mutex::new(None);

fn current_version(app: &AppHandle) -> String {
    app.package_info().version.to_string()
}

fn snapshot(app: &AppHandle) -> UpdateState {
    let mut guard = STATE.lock().unwrap();
    guard
        .get_or_insert_with(|| UpdateState {
            current_version: current_version(app),
            supported: UPDATE_HOST.is_some(),
            ..Default::default()
        })
        .clone()
}

fn modify(app: &AppHandle, f: impl FnOnce(&mut UpdateState)) -> UpdateState {
    snapshot(app);
    let state = {
        let mut guard = STATE.lock().unwrap();
        let s = guard.as_mut().unwrap();
        f(s);
        s.clone()
    };
    let _ = app.emit("update-state", state.clone());
    state
}

struct BusyGuard;

impl BusyGuard {
    fn acquire() -> Option<Self> {
        BUSY.compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .ok()
            .map(|_| BusyGuard)
    }
}

impl Drop for BusyGuard {
    fn drop(&mut self) {
        BUSY.store(false, Ordering::SeqCst);
    }
}

// ---------------------------------------------------------------------------
// Проверка версии
// ---------------------------------------------------------------------------

fn http_client(timeout: Option<Duration>) -> Result<reqwest::Client, String> {
    let mut b = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(15))
        // сборку берём только с нашего хоста: перенаправление на чужой не выполняем
        .redirect(reqwest::redirect::Policy::none())
        .user_agent(concat!("ultunnel-desktop/", env!("CARGO_PKG_VERSION")));
    if let Some(t) = timeout {
        b = b.timeout(t);
    }
    b.build().map_err(|e| e.to_string())
}

pub(crate) async fn check(app: &AppHandle) -> Result<UpdateState, String> {
    let Some(host) = UPDATE_HOST else {
        return Ok(snapshot(app));
    };
    let current = current_version(app);
    modify(app, |s| {
        s.checking = true;
        s.error = None;
    });

    let result: Result<ReleaseInfo, String> = async {
        let url = reqwest::Url::parse_with_params(
            &format!("https://{host}/update"),
            &[("version", current.as_str()), ("format", "json")],
        )
        .map_err(|e| e.to_string())?;
        let r = http_client(Some(Duration::from_secs(20)))?
            .get(url)
            .header("Accept", "application/json")
            .send()
            .await
            .map_err(|e| format!("Сервер обновлений недоступен: {}", e.without_url()))?;
        if r.status() == reqwest::StatusCode::NOT_FOUND {
            return Err("На сервере пока нет сборки для этой платформы".into());
        }
        if r.status().is_redirection() {
            return Err(format!("Сервер обновлений перенаправляет запрос (HTTP {}) — раздача ещё не настроена", r.status()));
        }
        if !r.status().is_success() {
            return Err(format!("Сервер обновлений ответил HTTP {}", r.status()));
        }
        r.json::<ReleaseInfo>()
            .await
            .map_err(|e| format!("Некорректный ответ сервера обновлений: {}", e.without_url()))
    }
    .await;

    match result {
        Ok(release) => {
            // Сервер тоже сравнивает версии, но доверяем только своему сравнению:
            // откат на старую сборку или ошибка в updateAvailable не должны ставить её.
            let newer = valid_version(&release.version) && is_newer(&release.version, &current);
            info!(
                "Проверка обновлений: текущая {}, на сервере {} (updateAvailable={}, mandatory={}) → {}",
                current,
                release.version,
                release.update_available,
                release.mandatory,
                if newer { "есть обновление" } else { "обновлений нет" }
            );
            Ok(modify(app, |s| {
                s.checking = false;
                s.last_checked_at = Some(unix_now());
                let changed = s.available.as_ref().map(|a| &a.version) != Some(&release.version);
                if newer {
                    if changed {
                        s.ready = false;
                        s.downloaded_bytes = 0;
                        s.total_bytes = release.size_bytes;
                    }
                    s.available = Some(release);
                } else {
                    s.available = None;
                    s.ready = false;
                }
            }))
        }
        Err(e) => {
            warn!("Проверка обновлений не удалась: {}", e);
            modify(app, |s| {
                s.checking = false;
                s.error = Some(e.clone());
            });
            Err(e)
        }
    }
}

// ---------------------------------------------------------------------------
// Скачивание
// ---------------------------------------------------------------------------

#[cfg(target_os = "windows")]
const ALLOWED_EXTENSIONS: &[&str] = &["exe", "msi"];
#[cfg(target_os = "macos")]
const ALLOWED_EXTENSIONS: &[&str] = &["dmg"];
#[cfg(not(any(target_os = "windows", target_os = "macos")))]
const ALLOWED_EXTENSIONS: &[&str] = &[];

/// Безопасное имя файла сборки: только базовое имя, только ожидаемое расширение.
pub(crate) fn installer_file_name(release: &ReleaseInfo) -> Result<String, String> {
    let default_ext = ALLOWED_EXTENSIONS.first().copied().unwrap_or("bin");
    let raw = release
        .file_name
        .clone()
        .unwrap_or_else(|| format!("ultunnel-{}.{}", release.version, default_ext));
    let base = raw.rsplit(['/', '\\']).next().unwrap_or("").to_string();
    let clean: String = base
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || "._-".contains(c) { c } else { '_' })
        .collect();
    let ext = Path::new(&clean)
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .unwrap_or_default();
    if clean.starts_with('.') || !ALLOWED_EXTENSIONS.contains(&ext.as_str()) {
        return Err(format!("Неожиданный тип файла сборки: {raw}"));
    }
    Ok(clean)
}

fn updates_dir(app: &AppHandle) -> Result<PathBuf, String> {
    Ok(app.path().app_cache_dir().map_err(|e| e.to_string())?.join("updates"))
}

fn sha256_file(path: &Path) -> Result<String, String> {
    let mut f = fs::File::open(path).map_err(|e| e.to_string())?;
    let mut h = Sha256::new();
    std::io::copy(&mut f, &mut h).map_err(|e| e.to_string())?;
    Ok(format!("{:x}", h.finalize()))
}

fn verify_file(path: &Path, release: &ReleaseInfo) -> Result<(), String> {
    let size = fs::metadata(path).map_err(|e| e.to_string())?.len();
    if let Some(expected) = release.size_bytes {
        if expected > 0 && size != expected {
            return Err(format!("Размер файла {size} байт, ожидалось {expected}"));
        }
    }
    match release.sha256.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        Some(expected) => {
            let actual = sha256_file(path)?;
            if !actual.eq_ignore_ascii_case(expected) {
                return Err("Контрольная сумма скачанной сборки не совпадает (sha256)".into());
            }
        }
        None => warn!("Сервер не прислал sha256 сборки — проверяется только размер"),
    }
    Ok(())
}

/// Скачивает сборку `release` (если ещё не скачана) и возвращает путь к проверенному файлу.
async fn download(app: &AppHandle, release: &ReleaseInfo) -> Result<PathBuf, String> {
    let host = UPDATE_HOST.ok_or("Обновления не поддерживаются на этой платформе")?;
    if !valid_version(&release.version) {
        return Err(format!("Некорректная версия сборки: {}", release.version));
    }
    let dir = updates_dir(app)?.join(&release.version);
    let target = dir.join(installer_file_name(release)?);

    if target.exists() {
        if verify_file(&target, release).is_ok() {
            modify(app, |s| s.ready = true);
            return Ok(target);
        }
        let _ = fs::remove_file(&target);
    }

    // старые скачанные версии больше не нужны
    if let Ok(entries) = fs::read_dir(updates_dir(app)?) {
        for e in entries.flatten() {
            if e.file_name().to_string_lossy() != release.version {
                let _ = fs::remove_dir_all(e.path());
            }
        }
    }
    fs::create_dir_all(&dir).map_err(|e| e.to_string())?;

    // качаем конкретную версию, а не «текущую»: если пока мы качаем опубликуют
    // новую сборку, sha256 из проверки всё равно совпадёт
    let url = format!("https://{host}/{}", release.version);
    info!("Скачивание обновления {}: {}", release.version, url);
    modify(app, |s| {
        s.downloading = true;
        s.downloaded_bytes = 0;
        s.total_bytes = release.size_bytes;
        s.error = None;
    });

    let result: Result<(), String> = async {
        let resp = http_client(None)?
            .get(&url)
            .send()
            .await
            .map_err(|e| format!("Не удалось скачать обновление: {}", e.without_url()))?;
        if !resp.status().is_success() {
            return Err(format!("Сервер обновлений ответил HTTP {}", resp.status()));
        }
        let total = resp.content_length().or(release.size_bytes);
        let part = target.with_extension("part");
        let mut file = fs::File::create(&part).map_err(|e| e.to_string())?;
        let mut stream = resp.bytes_stream();
        let mut done: u64 = 0;
        let mut last_emit = Instant::now();
        loop {
            let next = tokio::time::timeout(Duration::from_secs(60), stream.next())
                .await
                .map_err(|_| "Скачивание обновления зависло (нет данных 60 с)".to_string())?;
            let Some(chunk) = next else { break };
            let chunk = chunk.map_err(|e| format!("Обрыв при скачивании обновления: {}", e.without_url()))?;
            file.write_all(&chunk).map_err(|e| e.to_string())?;
            done += chunk.len() as u64;
            if last_emit.elapsed() >= Duration::from_millis(300) {
                last_emit = Instant::now();
                modify(app, |s| {
                    s.downloaded_bytes = done;
                    s.total_bytes = total;
                });
            }
        }
        file.flush().map_err(|e| e.to_string())?;
        drop(file);
        if let Err(e) = verify_file(&part, release) {
            let _ = fs::remove_file(&part);
            return Err(e);
        }
        fs::rename(&part, &target).map_err(|e| e.to_string())?;
        info!("Обновление {} скачано и проверено: {}", release.version, target.display());
        Ok(())
    }
    .await;

    match result {
        Ok(()) => {
            modify(app, |s| {
                s.downloading = false;
                s.ready = true;
                s.downloaded_bytes = s.total_bytes.unwrap_or(s.downloaded_bytes);
            });
            Ok(target)
        }
        Err(e) => {
            error!("Скачивание обновления не удалось: {}", e);
            modify(app, |s| {
                s.downloading = false;
                s.error = Some(e.clone());
            });
            Err(e)
        }
    }
}

/// Уже скачанная и проверенная сборка новее текущей (с прошлого запуска).
fn downloaded_newer(app: &AppHandle) -> Option<(String, PathBuf)> {
    let current = current_version(app);
    let dir = updates_dir(app).ok()?;
    let mut best: Option<(String, PathBuf)> = None;
    for e in fs::read_dir(&dir).ok()?.flatten() {
        let version = e.file_name().to_string_lossy().to_string();
        if !valid_version(&version) || !is_newer(&version, &current) {
            let _ = fs::remove_dir_all(e.path());
            continue;
        }
        let file = fs::read_dir(e.path()).ok()?.flatten().map(|f| f.path()).find(|p| {
            let ext = p.extension().and_then(|x| x.to_str()).unwrap_or("").to_ascii_lowercase();
            ALLOWED_EXTENSIONS.contains(&ext.as_str())
        });
        if let Some(file) = file {
            if best.as_ref().map(|(v, _)| is_newer(&version, v)).unwrap_or(true) {
                best = Some((version, file));
            }
        }
    }
    best
}

// ---------------------------------------------------------------------------
// Установка
// ---------------------------------------------------------------------------

#[cfg(target_os = "windows")]
fn launch_installer(path: &Path, silent: bool) -> Result<(), String> {
    use std::os::windows::process::CommandExt;
    use std::process::Command;

    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
    match ext.as_str() {
        // NSIS-установщик Tauri: /S — тихо, /P — с окном прогресса, /R — запустить
        // приложение после установки, /UPDATE — режим обновления (без ярлыков заново).
        "exe" => {
            Command::new(path)
                .args([if silent { "/S" } else { "/P" }, "/R", "/UPDATE"])
                .spawn()
                .map_err(|e| format!("Не удалось запустить установщик: {e}"))?;
        }
        "msi" => {
            let exe = std::env::current_exe().map_err(|e| e.to_string())?;
            let raw = format!(
                "/C msiexec /i \"{}\" {} /norestart & start \"\" \"{}\"",
                path.display(),
                if silent { "/qn" } else { "/passive" },
                exe.display()
            );
            Command::new("cmd")
                .raw_arg(raw)
                .creation_flags(crate::singbox::CREATE_NO_WINDOW)
                .spawn()
                .map_err(|e| format!("Не удалось запустить msiexec: {e}"))?;
        }
        other => return Err(format!("Неподдерживаемый установщик: .{other}")),
    }
    Ok(())
}

#[cfg(target_os = "macos")]
mod mac {
    use std::path::Path;
    use std::path::PathBuf;
    use std::process::Command;

    use tracing::info;
    use tracing::warn;

    fn run(cmd: &str, args: &[&str]) -> Result<String, String> {
        let out = Command::new(cmd)
            .args(args)
            .output()
            .map_err(|e| format!("{cmd}: {e}"))?;
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        if out.status.success() {
            Ok(text)
        } else {
            Err(format!("{cmd} завершился с ошибкой: {}", text.trim()))
        }
    }

    /// Путь к .app, из которого запущено приложение.
    pub fn current_bundle() -> Result<PathBuf, String> {
        let exe = std::env::current_exe().map_err(|e| e.to_string())?;
        let bundle = exe
            .ancestors()
            .nth(3)
            .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("app"))
            .ok_or("Обновление ставится только в собранное приложение (.app)")?;
        Ok(bundle.to_path_buf())
    }

    fn team_id(app: &Path) -> Option<String> {
        let out = Command::new("/usr/bin/codesign")
            .args(["-dv", "--verbose=2"])
            .arg(app)
            .output()
            .ok()?;
        let text = String::from_utf8_lossy(&out.stderr).to_string();
        text.lines()
            .find_map(|l| l.strip_prefix("TeamIdentifier="))
            .map(|s| s.trim().to_string())
            .filter(|s| s != "not set")
    }

    fn check_new_app(new_app: &Path, current: &Path) -> Result<(), String> {
        // архитектура: один DMG на платформу, поэтому проверяем, что сборка запустится
        let exe_name = std::env::current_exe()
            .ok()
            .and_then(|p| p.file_name().map(|n| n.to_owned()))
            .ok_or("Не удалось определить имя исполняемого файла")?;
        let new_exe = new_app.join("Contents/MacOS").join(&exe_name);
        if !new_exe.exists() {
            return Err(format!("В сборке нет {}", new_exe.display()));
        }
        let arch = if cfg!(target_arch = "aarch64") { "arm64" } else { "x86_64" };
        let archs = run("/usr/bin/lipo", &["-archs", &new_exe.to_string_lossy()])?;
        if !archs.split_whitespace().any(|a| a == arch) {
            return Err(format!("Сборка собрана для {} — не подходит для этого Mac ({arch})", archs.trim()));
        }
        // подпись: если текущее приложение подписано, новое должно быть подписано тем же разработчиком
        if let Some(team) = team_id(current) {
            run("/usr/bin/codesign", &["--verify", "--deep", "--strict", &new_app.to_string_lossy()])
                .map_err(|e| format!("Подпись новой сборки не прошла проверку: {e}"))?;
            match team_id(new_app) {
                Some(t) if t == team => {}
                other => return Err(format!("Сборка подписана другим разработчиком ({:?})", other)),
            }
        } else {
            warn!("Текущее приложение не подписано — проверка подписи обновления пропущена");
        }
        Ok(())
    }

    fn sh_quote(p: &Path) -> String {
        format!("'{}'", p.to_string_lossy().replace('\'', "'\\''"))
    }

    /// Ставит .app из DMG на место текущего. `interactive` — можно спросить пароль
    /// администратора, если нет прав на запись в папку приложения.
    pub fn install_dmg(dmg: &Path, mount_dir: &Path, interactive: bool) -> Result<PathBuf, String> {
        let current = current_bundle()?;
        let parent = current.parent().ok_or("Нет папки приложения")?.to_path_buf();
        let _ = std::fs::create_dir_all(mount_dir);
        let _ = run("/usr/bin/hdiutil", &["detach", "-force", "-quiet", &mount_dir.to_string_lossy()]);
        run(
            "/usr/bin/hdiutil",
            &["attach", "-nobrowse", "-noautoopen", "-readonly", "-mountpoint", &mount_dir.to_string_lossy(), &dmg.to_string_lossy()],
        )?;
        let result = (|| {
            let new_app = std::fs::read_dir(mount_dir)
                .map_err(|e| e.to_string())?
                .flatten()
                .map(|e| e.path())
                .find(|p| p.extension().and_then(|e| e.to_str()) == Some("app"))
                .ok_or("В образе DMG нет приложения .app")?;
            check_new_app(&new_app, &current)?;

            let name = current.file_name().unwrap().to_string_lossy().to_string();
            let staged = parent.join(format!(".{name}.update"));
            let old = parent.join(format!(".{name}.old"));
            let script = format!(
                "set -e; rm -rf {staged} {old}; /usr/bin/ditto {new} {staged}; mv {cur} {old}; mv {staged} {cur}; rm -rf {old}",
                staged = sh_quote(&staged),
                old = sh_quote(&old),
                new = sh_quote(&new_app),
                cur = sh_quote(&current),
            );
            match run("/bin/sh", &["-c", &script]) {
                Ok(_) => Ok(()),
                Err(e) if interactive => {
                    warn!("Нет прав на замену приложения ({}), запрашиваем пароль администратора", e);
                    let osa = format!(
                        "do shell script \"{}\" with administrator privileges",
                        script.replace('\\', "\\\\").replace('"', "\\\"")
                    );
                    run("/usr/bin/osascript", &["-e", &osa]).map(|_| ())
                }
                Err(e) => Err(format!("Нет прав на замену приложения в {}: {e}", parent.display())),
            }
        })();
        let _ = run("/usr/bin/hdiutil", &["detach", "-force", "-quiet", &mount_dir.to_string_lossy()]);
        result?;
        info!("Приложение обновлено: {}", current.display());
        Ok(current)
    }

    /// Запускает обновлённое приложение после выхода текущего.
    pub fn relaunch_later(bundle: &Path) -> Result<(), String> {
        Command::new("/bin/sh")
            .args(["-c", "sleep 2; /usr/bin/open -n \"$0\"", &bundle.to_string_lossy()])
            .spawn()
            .map(|_| ())
            .map_err(|e| format!("Не удалось перезапустить приложение: {e}"))
    }
}

/// Скачивает (если нужно) и ставит доступное обновление, затем закрывает приложение.
/// `silent` — без окон установщика и без запроса пароля (автообновление).
pub(crate) async fn install(app: &AppHandle, silent: bool) -> Result<(), String> {
    let Some(_busy) = BusyGuard::acquire() else {
        return Err("Обновление уже скачивается или устанавливается".into());
    };
    let release = snapshot(app)
        .available
        .ok_or("Нет доступного обновления — сначала проверьте наличие")?;
    let file = download(app, &release).await?;
    install_file(app, &release.version, &file, silent).await
}

async fn install_file(app: &AppHandle, version: &str, file: &Path, silent: bool) -> Result<(), String> {
    let state = app.state::<Arc<AppState>>().inner().clone();
    info!("Установка обновления {} ({}), тихо={}", version, file.display(), silent);
    modify(app, |s| {
        s.installing = true;
        s.error = None;
    });

    // Ядро запущено из папки приложения: его нужно остановить до замены файлов.
    let was_on = USER_WANTS_RUNNING.load(Ordering::SeqCst) || state.running.load(Ordering::Relaxed);
    {
        let mut s = state.settings.lock().unwrap();
        s.resume_vpn_after_update = was_on;
        let _ = s.save(&state.settings_path);
    }
    if state.running.load(Ordering::Relaxed) || !crate::singbox::diagnostics::singbox_pids().is_empty() {
        USER_WANTS_RUNNING.store(false, Ordering::SeqCst);
        if let Err(e) = singbox_stop_platform(app.clone(), app.state()).await {
            warn!("Перед обновлением не удалось остановить VPN: {}", e);
        }
    }

    let result: Result<(), String> = {
        #[cfg(target_os = "windows")]
        {
            launch_installer(file, silent)
        }
        #[cfg(target_os = "macos")]
        {
            let mount = app.path().app_cache_dir().map_err(|e| e.to_string())?.join("update-mnt");
            let file = file.to_path_buf();
            tauri::async_runtime::spawn_blocking(move || mac::install_dmg(&file, &mount, !silent))
                .await
                .map_err(|e| e.to_string())
                .and_then(|r| r)
                .and_then(|bundle| mac::relaunch_later(&bundle))
        }
        #[cfg(not(any(target_os = "windows", target_os = "macos")))]
        {
            let _ = (file, silent);
            Err("Обновления не поддерживаются на этой платформе".to_string())
        }
    };

    match result {
        Ok(()) => {
            info!("Установщик обновления {} запущен, приложение закрывается", version);
            EXITING.store(true, Ordering::SeqCst);
            app.exit(0);
            Ok(())
        }
        Err(e) => {
            error!("Установка обновления {} не удалась: {}", version, e);
            {
                let mut s = state.settings.lock().unwrap();
                s.resume_vpn_after_update = false;
                let _ = s.save(&state.settings_path);
            }
            modify(app, |s| {
                s.installing = false;
                s.error = Some(e.clone());
            });
            if was_on {
                let _ = crate::vpn::connect(app, crate::vpn::Origin::Launch).await;
            }
            Err(e)
        }
    }
}

// ---------------------------------------------------------------------------
// Команды
// ---------------------------------------------------------------------------

#[tauri::command]
pub fn get_update_state(app: AppHandle) -> UpdateState {
    snapshot(&app)
}

#[tauri::command]
pub async fn check_for_updates(app: AppHandle) -> Result<UpdateState, String> {
    check(&app).await
}

/// «Обновить сейчас» из окна: скачивание с прогрессом и установка с окном установщика.
#[tauri::command]
pub async fn install_update(app: AppHandle) -> Result<(), String> {
    install(&app, false).await
}

// ---------------------------------------------------------------------------
// Фон: проверка раз в 6 часов, тихое скачивание и установка
// ---------------------------------------------------------------------------

/// Подходящий момент для тихой установки: VPN выключен и окно не в фокусе
/// (пользователь ничем не занят в приложении). Обязательное обновление ставим сразу.
fn good_moment(app: &AppHandle, mandatory: bool) -> bool {
    if mandatory {
        return true;
    }
    let state = app.state::<Arc<AppState>>();
    let vpn_off = !USER_WANTS_RUNNING.load(Ordering::SeqCst) && !state.running.load(Ordering::Relaxed);
    let focused = app
        .get_webview_window("main")
        .map(|w| w.is_visible().unwrap_or(false) && w.is_focused().unwrap_or(false))
        .unwrap_or(false);
    vpn_off && !focused
}

/// Вызывается при старте до подключения VPN. Возвращает true, если началась установка
/// (приложение сейчас закроется и подключаться не нужно).
pub(crate) fn on_startup(app: &AppHandle, state: &Arc<AppState>) -> bool {
    let current = current_version(app);
    let (previous, auto, attempted) = {
        let mut s = state.settings.lock().unwrap();
        let previous = s.last_run_version.replace(current.clone());
        let _ = s.save(&state.settings_path);
        (previous, s.auto_update, s.update_attempted_version.clone())
    };
    if let Some(prev) = previous.filter(|p| is_newer(&current, p)) {
        info!("Приложение обновлено: {} → {}", prev, current);
        notify(app, "ULtunnel обновлён", &format!("Установлена версия {current}"));
    }

    if UPDATE_HOST.is_none() || !auto {
        return false;
    }
    // Сборка, скачанная в прошлый раз, ставится при запуске, пока VPN ещё не поднят.
    let Some((version, file)) = downloaded_newer(app) else {
        return false;
    };
    if attempted.as_deref() == Some(version.as_str()) {
        warn!("Обновление {} уже пробовали установить при запуске — ждём ручной установки", version);
        return false;
    }
    {
        let mut s = state.settings.lock().unwrap();
        s.update_attempted_version = Some(version.clone());
        let _ = s.save(&state.settings_path);
    }
    info!("При запуске найдено скачанное обновление {} — устанавливаем", version);
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let Some(_busy) = BusyGuard::acquire() else { return };
        modify(&app, |s| {
            s.ready = true;
        });
        let _ = install_file(&app, &version, &file, true).await;
    });
    true
}

pub(crate) fn spawn_update_loop(app: AppHandle, state: Arc<AppState>) {
    if UPDATE_HOST.is_none() {
        return;
    }
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(Duration::from_secs(20)).await;
        let mut next_check = 0u64;
        loop {
            if EXITING.load(Ordering::SeqCst) {
                return;
            }
            let now = unix_now();
            if now >= next_check && !BUSY.load(Ordering::SeqCst) {
                next_check = match check(&app).await {
                    Ok(_) => now + CHECK_INTERVAL_SECS,
                    Err(_) => now + CHECK_RETRY_SECS,
                };
            }

            let auto = state.settings.lock().unwrap().auto_update;
            let st = snapshot(&app);
            if let Some(release) = st.available.clone() {
                if auto {
                    if !st.ready && !st.downloading && !BUSY.load(Ordering::SeqCst) {
                        if let Some(_busy) = BusyGuard::acquire() {
                            let _ = download(&app, &release).await;
                        }
                    }
                    let st = snapshot(&app);
                    if st.ready && !st.installing && good_moment(&app, release.mandatory) {
                        info!("Тихая установка обновления {}", release.version);
                        let _ = install(&app, true).await;
                    }
                } else {
                    let mut notified = NOTIFIED_VERSION.lock().unwrap();
                    if notified.as_deref() != Some(release.version.as_str()) {
                        *notified = Some(release.version.clone());
                        let body = if release.mandatory {
                            format!("Версия {} обязательна к установке. Откройте приложение, чтобы обновиться", release.version)
                        } else {
                            format!("Доступна версия {}. Откройте приложение, чтобы обновиться", release.version)
                        };
                        notify(&app, "Доступно обновление ULtunnel", &body);
                    }
                }
            }
            tokio::time::sleep(Duration::from_secs(5 * 60)).await;
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cmp::Ordering::*;

    #[test]
    fn compares_versions_like_the_server() {
        assert!(is_newer("1.10", "1.9"));
        assert!(is_newer("1.4.0", "1.4.0-rc1"));
        assert!(is_newer("1.4.0-rc2", "1.4.0-rc1"));
        assert!(is_newer("1.4.1", "1.4.rc1"));
        assert!(is_newer("v2.0", "1.99.99"));
        assert_eq!(compare_versions("1.4", "1.4.0"), Equal);
        assert_eq!(compare_versions("0.2.0", "0.2.0"), Equal);
        assert!(!is_newer("0.1.9", "0.2.0"));
        assert!(!is_newer("", "0.2.0"));
    }

    #[test]
    fn validates_version_for_url() {
        assert!(valid_version("1.4.2"));
        assert!(valid_version("1.4.2-rc1+build.5"));
        assert!(!valid_version("../1.4"));
        assert!(!valid_version("v1.4"));
        assert!(!valid_version("1.4/../../x"));
    }

    fn release(file: Option<&str>) -> ReleaseInfo {
        ReleaseInfo {
            version: "1.4.2".into(),
            file_name: file.map(|s| s.to_string()),
            size_bytes: None,
            sha256: None,
            release_notes: None,
            mandatory: false,
            published_at: None,
            update_available: true,
        }
    }

    #[test]
    fn installer_name_is_sanitized() {
        #[cfg(target_os = "macos")]
        {
            assert_eq!(installer_file_name(&release(Some("UlTunnel 1.4.2.dmg"))).unwrap(), "UlTunnel_1.4.2.dmg");
            assert_eq!(installer_file_name(&release(Some("../../evil/x.dmg"))).unwrap(), "x.dmg");
            assert!(installer_file_name(&release(Some("setup.exe"))).is_err());
            assert_eq!(installer_file_name(&release(None)).unwrap(), "ultunnel-1.4.2.dmg");
        }
        #[cfg(target_os = "windows")]
        {
            assert_eq!(installer_file_name(&release(Some("UlTunnel-1.4.2-setup.exe"))).unwrap(), "UlTunnel-1.4.2-setup.exe");
            assert!(installer_file_name(&release(Some("x.bat"))).is_err());
        }
        assert!(installer_file_name(&release(Some(".hidden"))).is_err());
    }

    #[test]
    fn parses_server_answer() {
        let json = r#"{"platform":"windows","version":"1.4.2","versionCode":null,
            "fileName":"UlTunnel-1.4.2-setup.exe","sizeBytes":3200000,"sha256":"d878",
            "releaseNotes":"Починили реконнект","mandatory":true,
            "publishedAt":"2026-11-18T18:27:02.720732Z","downloadUrl":"https://win.ultunnel.ru",
            "currentVersion":"1.4.1","updateAvailable":true}"#;
        let r: ReleaseInfo = serde_json::from_str(json).unwrap();
        assert_eq!(r.version, "1.4.2");
        assert!(r.mandatory && r.update_available);
        assert_eq!(r.size_bytes, Some(3_200_000));
    }
}
