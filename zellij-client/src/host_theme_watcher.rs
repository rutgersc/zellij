use crate::os_input_output::ClientOsApi;
use zellij_utils::data::HostTerminalThemeMode;
use zellij_utils::ipc::ClientToServerMsg;

/// Most Windows terminals (WezTerm included — wezterm/wezterm#6454) don't
/// implement CSI 2031 / DSR 996, so the `CSI ?996n` query sent at attach goes
/// unanswered and the session never learns the host's palette mode. Stand in
/// for the host: report the OS "apps use light theme" toggle at attach, then
/// watch it for changes. A real host reply also lands in the same server-side
/// state, and both track the OS setting, so they can't fight.
///
/// ponytail: 2s registry poll, swap for a WM_SETTINGCHANGE message window if
/// the latency ever matters.
pub(crate) fn spawn(os_input: Box<dyn ClientOsApi>) {
    let _ = std::thread::Builder::new()
        .name("host_theme_watcher".to_string())
        .spawn(move || {
            let Some(mut mode) = os_app_theme_mode() else {
                return; // read failure already logged; a retry won't fare better
            };
            os_input.send_to_server(ClientToServerMsg::HostTerminalThemeChanged { mode });
            loop {
                std::thread::sleep(std::time::Duration::from_secs(2));
                let Some(current) = os_app_theme_mode() else {
                    return;
                };
                if current != mode {
                    mode = current;
                    os_input
                        .send_to_server(ClientToServerMsg::HostTerminalThemeChanged { mode });
                }
            }
        });
}

/// Read `HKCU\...\Themes\Personalize\AppsUseLightTheme` (0 = dark, 1 = light).
fn os_app_theme_mode() -> Option<HostTerminalThemeMode> {
    use windows_sys::Win32::System::Registry::{
        RegGetValueW, HKEY_CURRENT_USER, RRF_RT_REG_DWORD,
    };
    let subkey: Vec<u16> = "Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize\0"
        .encode_utf16()
        .collect();
    let value: Vec<u16> = "AppsUseLightTheme\0".encode_utf16().collect();
    let mut data: u32 = 0;
    let mut size: u32 = std::mem::size_of::<u32>() as u32;
    let rc = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            subkey.as_ptr(),
            value.as_ptr(),
            RRF_RT_REG_DWORD,
            std::ptr::null_mut(),
            &mut data as *mut u32 as *mut _,
            &mut size,
        )
    };
    if rc != 0 {
        log::warn!(
            "could not read AppsUseLightTheme from registry (rc={}); host theme not watched",
            rc
        );
        return None;
    }
    Some(if data == 0 {
        HostTerminalThemeMode::Dark
    } else {
        HostTerminalThemeMode::Light
    })
}

#[cfg(test)]
mod tests {
    #[test]
    fn reads_apps_use_light_theme() {
        assert!(super::os_app_theme_mode().is_some());
    }
}
