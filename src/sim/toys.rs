//! Игрушки: снежки и снеговики, лопата, снегоуборщик по Dock.
//! Это подмодуль `sim`, поэтому видит приватные поля Sim, Pile и Flake.

use super::{Flake, Instance, Sim, COL, MAX_FLAKES};

/// id «окна» Dock в списке поверхностей.
pub const DOCK_ID: u64 = u64::MAX - 1;
/// Ускорение свободного падения шара, пт/с².
const G: f32 = 1100.0;
const BALL_MIN: f32 = 3.5;
const BALL_MAX: f32 = 34.0;
const MAX_BALLS: usize = 16;
/// Шар, ударившийся быстрее этого, рассыпается.
const SHATTER_SPEED: f32 = 1250.0;
/// Предельная скорость падения (сопротивление воздуха): просто упавший шар не рассыпается.
const TERMINAL_VY: f32 = 950.0;
const PLOW_SPEED: f32 = 140.0;
/// Сколько частиц разрешено добавить сверх обычного лимита снежинок.
const EXTRA_FLAKES: usize = 1500;

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Tool {
    /// Смахивать снег, просто проводя курсором.
    Mitten,
    /// Зажать кнопку на снегу — слепить шар, тащить — катать.
    Snowballs,
    /// Клик по сугробу — убрать весь снег с окна.
    Shovel,
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Support {
    /// Летит.
    Air,
    /// Лежит на окне `id`; `dx` — от левого края окна, чтобы ехать вместе с ним.
    Pile(u64, f32),
    /// Стоит на другом шаре; `dx` — смещение от его центра.
    Ball(u64, f32),
    /// Лежит на нижнем краю экрана.
    Ground,
}

#[derive(Clone, Copy, Debug)]
struct Ball {
    id: u64,
    x: f32,
    y: f32,
    r: f32,
    vx: f32,
    vy: f32,
    support: Support,
    held: bool,
}

struct Plow {
    x: f32,
    y: f32,
    vy: f32,
    dir: f32,
    t: f32,
}

#[derive(Default)]
pub struct Toys {
    balls: Vec<Ball>,
    next_id: u64,
    held: Option<u64>,
    grab: (f32, f32),
    plow: Option<Plow>,
}

impl Sim {
    /// Верх снега (или окна, если снега нет) в точке x на поверхности `id`.
    fn surface_top(&self, id: u64, x: f32) -> Option<f32> {
        let p = self.piles.get(&id)?;
        if !p.present || x < p.x || x >= p.x + p.w {
            return None;
        }
        let c = (((x - p.x) / COL) as usize).min(p.h.len() - 1);
        if !p.vis[c] {
            return None;
        }
        Some(p.base(c) - p.h[c])
    }

    /// Курсор над тем, с чем можно взаимодействовать (шар или сугроб)?
    pub fn hit_test(&self, x: f32, y: f32, tool: Tool) -> bool {
        if tool == Tool::Mitten {
            return false;
        }
        if tool == Tool::Snowballs && self.ball_at(x, y).is_some() {
            return true;
        }
        self.snow_at(x, y, 1.0).is_some()
    }

    fn ball_at(&self, x: f32, y: f32) -> Option<usize> {
        // верхний (последний нарисованный) шар в приоритете
        let mut best: Option<(usize, f32)> = None;
        for (i, b) in self.toys.balls.iter().enumerate() {
            let d = ((b.x - x).powi(2) + (b.y - y).powi(2)).sqrt();
            if d < b.r + 4.0 && best.is_none_or(|(_, by)| b.y < by) {
                best = Some((i, b.y));
            }
        }
        best.map(|(i, _)| i)
    }

    /// Сугроб под курсором: id поверхности, если там лежит хотя бы `min_h` снега.
    fn snow_at(&self, x: f32, y: f32, min_h: f32) -> Option<u64> {
        for id in &self.order {
            let Some(p) = self.piles.get(id) else { continue };
            if x < p.x || x >= p.x + p.w {
                continue;
            }
            let c = (((x - p.x) / COL) as usize).min(p.h.len() - 1);
            if !p.vis[c] || p.h[c] < min_h {
                continue;
            }
            let base = p.base(c);
            if y >= base - p.h[c] - 6.0 && y <= base + 3.0 {
                return Some(*id);
            }
        }
        None
    }

    /// Нажатие кнопки мыши. Возвращает true, если начали что-то тащить.
    pub fn press(&mut self, x: f32, y: f32, tool: Tool) -> bool {
        match tool {
            Tool::Mitten => false,
            Tool::Shovel => {
                if let Some(id) = self.snow_at(x, y, 0.5) {
                    self.shovel(id, x);
                }
                false
            }
            Tool::Snowballs => {
                if let Some(i) = self.ball_at(x, y) {
                    let b = &mut self.toys.balls[i];
                    b.held = true;
                    b.support = Support::Air;
                    self.toys.held = Some(b.id);
                    self.toys.grab = (b.x - x, b.y - y);
                    return true;
                }
                if self.snow_at(x, y, 1.0).is_none() || self.toys.balls.len() >= MAX_BALLS {
                    return false;
                }
                let id = self.toys.next_id;
                self.toys.next_id += 1;
                self.toys.balls.push(Ball {
                    id,
                    x,
                    y,
                    r: BALL_MIN,
                    vx: 0.0,
                    vy: 0.0,
                    support: Support::Air,
                    held: true,
                });
                self.toys.held = Some(id);
                self.toys.grab = (0.0, 0.0);
                self.absorb(id);
                true
            }
        }
    }

    /// Кнопка зажата, курсор в (x, y): шар едет за курсором и подбирает снег.
    pub fn drag(&mut self, x: f32, y: f32, dt: f32) {
        let Some(id) = self.toys.held else { return };
        let (gx, gy) = self.toys.grab;
        let (w, h) = (self.w, self.h);
        if let Some(b) = self.toys.balls.iter_mut().find(|b| b.id == id) {
            let (nx, ny) = ((x + gx).clamp(0.0, w), (y + gy).clamp(0.0, h));
            let k = (dt * 12.0).min(1.0);
            // сглаженная скорость — с ней шар полетит, когда отпустят
            b.vx += ((nx - b.x) / dt.max(0.001) - b.vx) * k;
            b.vy += ((ny - b.y) / dt.max(0.001) - b.vy) * k;
            b.x = nx;
            b.y = ny;
        }
        self.absorb(id);
    }

    /// Кнопку отпустили: шар летит с той скоростью, с какой его тащили.
    pub fn release(&mut self) {
        let Some(id) = self.toys.held.take() else { return };
        if let Some(b) = self.toys.balls.iter_mut().find(|b| b.id == id) {
            b.held = false;
            b.vx = b.vx.clamp(-1600.0, 1600.0);
            b.vy = b.vy.clamp(-1600.0, 1600.0);
        }
    }

    /// Шар забирает снег из сугробов, в которые он погружён.
    fn absorb(&mut self, id: u64) {
        let Some(bi) = self.toys.balls.iter().position(|b| b.id == id) else { return };
        let b = self.toys.balls[bi];
        let bottom = b.y + b.r;
        let mut area = 0.0;
        for pid in &self.order {
            let Some(p) = self.piles.get_mut(pid) else { continue };
            if b.x + b.r < p.x || b.x - b.r > p.x + p.w {
                continue;
            }
            for c in 0..p.h.len() {
                let cx = p.cx(c);
                if (cx - b.x).abs() > b.r * 0.9 || !p.vis[c] || p.h[c] < 0.05 {
                    continue;
                }
                let base = p.base(c);
                let top = base - p.h[c];
                // шар должен хотя бы касаться снега и не быть глубоко под окном
                if bottom < top || b.y - b.r > base {
                    continue;
                }
                let take = p.h[c].min(bottom - top + 0.6) * 0.5;
                p.h[c] -= take;
                area += take * COL;
            }
        }
        if area > 0.0 {
            let b = &mut self.toys.balls[bi];
            b.r = (b.r * b.r + area * 1.3 / std::f32::consts::PI).sqrt().min(BALL_MAX);
        }
    }

    /// Лопата: весь снег с поверхности `id` разлетается от курсора.
    fn shovel(&mut self, id: u64, from_x: f32) {
        let Some(p) = self.piles.get_mut(&id) else { return };
        let mut budget = 900usize;
        for c in 0..p.h.len() {
            let hh = p.h[c];
            if hh < 0.2 {
                continue;
            }
            p.h[c] = 0.0;
            if !p.vis[c] {
                continue;
            }
            let cx = p.cx(c);
            let top = p.base(c) - hh;
            let side = if cx >= from_x { 1.0 } else { -1.0 };
            let n = ((hh / 5.0).ceil() as usize).min(budget);
            budget -= n;
            for _ in 0..n {
                let rng = &mut self.rng;
                let args = (cx, top + rng.range(0.0, hh), side * rng.range(40.0, 260.0), -rng.range(120.0, 380.0));
                let f = particle(rng, args.0, args.1, args.2, args.3);
                if self.flakes.len() < MAX_FLAKES + EXTRA_FLAKES {
                    self.flakes.push(f);
                }
            }
        }
        // шары на этом окне тоже слетают
        for b in &mut self.toys.balls {
            if matches!(b.support, Support::Pile(pid, _) if pid == id) {
                b.support = Support::Air;
                b.vx = if b.x >= from_x { 220.0 } else { -220.0 };
                b.vy = -260.0;
            }
        }
    }

    /// Есть ли на этом экране Dock/панель, по которой может проехать снегоуборщик.
    pub fn has_dock(&self) -> bool {
        self.piles.get(&DOCK_ID).is_some_and(|p| p.present)
    }

    /// Вызвать снегоуборщик: проедет по Dock слева направо.
    pub fn call_plow(&mut self) -> bool {
        if self.toys.plow.is_some() {
            return true;
        }
        let Some(p) = self.piles.get(&DOCK_ID).filter(|p| p.present) else {
            return false;
        };
        self.toys.plow = Some(Plow {
            x: p.x + 18.0,
            y: p.y,
            vy: 0.0,
            dir: 1.0,
            t: 0.0,
        });
        true
    }

    pub(super) fn update_toys(&mut self, dt: f32) {
        self.update_plow(dt);
        self.update_balls(dt);
    }

    fn update_plow(&mut self, dt: f32) {
        let Some(mut plow) = self.toys.plow.take() else { return };
        plow.t += dt;
        plow.x += PLOW_SPEED * plow.dir * dt;
        let front = plow.x + plow.dir * 17.0;
        let mut on_dock = false;
        if let Some(p) = self.piles.get_mut(&DOCK_ID).filter(|p| p.present) {
            if front >= p.x && front < p.x + p.w {
                on_dock = true;
                plow.y = p.y;
                let c0 = ((front - p.x) / COL) as usize;
                for c in c0.saturating_sub(2)..(c0 + 3).min(p.h.len()) {
                    let hh = p.h[c];
                    if hh < 0.2 {
                        continue;
                    }
                    p.h[c] = 0.0;
                    let cx = p.cx(c);
                    let top = p.base(c) - hh;
                    let n = (hh / 3.0).ceil() as usize;
                    for _ in 0..n {
                        let rng = &mut self.rng;
                        let args = (cx, top + rng.range(0.0, hh), plow.dir * rng.range(50.0, 210.0), -rng.range(110.0, 260.0));
                        let f = particle(rng, args.0, args.1, args.2, args.3);
                        if self.flakes.len() < MAX_FLAKES + EXTRA_FLAKES {
                            self.flakes.push(f);
                        }
                    }
                }
            }
        }
        // Шары на Dock отлетают от отвала.
        for b in &mut self.toys.balls {
            if matches!(b.support, Support::Pile(DOCK_ID, _)) && (b.x - front).abs() < b.r + 2.0 {
                b.support = Support::Air;
                b.vx = plow.dir * 280.0;
                b.vy = -300.0;
            }
        }
        if !on_dock && plow.t > 0.3 {
            // съехал с края Dock — падает
            plow.vy += G * dt;
            plow.y += plow.vy * dt;
        }
        if plow.y < self.h + 60.0 {
            self.toys.plow = Some(plow);
        }
    }

    fn update_balls(&mut self, dt: f32) {
        let mut balls = std::mem::take(&mut self.toys.balls);
        // Сначала нижние: стоящие на них шары берут уже новую позицию.
        balls.sort_by(|a, b| b.y.total_cmp(&a.y));
        let mut shattered: Vec<Ball> = Vec::new();
        for i in 0..balls.len() {
            let mut b = balls[i];
            if b.held {
                continue;
            }
            match b.support {
                Support::Pile(id, dx) => {
                    let x = self.piles.get(&id).map(|p| p.x + dx);
                    match x.and_then(|x| self.surface_top(id, x).map(|t| (x, t))) {
                        Some((x, top)) => {
                            b.x = x;
                            b.y = top - b.r * 0.9;
                        }
                        None => b.support = Support::Air,
                    }
                }
                Support::Ball(bid, dx) => match balls.iter().find(|o| o.id == bid && !o.held) {
                    Some(o) => {
                        b.x = o.x + dx;
                        b.y = o.y - (o.r + b.r) * 0.82;
                        if o.support == Support::Air {
                            // основание летит — верх летит вместе с ним
                            b.vx = o.vx;
                            b.vy = o.vy;
                        }
                    }
                    None => b.support = Support::Air,
                },
                Support::Ground => b.y = self.h - b.r,
                Support::Air => {}
            }
            if b.support == Support::Air {
                let prev_bottom = b.y + b.r;
                if b.vy < TERMINAL_VY {
                    b.vy = (b.vy + G * dt).min(TERMINAL_VY);
                }
                b.vx *= 1.0 - 0.3 * dt;
                b.x += b.vx * dt;
                b.y += b.vy * dt;
                let speed = (b.vx * b.vx + b.vy * b.vy).sqrt();

                // встать на другой шар — так лепится снеговик
                for o in balls.iter() {
                    if o.id == b.id || o.held || matches!(o.support, Support::Ball(x, _) if x == b.id) {
                        continue;
                    }
                    let dx = b.x - o.x;
                    let dy = o.y - b.y;
                    if b.vy > 0.0 && dy > 0.0 && dx.abs() < o.r * 0.9 && (dx * dx + dy * dy).sqrt() < (o.r + b.r) * 0.9 {
                        // если на этом шаре уже кто-то стоит — поднимаемся по стопке на самый верх
                        let mut top = o.id;
                        while let Some(up) = balls
                            .iter()
                            .find(|u| u.id != b.id && !u.held && matches!(u.support, Support::Ball(t, _) if t == top))
                        {
                            top = up.id;
                        }
                        b.support = Support::Ball(top, if top == o.id { dx * 0.25 } else { 0.0 });
                        break;
                    }
                }
                if b.support == Support::Air && b.vy > 0.0 {
                    for id in self.order.iter() {
                        let Some(top) = self.surface_top(*id, b.x) else { continue };
                        if prev_bottom <= top + 2.0 && b.y + b.r >= top {
                            if speed > SHATTER_SPEED {
                                shattered.push(b);
                            } else {
                                let px = self.piles[id].x;
                                b.support = Support::Pile(*id, b.x - px);
                                b.y = top - b.r * 0.9;
                            }
                            break;
                        }
                    }
                }
                if b.support == Support::Air && b.y + b.r >= self.h {
                    if speed > SHATTER_SPEED {
                        shattered.push(b);
                    } else {
                        b.support = Support::Ground;
                        b.y = self.h - b.r;
                    }
                }
                if b.support != Support::Air {
                    b.vx = 0.0;
                    b.vy = 0.0;
                }
            }
            balls[i] = b;
        }
        let (w, h) = (self.w, self.h);
        balls.retain(|b| {
            !shattered.iter().any(|s| s.id == b.id)
                && b.x > -200.0
                && b.x < w + 200.0
                && b.y < h + 200.0
        });
        self.toys.balls = balls;
        for b in shattered {
            let n = ((b.r * b.r / 5.0) as usize).clamp(8, 160);
            for _ in 0..n {
                let rng = &mut self.rng;
                let a = rng.range(0.0, std::f32::consts::TAU);
                let d = rng.range(0.0, b.r);
                let args = (b.x + a.cos() * d, b.y + a.sin() * d, b.vx * 0.25 + rng.range(-160.0, 160.0), -rng.range(60.0, 320.0));
                let f = particle(rng, args.0, args.1, args.2, args.3);
                if self.flakes.len() < MAX_FLAKES + EXTRA_FLAKES {
                    self.flakes.push(f);
                }
            }
        }
    }

    pub(super) fn toy_instances(&self, out: &mut Vec<Instance>) {
        let mut balls: Vec<&Ball> = self.toys.balls.iter().collect();
        balls.sort_by(|a, b| b.y.total_cmp(&a.y));
        for b in &balls {
            out.push(Instance {
                pos: [b.x, b.y],
                half: [b.r, b.r],
                color: [1.0, 1.0, 1.0, 1.0],
                params: [3.0, 0.0, 0.0, 0.0],
            });
            let on_ball = matches!(b.support, Support::Ball(id, _) if self.toys.balls.iter().any(|o| o.id == id));
            let has_top = self
                .toys
                .balls
                .iter()
                .any(|o| matches!(o.support, Support::Ball(id, _) if id == b.id));
            if on_ball && !has_top {
                snowman_head(out, b.x, b.y, b.r);
            } else if on_ball && has_top {
                buttons(out, b.x, b.y, b.r);
            }
        }
        if let Some(p) = &self.toys.plow {
            plow_instances(out, p);
        }
    }
}

/// Частица снега, вылетевшая из сугроба: летит и снова может лечь через полсекунды.
fn particle(rng: &mut super::Rng, x: f32, y: f32, vx: f32, vy: f32) -> Flake {
    let depth = rng.range(0.5, 1.0);
    Flake {
        x,
        y,
        vx,
        vy,
        term: (20.0 + 46.0 * depth) * rng.range(1.0, 1.4),
        r: 0.8 + 2.0 * depth,
        depth,
        phase: rng.range(0.0, 6.283),
        freq: rng.range(0.4, 1.5),
        ghost: 0.5,
        zd: rng.f(),
    }
}

fn rect(out: &mut Vec<Instance>, x: f32, y: f32, hw: f32, hh: f32, radius: f32, c: [f32; 4]) {
    out.push(Instance {
        pos: [x, y],
        half: [hw, hh],
        color: c,
        params: [2.0, radius, 0.0, 0.0],
    });
}

fn dot(out: &mut Vec<Instance>, x: f32, y: f32, r: f32, c: [f32; 4]) {
    rect(out, x, y, r, r, r, c);
}

const COAL: [f32; 4] = [0.09, 0.09, 0.11, 1.0];
const SCARF: [f32; 4] = [0.86, 0.16, 0.2, 1.0];

fn snowman_head(out: &mut Vec<Instance>, x: f32, y: f32, r: f32) {
    // глаза-угольки
    dot(out, x - 0.3 * r, y - 0.2 * r, 0.1 * r, COAL);
    dot(out, x + 0.3 * r, y - 0.2 * r, 0.1 * r, COAL);
    // морковка
    out.push(Instance {
        pos: [x + 0.3 * r, y + 0.05 * r],
        half: [0.32 * r, 0.1 * r],
        color: [1.0, 0.52, 0.12, 1.0],
        params: [4.0, 0.0, 0.0, 0.0],
    });
    // улыбка из угольков
    for k in 0..5 {
        let a = std::f32::consts::PI * (0.32 + 0.09 * k as f32);
        dot(out, x + a.cos() * 0.5 * r, y + a.sin() * 0.5 * r, 0.05 * r, COAL);
    }
    // шарф: обхват шеи + свисающий конец
    rect(out, x, y + 0.84 * r, 0.82 * r, 0.15 * r, 0.12 * r, SCARF);
    rect(out, x + 0.42 * r, y + 1.2 * r, 0.13 * r, 0.34 * r, 0.08 * r, SCARF);
}

fn buttons(out: &mut Vec<Instance>, x: f32, y: f32, r: f32) {
    for k in 0..3 {
        dot(out, x, y - 0.1 * r + 0.3 * r * k as f32, 0.08 * r, COAL);
    }
}

fn plow_instances(out: &mut Vec<Instance>, p: &Plow) {
    let (x, y, d) = (p.x, p.y, p.dir);
    let yellow = [1.0, 0.76, 0.1, 1.0];
    let dark = [0.85, 0.6, 0.05, 1.0];
    let tire = [0.12, 0.12, 0.13, 1.0];
    let steel = [0.62, 0.66, 0.7, 1.0];
    // отвал спереди и снег на нём
    rect(out, x + d * 16.0, y - 7.0, 2.2, 8.0, 1.0, steel);
    rect(out, x + d * 19.5, y - 4.0, 2.5, 4.0, 2.0, [1.0, 1.0, 1.0, 0.95]);
    // корпус и кабина
    rect(out, x, y - 10.0, 13.0, 6.0, 2.5, yellow);
    rect(out, x - d * 4.0, y - 20.0, 7.0, 6.0, 2.0, dark);
    rect(out, x - d * 3.0, y - 21.0, 4.5, 3.5, 1.0, [0.62, 0.84, 1.0, 1.0]);
    // мигалка
    let blink = if (p.t * 4.0).fract() < 0.5 { 1.0 } else { 0.35 };
    dot(out, x - d * 4.0, y - 27.5, 1.8, [1.0, 0.45 * blink + 0.2, 0.05, 1.0]);
    // колёса
    for wx in [-7.5, 7.5] {
        dot(out, x + wx, y - 4.0, 4.5, tire);
        dot(out, x + wx, y - 4.0, 1.6, steel);
    }
}

impl Sim {
    /// Для демо и отладки: уронить шар радиуса r в точке (x, y).
    pub fn drop_ball(&mut self, x: f32, y: f32, r: f32) {
        let id = self.toys.next_id;
        self.toys.next_id += 1;
        self.toys.balls.push(Ball {
            id,
            x,
            y,
            r,
            vx: 0.0,
            vy: 0.0,
            support: Support::Air,
            held: false,
        });
    }
}
