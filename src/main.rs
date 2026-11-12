//! Snowdesk — снег на рабочем столе. Иконка в трее / строке меню, снег ложится на окна
//! и Dock (на Windows — на панель задач), из сугробов лепятся снежки и снеговики.

// Release-сборка на Windows без консольного окна.
#![cfg_attr(all(target_os = "windows", not(debug_assertions)), windows_subsystem = "windows")]

mod platform;
mod render;
mod settings;
mod sim;
mod tray;

use std::sync::Arc;
use std::time::{Duration, Instant};

use winit::application::ApplicationHandler;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::window::{Window, WindowId};

use platform::{Platform, Screen};
use render::{Gpu, Target};
use settings::Settings;
use sim::{DOCK_ID, Instance, Params, Sim, Tool, WinRect};

const FRAME: Duration = Duration::from_micros(16_667);
/// Пикселей на пункт при отрисовке (используется на маке; `SNOW_SCALE=2` — полный Retina).
const RENDER_SCALE: f32 = 1.0;

/// Пишем и в stderr, и в файл рядом с настройками: на Windows у release-сборки нет консоли.
fn log(msg: &str) {
    eprintln!("snowdesk: {msg}");
    if let Some(dir) = platform::config_dir() {
        let _ = std::fs::create_dir_all(&dir);
        let line = format!("{msg}\n");
        let _ = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(dir.join("snowdesk.log"))
            .and_then(|mut f| std::io::Write::write_all(&mut f, line.as_bytes()));
    }
}

struct ScreenCtx {
    window: Arc<Window>,
    target: Target,
    sim: Sim,
    screen: Screen,
    occluded: bool,
    /// Окно сейчас ловит мышь (курсор над снегом/шаром в режиме снежков или лопаты).
    hittest: bool,
    /// Тащим шар.
    dragging: bool,
}

#[derive(Default)]
struct Stats {
    frames: u32,
    busy: Duration,
    since: Option<Instant>,
}

struct App {
    platform: Platform,
    gpu: Option<Gpu>,
    screens: Vec<ScreenCtx>,
    tray: Option<tray::Tray>,
    settings: Settings,
    last: Instant,
    next_frame: Instant,
    slow_checked: Instant,
    inst: Vec<Instance>,
    mouse_prev: Option<(f32, f32)>,
    was_pressed: bool,
    low_power: bool,
    started: Instant,
    demo_step: usize,
    stats: Stats,
}

impl App {
    fn new() -> Self {
        let now = Instant::now();
        App {
            platform: Platform::new(),
            gpu: None,
            screens: Vec::new(),
            tray: None,
            settings: Settings::load(),
            last: now,
            next_frame: now,
            slow_checked: now,
            inst: Vec::with_capacity(8192),
            mouse_prev: None,
            was_pressed: false,
            low_power: false,
            started: now,
            demo_step: 0,
            stats: Stats::default(),
        }
    }

    fn frame_dt(&self) -> Duration {
        // В режиме энергосбережения системы тоже переходим на 30 кадров/с.
        if self.settings.eco || self.low_power { FRAME * 2 } else { FRAME }
    }

    /// Оверлей на каждый экран. Ошибка — если прозрачного окна сделать нельзя.
    fn build_screens(&mut self, el: &ActiveEventLoop) -> Result<(), String> {
        self.screens.clear();
        let scale = std::env::var("SNOW_SCALE")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(RENDER_SCALE);
        for (i, screen) in self.platform.screens(el).into_iter().enumerate() {
            let window = Arc::new(
                el.create_window(self.platform.overlay_attributes(&screen))
                    .map_err(|e| format!("окно: {e}"))?,
            );
            self.platform.setup_overlay(&window, &screen);
            let surface = match &self.gpu {
                Some(gpu) => gpu.create_surface(window.clone()),
                None => {
                    let desc = self.platform.instance_descriptor(window.clone());
                    let (gpu, surface) = Gpu::new(window.clone(), desc);
                    self.gpu = Some(gpu);
                    surface
                }
            };
            let (w, h) = screen.logical_size();
            let px = self.platform.surface_px(&window, &screen, scale);
            let target = self.gpu.as_ref().unwrap().target(surface, w, h, px)?;
            self.screens.push(ScreenCtx {
                window,
                target,
                sim: Sim::new(w, h, 0x5EED + i as u64 * 7919),
                screen,
                occluded: false,
                hittest: false,
                dragging: false,
            });
        }
        Ok(())
    }

    fn menu_state(&self) -> tray::MenuState {
        let p = &self.platform;
        let autostart = p.autostart_supported().then(|| p.autostart_enabled());
        let has_dock = self.screens.iter().any(|s| s.sim.has_dock());
        tray::MenuState::new(&self.settings, autostart, p.dock_access(), has_dock)
    }

    fn handle_menu(&mut self, el: &ActiveEventLoop) {
        let Some(tray) = &self.tray else { return };
        let mut changed = false;
        while let Some(action) = tray.poll() {
            changed = true;
            match action {
                tray::Action::Fall(f) => self.settings.fall = f,
                tray::Action::Wind(w) => self.settings.wind = w,
                tray::Action::Tool(t) => self.settings.tool = t,
                tray::Action::TogglePiles => self.settings.piles = !self.settings.piles,
                tray::Action::TogglePause => self.settings.paused = !self.settings.paused,
                tray::Action::ToggleEco => self.settings.eco = !self.settings.eco,
                tray::Action::ToggleAutostart => {
                    let on = !self.platform.autostart_enabled();
                    if let Err(e) = self.platform.set_autostart(on) {
                        log(&format!("автозапуск: {e}"));
                    }
                }
                tray::Action::Clear => self.screens.iter_mut().for_each(|s| s.sim.clear_piles()),
                tray::Action::Plow => {
                    for s in &mut self.screens {
                        s.sim.call_plow();
                    }
                }
                tray::Action::DockAccess => self.platform.request_dock_access(),
                tray::Action::Quit => {
                    el.exit();
                    return;
                }
            }
        }
        if changed {
            self.settings.save();
            let st = self.menu_state();
            if let Some(t) = &self.tray {
                t.sync(&st);
            }
        }
    }

    fn tick(&mut self, el: &ActiveEventLoop) {
        let t0 = Instant::now();
        let dt = (t0 - self.last).as_secs_f32().min(0.05);
        self.last = t0;
        self.handle_menu(el);

        // Раз в 2 с: смена мониторов, режим энергосбережения, меню (доступ мог появиться).
        if t0 - self.slow_checked > Duration::from_secs(2) {
            self.slow_checked = t0;
            self.low_power = self.platform.low_power();
            let now: Vec<Screen> = self.platform.screens(el);
            let cur: Vec<Screen> = self.screens.iter().map(|s| s.screen).collect();
            if now != cur {
                if let Err(e) = self.build_screens(el) {
                    log(&e);
                    el.exit();
                    return;
                }
            }
            let st = self.menu_state();
            if let Some(t) = &self.tray {
                t.sync(&st);
            }
        }

        let all = self.platform.windows();
        let dock = self.platform.dock();
        let mouse = self.platform.mouse();
        let (gvx, gspeed) = self
            .mouse_prev
            .map(|(px, py)| {
                let (dx, dy) = (mouse.x - px, mouse.y - py);
                (dx / dt.max(0.001), (dx * dx + dy * dy).sqrt() / dt.max(0.001))
            })
            .unwrap_or((0.0, 0.0));
        self.mouse_prev = Some((mouse.x, mouse.y));
        let tool = self.settings.tool;
        let press_edge = mouse.left && !self.was_pressed;
        self.was_pressed = mouse.left;
        if self.screens.iter().any(|s| s.hittest) && !mouse.left {
            self.platform.remember_focus();
        }
        let mut clicked_snow = false;

        let fall = self.settings.fall.params();
        let params = Params {
            rate: fall.0,
            speed: fall.1,
            gust: fall.2,
            wind: self.settings.wind as f32 * 45.0,
            piles: self.settings.piles,
        };
        let radius = self.platform.window_radius();
        let demo = std::env::var("SNOW_DEMO").ok().and_then(|v| {
            let (a, b) = v.split_once(',')?;
            Some((a.parse::<f32>().ok()?, b.parse::<f32>().ok()?))
        });

        let gpu = self.gpu.as_ref().unwrap();
        for s in &mut self.screens {
            let scr = s.screen;
            let f = scr.frame;
            // Окна этого экрана в логических пунктах, спереди назад.
            let mut local: Vec<WinRect> = Vec::with_capacity(all.len() + 1);
            if let Some(d) = dock {
                let r = d.rect;
                // Dock целиком на этом экране (спрятанный уезжает за край — не считаем)
                if r.y > f.y && r.y + r.h <= f.y + f.h + 2.0 * scr.scale && r.intersects(&f) {
                    let l = scr.to_local(&r);
                    local.push(WinRect { id: DOCK_ID, x: l.x, y: l.y, w: l.w, h: l.h, radius: d.radius, surface: true });
                }
            }
            for w in all.iter().filter(|w| w.rect.intersects(&f)) {
                let l = scr.to_local(&w.rect);
                local.push(WinRect { id: w.id, x: l.x, y: l.y, w: l.w, h: l.h, radius, surface: w.surface });
            }
            let fullscreen = self.platform.fullscreen(&scr, &all);

            if self.settings.paused || fullscreen || s.occluded {
                self.inst.clear();
                if s.hittest {
                    self.platform.set_click_through(&s.window, true);
                    s.hittest = false;
                }
            } else {
                s.sim.update(dt, &local, &params);
                if let Some((x, y)) = demo {
                    // демо: через 4/5/6 с роняем три шара в одну точку, через 7 с — снегоуборщик
                    let t = self.started.elapsed().as_secs_f32();
                    for (k, (at, r)) in [(4.0, 22.0), (5.0, 16.0), (6.0, 11.0)].into_iter().enumerate() {
                        if self.demo_step == k && t > at {
                            s.sim.drop_ball(x, y, r);
                            self.demo_step += 1;
                        }
                    }
                    if self.demo_step == 3 && t > 7.0 {
                        s.sim.call_plow();
                        self.demo_step += 1;
                    }
                }
                let (mx, my) = scr.point_to_local(mouse.x, mouse.y);
                let (w, h) = scr.logical_size();
                let on_screen = mx >= 0.0 && my >= 0.0 && mx < w && my < h;
                // Варежка: модификатор + курсор всегда; без него — если выбрана варежка и мышь движется.
                let speed = gspeed / scr.scale;
                if on_screen && (mouse.modifier || (tool == Tool::Mitten && speed > 40.0)) {
                    s.sim.brush(mx, my, 16.0, gvx / scr.scale);
                }
                // Снежки и лопата. Клик дошёл до нас, только если окно уже ловило мышь.
                if press_edge && s.hittest && on_screen {
                    clicked_snow = true;
                    s.dragging = s.sim.press(mx, my, tool);
                }
                if s.dragging {
                    if mouse.left {
                        s.sim.drag(mx, my, dt);
                    } else {
                        s.sim.release();
                        s.dragging = false;
                    }
                }
                // Ловим мышь только над снегом/шаром, иначе клики идут насквозь как обычно.
                let want = s.dragging || (on_screen && !mouse.left && s.sim.hit_test(mx, my, tool));
                if want != s.hittest {
                    self.platform.set_click_through(&s.window, !want);
                    s.hittest = want;
                }
                s.sim.instances(&mut self.inst);
            }
            gpu.draw(&mut s.target, &self.inst);
        }
        if clicked_snow {
            self.platform.restore_focus();
        }
        let wins: Vec<&Window> = self.screens.iter().map(|s| s.window.as_ref()).collect();
        self.platform.keep_on_top(&wins);

        self.stats.frames += 1;
        self.stats.busy += t0.elapsed();
        let since = *self.stats.since.get_or_insert(t0);
        if std::env::var_os("SNOW_STATS").is_some() && t0 - since > Duration::from_secs(5) {
            let flakes: usize = self.screens.iter().map(|s| s.sim.flake_count()).sum();
            eprintln!(
                "{:.0} fps, {:.2} мс/кадр, снежинок {}, окон {}",
                self.stats.frames as f32 / (t0 - since).as_secs_f32(),
                self.stats.busy.as_secs_f64() * 1000.0 / self.stats.frames as f64,
                flakes,
                all.len()
            );
            self.stats = Stats::default();
        }
    }
}

impl ApplicationHandler for App {
    fn resumed(&mut self, el: &ActiveEventLoop) {
        if self.screens.is_empty() {
            if !self.platform.transparency_available() {
                log("прозрачные окна недоступны (на Linux нужен композитный менеджер) — выходим");
                el.exit();
                return;
            }
            if let Err(e) = self.build_screens(el) {
                log(&e);
                el.exit();
                return;
            }
        }
        if self.tray.is_none() {
            if self.platform.prompt_dock_access_on_first_run() && !self.settings.asked_ax {
                // Первый запуск .app на маке: сразу спрашиваем доступ, без него нет снега на Dock.
                self.settings.asked_ax = true;
                self.settings.save();
                self.platform.request_dock_access();
            }
            let tray = tray::Tray::new(platform::MODIFIER);
            tray.sync(&self.menu_state());
            self.tray = Some(tray);
        }
    }

    fn window_event(&mut self, _el: &ActiveEventLoop, id: WindowId, event: WindowEvent) {
        if let WindowEvent::Occluded(occ) = event {
            if let Some(s) = self.screens.iter_mut().find(|s| s.window.id() == id) {
                s.occluded = occ;
            }
        }
    }

    fn about_to_wait(&mut self, el: &ActiveEventLoop) {
        let now = Instant::now();
        if now >= self.next_frame && !self.screens.is_empty() {
            self.tick(el);
            let frame = self.frame_dt();
            self.next_frame += frame;
            if self.next_frame < now {
                self.next_frame = now + frame;
            }
        }
        el.set_control_flow(ControlFlow::WaitUntil(self.next_frame));
    }
}

fn main() {
    if std::env::args().any(|a| a == "--debug-windows") {
        let mut p = Platform::new();
        for w in p.windows() {
            println!("{:>12} {:?} surface={}", w.id, w.rect, w.surface);
        }
        println!("dock={:?}", p.dock());
        return;
    }
    let mut builder = EventLoop::builder();
    Platform::configure_event_loop(&mut builder);
    let event_loop = builder.build().expect("event loop");
    let mut app = App::new();
    event_loop.run_app(&mut app).expect("run");
}
