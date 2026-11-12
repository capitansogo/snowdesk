//! Linux, только X11 (на Wayland приложения не могут узнать, где чужие окна).
//! Окна — через EWMH (`_NET_CLIENT_LIST_STACKING`), «Dock» — нижняя панель
//! (`_NET_WM_WINDOW_TYPE_DOCK`), оверлей — override-redirect окно с ARGB-визуалом.
//! Координаты — физические пиксели корневого окна.
//!
//! ВНИМАНИЕ: код собирается в CI, но на живом Linux не проверялся.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use winit::dpi::{PhysicalPosition, PhysicalSize};
use winit::event_loop::{ActiveEventLoop, EventLoopBuilder};
use winit::platform::x11::{EventLoopBuilderExtX11, WindowAttributesExtX11};
use winit::window::{Window, WindowAttributes, WindowLevel};
use x11rb::connection::Connection;
use x11rb::protocol::xproto::{
    Atom, AtomEnum, ConfigureWindowAux, ConnectionExt as _, MapState, StackMode,
};
use x11rb::rust_connection::RustConnection;

use super::{Dock, DockAccess, Mouse, Rect, Screen, WinInfo};

pub const MODIFIER: &str = "Ctrl";

struct Atoms {
    client_list_stacking: Atom,
    frame_extents: Atom,
    gtk_frame_extents: Atom,
    wm_state: Atom,
    state_hidden: Atom,
    state_fullscreen: Atom,
    wm_desktop: Atom,
    current_desktop: Atom,
    window_type: Atom,
    type_dock: Atom,
    type_desktop: Atom,
    wm_pid: Atom,
    cm_selection: Atom,
}

struct X {
    conn: RustConnection,
    root: u32,
    atoms: Atoms,
}

impl X {
    fn connect() -> Option<X> {
        let (conn, screen_num) = x11rb::connect(None).ok()?;
        let root = conn.setup().roots.get(screen_num)?.root;
        let intern = |name: &str| -> Option<Atom> {
            Some(conn.intern_atom(false, name.as_bytes()).ok()?.reply().ok()?.atom)
        };
        let atoms = Atoms {
            client_list_stacking: intern("_NET_CLIENT_LIST_STACKING")?,
            frame_extents: intern("_NET_FRAME_EXTENTS")?,
            gtk_frame_extents: intern("_GTK_FRAME_EXTENTS")?,
            wm_state: intern("_NET_WM_STATE")?,
            state_hidden: intern("_NET_WM_STATE_HIDDEN")?,
            state_fullscreen: intern("_NET_WM_STATE_FULLSCREEN")?,
            wm_desktop: intern("_NET_WM_DESKTOP")?,
            current_desktop: intern("_NET_CURRENT_DESKTOP")?,
            window_type: intern("_NET_WM_WINDOW_TYPE")?,
            type_dock: intern("_NET_WM_WINDOW_TYPE_DOCK")?,
            type_desktop: intern("_NET_WM_WINDOW_TYPE_DESKTOP")?,
            wm_pid: intern("_NET_WM_PID")?,
            cm_selection: intern(&format!("_NET_WM_CM_S{screen_num}"))?,
        };
        Some(X { conn, root, atoms })
    }

    fn prop32(&self, w: u32, prop: Atom) -> Vec<u32> {
        self.conn
            .get_property(false, w, prop, AtomEnum::ANY, 0, 1024)
            .ok()
            .and_then(|c| c.reply().ok())
            .and_then(|r| r.value32().map(|v| v.collect()))
            .unwrap_or_default()
    }

    /// Видимая рамка окна в координатах корня: с заголовком WM, без теней GTK.
    fn frame(&self, w: u32) -> Option<Rect> {
        let g = self.conn.get_geometry(w).ok()?.reply().ok()?;
        let t = self.conn.translate_coordinates(w, self.root, 0, 0).ok()?.reply().ok()?;
        let (mut x, mut y) = (t.dst_x as f32, t.dst_y as f32);
        let (mut ww, mut hh) = (g.width as f32, g.height as f32);
        let fe = self.prop32(w, self.atoms.frame_extents); // left, right, top, bottom
        if fe.len() == 4 {
            x -= fe[0] as f32;
            y -= fe[2] as f32;
            ww += (fe[0] + fe[1]) as f32;
            hh += (fe[2] + fe[3]) as f32;
        }
        let ge = self.prop32(w, self.atoms.gtk_frame_extents);
        if ge.len() == 4 {
            x += ge[0] as f32;
            y += ge[2] as f32;
            ww -= (ge[0] + ge[1]) as f32;
            hh -= (ge[2] + ge[3]) as f32;
        }
        Some(Rect { x, y, w: ww, h: hh })
    }
}

pub struct Platform {
    x: Option<X>,
    my_pid: u32,
    own: Vec<u32>,
    dock: Option<Dock>,
    /// Окна в полноэкранном режиме (по _NET_WM_STATE) с последнего опроса.
    fullscreen_rects: Vec<Rect>,
    last_raise: Instant,
}

impl Platform {
    pub fn new() -> Self {
        let x = X::connect();
        if x.is_none() {
            eprintln!("snowdesk: нет соединения с X11 — окна и сугробы недоступны");
        }
        Platform {
            x,
            my_pid: std::process::id(),
            own: Vec::new(),
            dock: None,
            fullscreen_rects: Vec::new(),
            last_raise: Instant::now(),
        }
    }

    /// Работаем через X11 даже в сессии Wayland (через XWayland): у Wayland нет способа
    /// узнать положение чужих окон.
    pub fn configure_event_loop(b: &mut EventLoopBuilder<()>) {
        b.with_x11();
    }

    /// Без композитора прозрачное окно станет чёрным поверх всего экрана — не запускаемся.
    pub fn transparency_available(&self) -> bool {
        let Some(x) = &self.x else { return false };
        x.conn
            .get_selection_owner(x.atoms.cm_selection)
            .ok()
            .and_then(|c| c.reply().ok())
            .is_some_and(|r| r.owner != 0)
    }

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

    pub fn windows(&mut self) -> Vec<WinInfo> {
        let mut out = Vec::new();
        self.fullscreen_rects.clear();
        let mut dock: Option<Rect> = None;
        let Some(x) = &self.x else { return out };
        let a = &x.atoms;
        let current = x.prop32(x.root, a.current_desktop).first().copied();
        // список идёт снизу вверх — разворачиваем
        let mut list = x.prop32(x.root, a.client_list_stacking);
        list.reverse();
        for w in list {
            if self.own.contains(&w) || x.prop32(w, a.wm_pid).first() == Some(&self.my_pid) {
                continue;
            }
            let Some(attrs) = x.conn.get_window_attributes(w).ok().and_then(|c| c.reply().ok()) else {
                continue;
            };
            if attrs.map_state != MapState::VIEWABLE {
                continue;
            }
            let state = x.prop32(w, a.wm_state);
            if state.contains(&a.state_hidden) {
                continue;
            }
            if let (Some(cur), Some(d)) = (current, x.prop32(w, a.wm_desktop).first().copied()) {
                if d != cur && d != 0xFFFF_FFFF {
                    continue;
                }
            }
            let Some(rect) = x.frame(w) else { continue };
            if rect.w < 2.0 || rect.h < 2.0 {
                continue;
            }
            let types = x.prop32(w, a.window_type);
            if types.contains(&a.type_desktop) {
                continue;
            }
            if types.contains(&a.type_dock) {
                // горизонтальная панель, самая нижняя — наш «Dock»
                if rect.w > rect.h * 3.0 && dock.is_none_or(|d| rect.y > d.y) {
                    dock = Some(rect);
                }
                continue;
            }
            if state.contains(&a.state_fullscreen) {
                self.fullscreen_rects.push(rect);
            }
            out.push(WinInfo {
                id: w as u64,
                rect,
                surface: rect.w >= 120.0 && rect.h >= 60.0,
            });
        }
        self.dock = dock.map(|rect| Dock { rect, radius: 6.0 });
        out
    }

    pub fn dock(&mut self) -> Option<Dock> {
        self.dock
    }

    pub fn fullscreen(&self, s: &Screen, _wins: &[WinInfo]) -> bool {
        self.fullscreen_rects.iter().any(|r| r.intersects(&s.frame))
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
            // оконный менеджер не трогает окно: без рамки, вне панели задач и Alt+Tab
            .with_override_redirect(true)
    }

    pub fn setup_overlay(&mut self, window: &Window, _s: &Screen) {
        window.set_cursor_hittest(false).ok();
        if let Ok(h) = window.window_handle() {
            match h.as_raw() {
                RawWindowHandle::Xlib(h) => self.own.push(h.window as u32),
                RawWindowHandle::Xcb(h) => self.own.push(h.window.get()),
                _ => {}
            }
        }
    }

    pub fn set_click_through(&self, window: &Window, through: bool) {
        window.set_cursor_hittest(!through).ok();
    }

    /// Override-redirect окно никто не держит сверху — раз в секунду поднимаем сами.
    pub fn keep_on_top(&mut self, _windows: &[&Window]) {
        if self.last_raise.elapsed() < Duration::from_secs(1) {
            return;
        }
        self.last_raise = Instant::now();
        let Some(x) = &self.x else { return };
        for w in &self.own {
            let _ = x
                .conn
                .configure_window(*w, &ConfigureWindowAux::new().stack_mode(StackMode::ABOVE));
        }
        let _ = x.conn.flush();
    }

    pub fn mouse(&self) -> Mouse {
        let Some(x) = &self.x else { return Mouse::default() };
        let Some(p) = x.conn.query_pointer(x.root).ok().and_then(|c| c.reply().ok()) else {
            return Mouse::default();
        };
        let mask = u16::from(p.mask);
        Mouse {
            x: p.root_x as f32,
            y: p.root_y as f32,
            left: mask & (1 << 8) != 0,  // Button1Mask
            modifier: mask & (1 << 2) != 0, // ControlMask
        }
    }

    // Override-redirect окно не получает фокус — возвращать нечего.
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
        autostart_file().is_some()
    }

    pub fn autostart_enabled(&self) -> bool {
        autostart_file().is_some_and(|p| p.exists())
    }

    /// ~/.config/autostart/snowdesk.desktop — стандарт freedesktop, работает в GNOME/KDE/XFCE.
    pub fn set_autostart(&self, on: bool) -> Result<(), String> {
        let p = autostart_file().ok_or("нет HOME")?;
        if on {
            let exe = std::env::current_exe().map_err(|e| e.to_string())?;
            if let Some(dir) = p.parent() {
                std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
            }
            std::fs::write(
                &p,
                format!(
                    "[Desktop Entry]\nType=Application\nName=Snowdesk\nExec=\"{}\"\nX-GNOME-Autostart-enabled=true\n",
                    exe.display()
                ),
            )
            .map_err(|e| e.to_string())
        } else {
            match std::fs::remove_file(&p) {
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.to_string()),
                _ => Ok(()),
            }
        }
    }

    pub fn low_power(&self) -> bool {
        false
    }

    pub fn window_radius(&self) -> f32 {
        8.0
    }

    pub fn surface_px(&self, w: &Window, _s: &Screen, _render_scale: f32) -> (u32, u32) {
        let s = w.inner_size();
        (s.width.max(1), s.height.max(1))
    }

    pub fn instance_descriptor(&self, w: Arc<Window>) -> wgpu::InstanceDescriptor {
        let mut d = wgpu::InstanceDescriptor::new_with_display_handle(Box::new(w));
        d.backends = wgpu::Backends::VULKAN | wgpu::Backends::GL;
        d
    }
}

fn config_home() -> Option<PathBuf> {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| Some(PathBuf::from(std::env::var_os("HOME")?).join(".config")))
}

pub fn config_dir() -> Option<PathBuf> {
    Some(config_home()?.join("snowdesk"))
}

fn autostart_file() -> Option<PathBuf> {
    Some(config_home()?.join("autostart/snowdesk.desktop"))
}
