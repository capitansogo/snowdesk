//! Windows: окна через EnumWindows + DWM, «Dock» — панель задач, прозрачный оверлей
//! через DirectComposition (wgpu DX12 `DxgiFromVisual`), автозапуск через реестр.
//! Координаты — физические пиксели (winit включает per-monitor DPI awareness).
//!
//! ВНИМАНИЕ: код собирается в CI, но на живой Windows не проверялся.

use std::ffi::c_void;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use windows_sys::Win32::Foundation::{HWND, LPARAM, POINT, RECT};
use windows_sys::Win32::Graphics::Dwm::{
    DWMWA_CLOAKED, DWMWA_EXTENDED_FRAME_BOUNDS, DwmGetWindowAttribute,
};
use windows_sys::Win32::System::Power::{GetSystemPowerStatus, SYSTEM_POWER_STATUS};
use windows_sys::Win32::System::Registry::{
    HKEY_CURRENT_USER, REG_SZ, RRF_RT_REG_SZ, RegDeleteKeyValueW, RegGetValueW, RegSetKeyValueW,
};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_CONTROL, VK_LBUTTON};
use windows_sys::Win32::UI::Shell::SHQueryUserNotificationState;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    EnumWindows, FindWindowW, GWL_EXSTYLE, GetClassNameW, GetCursorPos, GetWindowLongPtrW,
    GetWindowRect, GetWindowThreadProcessId, IsIconic, IsWindowVisible, LWA_ALPHA,
    SetLayeredWindowAttributes, SetWindowLongPtrW, WS_EX_LAYERED, WS_EX_NOACTIVATE,
    WS_EX_TOOLWINDOW, WS_EX_TRANSPARENT,
};
use winit::dpi::{PhysicalPosition, PhysicalSize};
use winit::event_loop::{ActiveEventLoop, EventLoopBuilder};
use winit::platform::windows::WindowAttributesExtWindows;
use winit::window::{Window, WindowAttributes, WindowLevel};

use super::{Dock, DockAccess, Mouse, Rect, Screen, WinInfo};

pub const MODIFIER: &str = "Ctrl";

const RUN_KEY: &str = "Software\\Microsoft\\Windows\\CurrentVersion\\Run";
const RUN_VALUE: &str = "Snowdesk";

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

pub struct Platform {
    my_pid: u32,
    dock: Option<Dock>,
    dock_checked: Option<Instant>,
}

impl Platform {
    pub fn new() -> Self {
        Platform {
            my_pid: std::process::id(),
            dock: None,
            dock_checked: None,
        }
    }

    pub fn configure_event_loop(_b: &mut EventLoopBuilder<()>) {}

    pub fn screens(&self, el: &ActiveEventLoop) -> Vec<Screen> {
        el.available_monitors()
            .map(|m| {
                let p = m.position();
                let s = m.size();
                Screen {
                    frame: Rect {
                        x: p.x as f32,
                        y: p.y as f32,
                        w: s.width as f32,
                        h: s.height as f32,
                    },
                    scale: m.scale_factor() as f32,
                }
            })
            .collect()
    }

    /// Окна верхнего уровня спереди назад (EnumWindows идёт по Z-порядку сверху вниз).
    pub fn windows(&mut self) -> Vec<WinInfo> {
        let mut hwnds: Vec<HWND> = Vec::with_capacity(256);
        unsafe extern "system" fn collect(hwnd: HWND, lp: LPARAM) -> windows_sys::core::BOOL {
            let v = unsafe { &mut *(lp as *mut Vec<HWND>) };
            v.push(hwnd);
            1
        }
        unsafe {
            EnumWindows(Some(collect), &mut hwnds as *mut Vec<HWND> as LPARAM);
        }
        let mut out = Vec::with_capacity(32);
        for hwnd in hwnds {
            unsafe {
                if IsWindowVisible(hwnd) == 0 || IsIconic(hwnd) != 0 {
                    continue;
                }
                // «спрятанные» окна: другие рабочие столы, фоновые UWP
                let mut cloaked: u32 = 0;
                DwmGetWindowAttribute(
                    hwnd,
                    DWMWA_CLOAKED as u32,
                    &mut cloaked as *mut u32 as *mut c_void,
                    4,
                );
                if cloaked != 0 {
                    continue;
                }
                let mut pid = 0u32;
                GetWindowThreadProcessId(hwnd, &mut pid);
                if pid == self.my_pid {
                    continue;
                }
                let class = class_name(hwnd);
                if matches!(
                    class.as_str(),
                    "Progman" | "WorkerW" | "Shell_TrayWnd" | "Shell_SecondaryTrayWnd"
                ) {
                    continue;
                }
                // рамка без невидимых краёв для ресайза
                let mut r: RECT = std::mem::zeroed();
                if DwmGetWindowAttribute(
                    hwnd,
                    DWMWA_EXTENDED_FRAME_BOUNDS as u32,
                    &mut r as *mut RECT as *mut c_void,
                    std::mem::size_of::<RECT>() as u32,
                ) != 0
                {
                    GetWindowRect(hwnd, &mut r);
                }
                let (w, h) = ((r.right - r.left) as f32, (r.bottom - r.top) as f32);
                if w < 2.0 || h < 2.0 {
                    continue;
                }
                let ex = GetWindowLongPtrW(hwnd, GWL_EXSTYLE) as u32;
                let tool = ex & WS_EX_TOOLWINDOW != 0;
                out.push(WinInfo {
                    id: hwnd as usize as u64,
                    rect: Rect {
                        x: r.left as f32,
                        y: r.top as f32,
                        w,
                        h,
                    },
                    surface: !tool && w >= 120.0 && h >= 60.0,
                });
            }
        }
        out
    }

    /// Панель задач внизу экрана — её верхний край работает как Dock.
    pub fn dock(&mut self) -> Option<Dock> {
        let now = Instant::now();
        if self.dock_checked.is_some_and(|t| now - t < Duration::from_millis(500)) {
            return self.dock;
        }
        self.dock_checked = Some(now);
        self.dock = unsafe {
            let cls = wide("Shell_TrayWnd");
            let hwnd = FindWindowW(cls.as_ptr(), std::ptr::null());
            if hwnd.is_null() || IsWindowVisible(hwnd) == 0 {
                None
            } else {
                let mut r: RECT = std::mem::zeroed();
                GetWindowRect(hwnd, &mut r);
                let rect = Rect {
                    x: r.left as f32,
                    y: r.top as f32,
                    w: (r.right - r.left) as f32,
                    h: (r.bottom - r.top) as f32,
                };
                // только горизонтальная панель (сбоку/сверху снегу лежать негде)
                (rect.w > rect.h * 3.0).then_some(Dock { rect, radius: 0.0 })
            }
        };
        self.dock
    }

    /// Полноэкранная игра/видео/презентация — по мнению Windows или по геометрии окна.
    pub fn fullscreen(&self, s: &Screen, wins: &[WinInfo]) -> bool {
        let mut state = 0i32;
        let busy = unsafe { SHQueryUserNotificationState(&mut state) } == 0
            && matches!(state, 2 | 3 | 4); // QUNS_BUSY, RUNNING_D3D_FULL_SCREEN, PRESENTATION_MODE
        let f = s.frame;
        busy || wins.iter().any(|w| {
            let r = w.rect;
            r.x <= f.x + 1.0 && r.y <= f.y + 1.0 && r.x + r.w >= f.x + f.w - 1.0 && r.y + r.h >= f.y + f.h - 1.0
        })
    }

    pub fn overlay_attributes(&self, s: &Screen) -> WindowAttributes {
        let f = s.frame;
        Window::default_attributes()
            .with_title("snowdesk")
            .with_decorations(false)
            .with_transparent(true)
            .with_resizable(false)
            .with_active(false)
            .with_window_level(WindowLevel::AlwaysOnTop)
            .with_position(PhysicalPosition::new(f.x as i32, f.y as i32))
            .with_inner_size(PhysicalSize::new(f.w as u32, f.h as u32))
            .with_skip_taskbar(true)
            // без этого DirectComposition не даст прозрачности
            .with_no_redirection_bitmap(true)
    }

    pub fn setup_overlay(&mut self, window: &Window, _s: &Screen) {
        self.set_click_through(window, true);
    }

    /// Мышь насквозь: WS_EX_TRANSPARENT. Стиль ставим сами, а не через winit,
    /// чтобы сохранить WS_EX_NOACTIVATE (клик по снегу не отбирает фокус) и
    /// WS_EX_TOOLWINDOW (нет в Alt+Tab).
    pub fn set_click_through(&self, window: &Window, through: bool) {
        let Some(hwnd) = hwnd_of(window) else { return };
        unsafe {
            let mut ex = GetWindowLongPtrW(hwnd, GWL_EXSTYLE) as u32;
            ex |= WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW | WS_EX_LAYERED;
            if through {
                ex |= WS_EX_TRANSPARENT;
            } else {
                ex &= !WS_EX_TRANSPARENT;
            }
            SetWindowLongPtrW(hwnd, GWL_EXSTYLE, ex as isize);
            // слоистое окно без этого вызова может не показаться вовсе
            SetLayeredWindowAttributes(hwnd, 0, 255, LWA_ALPHA);
        }
    }

    pub fn keep_on_top(&mut self, _windows: &[&Window]) {}

    /// DirectComposition есть с Windows 8; если DX12 не даст прозрачную поверхность,
    /// это поймает проверка альфа-режима при создании поверхности.
    pub fn transparency_available(&self) -> bool {
        true
    }

    pub fn mouse(&self) -> Mouse {
        unsafe {
            let mut p = POINT { x: 0, y: 0 };
            GetCursorPos(&mut p);
            Mouse {
                x: p.x as f32,
                y: p.y as f32,
                left: GetAsyncKeyState(VK_LBUTTON as i32) as u16 & 0x8000 != 0,
                modifier: GetAsyncKeyState(VK_CONTROL as i32) as u16 & 0x8000 != 0,
            }
        }
    }

    // Окно с WS_EX_NOACTIVATE не забирает фокус — возвращать нечего.
    pub fn remember_focus(&mut self) {}
    pub fn restore_focus(&self) {}

    pub fn dock_access(&self) -> DockAccess {
        DockAccess::NotNeeded
    }
    pub fn request_dock_access(&self) {}
    pub fn prompt_dock_access_on_first_run(&self) -> bool {
        false
    }

    pub fn autostart_supported(&self) -> bool {
        true
    }

    pub fn autostart_enabled(&self) -> bool {
        let (key, val) = (wide(RUN_KEY), wide(RUN_VALUE));
        let mut size = 0u32;
        unsafe {
            RegGetValueW(
                HKEY_CURRENT_USER,
                key.as_ptr(),
                val.as_ptr(),
                RRF_RT_REG_SZ,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                &mut size,
            ) == 0
        }
    }

    pub fn set_autostart(&self, on: bool) -> Result<(), String> {
        let (key, val) = (wide(RUN_KEY), wide(RUN_VALUE));
        let err = unsafe {
            if on {
                let exe = std::env::current_exe().map_err(|e| e.to_string())?;
                let cmd = wide(&format!("\"{}\"", exe.display()));
                RegSetKeyValueW(
                    HKEY_CURRENT_USER,
                    key.as_ptr(),
                    val.as_ptr(),
                    REG_SZ,
                    cmd.as_ptr() as *const c_void,
                    (cmd.len() * 2) as u32,
                )
            } else {
                RegDeleteKeyValueW(HKEY_CURRENT_USER, key.as_ptr(), val.as_ptr())
            }
        };
        if err == 0 { Ok(()) } else { Err(format!("реестр: ошибка {err}")) }
    }

    /// Режим экономии заряда Windows.
    pub fn low_power(&self) -> bool {
        unsafe {
            let mut s: SYSTEM_POWER_STATUS = std::mem::zeroed();
            GetSystemPowerStatus(&mut s) != 0 && s.SystemStatusFlag == 1
        }
    }

    /// Windows 11 скругляет окна на 8 пт.
    pub fn window_radius(&self) -> f32 {
        8.0
    }

    /// Цепочка DXGI рисует в размер окна — берём его физический размер.
    pub fn surface_px(&self, w: &Window, _s: &Screen, _render_scale: f32) -> (u32, u32) {
        let s = w.inner_size();
        (s.width.max(1), s.height.max(1))
    }

    pub fn instance_descriptor(&self, _w: Arc<Window>) -> wgpu::InstanceDescriptor {
        let mut d = wgpu::InstanceDescriptor::new_without_display_handle();
        // Прозрачность на Windows есть только у DX12 через DirectComposition.
        d.backends = wgpu::Backends::DX12;
        d.backend_options.dx12.presentation_system = wgpu::Dx12SwapchainKind::DxgiFromVisual;
        d
    }
}

pub fn config_dir() -> Option<PathBuf> {
    Some(PathBuf::from(std::env::var_os("APPDATA")?).join("snowdesk"))
}

fn hwnd_of(window: &Window) -> Option<HWND> {
    match window.window_handle().ok()?.as_raw() {
        RawWindowHandle::Win32(h) => Some(h.hwnd.get() as HWND),
        _ => None,
    }
}

unsafe fn class_name(hwnd: HWND) -> String {
    let mut buf = [0u16; 128];
    let n = unsafe { GetClassNameW(hwnd, buf.as_mut_ptr(), buf.len() as i32) };
    String::from_utf16_lossy(&buf[..n.max(0) as usize])
}
