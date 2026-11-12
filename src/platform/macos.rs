//! macOS: экраны через NSScreen, окна через CGWindowList, Dock через Accessibility,
//! оверлей — NSWindow над Dock и строкой меню, автозапуск через SMAppService.

use std::ffi::c_void;
use std::path::PathBuf;
use std::ptr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use core_foundation::array::CFArray;
use core_foundation::base::{CFType, CFTypeRef, TCFType};
use core_foundation::boolean::CFBoolean;
use core_foundation::dictionary::{CFDictionary, CFDictionaryRef};
use core_foundation::number::CFNumber;
use core_foundation::string::{CFString, CFStringRef};
use core_graphics::window::{
    copy_window_info, kCGNullWindowID, kCGWindowAlpha, kCGWindowBounds, kCGWindowLayer,
    kCGWindowListExcludeDesktopElements, kCGWindowListOptionOnScreenOnly, kCGWindowNumber,
    kCGWindowOwnerName, kCGWindowOwnerPID,
};
use objc2::MainThreadMarker;
use objc2_app_kit::{NSScreen, NSView, NSWindowCollectionBehavior};
use objc2_foundation::{NSPoint, NSRect, NSSize};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use winit::dpi::{LogicalPosition, LogicalSize};
use winit::event_loop::{ActiveEventLoop, EventLoopBuilder};
use winit::platform::macos::{ActivationPolicy, EventLoopBuilderExtMacOS, WindowAttributesExtMacOS};
use winit::window::{Window, WindowAttributes, WindowLevel};

use super::{Dock, DockAccess, Mouse, Rect, Screen, WinInfo};

pub const MODIFIER: &str = "⌥";

/// Сырое окно из CGWindowList.
#[derive(Clone, Debug)]
struct RawWin {
    id: u32,
    pid: i32,
    layer: i32,
    owner: String,
    alpha: f32,
    rect: Rect,
}

pub struct Platform {
    mtm: MainThreadMarker,
    my_pid: i32,
    dock_pid: i32,
    dock: Option<Dock>,
    dock_checked: Option<Instant>,
    /// Последний сырой список — нужен для проверки полноэкранного режима.
    raw: Vec<RawWin>,
    front_pid: Option<i32>,
}

impl Platform {
    pub fn new() -> Self {
        Platform {
            mtm: MainThreadMarker::new().expect("main thread"),
            my_pid: std::process::id() as i32,
            dock_pid: 0,
            dock: None,
            dock_checked: None,
            raw: Vec::new(),
            front_pid: None,
        }
    }

    pub fn configure_event_loop(b: &mut EventLoopBuilder<()>) {
        // без иконки в Dock и меню приложения — только снежинка в строке меню
        b.with_activation_policy(ActivationPolicy::Accessory);
    }

    fn primary_height(&self) -> f64 {
        NSScreen::screens(self.mtm)
            .firstObject()
            .map(|s| s.frame().size.height)
            .unwrap_or(0.0)
    }

    pub fn screens(&self, _el: &ActiveEventLoop) -> Vec<Screen> {
        let ph = self.primary_height();
        NSScreen::screens(self.mtm)
            .iter()
            .map(|s| {
                let r = s.frame();
                Screen {
                    frame: Rect {
                        x: r.origin.x as f32,
                        y: (ph - (r.origin.y + r.size.height)) as f32,
                        w: r.size.width as f32,
                        h: r.size.height as f32,
                    },
                    scale: 1.0,
                }
            })
            .collect()
    }

    /// Обычные окна спереди назад (CGWindowList, без разрешения «Запись экрана»).
    pub fn windows(&mut self) -> Vec<WinInfo> {
        self.raw = list_raw();
        self.raw
            .iter()
            .filter(|w| w.pid != self.my_pid && w.alpha >= 0.05 && (0..=8).contains(&w.layer))
            .map(|w| WinInfo {
                id: w.id as u64,
                rect: w.rect,
                surface: w.rect.w >= 60.0 && w.rect.h >= 30.0,
            })
            .collect()
    }

    pub fn dock(&mut self) -> Option<Dock> {
        let now = Instant::now();
        if self.dock_checked.is_some_and(|t| now - t < Duration::from_millis(500)) {
            return self.dock;
        }
        self.dock_checked = Some(now);
        if self.dock_pid == 0 {
            self.dock_pid = dock_pid();
        }
        self.dock = if self.dock_pid != 0 && ax_trusted() {
            dock_rect(self.dock_pid).map(|rect| Dock { rect, radius: 22.0 })
        } else {
            None
        };
        self.dock
    }

    /// Полноэкранное приложение на этом экране: окна Dock в списке нет (в полноэкранном
    /// пространстве его не показывают), а какое-то окно на всю ширину доходит до низа.
    /// Верх не проверяем: на маках с «чёлкой» такое окно начинается под ней.
    pub fn fullscreen(&self, s: &Screen, wins: &[WinInfo]) -> bool {
        let f = s.frame;
        let dock_here = self
            .raw
            .iter()
            .any(|w| w.owner == "Dock" && w.layer == 20 && w.rect.intersects(&f));
        !dock_here
            && wins.iter().any(|w| {
                let r = w.rect;
                r.x <= f.x && r.w >= f.w && r.y + r.h >= f.y + f.h && r.y <= f.y + 40.0 + f.h * 0.2
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
            .with_position(LogicalPosition::new(f.x as f64, f.y as f64))
            .with_inner_size(LogicalSize::new(f.w as f64, f.h as f64))
            .with_has_shadow(false)
    }

    /// Поверх Dock и строки меню, на всех рабочих столах, без тени, мышь насквозь.
    pub fn setup_overlay(&mut self, window: &Window, s: &Screen) {
        window.set_cursor_hittest(false).ok();
        let Ok(h) = window.window_handle() else { return };
        let RawWindowHandle::AppKit(h) = h.as_raw() else { return };
        let view: &NSView = unsafe { &*(h.ns_view.as_ptr() as *const NSView) };
        let Some(win) = view.window() else { return };
        let f = s.frame;
        let ph = self.primary_height();
        let cocoa = NSRect::new(
            NSPoint::new(f.x as f64, ph - (f.y as f64 + f.h as f64)),
            NSSize::new(f.w as f64, f.h as f64),
        );
        win.setLevel(25); // NSStatusWindowLevel: выше Dock (20) и строки меню (24)
        win.setCollectionBehavior(
            NSWindowCollectionBehavior::CanJoinAllSpaces
                | NSWindowCollectionBehavior::Stationary
                | NSWindowCollectionBehavior::IgnoresCycle
                | NSWindowCollectionBehavior::FullScreenAuxiliary,
        );
        win.setHasShadow(false);
        win.setIgnoresMouseEvents(true);
        win.setFrame_display(cocoa, true);
    }

    pub fn keep_on_top(&mut self, _windows: &[&Window]) {}

    pub fn set_click_through(&self, window: &Window, through: bool) {
        window.set_cursor_hittest(!through).ok();
    }

    /// На маке прозрачные окна есть всегда.
    pub fn transparency_available(&self) -> bool {
        true
    }

    pub fn mouse(&self) -> Mouse {
        let p = objc2_app_kit::NSEvent::mouseLocation();
        let ph = self.primary_height();
        Mouse {
            x: p.x as f32,
            y: (ph - p.y) as f32,
            left: objc2_app_kit::NSEvent::pressedMouseButtons() & 1 != 0,
            modifier: objc2_app_kit::NSEvent::modifierFlags_class()
                .contains(objc2_app_kit::NSEventModifierFlags::Option),
        }
    }

    /// Пока курсор над снегом и мышь ещё не нажата — запоминаем, у кого фокус.
    pub fn remember_focus(&mut self) {
        let pid = objc2_app_kit::NSWorkspace::sharedWorkspace()
            .frontmostApplication()
            .map(|a| a.processIdentifier());
        if let Some(pid) = pid.filter(|p| *p != self.my_pid) {
            self.front_pid = Some(pid);
        }
    }

    /// Клик по нашему окну делает нас активным приложением — возвращаем фокус.
    pub fn restore_focus(&self) {
        let Some(pid) = self.front_pid else { return };
        if let Some(app) =
            objc2_app_kit::NSRunningApplication::runningApplicationWithProcessIdentifier(pid)
        {
            #[allow(deprecated)]
            app.activateWithOptions(objc2_app_kit::NSApplicationActivationOptions::empty());
        }
    }

    pub fn dock_access(&self) -> DockAccess {
        if ax_trusted() {
            DockAccess::Granted
        } else {
            DockAccess::Missing
        }
    }

    /// Системный диалог «разрешить управление компьютером».
    pub fn request_dock_access(&self) {
        unsafe {
            let key = CFString::wrap_under_get_rule(kAXTrustedCheckOptionPrompt);
            let opts = CFDictionary::from_CFType_pairs(&[(
                key.as_CFType(),
                CFBoolean::true_value().as_CFType(),
            )]);
            AXIsProcessTrustedWithOptions(opts.as_concrete_TypeRef());
        }
    }

    /// При первом запуске .app сразу спросить доступ (без него нет снега на Dock).
    pub fn prompt_dock_access_on_first_run(&self) -> bool {
        in_bundle() && !ax_trusted()
    }

    /// Автозапуск через SMAppService работает только у собранного .app.
    pub fn autostart_supported(&self) -> bool {
        in_bundle()
    }

    pub fn autostart_enabled(&self) -> bool {
        use objc2_service_management::{SMAppService, SMAppServiceStatus};
        in_bundle() && unsafe { SMAppService::mainAppService().status() == SMAppServiceStatus::Enabled }
    }

    pub fn set_autostart(&self, on: bool) -> Result<(), String> {
        use objc2_service_management::SMAppService;
        let svc = unsafe { SMAppService::mainAppService() };
        let r = unsafe {
            if on {
                svc.registerAndReturnError()
            } else {
                svc.unregisterAndReturnError()
            }
        };
        r.map_err(|e| e.localizedDescription().to_string())
    }

    pub fn low_power(&self) -> bool {
        objc2_foundation::NSProcessInfo::processInfo().isLowPowerModeEnabled()
    }

    /// Радиус скругления верхних углов окон, пт.
    pub fn window_radius(&self) -> f32 {
        16.0
    }

    /// Размер кадра в пикселях. На маке рисуем в 1 пиксель на пункт: CoreAnimation
    /// растягивает кадр сама, а видеопамяти в 4 раза меньше, чем в полном Retina.
    pub fn surface_px(&self, _w: &Window, s: &Screen, render_scale: f32) -> (u32, u32) {
        let (w, h) = s.logical_size();
        (((w * render_scale).round() as u32).max(1), ((h * render_scale).round() as u32).max(1))
    }

    pub fn instance_descriptor(&self, _w: Arc<Window>) -> wgpu::InstanceDescriptor {
        wgpu::InstanceDescriptor::new_without_display_handle()
    }
}

pub fn config_dir() -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    Some(PathBuf::from(home).join("Library/Application Support/snowdesk"))
}

fn in_bundle() -> bool {
    std::env::current_exe()
        .map(|p| p.to_string_lossy().contains(".app/Contents/MacOS/"))
        .unwrap_or(false)
}

// ---------- CGWindowList ----------

fn num(d: &CFDictionary<CFString, CFType>, key: &CFString) -> Option<f64> {
    d.find(key)?.downcast::<CFNumber>()?.to_f64()
}

fn list_raw() -> Vec<RawWin> {
    let mut out = Vec::with_capacity(64);
    let Some(arr) = copy_window_info(
        kCGWindowListOptionOnScreenOnly | kCGWindowListExcludeDesktopElements,
        kCGNullWindowID,
    ) else {
        return out;
    };
    unsafe {
        let k_num = CFString::wrap_under_get_rule(kCGWindowNumber);
        let k_pid = CFString::wrap_under_get_rule(kCGWindowOwnerPID);
        let k_layer = CFString::wrap_under_get_rule(kCGWindowLayer);
        let k_bounds = CFString::wrap_under_get_rule(kCGWindowBounds);
        let k_alpha = CFString::wrap_under_get_rule(kCGWindowAlpha);
        let k_owner = CFString::wrap_under_get_rule(kCGWindowOwnerName);
        let (kx, ky, kw, kh) = (
            CFString::from_static_string("X"),
            CFString::from_static_string("Y"),
            CFString::from_static_string("Width"),
            CFString::from_static_string("Height"),
        );
        for item in arr.iter() {
            let d: CFDictionary<CFString, CFType> =
                CFDictionary::wrap_under_get_rule(*item as *const _ as _);
            let Some(b) = d.find(&k_bounds).and_then(|v| v.downcast::<CFDictionary>()) else {
                continue;
            };
            let b: CFDictionary<CFString, CFType> =
                CFDictionary::wrap_under_get_rule(b.as_concrete_TypeRef());
            out.push(RawWin {
                id: num(&d, &k_num).unwrap_or(0.0) as u32,
                pid: num(&d, &k_pid).unwrap_or(0.0) as i32,
                layer: num(&d, &k_layer).unwrap_or(0.0) as i32,
                owner: d
                    .find(&k_owner)
                    .and_then(|v| v.downcast::<CFString>())
                    .map(|s| s.to_string())
                    .unwrap_or_default(),
                alpha: num(&d, &k_alpha).unwrap_or(1.0) as f32,
                rect: Rect {
                    x: num(&b, &kx).unwrap_or(0.0) as f32,
                    y: num(&b, &ky).unwrap_or(0.0) as f32,
                    w: num(&b, &kw).unwrap_or(0.0) as f32,
                    h: num(&b, &kh).unwrap_or(0.0) as f32,
                },
            });
        }
    }
    out
}

// ---------- Accessibility: где нарисована панель Dock ----------

#[link(name = "ApplicationServices", kind = "framework")]
unsafe extern "C" {
    fn AXIsProcessTrusted() -> u8;
    fn AXIsProcessTrustedWithOptions(options: CFDictionaryRef) -> u8;
    fn AXUIElementCreateApplication(pid: i32) -> CFTypeRef;
    fn AXUIElementCopyAttributeValue(el: CFTypeRef, attr: CFStringRef, value: *mut CFTypeRef)
        -> i32;
    fn AXValueGetValue(value: CFTypeRef, ty: u32, out: *mut c_void) -> u8;
    static kAXTrustedCheckOptionPrompt: CFStringRef;
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFPreferencesCopyAppValue(key: CFStringRef, app: CFStringRef) -> CFTypeRef;
}

fn ax_trusted() -> bool {
    unsafe { AXIsProcessTrusted() != 0 }
}

unsafe fn ax_attr(el: CFTypeRef, name: &str) -> Option<CFType> {
    let key = CFString::new(name);
    let mut v: CFTypeRef = ptr::null();
    let err = unsafe { AXUIElementCopyAttributeValue(el, key.as_concrete_TypeRef(), &mut v) };
    if err != 0 || v.is_null() {
        return None;
    }
    Some(unsafe { CFType::wrap_under_create_rule(v) })
}

/// PID процесса Dock (по bundle id: окно Dock в CGWindowList бывает не всегда).
fn dock_pid() -> i32 {
    let id = objc2_foundation::NSString::from_str("com.apple.dock");
    let apps = objc2_app_kit::NSRunningApplication::runningApplicationsWithBundleIdentifier(&id);
    apps.firstObject().map(|a| a.processIdentifier()).unwrap_or(0)
}

/// Размер иконок Dock из настроек (`defaults read com.apple.dock tilesize`).
fn dock_tilesize() -> Option<f32> {
    unsafe {
        let key = CFString::from_static_string("tilesize");
        let app = CFString::from_static_string("com.apple.dock");
        let v = CFPreferencesCopyAppValue(key.as_concrete_TypeRef(), app.as_concrete_TypeRef());
        if v.is_null() {
            return None;
        }
        CFType::wrap_under_create_rule(v).downcast::<CFNumber>()?.to_f32()
    }
}

/// Видимая «таблетка» Dock: рамка AXList процесса Dock, сдвинутая вниз.
fn dock_rect(pid: i32) -> Option<Rect> {
    unsafe {
        let app = AXUIElementCreateApplication(pid);
        if app.is_null() {
            return None;
        }
        let app = CFType::wrap_under_create_rule(app);
        let children = ax_attr(app.as_CFTypeRef(), "AXChildren")?.downcast::<CFArray>()?;
        for child in children.get_all_values() {
            let role = ax_attr(child, "AXRole").and_then(|r| r.downcast::<CFString>());
            if role.map(|r| r.to_string()).as_deref() != Some("AXList") {
                continue;
            }
            let pos = ax_attr(child, "AXPosition")?;
            let size = ax_attr(child, "AXSize")?;
            let mut p = [0f64; 2];
            let mut s = [0f64; 2];
            // kAXValueTypeCGPoint = 1, kAXValueTypeCGSize = 2
            if AXValueGetValue(pos.as_CFTypeRef(), 1, p.as_mut_ptr().cast()) == 0
                || AXValueGetValue(size.as_CFTypeRef(), 2, s.as_mut_ptr().cast()) == 0
            {
                return None;
            }
            // Рамка списка выше видимой «таблетки»: сверху запас под прыгающие иконки.
            let tile = dock_tilesize().unwrap_or(48.0);
            let inset = ((s[1] as f32 - tile) * 0.5).max(0.0) + 1.0;
            return Some(Rect {
                x: p[0] as f32,
                y: p[1] as f32 + inset,
                w: s[0] as f32,
                h: s[1] as f32 - inset,
            });
        }
        None
    }
}
