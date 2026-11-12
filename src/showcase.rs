//! `snowdesk --showcase <папка>`: картинки и кадры для README без чьего-то реального экрана.
//! Рисуем выдуманный рабочий стол (абстрактные окна, Dock из цветных квадратов),
//! гоняем по нему настоящую симуляцию снега и сохраняем кадры в PPM.
//! В PNG/GIF их превращает `scripts/showcase.sh` (через ffmpeg).

use std::io::Write;
use std::path::Path;

use crate::render::Gpu;
use crate::settings::Fall;
use crate::sim::{DOCK_ID, Instance, Params, Sim, WinRect};

const W: f32 = 1440.0;
const H: f32 = 900.0;
const DT: f32 = 1.0 / 30.0;

fn rect(out: &mut Vec<Instance>, x: f32, y: f32, w: f32, h: f32, r: f32, c: [f32; 4]) {
    out.push(Instance {
        pos: [x + w / 2.0, y + h / 2.0],
        half: [w / 2.0, h / 2.0],
        color: c,
        params: [2.0, r, 0.0, 0.0],
    });
}

fn gradient(out: &mut Vec<Instance>, x: f32, y: f32, w: f32, h: f32, top: [f32; 3], bottom: [f32; 3]) {
    out.push(Instance {
        pos: [x + w / 2.0, y + h / 2.0],
        half: [w / 2.0, h / 2.0],
        color: [top[0], top[1], top[2], 1.0],
        params: [5.0, bottom[0], bottom[1], bottom[2]],
    });
}

fn circle(out: &mut Vec<Instance>, x: f32, y: f32, r: f32, c: [f32; 4]) {
    rect(out, x - r, y - r, 2.0 * r, 2.0 * r, r, c);
}

fn glow(out: &mut Vec<Instance>, x: f32, y: f32, r: f32, a: f32) {
    out.push(Instance { pos: [x, y], half: [r, r], color: [1.0, 1.0, 0.9, a], params: [0.0; 4] });
}

/// Окно: тень, тело, заголовок со «светофором», содержимое рисует `content`.
struct Win {
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    dark: bool,
}

const RADIUS: f32 = 12.0;

impl Win {
    fn chrome(&self, out: &mut Vec<Instance>) {
        // мягкая тень из нескольких полупрозрачных слоёв
        for k in 1..=5 {
            let g = k as f32 * 4.0;
            rect(out, self.x - g, self.y - g + 10.0, self.w + 2.0 * g, self.h + 2.0 * g, RADIUS + g, [0.0, 0.0, 0.0, 0.07]);
        }
        let body = if self.dark { [0.12, 0.13, 0.17, 1.0] } else { [0.96, 0.96, 0.97, 1.0] };
        let bar = if self.dark { [0.17, 0.18, 0.23, 1.0] } else { [0.90, 0.90, 0.92, 1.0] };
        rect(out, self.x, self.y, self.w, self.h, RADIUS, body);
        rect(out, self.x, self.y, self.w, 36.0, RADIUS, bar);
        rect(out, self.x, self.y + 20.0, self.w, 16.0, 0.0, bar);
        for (i, c) in [[1.0, 0.37, 0.34, 1.0], [1.0, 0.74, 0.18, 1.0], [0.16, 0.79, 0.26, 1.0]].iter().enumerate() {
            circle(out, self.x + 20.0 + 20.0 * i as f32, self.y + 18.0, 6.0, *c);
        }
    }
    fn surface(&self, id: u64) -> WinRect {
        WinRect { id, x: self.x, y: self.y, w: self.w, h: self.h, radius: RADIUS, surface: true }
    }
}

/// «Редактор кода»: боковая панель и строки разноцветных плашек.
fn editor(out: &mut Vec<Instance>, w: &Win) {
    w.chrome(out);
    rect(out, w.x, w.y + 36.0, 150.0, w.h - 36.0, 0.0, [0.10, 0.11, 0.14, 1.0]);
    rect(out, w.x, w.y + w.h - 14.0, 150.0, 14.0, RADIUS, [0.10, 0.11, 0.14, 1.0]);
    for i in 0..9 {
        rect(out, w.x + 18.0, w.y + 58.0 + i as f32 * 26.0, 70.0 + (i * 37 % 50) as f32, 8.0, 4.0, [0.45, 0.48, 0.56, 0.7]);
    }
    let palette = [[0.78, 0.55, 1.0, 1.0], [0.42, 0.82, 0.95, 1.0], [1.0, 0.68, 0.38, 1.0], [0.55, 0.58, 0.66, 1.0], [0.55, 0.88, 0.55, 1.0]];
    let mut seed = 7u32;
    for line in 0..14 {
        let y = w.y + 60.0 + line as f32 * 24.0;
        let mut x = w.x + 172.0 + ((line * 3) % 4) as f32 * 22.0;
        for _ in 0..3 {
            seed = seed.wrapping_mul(1103515245).wrapping_add(12345);
            let len = 30.0 + (seed >> 16) as f32 % 120.0;
            if x + len > w.x + w.w - 24.0 {
                break;
            }
            rect(out, x, y, len, 9.0, 4.5, palette[(seed >> 8) as usize % palette.len()]);
            x += len + 12.0;
        }
    }
}

/// «Плеер»: обложка-градиент, название, прогресс, кнопки.
fn player(out: &mut Vec<Instance>, w: &Win) {
    w.chrome(out);
    let (ax, ay) = (w.x + 28.0, w.y + 64.0);
    gradient(out, ax, ay, 190.0, 190.0, [1.0, 0.62, 0.28], [0.93, 0.27, 0.55]);
    circle(out, ax + 95.0, ay + 95.0, 34.0, [1.0, 1.0, 1.0, 0.25]);
    rect(out, ax + 214.0, ay + 20.0, 180.0, 16.0, 8.0, [0.15, 0.16, 0.2, 0.9]);
    rect(out, ax + 214.0, ay + 50.0, 120.0, 11.0, 5.5, [0.55, 0.57, 0.62, 0.9]);
    rect(out, ax, ay + 220.0, w.w - 56.0, 6.0, 3.0, [0.82, 0.83, 0.86, 1.0]);
    rect(out, ax, ay + 220.0, (w.w - 56.0) * 0.42, 6.0, 3.0, [0.93, 0.27, 0.55, 1.0]);
    let cy = ay + 268.0;
    let cx = w.x + w.w / 2.0;
    circle(out, cx, cy, 24.0, [0.15, 0.16, 0.2, 1.0]);
    for dx in [-70.0, 70.0] {
        circle(out, cx + dx, cy, 14.0, [0.6, 0.62, 0.68, 1.0]);
    }
    out.push(Instance { pos: [cx + 3.0, cy], half: [10.0, 10.0], color: [1.0, 1.0, 1.0, 1.0], params: [4.0, 0.0, 0.0, 0.0] });
}

/// «Браузер»: адресная строка, картинка-градиент, карточки.
fn browser(out: &mut Vec<Instance>, w: &Win) {
    w.chrome(out);
    rect(out, w.x + 100.0, w.y + 8.0, w.w - 200.0, 20.0, 10.0, [0.24, 0.26, 0.32, 1.0]);
    gradient(out, w.x + 24.0, w.y + 56.0, w.w - 48.0, 160.0, [0.22, 0.75, 0.78], [0.18, 0.36, 0.85]);
    rect(out, w.x + 48.0, w.y + 90.0, 260.0, 22.0, 11.0, [1.0, 1.0, 1.0, 0.9]);
    rect(out, w.x + 48.0, w.y + 126.0, 180.0, 12.0, 6.0, [1.0, 1.0, 1.0, 0.6]);
    let cw = (w.w - 48.0 - 2.0 * 16.0) / 3.0;
    for i in 0..3 {
        let x = w.x + 24.0 + i as f32 * (cw + 16.0);
        rect(out, x, w.y + 236.0, cw, 200.0, 10.0, [0.26, 0.28, 0.35, 1.0]);
        rect(out, x + 14.0, w.y + 252.0, cw - 28.0, 90.0, 8.0, [0.34, 0.37, 0.46, 1.0]);
        rect(out, x + 14.0, w.y + 356.0, cw - 60.0, 10.0, 5.0, [0.6, 0.63, 0.7, 1.0]);
        rect(out, x + 14.0, w.y + 376.0, cw - 100.0, 10.0, 5.0, [0.45, 0.48, 0.56, 1.0]);
    }
}

struct Scene {
    a: Win, // редактор (спереди)
    b: Win, // плеер
    c: Win, // браузер (сзади)
    dock: (f32, f32, f32, f32),
}

impl Scene {
    fn new() -> Self {
        let n = 9.0;
        let dw = n * 58.0 + 14.0;
        Scene {
            a: Win { x: 150.0, y: 360.0, w: 640.0, h: 420.0, dark: true },
            b: Win { x: 880.0, y: 250.0, w: 430.0, h: 380.0, dark: false },
            c: Win { x: 360.0, y: 120.0, w: 700.0, h: 470.0, dark: true },
            dock: ((W - dw) / 2.0, H - 8.0 - 66.0, dw, 66.0),
        }
    }

    /// Поверхности для симуляции, спереди назад: Dock, редактор, плеер, браузер.
    fn surfaces(&self) -> Vec<WinRect> {
        let (dx, dy, dw, dh) = self.dock;
        vec![
            WinRect { id: DOCK_ID, x: dx, y: dy, w: dw, h: dh, radius: 20.0, surface: true },
            self.a.surface(1),
            self.b.surface(2),
            self.c.surface(3),
        ]
    }

    fn background(&self, out: &mut Vec<Instance>) {
        gradient(out, 0.0, 0.0, W, H, [0.07, 0.09, 0.22], [0.36, 0.26, 0.48]);
        glow(out, 1250.0, 140.0, 150.0, 0.16);
        circle(out, 1250.0, 140.0, 36.0, [0.98, 0.96, 0.86, 1.0]);
        // холмы
        circle(out, 250.0, H + 380.0, 620.0, [0.14, 0.15, 0.32, 1.0]);
        circle(out, 1200.0, H + 420.0, 640.0, [0.11, 0.12, 0.27, 1.0]);
        // строка меню
        rect(out, 0.0, 0.0, W, 26.0, 0.0, [0.05, 0.06, 0.12, 0.55]);
        circle(out, 22.0, 13.0, 6.0, [1.0, 1.0, 1.0, 0.85]);
        let mut x = 44.0;
        for len in [46.0, 34.0, 40.0, 30.0, 52.0] {
            rect(out, x, 9.0, len, 8.0, 4.0, [1.0, 1.0, 1.0, 0.75]);
            x += len + 18.0;
        }
        let mut x = W - 20.0;
        for len in [70.0, 16.0, 16.0, 16.0, 16.0] {
            x -= len;
            rect(out, x, 8.0, len, 10.0, 5.0, [1.0, 1.0, 1.0, 0.75]);
            x -= 14.0;
        }
        // окна сзади вперёд
        browser(out, &self.c);
        player(out, &self.b);
        editor(out, &self.a);
        // Dock: стеклянная «таблетка» и иконки — цветные квадраты
        let (dx, dy, dw, dh) = self.dock;
        rect(out, dx, dy, dw, dh, 20.0, [1.0, 1.0, 1.0, 0.16]);
        let colors = [
            [0.25, 0.55, 1.0], [0.3, 0.8, 0.45], [1.0, 0.55, 0.2], [0.65, 0.4, 0.95], [0.95, 0.3, 0.35],
            [0.2, 0.75, 0.8], [1.0, 0.8, 0.2], [0.55, 0.58, 0.65], [0.95, 0.45, 0.7],
        ];
        for (i, c) in colors.iter().enumerate() {
            let x = dx + 12.0 + i as f32 * 58.0;
            gradient_rounded(out, x, dy + 9.0, 48.0, 48.0, *c);
        }
    }
}

/// Иконка: скруглённый квадрат + блик сверху.
fn gradient_rounded(out: &mut Vec<Instance>, x: f32, y: f32, w: f32, h: f32, c: [f32; 3]) {
    rect(out, x, y, w, h, 12.0, [c[0], c[1], c[2], 1.0]);
    rect(out, x + 3.0, y + 3.0, w - 6.0, h * 0.45, 10.0, [1.0, 1.0, 1.0, 0.18]);
}

fn params(fall: Fall, wind: f32) -> Params {
    let f = fall.params();
    Params { rate: f.0, speed: f.1, gust: f.2, wind, piles: true }
}

fn save_ppm(path: &Path, w: u32, h: u32, rgba: &[u8]) {
    let mut f = std::io::BufWriter::new(std::fs::File::create(path).expect("ppm"));
    write!(f, "P6\n{w} {h}\n255\n").unwrap();
    for px in rgba.chunks_exact(4) {
        f.write_all(&px[..3]).unwrap();
    }
}

pub fn run(dir: &Path) {
    std::fs::create_dir_all(dir).expect("dir");
    let gpu = Gpu::headless();
    let mut scene = Scene::new();
    let mut sim = Sim::new(W, H, 2026);
    let mut bg = Vec::new();
    let mut inst = Vec::new();
    let frame = |sim: &Sim, scene: &Scene, bg: &mut Vec<Instance>, inst: &mut Vec<Instance>, scale: f32| {
        bg.clear();
        scene.background(bg);
        sim.instances(inst);
        bg.extend_from_slice(inst);
        gpu.render_rgba(W, H, scale, bg)
    };

    // 1. Три с половиной минуты метели: сугробы заметные, но ещё не упёрлись в максимум.
    eprintln!("метель…");
    let surf = scene.surfaces();
    for _ in 0..(210.0 / DT) as usize {
        sim.update(DT, &surf, &params(Fall::Blizzard, 0.0));
    }
    // 2. Снеговик на редакторе и снежок на Dock, потом ровный снегопад, чтобы воздух поредел.
    for (i, r) in [32.0, 23.0, 16.0].into_iter().enumerate() {
        sim.drop_ball(260.0, 250.0 - i as f32 * 50.0, r);
        for _ in 0..30 {
            sim.update(DT, &surf, &params(Fall::Steady, 12.0));
        }
    }
    sim.drop_ball(scene.dock.0 + scene.dock.2 - 60.0, scene.dock.1 - 120.0, 12.0);
    for _ in 0..(14.0 / DT) as usize {
        sim.update(DT, &surf, &params(Fall::Steady, 12.0));
    }
    let (w, h, px) = frame(&sim, &scene, &mut bg, &mut inst, 2.0);
    save_ppm(&dir.join("screenshot.ppm"), w, h, &px);
    eprintln!("screenshot.ppm {w}×{h}");

    // 3. Анимация: тряхнуть плеер (снег осыпается), потом снегоуборщик по Dock.
    let base_x = scene.b.x;
    let total = 7.0;
    let steps = (total / DT) as usize;
    let mut plow_called = false;
    for i in 0..steps {
        let t = i as f32 * DT;
        scene.b.x = if (1.2..2.0).contains(&t) {
            base_x + 70.0 * ((t - 1.2) * 28.0).sin()
        } else {
            base_x
        };
        if t >= 2.6 && !plow_called {
            sim.call_plow();
            plow_called = true;
        }
        sim.update(DT, &scene.surfaces(), &params(Fall::Steady, 12.0));
        let (w, h, px) = frame(&sim, &scene, &mut bg, &mut inst, 1.0);
        save_ppm(&dir.join(format!("anim_{i:04}.ppm")), w, h, &px);
    }
    eprintln!("anim: {steps} кадров");
}
