//! Настройки, которые переживают перезапуск. Простой файл key=value.

use std::path::PathBuf;

use crate::sim::Tool;

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Fall {
    Light,
    Steady,
    Blizzard,
}

impl Fall {
    /// (снежинок/с на 1000 пт, множитель скорости, сила порывов)
    pub fn params(self) -> (f32, f32, f32) {
        match self {
            Fall::Light => (30.0, 0.8, 0.15),
            Fall::Steady => (100.0, 1.0, 0.3),
            Fall::Blizzard => (360.0, 1.6, 1.0),
        }
    }
    fn key(self) -> &'static str {
        match self {
            Fall::Light => "light",
            Fall::Steady => "steady",
            Fall::Blizzard => "blizzard",
        }
    }
}

fn tool_key(t: Tool) -> &'static str {
    match t {
        Tool::Mitten => "mitten",
        Tool::Snowballs => "snowballs",
        Tool::Shovel => "shovel",
    }
}

pub struct Settings {
    pub fall: Fall,
    /// -2..=2: сильный влево .. сильный вправо
    pub wind: i32,
    pub piles: bool,
    pub paused: bool,
    pub tool: Tool,
    /// Эконом: 30 кадров/с вместо 60.
    pub eco: bool,
    /// Уже спрашивали доступ Accessibility при первом запуске .app.
    pub asked_ax: bool,
}

fn path() -> Option<PathBuf> {
    Some(crate::platform::config_dir()?.join("settings.txt"))
}

impl Settings {
    pub fn load() -> Self {
        let mut s = Settings {
            fall: Fall::Steady,
            wind: 0,
            piles: true,
            paused: false,
            tool: Tool::Mitten,
            eco: false,
            asked_ax: false,
        };
        let Some(text) = path().and_then(|p| std::fs::read_to_string(p).ok()) else {
            return s;
        };
        for line in text.lines() {
            let Some((k, v)) = line.split_once('=') else { continue };
            match (k.trim(), v.trim()) {
                ("fall", "light") => s.fall = Fall::Light,
                ("fall", "steady") => s.fall = Fall::Steady,
                ("fall", "blizzard") => s.fall = Fall::Blizzard,
                ("wind", v) => s.wind = v.parse::<i32>().unwrap_or(0).clamp(-2, 2),
                ("piles", v) => s.piles = v == "1",
                ("tool", "mitten") => s.tool = Tool::Mitten,
                ("tool", "snowballs") => s.tool = Tool::Snowballs,
                ("tool", "shovel") => s.tool = Tool::Shovel,
                ("eco", v) => s.eco = v == "1",
                ("asked_ax", v) => s.asked_ax = v == "1",
                _ => {}
            }
        }
        s
    }

    pub fn save(&self) {
        let Some(p) = path() else { return };
        if let Some(dir) = p.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = std::fs::write(
            p,
            format!(
                "fall={}\nwind={}\npiles={}\ntool={}\neco={}\nasked_ax={}\n",
                self.fall.key(),
                self.wind,
                self.piles as u8,
                tool_key(self.tool),
                self.eco as u8,
                self.asked_ax as u8
            ),
        );
    }
}
