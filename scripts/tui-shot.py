#!/usr/bin/env python3
"""Run a terminal program in a fixed-size tmux pane, send it keys, and save the
screen as HTML with its colors, so a TUI can be checked and reviewed as an image.

    scripts/tui-shot.py --size 100x30 --keys '/ arch Enter' --out shot.html -- target/debug/torrential

Keys are space-separated tmux key names (Enter, Escape, Tab, Up, C-p, Space) or
literal text. `sleep:0.5` pauses between keys. The page is captured after --settle
seconds, and --frames N captures N screens --interval seconds apart.
"""

import argparse
import html
import os
import re
import shlex
import subprocess
import sys
import time

BG = "#101010"
FG = "#ffffff"
XTERM_16 = [
    "#000000", "#cd0000", "#00cd00", "#cdcd00", "#0000ee", "#cd00cd", "#00cdcd", "#e5e5e5",
    "#7f7f7f", "#ff0000", "#00ff00", "#ffff00", "#5c5cff", "#ff00ff", "#00ffff", "#ffffff",
]
SGR = re.compile(r"\x1b\[([0-9;:]*)m")


def color_256(n):
    if n < 16:
        return XTERM_16[n]
    if n < 232:
        n -= 16
        steps = [0, 95, 135, 175, 215, 255]
        return "#%02x%02x%02x" % (steps[n // 36], steps[n // 6 % 6], steps[n % 6])
    level = 8 + (n - 232) * 10
    return "#%02x%02x%02x" % (level, level, level)


def parse_color(params, i):
    kind = params[i + 1] if i + 1 < len(params) else None
    if kind == 2 and i + 4 < len(params):
        r, g, b = params[i + 2 : i + 5]
        return "#%02x%02x%02x" % (r, g, b), i + 5
    if kind == 5 and i + 2 < len(params):
        return color_256(params[i + 2]), i + 3
    return None, i + 1


def apply_sgr(state, raw):
    params = [int(p) if p else 0 for p in re.split("[;:]", raw)] if raw else [0]
    i = 0
    while i < len(params):
        p = params[i]
        if p == 0:
            state.clear()
        elif p == 1:
            state["bold"] = True
        elif p == 2:
            state["dim"] = True
        elif p == 3:
            state["italic"] = True
        elif p == 4:
            state["underline"] = True
        elif p == 7:
            state["reverse"] = True
        elif p == 22:
            state.pop("bold", None)
            state.pop("dim", None)
        elif p == 23:
            state.pop("italic", None)
        elif p == 24:
            state.pop("underline", None)
        elif p == 27:
            state.pop("reverse", None)
        elif 30 <= p <= 37:
            state["fg"] = XTERM_16[p - 30]
        elif 90 <= p <= 97:
            state["fg"] = XTERM_16[p - 90 + 8]
        elif 40 <= p <= 47:
            state["bg"] = XTERM_16[p - 40]
        elif 100 <= p <= 107:
            state["bg"] = XTERM_16[p - 100 + 8]
        elif p == 39:
            state.pop("fg", None)
        elif p == 49:
            state.pop("bg", None)
        elif p in (38, 48):
            color, i = parse_color(params, i)
            if color:
                state["fg" if p == 38 else "bg"] = color
            continue
        i += 1


def style_of(state):
    fg, bg = state.get("fg", FG), state.get("bg")
    if state.get("reverse"):
        fg, bg = bg or BG, fg
    parts = [f"color:{fg}"]
    if bg:
        parts.append(f"background:{bg}")
    if state.get("bold"):
        parts.append("font-weight:700")
    if state.get("dim"):
        parts.append("opacity:0.6")
    if state.get("italic"):
        parts.append("font-style:italic")
    if state.get("underline"):
        parts.append("text-decoration:underline")
    return ";".join(parts)


def ansi_to_html(lines):
    out = []
    state = {}
    for line in lines:
        cells = []
        pos = 0
        for m in SGR.finditer(line):
            if m.start() > pos:
                cells.append((style_of(state), line[pos : m.start()]))
            apply_sgr(state, m.group(1))
            pos = m.end()
        if pos < len(line):
            cells.append((style_of(state), line[pos:]))
        out.append("".join(f'<span style="{s}">{html.escape(t)}</span>' for s, t in cells) or " ")
    return "\n".join(out)


def page(screens, cols, title):
    body = "".join(
        f'<figure><figcaption>{html.escape(label)}</figcaption><pre style="width:{cols}ch">{content}</pre></figure>'
        for label, content in screens
    )
    return f"""<!doctype html><html><head><meta charset="utf-8"><title>{html.escape(title)}</title><style>
body{{margin:0;padding:16px;background:transparent;font-family:ui-sans-serif,system-ui}}
figure{{margin:0 0 20px}} figcaption{{color:#a0a0a0;font:12px ui-monospace,monospace;margin:0 0 6px}}
pre{{margin:0;padding:12px 14px;background:{BG};color:{FG};border-radius:8px;
font:13px/1.35 "JetBrains Mono","SF Mono",Menlo,monospace;white-space:pre;overflow:hidden}}
</style></head><body>{body}</body></html>"""


def tmux(*args):
    return subprocess.run(["tmux", *args], check=True, capture_output=True, text=True).stdout


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--size", default="100x30")
    ap.add_argument("--keys", default="")
    ap.add_argument("--settle", type=float, default=1.0)
    ap.add_argument("--frames", type=int, default=1)
    ap.add_argument("--interval", type=float, default=1.0)
    ap.add_argument("--out", required=True)
    ap.add_argument("--title", default="torrential")
    ap.add_argument("--env", action="append", default=[], help="KEY=VALUE for the program")
    ap.add_argument("command", nargs=argparse.REMAINDER)
    args = ap.parse_args()
    command = args.command[1:] if args.command[:1] == ["--"] else args.command
    if not command:
        sys.exit("give the program to run after --")
    cols, rows = (int(x) for x in args.size.split("x"))
    session = f"tui-shot-{os.getpid()}"
    env = " ".join(shlex.quote(e) for e in ["TERM=xterm-256color", "COLORTERM=truecolor", *args.env])
    tmux("new-session", "-d", "-s", session, "-x", str(cols), "-y", str(rows), f"env {env} {shlex.join(command)}; sleep 30")
    try:
        time.sleep(0.6)
        for key in args.keys.split():
            if key.startswith("sleep:"):
                time.sleep(float(key[6:]))
                continue
            named = re.fullmatch(r"(Enter|Escape|Tab|BTab|Up|Down|Left|Right|Space|BSpace|Home|End|PageUp|PageDown|[CM]-.|F\d+)", key)
            tmux("send-keys", "-t", session, *([key] if named else ["-l", key]))
            time.sleep(0.08)
        time.sleep(args.settle)
        screens = []
        for n in range(args.frames):
            if n:
                time.sleep(args.interval)
            raw = tmux("capture-pane", "-p", "-e", "-t", session)
            label = f"{cols}x{rows} · keys: {args.keys or '(none)'}" + (f" · frame {n + 1}" if args.frames > 1 else "")
            screens.append((label, ansi_to_html(raw.rstrip("\n").split("\n")[:rows])))
    finally:
        subprocess.run(["tmux", "kill-session", "-t", session], capture_output=True)
    with open(args.out, "w", encoding="utf-8") as f:
        f.write(page(screens, cols, args.title))
    print(args.out)


if __name__ == "__main__":
    main()
