//! Слой платформы: всё, что отличается между macOS, Windows и Linux (X11).
//! Каждая реализация даёт структуру `Platform` с одинаковым набором методов,
//! остальная программа (физика, отрисовка, меню) от ОС не зависит.
//!
//! Координаты: `Rect` в «глобальных единицах» платформы с началом в левом верхнем углу
//! (macOS — пункты, Windows и X11 — физические пиксели). В логические пункты экрана
//! переводит `Screen::to_local` — делит на `scale`.

#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
pub use macos::*;

#[cfg(target_os = "windows")]
mod win;
#[cfg(target_os = "windows")]
pub use win::*;

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
pub use linux::*;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Rect {
    pub fn intersects(&self, o: &Rect) -> bool {
        self.x < o.x + o.w && self.x + self.w > o.x && self.y < o.y + o.h && self.y + self.h > o.y
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Screen {
    pub frame: Rect,
    /// Глобальных единиц на логический пункт (на маке всегда 1).
    pub scale: f32,
}

impl Screen {
    /// Размер экрана в логических пунктах.
    pub fn logical_size(&self) -> (f32, f32) {
        (self.frame.w / self.scale, self.frame.h / self.scale)
    }
    /// Перевод прямоугольника из глобальных единиц в логические пункты этого экрана.
    pub fn to_local(&self, r: &Rect) -> Rect {
        Rect {
            x: (r.x - self.frame.x) / self.scale,
            y: (r.y - self.frame.y) / self.scale,
            w: r.w / self.scale,
            h: r.h / self.scale,
        }
    }
    pub fn point_to_local(&self, x: f32, y: f32) -> (f32, f32) {
        ((x - self.frame.x) / self.scale, (y - self.frame.y) / self.scale)
    }
}

/// Обычное окно приложения на экране. Список идёт спереди назад.
#[derive(Clone, Debug)]
pub struct WinInfo {
    pub id: u64,
    pub rect: Rect,
    /// Можно ли на нём держать снег (мелкие окна только загораживают).
    pub surface: bool,
}

/// «Dock»: на маке — Dock, на Windows — панель задач, на Linux — нижняя панель.
#[derive(Clone, Copy, Debug)]
pub struct Dock {
    pub rect: Rect,
    /// Скругление краёв, логические пункты.
    pub radius: f32,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Mouse {
    /// Глобальные единицы.
    pub x: f32,
    pub y: f32,
    pub left: bool,
    /// Модификатор «варежка всегда»: ⌥ на маке, Ctrl на Windows и Linux.
    pub modifier: bool,
}

#[derive(Clone, Copy, Debug, PartialEq)]
#[allow(dead_code)] // NotNeeded — только Windows/Linux, Missing — только macOS
pub enum DockAccess {
    /// Платформе не нужно разрешение.
    NotNeeded,
    Granted,
    /// macOS: нет «Универсального доступа» — снега на Dock не будет.
    Missing,
}
