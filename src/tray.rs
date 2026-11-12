//! Иконка-снежинка в трее / строке меню и её меню.
//! Пункты опознаются по строковым id, состояние меню задаёт `MenuState`.
//! На macOS и Windows меню живёт в главном потоке. На Linux tray-icon требует GTK,
//! поэтому меню создаётся в отдельном потоке с gtk::main, а состояние передаётся каналом.

use tray_icon::menu::{CheckMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem, Submenu};
use tray_icon::{Icon, TrayIcon, TrayIconBuilder};

use crate::platform::DockAccess;
use crate::settings::{Fall, Settings};
use crate::sim::Tool;

pub enum Action {
    Fall(Fall),
    Wind(i32),
    Tool(Tool),
    TogglePiles,
    TogglePause,
    ToggleEco,
    ToggleAutostart,
    Clear,
    Plow,
    DockAccess,
    Quit,
}

/// Всё, что меню показывает галочками и доступностью пунктов.
#[derive(Clone)]
pub struct MenuState {
    pub fall: Fall,
    pub wind: i32,
    pub tool: Tool,
    pub piles: bool,
    pub paused: bool,
    pub eco: bool,
    /// None — автозапуск на этой платформе/сборке недоступен.
    pub autostart: Option<bool>,
    pub dock: DockAccess,
    pub has_dock: bool,
}

impl MenuState {
    pub fn new(s: &Settings, autostart: Option<bool>, dock: DockAccess, has_dock: bool) -> Self {
        MenuState {
            fall: s.fall,
            wind: s.wind,
            tool: s.tool,
            piles: s.piles,
            paused: s.paused,
            eco: s.eco,
            autostart,
            dock,
            has_dock,
        }
    }
}

const FALLS: [(Fall, &str, &str); 3] = [
    (Fall::Light, "fall.light", "Лёгкий"),
    (Fall::Steady, "fall.steady", "Ровный"),
    (Fall::Blizzard, "fall.blizzard", "Метель"),
];
const WINDS: [(i32, &str, &str); 5] = [
    (-2, "wind.-2", "⇇  Сильный влево"),
    (-1, "wind.-1", "←  Влево"),
    (0, "wind.0", "Штиль"),
    (1, "wind.1", "Вправо  →"),
    (2, "wind.2", "Сильный вправо  ⇉"),
];
const TOOLS: [(Tool, &str, &str); 3] = [
    (Tool::Mitten, "tool.mitten", "Варежка — смахнуть курсором"),
    (Tool::Snowballs, "tool.snowballs", "Снежки — зажать на снегу и катать"),
    (Tool::Shovel, "tool.shovel", "Лопата — клик по сугробу"),
];

/// Пункты меню (не Send на Linux — живут в потоке GTK).
struct Items {
    checks: Vec<(&'static str, CheckMenuItem)>,
    plow: MenuItem,
    dock: MenuItem,
    autostart: CheckMenuItem,
}

fn build(modifier: &str) -> (Menu, Items) {
    let check = |id: &'static str, text: &str| (id, CheckMenuItem::with_id(id, text, true, false, None));
    let mut checks: Vec<(&'static str, CheckMenuItem)> = Vec::new();
    let menu = Menu::new();
    let header = |t: &str| MenuItem::new(t, false, None);

    menu.append(&header("Снегопад")).unwrap();
    for (_, id, text) in FALLS {
        let c = check(id, text);
        menu.append(&c.1).unwrap();
        checks.push(c);
    }
    let wind = Submenu::new("Ветер", true);
    for (_, id, text) in WINDS {
        let c = check(id, text);
        wind.append(&c.1).unwrap();
        checks.push(c);
    }
    menu.append_items(&[&wind, &PredefinedMenuItem::separator(), &header("Инструмент")]).unwrap();
    for (_, id, text) in TOOLS {
        let c = check(id, text);
        menu.append(&c.1).unwrap();
        checks.push(c);
    }
    let piles = check("piles", "Сугробы на окнах");
    let eco = check("eco", "Эконом: 30 кадров/с");
    let pause = check("pause", "Пауза");
    let autostart = CheckMenuItem::with_id("autostart", "Запускать при входе", true, false, None);
    let plow = MenuItem::with_id("plow", "Вызвать снегоуборщик", true, None);
    let dock = MenuItem::with_id("dock", "Снег на Dock…", true, None);
    menu.append_items(&[
        &header(&format!("{modifier} + курсор — смахнуть всегда")),
        &PredefinedMenuItem::separator(),
        &piles.1,
        &MenuItem::with_id("clear", "Очистить окна", true, None),
        &plow,
        &dock,
        &PredefinedMenuItem::separator(),
        &eco.1,
        &autostart,
        &pause.1,
        &MenuItem::with_id("quit", "Выйти", true, None),
    ])
    .unwrap();
    checks.extend([piles, eco, pause]);
    (menu, Items { checks, plow, dock, autostart })
}

impl Items {
    fn sync(&self, s: &MenuState) {
        for (id, item) in &self.checks {
            let on = match *id {
                "piles" => s.piles,
                "eco" => s.eco,
                "pause" => s.paused,
                _ => {
                    FALLS.iter().any(|(f, i, _)| i == id && *f == s.fall)
                        || WINDS.iter().any(|(w, i, _)| i == id && *w == s.wind)
                        || TOOLS.iter().any(|(t, i, _)| i == id && *t == s.tool)
                }
            };
            item.set_checked(on);
        }
        self.plow.set_enabled(s.has_dock);
        self.plow.set_text(if s.has_dock {
            "Вызвать снегоуборщик"
        } else {
            "Вызвать снегоуборщик (нет Dock/панели)"
        });
        match s.dock {
            DockAccess::Missing => {
                self.dock.set_text("Снег на Dock — дать доступ…");
                self.dock.set_enabled(true);
            }
            DockAccess::Granted => {
                self.dock.set_text("Снег на Dock: включён");
                self.dock.set_enabled(false);
            }
            DockAccess::NotNeeded => {
                self.dock.set_text("Снег на панели задач: включён");
                self.dock.set_enabled(false);
            }
        }
        self.autostart.set_enabled(s.autostart.is_some());
        self.autostart.set_checked(s.autostart == Some(true));
    }
}

fn tray_icon(menu: Menu) -> TrayIcon {
    let b = TrayIconBuilder::new().with_menu(Box::new(menu)).with_tooltip("Snowdesk");
    // На маке шаблонная иконка (система сама перекрасит под тему), в остальных — белая.
    #[cfg(target_os = "macos")]
    let b = b.with_icon_templated(snowflake_icon([0, 0, 0]));
    #[cfg(not(target_os = "macos"))]
    let b = b.with_icon(snowflake_icon([255, 255, 255]));
    b.build().expect("tray icon")
}

#[cfg(not(target_os = "linux"))]
pub struct Tray {
    _icon: TrayIcon,
    items: Items,
}

#[cfg(target_os = "linux")]
pub struct Tray {
    tx: std::sync::mpsc::Sender<MenuState>,
}

impl Tray {
    #[cfg(not(target_os = "linux"))]
    pub fn new(modifier: &str) -> Self {
        let (menu, items) = build(modifier);
        Tray { _icon: tray_icon(menu), items }
    }

    #[cfg(target_os = "linux")]
    pub fn new(modifier: &str) -> Self {
        let (tx, rx) = std::sync::mpsc::channel::<MenuState>();
        let modifier = modifier.to_string();
        std::thread::spawn(move || {
            if gtk::init().is_err() {
                eprintln!("snowdesk: GTK не запустился — меню в трее не будет");
                return;
            }
            let (menu, items) = build(&modifier);
            let _icon = tray_icon(menu);
            gtk::glib::timeout_add_local(std::time::Duration::from_millis(150), move || {
                while let Ok(s) = rx.try_recv() {
                    items.sync(&s);
                }
                gtk::glib::ControlFlow::Continue
            });
            gtk::main();
        });
        Tray { tx }
    }

    pub fn sync(&self, s: &MenuState) {
        #[cfg(not(target_os = "linux"))]
        self.items.sync(s);
        #[cfg(target_os = "linux")]
        let _ = self.tx.send(s.clone());
    }

    /// Следующее действие из меню, если пользователь что-то нажал.
    pub fn poll(&self) -> Option<Action> {
        loop {
            let ev = MenuEvent::receiver().try_recv().ok()?;
            if let Some(a) = action(ev.id.as_ref()) {
                return Some(a);
            }
        }
    }
}

fn action(id: &str) -> Option<Action> {
    if let Some((f, _, _)) = FALLS.iter().find(|(_, i, _)| *i == id) {
        return Some(Action::Fall(*f));
    }
    if let Some((w, _, _)) = WINDS.iter().find(|(_, i, _)| *i == id) {
        return Some(Action::Wind(*w));
    }
    if let Some((t, _, _)) = TOOLS.iter().find(|(_, i, _)| *i == id) {
        return Some(Action::Tool(*t));
    }
    Some(match id {
        "piles" => Action::TogglePiles,
        "pause" => Action::TogglePause,
        "eco" => Action::ToggleEco,
        "autostart" => Action::ToggleAutostart,
        "clear" => Action::Clear,
        "plow" => Action::Plow,
        "dock" => Action::DockAccess,
        "quit" => Action::Quit,
        _ => return None,
    })
}

/// Снежинка 36×36 цвета `rgb` с антиалиасингом.
fn snowflake_icon(rgb: [u8; 3]) -> Icon {
    const N: usize = 36;
    let c = N as f32 / 2.0;
    let mut segs: Vec<[f32; 4]> = Vec::new();
    for k in 0..6 {
        let a = k as f32 * std::f32::consts::PI / 3.0 - std::f32::consts::FRAC_PI_2;
        let (s, co) = a.sin_cos();
        let len = 15.0;
        segs.push([c, c, c + co * len, c + s * len]);
        for (at, bl) in [(0.55f32, 5.0f32), (0.8, 3.5)] {
            let px = c + co * len * at;
            let py = c + s * len * at;
            for side in [-1.0f32, 1.0] {
                let b = a + side * 0.9;
                segs.push([px, py, px + b.cos() * bl, py + b.sin() * bl]);
            }
        }
    }
    let mut rgba = vec![0u8; N * N * 4];
    for y in 0..N {
        for x in 0..N {
            let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
            let d = segs.iter().map(|s| seg_dist(px, py, s)).fold(f32::MAX, f32::min);
            let a = (2.1 - d).clamp(0.0, 1.0);
            let i = (y * N + x) * 4;
            rgba[i..i + 3].copy_from_slice(&rgb);
            rgba[i + 3] = (a * 255.0) as u8;
        }
    }
    Icon::from_rgba(rgba, N as u32, N as u32).expect("icon")
}

fn seg_dist(px: f32, py: f32, s: &[f32; 4]) -> f32 {
    let (ax, ay, bx, by) = (s[0], s[1], s[2], s[3]);
    let (dx, dy) = (bx - ax, by - ay);
    let t = (((px - ax) * dx + (py - ay) * dy) / (dx * dx + dy * dy)).clamp(0.0, 1.0);
    let (qx, qy) = (ax + dx * t - px, ay + dy * t - py);
    (qx * qx + qy * qy).sqrt()
}
