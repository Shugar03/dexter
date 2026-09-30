#!/usr/bin/env python3
"""Render docs/assets/suite.gif — the README demo.

Runs the real hermetic sim suite (`suite_report`, rule-based engine) and
replays its captured stdout as a terminal animation. Nothing is scripted
by hand: the frames show exactly what the command printed.

    python3 scripts/demo_gif.py            # needs cargo + Pillow
    DEMO_FONT=/path/to/Mono.ttf python3 scripts/demo_gif.py
"""
import os
import subprocess
import textwrap
from pathlib import Path

from PIL import Image, ImageDraw, ImageFont

ROOT = Path(__file__).resolve().parent.parent
OUT = ROOT / "docs" / "assets" / "suite.gif"
CMD = ["cargo", "run", "-q", "-p", "dexter-eval", "--example", "suite_report", "1"]
PROMPT = "$ " + " ".join(CMD)

COLS, SIZE, PAD, LINE = 88, 14, 16, 19
BG, FG, DIM = (22, 24, 29), (220, 223, 228), (120, 126, 138)
GREEN, AMBER, CYAN = (120, 200, 120), (230, 185, 90), (110, 180, 230)


def font():
    candidates = [
        os.environ.get("DEMO_FONT", ""),
        "/usr/share/fonts/truetype/dejavu/DejaVuSansMono.ttf",
        "/System/Library/Fonts/Menlo.ttc",
    ]
    for c in candidates:
        if c and Path(c).exists():
            return ImageFont.truetype(c, SIZE)
    raise SystemExit("no monospace font found; set DEMO_FONT")


def color(line):
    if line.startswith("$"):
        return CYAN
    if "| completed |" in line:
        return GREEN
    if "| abstained |" in line:
        return AMBER
    if line.startswith("suite:"):
        return FG
    return DIM


def main():
    out = subprocess.run(CMD, cwd=ROOT, check=True, capture_output=True, text=True).stdout
    body = []
    for raw in out.rstrip("\n").splitlines():
        body += textwrap.wrap(raw, COLS, subsequent_indent="  ") or [""]

    f = font()
    width = PAD * 2 + int(f.getlength("M" * COLS))
    height = PAD * 2 + LINE * (len(body) + 2)

    def frame(lines, cursor=False):
        img = Image.new("RGB", (width, height), BG)
        d = ImageDraw.Draw(img)
        for i, ln in enumerate(lines):
            d.text((PAD, PAD + i * LINE), ln, font=f, fill=color(ln))
        if cursor:
            x = PAD + int(f.getlength(lines[-1]))
            y = PAD + (len(lines) - 1) * LINE
            d.rectangle([x + 1, y + 2, x + 8, y + SIZE + 2], fill=FG)
        return img

    frames, durations = [], []
    for n in range(2, len(PROMPT) + 1, 3):
        frames.append(frame([PROMPT[:n]], cursor=True))
        durations.append(35)
    frames.append(frame([PROMPT], cursor=True))
    durations.append(600)
    shown = [PROMPT, ""]
    for ln in body:
        shown.append(ln)
        frames.append(frame(shown))
        durations.append(120 if "|" in ln else 400)
    durations[-1] = 4000

    OUT.parent.mkdir(parents=True, exist_ok=True)
    frames[0].save(
        OUT, save_all=True, append_images=frames[1:], duration=durations,
        loop=0, optimize=True,
    )
    print(f"wrote {OUT.relative_to(ROOT)} ({OUT.stat().st_size // 1024} KiB, {len(frames)} frames)")


if __name__ == "__main__":
    main()
