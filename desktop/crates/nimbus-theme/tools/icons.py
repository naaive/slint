# SPDX-License-Identifier: MIT
#
# Generates ui/icons/*.svg and ui/icons.slint from the shapes below.
# Run from the crate directory: python3 tools/icons.py
# Shapes use a 16x16 grid. Stroked shapes use 1.5px round strokes; F() shapes are filled.

import math
import os

def P(d, **kw):
    return ("p", d, kw)

def F(d):
    return ("f", d, {})

def C(cx, cy, r, filled=False, **kw):
    return ("c", (cx, cy, r, filled), kw)

def R(x, y, w, h, rx=0.0, filled=False, **kw):
    return ("r", (x, y, w, h, rx, filled), kw)

FAINT = {"opacity": "0.3"}

def arc_path(cx, cy, r, a0, a1):
    """Arc from angle a0 to a1 in degrees, 0 = up, clockwise."""
    def pt(a):
        t = math.radians(a)
        return cx + r * math.sin(t), cy - r * math.cos(t)
    x0, y0 = pt(a0)
    x1, y1 = pt(a1)
    large = 1 if (a1 - a0) % 360 > 180 else 0
    return f"M{x0:.2f} {y0:.2f}A{r} {r} 0 {large} 1 {x1:.2f} {y1:.2f}"

def arrow_arc(cx, cy, r, a0, a1, head=2.6):
    """A clockwise arc from a0 to a1 with an arrowhead at a1."""
    t = math.radians(a1)
    tip = (cx + r * math.sin(t), cy - r * math.cos(t))
    tangent = math.atan2(math.sin(t), math.cos(t))
    wings = []
    for spread in (math.radians(150), math.radians(-150)):
        a = tangent + spread
        wings.append((tip[0] + head * math.cos(a), tip[1] + head * math.sin(a)))
    d = (f"M{wings[0][0]:.2f} {wings[0][1]:.2f}L{tip[0]:.2f} {tip[1]:.2f}L{wings[1][0]:.2f} {wings[1][1]:.2f}")
    return [P(arc_path(cx, cy, r, a0, a1)), P(d)]

def gear():
    pts = []
    teeth = 8
    for i in range(teeth * 4):
        a = 2 * math.pi * i / (teeth * 4) + math.pi / (teeth * 4)
        r = 6.5 if (i % 4) in (0, 1) else 4.9
        pts.append((8 + r * math.sin(a), 8 - r * math.cos(a)))
    d = "M" + "L".join(f"{x:.2f} {y:.2f}" for x, y in pts) + "Z"
    return [P(d), C(8, 8, 2.1)]

def wifi(level):
    shapes = [C(8, 12.75, 1.15, filled=True)]
    for i, r in enumerate((3.6, 6.6, 9.6)):
        shapes.append(P(arc_path(8, 12.75, r, -45, 45), **({} if i < level else FAINT)))
    return shapes

SPEAKER = P("M2.25 6.25h2.5L8 3.25v9.5l-3.25-3H2.25z")

def volume(level):
    waves = ["M10.5 6.25a2.5 2.5 0 0 1 0 3.5", "M12 4.5a5 5 0 0 1 0 7", "M13.5 2.75a7.5 7.5 0 0 1 0 10.5"]
    return [SPEAKER] + [P(w, **({} if i < level else FAINT)) for i, w in enumerate(waves)]

BATTERY = [R(1.25, 4.25, 12, 7.5, 1.75), P("M15 6.75v2.5")]

def battery(level):
    shapes = list(BATTERY)
    if level > 0:
        shapes.append(R(3, 6, 8.5 * level / 4, 4, 0.75, filled=True))
    return shapes

FILE = [P("M3.75 2.75a1 1 0 0 1 1-1H9.5l3.25 3.25v8.25a1 1 0 0 1-1 1h-7a1 1 0 0 1-1-1z"), P("M9.5 1.75V5h3.25")]
MONITOR = [R(1.75, 2.5, 12.5, 8.5, 1.5), P("M5.5 13.75h5M8 11v2.75")]
EYE = [P("M1.5 8s2.5-4.75 6.5-4.75S14.5 8 14.5 8s-2.5 4.75-6.5 4.75S1.5 8 1.5 8z"), C(8, 8, 2)]
BELL = [P("M4 11V7a4 4 0 0 1 8 0v4l1.25 1.5H2.75z"), P("M6.5 14.25h3")]
SLASH = P("M2.5 2.5l11 11")
JACK = [P("M2.5 4.25h11v6.5H11v2.5H5v-2.5H2.5z"), P("M6 6.75v1.5M8 6.75v1.5M10 6.75v1.5")]
MIC = [R(5.75, 1.75, 4.5, 7.5, 2.25), P("M3.5 7.5a4.5 4.5 0 0 0 9 0M8 12v2.25")]
SUN_RAYS = "".join(
    f"M{8 + 5.1 * math.sin(math.radians(a)):.2f} {8 - 5.1 * math.cos(math.radians(a)):.2f}"
    f"L{8 + 6.6 * math.sin(math.radians(a)):.2f} {8 - 6.6 * math.cos(math.radians(a)):.2f}"
    for a in range(0, 360, 45))

ICONS = {
    "search": [C(7, 7, 4.5), P("M10.5 10.5L14 14")],
    "close": [P("M4 4l8 8M12 4l-8 8")],
    "minimize": [P("M4 11.5h8")],
    "maximize": [R(3.5, 3.5, 9, 9, 1.5)],
    "restore": [R(2.75, 5.25, 8, 8, 1.5), P("M5.25 2.75h6.5a1.5 1.5 0 0 1 1.5 1.5v6.5")],
    "menu": [P("M2.75 4.25h10.5M2.75 8h10.5M2.75 11.75h10.5")],
    "apps-grid": [C(x, y, 1.35, filled=True) for y in (3.5, 8, 12.5) for x in (3.5, 8, 12.5)],
    "settings": gear(),
    "power": [P("M8 1.75V8"), P(arc_path(8, 8.5, 5.5, 40, 320))],
    "restart": arrow_arc(8, 8, 5.5, 60, 360),
    "logout": [P("M7 2.25H4a1.5 1.5 0 0 0-1.5 1.5v8.5a1.5 1.5 0 0 0 1.5 1.5h3"), P("M6.5 8h7M11 5.5L13.5 8L11 10.5")],
    "lock": [R(3.25, 7, 9.5, 7, 1.5), P("M5.5 7V5a2.5 2.5 0 0 1 5 0v2")],
    "suspend": [C(8, 8, 6), P("M6.5 5.75v4.5M9.5 5.75v4.5")],
    "user": [C(8, 5.25, 2.75), P("M2.75 14c0-2.75 2.25-4.5 5.25-4.5s5.25 1.75 5.25 4.5")],
    "bell": BELL,
    "bell-off": BELL + [SLASH],
    "do-not-disturb": [C(8, 8, 6.25), P("M5 8h6")],
    "wifi-0": wifi(0),
    "wifi-1": wifi(1),
    "wifi-2": wifi(2),
    "wifi-3": wifi(3),
    "wifi-off": [C(8, 12.75, 1.15, filled=True, **FAINT)]
    + [P(arc_path(8, 12.75, r, -45, 45), **FAINT) for r in (3.6, 6.6, 9.6)]
    + [P("M3 2.5l10 10.5")],
    "ethernet": JACK,
    "network-off": [P(d, **FAINT) for _, d, _ in JACK] + [P("M2 2l12 12")],
    "volume-muted": [SPEAKER, P("M10.75 6.25l3.5 3.5M14.25 6.25l-3.5 3.5")],
    "volume-low": volume(1),
    "volume-medium": volume(2),
    "volume-high": volume(3),
    "brightness": [C(8, 8, 3), F("M8 5a3 3 0 0 0 0 6z"), P(SUN_RAYS)],
    "sun": [C(8, 8, 2.75), P(SUN_RAYS)],
    "moon": [P("M13.25 9.75A5.75 5.75 0 1 1 6.25 2.75a4.75 4.75 0 0 0 7 7z")],
    "battery-0": battery(0),
    "battery-1": battery(1),
    "battery-2": battery(2),
    "battery-3": battery(3),
    "battery-4": battery(4),
    "battery-charging": BATTERY + [F("M8.25 5.25L4.75 8.5h2.5l-1 2.25L9.75 7.5h-2.5z")],
    "bluetooth": [P("M4.5 5l7 6l-3.5 3V2l3.5 3l-7 6")],
    "bluetooth-off": [P("M8 6.5V2l3.5 3l-1.5 1.25M8 9.5V14l3.5-3l-1.25-1M4.5 5l3.5 3"), SLASH],
    "play": [F("M4.75 3.1a.75.75 0 0 1 1.15-.63l7 4.9a.75.75 0 0 1 0 1.26l-7 4.9a.75.75 0 0 1-1.15-.63z")],
    "pause": [R(3.75, 3, 3, 10, 0.9, filled=True), R(9.25, 3, 3, 10, 0.9, filled=True)],
    "next": [F("M2.75 4a.75.75 0 0 1 1.17-.62l6 4a.75.75 0 0 1 0 1.24l-6 4A.75.75 0 0 1 2.75 12z"), R(11, 3.5, 2, 9, 0.75, filled=True)],
    "previous": [F("M13.25 4a.75.75 0 0 0-1.17-.62l-6 4a.75.75 0 0 0 0 1.24l6 4a.75.75 0 0 0 1.17-.62z"), R(3, 3.5, 2, 9, 0.75, filled=True)],
    "chevron-left": [P("M10 3.5L5.5 8l4.5 4.5")],
    "chevron-right": [P("M6 3.5L10.5 8L6 12.5")],
    "chevron-up": [P("M3.5 10L8 5.5l4.5 4.5")],
    "chevron-down": [P("M3.5 6L8 10.5L12.5 6")],
    "arrow-left": [P("M13 8H3M7.5 3.5L3 8l4.5 4.5")],
    "arrow-right": [P("M3 8h10M8.5 3.5L13 8l-4.5 4.5")],
    "arrow-up": [P("M8 13V3M3.5 7.5L8 3l4.5 4.5")],
    "check": [P("M3 8.5l3.25 3.25L13 5")],
    "plus": [P("M8 3v10M3 8h10")],
    "minus": [P("M3 8h10")],
    "more-horizontal": [C(x, 8, 1.35, filled=True) for x in (3.25, 8, 12.75)],
    "more-vertical": [C(8, y, 1.35, filled=True) for y in (3.25, 8, 12.75)],
    "folder": [P("M1.75 4.25A1.25 1.25 0 0 1 3 3h3.25l1.5 1.75H13a1.25 1.25 0 0 1 1.25 1.25v6A1.25 1.25 0 0 1 13 13.25H3A1.25 1.25 0 0 1 1.75 12z")],
    "folder-open": [P("M1.75 12V4.25A1.25 1.25 0 0 1 3 3h3.25l1.5 1.75H12a1.25 1.25 0 0 1 1.25 1.25V7"), P("M1.75 12l1.6-4.15A1.25 1.25 0 0 1 4.5 7h9.2a.75.75 0 0 1 .7 1l-1.6 4.4a1.25 1.25 0 0 1-1.17.85H3a1.25 1.25 0 0 1-1.25-1.25z")],
    "file": FILE,
    "file-text": FILE + [P("M6 8h4M6 10.5h4M6 5.5h1.5")],
    "file-image": FILE + [C(6.5, 7.5, 1, filled=True), P("M5.25 12l2-2.25 1.5 1.25 1-1 1.25 1.5")],
    "file-audio": FILE + [C(6.9, 11, 1.25), P("M8.15 11V6.5l2 .75")],
    "file-video": FILE + [F("M6.5 7.25a.5.5 0 0 1 .76-.43l3 1.75a.5.5 0 0 1 0 .86l-3 1.75a.5.5 0 0 1-.76-.43z")],
    "file-archive": FILE + [P("M7 3v.01M7 5v.01M7 7v.01"), R(6, 8.75, 2.5, 2.75, 0.6)],
    "home": [P("M2 7.75L8 2.5l6 5.25"), P("M3.75 6.5v6.75a.75.75 0 0 0 .75.75H6.75v-4h2.5v4h2.25a.75.75 0 0 0 .75-.75V6.5")],
    "desktop": MONITOR + [P("M4.5 8.5h7")],
    "monitor": MONITOR,
    "display": MONITOR,
    "documents": [P("M5.25 2.25a1 1 0 0 1 1-1h5l2.5 2.5v7.5a1 1 0 0 1-1 1"), R(2.25, 4.25, 8.5, 10.5, 1), P("M4.5 8h4M4.5 10.5h4")],
    "downloads": [P("M8 2v8M4.75 6.75L8 10l3.25-3.25"), P("M2.5 10.75v2a1 1 0 0 0 1 1h9a1 1 0 0 0 1-1v-2")],
    "upload": [P("M8 10V2M4.75 5.25L8 2l3.25 3.25"), P("M2.5 10.75v2a1 1 0 0 0 1 1h9a1 1 0 0 0 1-1v-2")],
    "music": [P("M6.25 12V3.5l7-1.5v8.5"), C(4.5, 12, 1.75), C(11.5, 10.5, 1.75)],
    "pictures": [R(1.75, 2.75, 12.5, 10.5, 1.5), C(5.5, 6.25, 1.25), P("M1.75 11.5l3.5-3.5l2.5 2.5l2.5-3l4 4")],
    "videos": [R(1.75, 3, 12.5, 10, 1.5), F("M6.5 6.1a.5.5 0 0 1 .76-.43l3.3 1.9a.5.5 0 0 1 0 .86l-3.3 1.9a.5.5 0 0 1-.76-.43z")],
    "trash": [P("M2.5 4.25h11M6 4.25v-1.5h4v1.5"), P("M3.75 4.25l.7 8.85a1 1 0 0 0 1 .9h5.1a1 1 0 0 0 1-.9l.7-8.85"), P("M6.75 7v4.25M9.25 7v4.25")],
    "drive": [R(1.75, 4.5, 12.5, 7, 1.5), C(11.25, 8, 0.9, filled=True), P("M4.25 8h3.5")],
    "terminal": [R(1.75, 2.75, 12.5, 10.5, 1.5), P("M4.5 6l2 2l-2 2M8.25 10.25h3.25")],
    "keyboard": [R(1, 3.75, 14, 8.5, 1.5), P("M3.75 6.5h.5M6 6.5h.5M8.25 6.5h.5M10.5 6.5h1.75M3.75 9.5h.5M5.75 9.5h4.5M11.75 9.5h.5")],
    "mouse": [R(4.25, 1.75, 7.5, 12.5, 3.75), P("M8 4.25v2")],
    "palette": [P("M8 1.75a6.25 6.25 0 1 0 0 12.5c.9 0 1.5-.65 1.5-1.4 0-1 .7-1.6 1.6-1.6h1.4a1.75 1.75 0 0 0 1.75-1.75C14.25 4.4 11.4 1.75 8 1.75z"),
                C(4.75, 7.75, 1, filled=True), C(6.5, 4.75, 1, filled=True), C(9.9, 4.85, 1, filled=True)],
    "info": [C(8, 8, 6.25), P("M8 7.25v4"), C(8, 4.85, 0.9, filled=True)],
    "warning": [P("M7.13 2.25a1 1 0 0 1 1.74 0l5.6 10a1 1 0 0 1-.87 1.5H2.4a1 1 0 0 1-.87-1.5z"), P("M8 6v3.5"), C(8, 11.6, 0.85, filled=True)],
    "error": [C(8, 8, 6.25), P("M5.75 5.75l4.5 4.5M10.25 5.75l-4.5 4.5")],
    "copy": [R(5.25, 5.25, 8.5, 8.5, 1.5), P("M10.75 5.25v-1.5a1.5 1.5 0 0 0-1.5-1.5h-5.5a1.5 1.5 0 0 0-1.5 1.5v5.5a1.5 1.5 0 0 0 1.5 1.5h1.5")],
    "paste": [P("M5.5 2.75H4.5a1.5 1.5 0 0 0-1.5 1.5v8.5a1.5 1.5 0 0 0 1.5 1.5h7a1.5 1.5 0 0 0 1.5-1.5v-8.5a1.5 1.5 0 0 0-1.5-1.5h-1"), R(5.75, 1.5, 4.5, 2.5, 0.75)],
    "cut": [C(4.5, 11.5, 2), C(11.5, 11.5, 2), P("M5.75 10L11.5 2.25M10.25 10L4.5 2.25")],
    "edit": [P("M10.5 2.5l3 3l-8 8H2.5v-3z"), P("M8.75 4.25l3 3")],
    "rename": [P("M2.25 5.25h7.5M2.25 10.75h3.5"), P("M11 6.25l2.25 2.25l-4.5 4.5H6.5v-2.25z")],
    "sort": [P("M4.5 2.5v11M2.25 11.25L4.5 13.5l2.25-2.25"), P("M9.25 4h4.5M9.25 7.5h3.25M9.25 11h2")],
    "grid-view": [R(2.25, 2.25, 4.75, 4.75, 1), R(9, 2.25, 4.75, 4.75, 1), R(2.25, 9, 4.75, 4.75, 1), R(9, 9, 4.75, 4.75, 1)],
    "list-view": [P("M6 4h8M6 8h8M6 12h8")] + [C(2.75, y, 1.1, filled=True) for y in (4, 8, 12)],
    "eye": EYE,
    "eye-off": EYE + [SLASH],
    "refresh": arrow_arc(8, 8, 5.5, 30, 160) + arrow_arc(8, 8, 5.5, 210, 340),
    "cpu": [R(4, 4, 8, 8, 1.25), R(6.25, 6.25, 3.5, 3.5, 0.5, filled=True),
            P("M6.25 1.75V4M9.75 1.75V4M6.25 12v2.25M9.75 12v2.25M1.75 6.25H4M1.75 9.75H4M12 6.25h2.25M12 9.75h2.25")],
    "memory": [R(1.5, 3.75, 13, 6.75, 1), R(3.5, 5.75, 2.25, 2.75, 0.4, filled=True), R(6.875, 5.75, 2.25, 2.75, 0.4, filled=True),
               R(10.25, 5.75, 2.25, 2.75, 0.4, filled=True), P("M3.75 10.5v2M6.25 10.5v2M8.75 10.5v2M11.25 10.5v2")],
    "disk": [C(8, 8, 6.25), C(8, 8, 1.75), P(arc_path(8, 8, 4, 290, 340))],
    "network-activity": [P("M5 13.5V2.5M2.5 5L5 2.5L7.5 5"), P("M11 2.5v11M8.5 11l2.5 2.5l2.5-2.5")],
    "calendar": [R(2, 3, 12, 11, 1.5), P("M2 6.5h12M5.5 1.75v2.5M10.5 1.75v2.5"),
                 C(5.25, 9.25, 0.85, filled=True), C(8, 9.25, 0.85, filled=True), C(10.75, 9.25, 0.85, filled=True), C(5.25, 11.75, 0.85, filled=True), C(8, 11.75, 0.85, filled=True)],
    "clock": [C(8, 8, 6.25), P("M8 4.5V8l2.5 1.5")],
    "workspaces": [R(1.75, 3.25, 5.5, 9.5, 1.25), R(8.75, 3.25, 5.5, 9.5, 1.25)],
    "sidebar": [R(1.75, 2.75, 12.5, 10.5, 1.5), P("M6.25 2.75v10.5")],
    "window": [R(1.75, 2.75, 12.5, 10.5, 1.5), P("M1.75 5.75h12.5")],
    "application": [R(2, 2, 12, 12, 3), C(8, 8, 2.5)],
    "pin": [P("M5.75 2.25h4.5M6.75 2.25v3.5L4.75 8.25v1.25h6.5V8.25l-2-2.5v-3.5M8 9.5v4.5")],
    "star": [P("M8 1.9l1.85 3.85l4.2.55l-3.08 2.9l.78 4.15L8 11.3l-3.75 2.05l.78-4.15l-3.08-2.9l4.2-.55z")],
    "airplane": [P("M8 1.75c.7 0 1.1.75 1.1 1.5v3.4l4.65 2.6v1.5l-4.65-1.4v2.55l1.5 1.1v1.25L8 13.5l-2.6.75V13l1.5-1.1V9.35l-4.65 1.4v-1.5L6.9 6.65v-3.4c0-.75.4-1.5 1.1-1.5z")],
    "microphone": MIC,
    "microphone-off": MIC + [SLASH],
    "camera": [P("M1.75 5.75a1.25 1.25 0 0 1 1.25-1.25h1.75l1.25-1.75h4l1.25 1.75H13a1.25 1.25 0 0 1 1.25 1.25v6.25A1.25 1.25 0 0 1 13 13.25H3A1.25 1.25 0 0 1 1.75 12z"), C(8, 8.75, 2.5)],
    "spinner": [P(arc_path(8, 8, 6, 0, 270), **{"stroke-width": "2"})],
}

def attrs(kw):
    return "".join(f' {k}="{v}"' for k, v in kw.items())

def render(shapes):
    out = []
    for kind, data, kw in shapes:
        if kind == "p":
            out.append(f'<path d="{data}"{attrs(kw)}/>')
        elif kind == "f":
            out.append(f'<path d="{data}" fill="#000" stroke="none"{attrs(kw)}/>')
        elif kind == "c":
            cx, cy, r, filled = data
            fill = ' fill="#000" stroke="none"' if filled else ""
            out.append(f'<circle cx="{cx}" cy="{cy}" r="{r}"{fill}{attrs(kw)}/>')
        elif kind == "r":
            x, y, w, h, rx, filled = data
            fill = ' fill="#000" stroke="none"' if filled else ""
            rxs = f' rx="{rx}"' if rx else ""
            out.append(f'<rect x="{x}" y="{y}" width="{w:g}" height="{h}"{rxs}{fill}{attrs(kw)}/>')
    body = "".join(out)
    return ('<svg xmlns="http://www.w3.org/2000/svg" width="16" height="16" viewBox="0 0 16 16" fill="none" '
            'stroke="#000" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round">' + body + "</svg>\n")

def main():
    here = os.path.dirname(os.path.abspath(__file__))
    ui = os.path.join(here, "..", "ui")
    icons = os.path.join(ui, "icons")
    os.makedirs(icons, exist_ok=True)
    for name in os.listdir(icons):
        if name.endswith(".svg"):
            os.remove(os.path.join(icons, name))
    for name, shapes in sorted(ICONS.items()):
        with open(os.path.join(icons, name + ".svg"), "w") as f:
            f.write(render(shapes))
    lines = ["// SPDX-License-Identifier: MIT", "",
             "// Generated by tools/icons.py; edit the shapes there.", "",
             "// Symbolic 16x16 icons. Show them with `Icon`, which colorizes them.",
             "export global Icons {"]
    for name in sorted(ICONS):
        lines.append(f'    out property <image> {name}: @image-url("icons/{name}.svg");')
    lines.append("}")
    with open(os.path.join(ui, "icons.slint"), "w") as f:
        f.write("\n".join(lines) + "\n")
    lines = ["// SPDX-License-Identifier: MIT", "",
             "// Generated by tools/icons.py; lists every icon for gallery.slint.", "",
             'import { Icons } from "icons.slint";', "",
             "export struct IconEntry {", "    name: string,", "    icon: image,", "}", "",
             "export global GalleryIcons {", "    out property <[IconEntry]> all: ["]
    for name in sorted(ICONS):
        lines.append(f'        {{ name: "{name}", icon: Icons.{name} }},')
    lines += ["    ];", "}"]
    with open(os.path.join(ui, "gallery-icons.slint"), "w") as f:
        f.write("\n".join(lines) + "\n")
    print(f"{len(ICONS)} icons")

if __name__ == "__main__":
    main()
