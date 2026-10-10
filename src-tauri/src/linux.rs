//! Linux: перезапуск приложения с правами root.

#[cfg(target_os = "linux")]
pub(crate) fn is_root() -> bool {
    unsafe { libc::geteuid() == 0 }
}

#[cfg(target_os = "linux")]
pub(crate) fn relaunch_as_root_if_needed() -> Result<(), String> {
    if is_root() {
        return Ok(());
    }
    // логгер на этом этапе ещё не инициализирован, поэтому только stderr
    eprintln!("Приложение запущено без root, перезапуск через pkexec");

    let exe = std::env::current_exe()
        .map_err(|e| format!("Не удалось получить путь к exe: {e}"))?;

    let display = std::env::var("DISPLAY").unwrap_or_default();
    let wayland_display = std::env::var("WAYLAND_DISPLAY").unwrap_or_default();
    let xauthority = std::env::var("XAUTHORITY").unwrap_or_default();
    let xdg_runtime_dir = std::env::var("XDG_RUNTIME_DIR").unwrap_or_default();
    let dbus_session_bus_address = std::env::var("DBUS_SESSION_BUS_ADDRESS").unwrap_or_default();

    let mut cmd = std::process::Command::new("pkexec");

    cmd.arg("env");

    if !display.is_empty() {
        cmd.arg(format!("DISPLAY={display}"));
    }

    if !wayland_display.is_empty() {
        cmd.arg(format!("WAYLAND_DISPLAY={wayland_display}"));
    }

    if !xauthority.is_empty() {
        cmd.arg(format!("XAUTHORITY={xauthority}"));
    }

    if !xdg_runtime_dir.is_empty() {
        cmd.arg(format!("XDG_RUNTIME_DIR={xdg_runtime_dir}"));
    }

    if !dbus_session_bus_address.is_empty() {
        cmd.arg(format!("DBUS_SESSION_BUS_ADDRESS={dbus_session_bus_address}"));
    }

    cmd.arg(exe);

    cmd.spawn()
        .map_err(|e| format!("Не удалось перезапустить приложение через pkexec: {e}"))?;

    std::process::exit(0);
}
