//! Рисует иконку приложения 1024×1024 в PNG: `cargo run --example make_icon -- out.png`.
//! Никаких графических библиотек — всё через функции расстояния, с антиалиасингом.

use std::f32::consts::PI;

const N: usize = 1024;

fn seg_dist(px: f32, py: f32, s: [f32; 4]) -> f32 {
    let (ax, ay, bx, by) = (s[0], s[1], s[2], s[3]);
    let (dx, dy) = (bx - ax, by - ay);
    let t = (((px - ax) * dx + (py - ay) * dy) / (dx * dx + dy * dy)).clamp(0.0, 1.0);
    ((ax + dx * t - px).powi(2) + (ay + dy * t - py).powi(2)).sqrt()
}

/// Супер-эллипс («сквиркл») как у иконок macOS: < 0 внутри.
fn squircle(px: f32, py: f32) -> f32 {
    let (cx, cy, half) = (512.0, 512.0, 412.0);
    let (x, y) = (((px - cx) / half).abs(), ((py - cy) / half).abs());
    let n = 5.0;
    ((x.powf(n) + y.powf(n)).powf(1.0 / n) - 1.0) * half
}

fn mix(a: [f32; 3], b: [f32; 3], t: f32) -> [f32; 3] {
    [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t, a[2] + (b[2] - a[2]) * t]
}

fn over(dst: &mut [f32; 4], c: [f32; 3], a: f32) {
    for i in 0..3 {
        dst[i] = dst[i] * (1.0 - a) + c[i] * a;
    }
    dst[3] = dst[3] + a * (1.0 - dst[3]);
}

fn main() {
    let out = std::env::args().nth(1).unwrap_or_else(|| "icon_1024.png".into());

    // Снежинка: 6 лучей с веточками.
    let (fx, fy, len) = (512.0f32, 440.0f32, 230.0f32);
    let mut segs: Vec<[f32; 4]> = Vec::new();
    for k in 0..6 {
        let a = k as f32 * PI / 3.0 - PI / 2.0;
        let (s, c) = a.sin_cos();
        segs.push([fx, fy, fx + c * len, fy + s * len]);
        for (at, bl) in [(0.45f32, 80.0f32), (0.72, 55.0)] {
            let (px, py) = (fx + c * len * at, fy + s * len * at);
            for side in [-1.0f32, 1.0] {
                let b = a + side * 0.85;
                segs.push([px, py, px + b.cos() * bl, py + b.sin() * bl]);
            }
        }
    }
    // Мелкие снежинки-точки.
    let dots: [(f32, f32, f32); 9] = [
        (250.0, 230.0, 9.0),
        (760.0, 210.0, 7.0),
        (820.0, 420.0, 10.0),
        (200.0, 480.0, 7.0),
        (330.0, 640.0, 6.0),
        (700.0, 650.0, 8.0),
        (600.0, 180.0, 5.0),
        (420.0, 170.0, 6.0),
        (880.0, 600.0, 5.0),
    ];

    let top = [0.04, 0.09, 0.25];
    let bottom = [0.13, 0.36, 0.78];
    let mut img = vec![0u8; N * N * 4];
    for y in 0..N {
        for x in 0..N {
            let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
            let shape = (0.5 - squircle(px, py)).clamp(0.0, 1.0);
            if shape <= 0.0 {
                continue;
            }
            let mut c = [0.0f32; 4];
            // ночное небо
            over(&mut c, mix(top, bottom, (py - 100.0) / 824.0), 1.0);
            // свечение вокруг снежинки
            let glow = (1.0 - ((px - fx).powi(2) + (py - fy).powi(2)).sqrt() / 360.0).clamp(0.0, 1.0);
            over(&mut c, [0.55, 0.75, 1.0], glow * glow * 0.35);
            for (dx, dy, r) in dots {
                let d = ((px - dx).powi(2) + (py - dy).powi(2)).sqrt();
                over(&mut c, [1.0, 1.0, 1.0], (r - d + 0.5).clamp(0.0, 1.0) * 0.85);
            }
            // снежинка
            let d = segs.iter().map(|s| seg_dist(px, py, *s)).fold(f32::MAX, f32::min);
            over(&mut c, [1.0, 1.0, 1.0], (17.0 - d + 0.5).clamp(0.0, 1.0));
            // сугроб: две волны, сверху белый, книзу голубоватый
            let h1 = 770.0 - 55.0 * ((px - 300.0) / 260.0).cos().max(0.0) * (1.0 - ((px - 300.0) / 520.0).powi(2)).max(0.0);
            let h2 = 800.0 - 70.0 * (1.0 - ((px - 760.0) / 330.0).powi(2)).max(0.0);
            let surf = h1.min(h2);
            let a = (py - surf + 0.5).clamp(0.0, 1.0);
            let snow = mix([1.0, 1.0, 1.0], [0.72, 0.82, 0.96], ((py - surf) / 180.0).clamp(0.0, 1.0));
            over(&mut c, snow, a);
            let i = (y * N + x) * 4;
            for k in 0..3 {
                img[i + k] = (c[k].clamp(0.0, 1.0) * 255.0) as u8;
            }
            img[i + 3] = (shape * 255.0) as u8;
        }
    }

    let file = std::fs::File::create(&out).expect("create png");
    let mut enc = png::Encoder::new(std::io::BufWriter::new(file), N as u32, N as u32);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    enc.write_header().unwrap().write_image_data(&img).unwrap();
    println!("{out}");
}
