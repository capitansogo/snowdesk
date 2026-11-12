//! Физика снега для одного экрана: снежинки + сугробы на верхних краях окон.
//! Все координаты — локальные для экрана, в пунктах, y вниз.

use std::collections::HashMap;

mod toys;
pub use toys::{Tool, DOCK_ID};

/// Ширина одного столбика сугроба, пт.
pub const COL: f32 = 2.0;
/// Максимальная высота сугроба, пт.
const MAX_H: f32 = 26.0;
/// Максимальный перепад между соседними столбиками (угол естественного откоса).
const REPOSE: f32 = 0.8;
/// Сколько снега может висеть на самом краю окна.
const EDGE: f32 = 2.0;
/// Снежинки «дальше» этого не задевают окна — они как бы падают за ними.
const COLLIDE_DEPTH: f32 = 0.35;
const MAX_FLAKES: usize = 4000;
/// Скорость окна, при которой с него стряхивается снег, пт/с.
const SHAKE_SPEED: f32 = 1800.0;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct Instance {
    pub pos: [f32; 2],
    pub half: [f32; 2],
    pub color: [f32; 4],
    /// x: вид (0 — снежинка, 1 — столбик сугроба, 2 — скруглённый прямоугольник,
    /// 3 — шар со светотенью, 4 — треугольник носом вправо), y: радиус скругления для 2
    pub params: [f32; 4],
}

/// Окно на экране (уже в локальных координатах), в порядке спереди назад.
#[derive(Clone, Copy, Debug)]
pub struct WinRect {
    pub id: u64,
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    /// Радиус скругления верхних углов — снег в углах опускается по дуге.
    pub radius: f32,
    /// Может ли на нём лежать снег (иначе только загораживает).
    pub surface: bool,
}

impl WinRect {
    fn contains(&self, x: f32, y: f32) -> bool {
        x >= self.x && x < self.x + self.w && y >= self.y && y < self.y + self.h
    }
}

pub struct Params {
    /// Снежинок в секунду на 1000 пт ширины экрана.
    pub rate: f32,
    pub speed: f32,
    /// Сила порывов ветра, 0..1.
    pub gust: f32,
    /// Постоянный ветер, пт/с (минус — влево).
    pub wind: f32,
    pub piles: bool,
}

pub struct Rng(u64);
impl Rng {
    pub fn new(seed: u64) -> Self {
        Rng(seed | 1)
    }
    fn next(&mut self) -> u64 {
        // xorshift64*
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545F4914F6CDD1D)
    }
    pub fn f(&mut self) -> f32 {
        (self.next() >> 40) as f32 / (1u64 << 24) as f32
    }
    pub fn range(&mut self, a: f32, b: f32) -> f32 {
        a + (b - a) * self.f()
    }
}

struct Flake {
    x: f32,
    y: f32,
    vx: f32,
    vy: f32,
    /// Скорость, к которой снежинка стремится при падении.
    term: f32,
    r: f32,
    depth: f32,
    phase: f32,
    freq: f32,
    /// Пока > 0, снежинка не цепляется за окна (только что стряхнули).
    ghost: f32,
    /// 0..1: «насколько близко к зрителю» для выбора окна, на которое лечь.
    zd: f32,
}

struct Pile {
    x: f32,
    y: f32,
    w: f32,
    radius: f32,
    h: Vec<f32>,
    vis: Vec<bool>,
    last_seen: f64,
    present: bool,
    /// Какую долю снежинок ловит окно: 1 — все, 1/2 — если перед ним есть ещё одно окно ниже и т.д.
    catch: f32,
}

impl Pile {
    fn new(w: &WinRect) -> Self {
        let n = ((w.w / COL).ceil() as usize).max(1);
        Pile {
            x: w.x,
            y: w.y,
            w: w.w,
            radius: w.radius,
            h: vec![0.0; n],
            vis: vec![false; n],
            last_seen: 0.0,
            present: false,
            catch: 1.0,
        }
    }

    fn cx(&self, c: usize) -> f32 {
        self.x + (c as f32 + 0.5) * COL
    }

    /// Высота верхнего края окна в столбике c с учётом скруглённых углов.
    fn base(&self, c: usize) -> f32 {
        let dx = (c as f32 + 0.5) * COL;
        let edge = dx.min(self.w - dx);
        let r = self.radius;
        if r > 0.0 && edge < r {
            let k = r - edge;
            self.y + r - (r * r - k * k).max(0.0).sqrt()
        } else {
            self.y
        }
    }

    fn resample(&mut self, new_w: f32) {
        let n = ((new_w / COL).ceil() as usize).max(1);
        let old = std::mem::take(&mut self.h);
        let on = old.len();
        self.h = (0..n)
            .map(|i| {
                let src = (i as f32 + 0.5) / n as f32 * on as f32;
                old[(src as usize).min(on - 1)]
            })
            .collect();
        self.vis = vec![false; n];
        self.w = new_w;
    }

    fn relax(&mut self) {
        let n = self.h.len();
        if n < 2 {
            return;
        }
        let bases: Vec<f32> = (0..n).map(|c| self.base(c)).collect();
        for _ in 0..3 {
            for c in 0..n - 1 {
                // «верх» снега: чем меньше y, тем выше
                let ta = bases[c] - self.h[c];
                let tb = bases[c + 1] - self.h[c + 1];
                let d = tb - ta;
                if d > REPOSE {
                    let t = ((d - REPOSE) * 0.5).min(self.h[c]);
                    self.h[c] -= t;
                    self.h[c + 1] += t;
                } else if d < -REPOSE {
                    let t = ((-d - REPOSE) * 0.5).min(self.h[c + 1]);
                    self.h[c + 1] -= t;
                    self.h[c] += t;
                }
            }
            self.h[0] = self.h[0].min(EDGE);
            self.h[n - 1] = self.h[n - 1].min(EDGE);
        }
    }
}

pub struct Sim {
    pub w: f32,
    pub h: f32,
    flakes: Vec<Flake>,
    piles: HashMap<u64, Pile>,
    /// id сугробов текущего кадра, спереди назад.
    order: Vec<u64>,
    rng: Rng,
    t: f64,
    spawn_acc: f32,
    wind_now: f32,
    primed: bool,
    toys: toys::Toys,
}

impl Sim {
    pub fn new(w: f32, h: f32, seed: u64) -> Self {
        Sim {
            w,
            h,
            flakes: Vec::with_capacity(MAX_FLAKES),
            piles: HashMap::new(),
            order: Vec::new(),
            rng: Rng::new(seed),
            t: 0.0,
            spawn_acc: 0.0,
            wind_now: 0.0,
            primed: false,
            toys: toys::Toys::default(),
        }
    }

    pub fn clear_piles(&mut self) {
        self.piles.clear();
        self.order.clear();
    }

    fn new_flake(&mut self, x: f32, y: f32, speed: f32) -> Flake {
        let rng = &mut self.rng;
        let depth = rng.f();
        let r = 0.6 + 2.4 * depth * depth + rng.range(0.0, 0.4);
        let term = (20.0 + 46.0 * depth) * speed * rng.range(0.85, 1.15);
        Flake {
            x,
            y,
            vx: self.wind_now * (0.35 + 0.9 * depth),
            vy: term,
            term,
            r,
            depth,
            phase: rng.range(0.0, 6.283),
            freq: rng.range(0.4, 1.5),
            ghost: 0.0,
            zd: rng.f(),
        }
    }

    pub fn update(&mut self, dt: f32, wins: &[WinRect], p: &Params) {
        self.t += dt as f64;
        let t = self.t as f32;

        // Ветер: постоянная часть + порывы.
        let gust = ((t * 0.37).sin() * 0.6 + (t * 0.13 + 1.0).sin() * 0.4) * p.gust * 70.0;
        let target = p.wind + gust;
        self.wind_now += (target - self.wind_now) * (dt * 0.8).min(1.0);

        self.sync_piles(dt, wins, p.piles);
        self.spawn(dt, p);
        self.step_flakes(dt, p.piles);
        if p.piles {
            for id in &self.order {
                if let Some(pile) = self.piles.get_mut(id) {
                    pile.relax();
                }
            }
        }
        self.update_toys(dt);
    }

    fn sync_piles(&mut self, dt: f32, wins: &[WinRect], enabled: bool) {
        self.order.clear();
        if !enabled {
            self.piles.clear();
            return;
        }
        let now = self.t;
        let screen_h = self.h;
        let Sim {
            piles, flakes, rng, ..
        } = self;
        for (i, wr) in wins.iter().enumerate() {
            if !wr.surface {
                continue;
            }
            let pile = piles.entry(wr.id).or_insert_with(|| Pile::new(wr));
            let dx = wr.x - pile.x;
            let dy = wr.y - pile.y;
            if pile.present && dt > 0.0 {
                let speed = (dx * dx + dy * dy).sqrt() / dt;
                if speed > SHAKE_SPEED {
                    shed(pile, flakes, rng, dx / dt, dy / dt);
                }
            }
            if (wr.w - pile.w).abs() > 0.5 {
                pile.resample(wr.w);
            }
            pile.x = wr.x;
            pile.y = wr.y;
            pile.radius = wr.radius;
            pile.present = true;
            pile.last_seen = now;
            // Сколько окон перед этим ниже его верхнего края и перекрывают по горизонтали:
            // они тоже должны получить свою долю снега.
            let rank = wins[..i]
                .iter()
                .filter(|o| o.surface && o.y > wr.y && o.x < wr.x + wr.w && o.x + o.w > wr.x)
                .count();
            pile.catch = 1.0 / (1.0 + rank as f32);
            for c in 0..pile.h.len() {
                let cx = pile.cx(c);
                let by = pile.base(c);
                pile.vis[c] = by > 1.0
                    && by < screen_h
                    && !wins[..i].iter().any(|o| o.contains(cx, by - 1.0));
            }
            self.order.push(wr.id);
        }
        // Окна, которых сейчас нет (свёрнуты, на другом столе): прячем, через 10 минут забываем.
        for pile in self.piles.values_mut() {
            if pile.last_seen < now {
                pile.present = false;
            }
        }
        self.piles.retain(|_, p| now - p.last_seen < 600.0);
    }

    fn spawn(&mut self, dt: f32, p: &Params) {
        let avg_vy = 45.0 * p.speed;
        // Насколько ветер сносит снежинку за время падения — заранее сдвигаем зону появления.
        let drift = self.wind_now * 0.8 * self.h / avg_vy;
        let x0 = 0.0f32.min(-drift) - 20.0;
        let x1 = self.w.max(self.w - drift) + 20.0;
        let per_sec = p.rate * (x1 - x0) / 1000.0;

        if !self.primed {
            // Первый кадр: сразу заполняем экран, а не ждём, пока снег долетит.
            self.primed = true;
            let n = (per_sec * self.h / avg_vy) as usize;
            for _ in 0..n.min(MAX_FLAKES) {
                let x = self.rng.range(x0, x1);
                let y = self.rng.range(-10.0, self.h);
                let f = self.new_flake(x, y, p.speed);
                self.flakes.push(f);
            }
        }

        self.spawn_acc += per_sec * dt;
        while self.spawn_acc >= 1.0 {
            self.spawn_acc -= 1.0;
            if self.flakes.len() >= MAX_FLAKES {
                continue;
            }
            let x = self.rng.range(x0, x1);
            let y = -6.0 - self.rng.range(0.0, 30.0);
            let f = self.new_flake(x, y, p.speed);
            self.flakes.push(f);
        }
    }

    fn step_flakes(&mut self, dt: f32, piles_on: bool) {
        let t = self.t as f32;
        let (w, h) = (self.w, self.h);
        let wind = self.wind_now;
        let piles = &mut self.piles;
        let order = &self.order;
        self.flakes.retain_mut(|f| {
            let prev_y = f.y;
            let target_vx = wind * (0.35 + 0.9 * f.depth);
            f.vx += (target_vx - f.vx) * (dt * 1.5).min(1.0);
            f.vy += (f.term - f.vy) * (dt * 2.0).min(1.0);
            let sway = (t * f.freq + f.phase).sin() * 12.0 * (0.3 + f.depth);
            f.x += (f.vx + sway) * dt;
            f.y += f.vy * dt;
            f.ghost -= dt;

            if piles_on && f.depth > COLLIDE_DEPTH && f.ghost <= 0.0 {
                for id in order {
                    let Some(p) = piles.get_mut(id) else { continue };
                    if f.x < p.x || f.x >= p.x + p.w {
                        continue;
                    }
                    let c = (((f.x - p.x) / COL) as usize).min(p.h.len() - 1);
                    if !p.vis[c] || f.zd > p.catch {
                        continue;
                    }
                    let base = p.base(c);
                    let top = base - p.h[c];
                    if f.y >= top && prev_y <= base + 1.0 {
                        let a = 0.35 + f.r * 0.35;
                        p.h[c] = (p.h[c] + a * 0.5).min(MAX_H);
                        if c > 0 {
                            p.h[c - 1] = (p.h[c - 1] + a * 0.25).min(MAX_H);
                        }
                        if c + 1 < p.h.len() {
                            p.h[c + 1] = (p.h[c + 1] + a * 0.25).min(MAX_H);
                        }
                        return false;
                    }
                }
            }
            f.y < h + 10.0 && f.x > -w * 0.6 && f.x < w * 1.6
        });
    }

    /// Варежка: смахнуть снег вокруг точки (локальные координаты).
    pub fn brush(&mut self, x: f32, y: f32, radius: f32, vx: f32) {
        let Sim {
            piles, flakes, rng, order, ..
        } = self;
        for id in order.iter() {
            let Some(p) = piles.get_mut(id) else { continue };
            if x < p.x - radius || x > p.x + p.w + radius {
                continue;
            }
            for c in 0..p.h.len() {
                if !p.vis[c] || p.h[c] < 0.3 {
                    continue;
                }
                let cx = p.cx(c);
                let base = p.base(c);
                let top = base - p.h[c];
                let dx = cx - x;
                // курсор внутри сугроба или чуть выше
                if dx.abs() > radius || y < top - 10.0 || y > base + 8.0 {
                    continue;
                }
                let k = 1.0 - dx.abs() / radius;
                let take = p.h[c] * (0.25 + 0.6 * k);
                p.h[c] -= take;
                let n = (take / 3.0).ceil() as usize;
                for _ in 0..n {
                    if flakes.len() >= MAX_FLAKES + 600 {
                        break;
                    }
                    let depth = rng.range(0.5, 1.0);
                    flakes.push(Flake {
                        x: cx + rng.range(-1.0, 1.0),
                        y: top + rng.range(0.0, take),
                        vx: vx.clamp(-400.0, 400.0) * 0.5 + rng.range(-40.0, 40.0),
                        vy: rng.range(-60.0, 0.0),
                        term: (20.0 + 46.0 * depth) * rng.range(0.9, 1.3),
                        r: 0.8 + 2.0 * depth,
                        depth,
                        phase: rng.range(0.0, 6.283),
                        freq: rng.range(0.4, 1.5),
                        ghost: 0.35,
                        zd: rng.f(),
                    });
                }
            }
        }
    }

    pub fn flake_count(&self) -> usize {
        self.flakes.len()
    }

    pub fn instances(&self, out: &mut Vec<Instance>) {
        out.clear();
        let flake = |f: &Flake| Instance {
            pos: [f.x, f.y],
            half: [f.r * 1.8, f.r * 1.8],
            color: [1.0, 1.0, 1.0, 0.4 + 0.6 * f.depth],
            params: [0.0; 4],
        };
        // Дальние снежинки — за сугробами, ближние — перед ними.
        out.extend(
            self.flakes
                .iter()
                .filter(|f| f.depth <= COLLIDE_DEPTH)
                .map(flake),
        );
        for id in &self.order {
            let Some(p) = self.piles.get(id) else { continue };
            let n = p.h.len();
            // Сглаженный верх сугроба (y верхней кромки) в центре каждого столбика.
            let at = |i: isize| p.h[i.clamp(0, n as isize - 1) as usize];
            let tops: Vec<f32> = (0..n as isize)
                .map(|ci| {
                    let hh = (at(ci - 2) + 2.0 * at(ci - 1) + 3.0 * at(ci) + 2.0 * at(ci + 1) + at(ci + 2)) / 9.0;
                    p.base(ci as usize) - hh
                })
                .collect();
            for c in 0..n {
                let base = p.base(c);
                let hh = base - tops[c];
                if hh < 0.08 || !p.vis[c] {
                    continue;
                }
                // Кромка на границах столбика — среднее с соседями, получается ломаная без ступенек.
                let tl = if c > 0 { (tops[c - 1] + tops[c]) * 0.5 } else { tops[c] };
                let tr = if c + 1 < n { (tops[c + 1] + tops[c]) * 0.5 } else { tops[c] };
                let y0 = tl.min(tr).min(tops[c]) - 1.0;
                let y1 = base + 0.5;
                let x0 = p.cx(c) - COL * 0.5;
                // Тонкий слой — полупрозрачная пыль, толстый — плотный снег.
                let alpha = 0.97 * (hh / 1.5).clamp(0.3, 1.0);
                out.push(Instance {
                    pos: [p.cx(c), (y0 + y1) * 0.5],
                    half: [COL * 0.5 + 0.02, (y1 - y0) * 0.5],
                    color: [1.0, 1.0, 1.0, alpha],
                    params: [1.0, tl, tr, x0],
                });
            }
        }
        self.toy_instances(out);
        out.extend(
            self.flakes
                .iter()
                .filter(|f| f.depth > COLLIDE_DEPTH)
                .map(flake),
        );
    }
}

/// Резкое движение окна: большая часть сугроба срывается и падает.
fn shed(pile: &mut Pile, flakes: &mut Vec<Flake>, rng: &mut Rng, vx: f32, vy: f32) {
    for c in 0..pile.h.len() {
        let hh = pile.h[c];
        if hh < 2.0 {
            continue;
        }
        if pile.vis[c] {
            let n = ((hh / 4.0) as usize).clamp(1, 4);
            let cx = pile.cx(c);
            let top = pile.base(c) - hh;
            for _ in 0..n {
                if flakes.len() >= MAX_FLAKES + 800 {
                    break;
                }
                let depth = rng.range(0.45, 1.0);
                flakes.push(Flake {
                    x: cx + rng.range(-1.0, 1.0),
                    y: top + rng.range(0.0, hh),
                    vx: vx.clamp(-300.0, 300.0) * 0.3 + rng.range(-30.0, 30.0),
                    vy: vy.clamp(-200.0, 200.0) * 0.2 + rng.range(-20.0, 10.0),
                    term: (20.0 + 46.0 * depth) * rng.range(0.9, 1.3),
                    r: 0.8 + 2.0 * depth,
                    depth,
                    phase: rng.range(0.0, 6.283),
                    freq: rng.range(0.4, 1.5),
                    ghost: 0.4,
                    zd: rng.f(),
                });
            }
        }
        pile.h[c] = hh * 0.3;
    }
}

