#!/bin/zsh
# Картинки для README: docs/screenshot.png и docs/demo.gif.
# Рабочий стол выдуманный (src/showcase.rs), снег — настоящая симуляция. Нужен ffmpeg.
set -euo pipefail
cd "$(dirname "$0")/.."
[[ -f "$HOME/.cargo/env" ]] && source "$HOME/.cargo/env"
OUT=target/showcase
rm -rf "$OUT" && mkdir -p docs
cargo run -q --release -- --showcase "$OUT"
ffmpeg -v error -y -i "$OUT/screenshot.ppm" -vf scale=1600:-1:flags=lanczos docs/screenshot.png
# GIF: 12 кадров/с, 720 пикселей (~4 МБ), своя палитра — иначе снег превращается в кашу
ffmpeg -v error -y -framerate 30 -i "$OUT/anim_%04d.ppm" \
  -vf "fps=12,scale=720:-1:flags=lanczos,split[a][b];[a]palettegen=max_colors=96:stats_mode=diff[p];[b][p]paletteuse=dither=bayer:bayer_scale=5:diff_mode=rectangle" \
  docs/demo.gif
ls -la docs/
