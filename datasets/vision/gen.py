#!/usr/bin/env python3
"""Generate datasets/vision/items.jsonl — the OCR-evidence domain.

Observations here mix a menu-only AX shell (what a degraded-grant or
AX-poor app reports) with `source: ocr` elements — inert text evidence,
no actions, no live handle. This is the surface `dexter observe
--vision` produces for canvas/Electron/custom-drawn apps.

What the items measure: does the engine treat OCR text as *evidence*
(route/abstain when nothing is actable) instead of fabricating acts —
and does OCR noise degrade picking when a real AX element exists?

Regenerate deliberately:  python3 datasets/vision/gen.py
"""

import json
import os

HERE = os.path.dirname(os.path.abspath(__file__))


def el(i, role, name=None, actions=(), parent=None, depth=0, enabled=True,
       value=None, bounds=(0.0, 0.0, 0.0, 0.0), source="accessibility"):
    x, y, w, h = bounds
    return {
        "id": i,
        "parent": parent,
        "depth": depth,
        "role": role,
        "raw_role": "ocr" if source == "ocr" else f"AX{role}",
        "subrole": None,
        "name": name,
        "value": value,
        "bounds": {"x": x, "y": y, "w": w, "h": h},
        "enabled": enabled,
        "focused": False,
        "actions": list(actions),
        "identifier": None,
        "source": source,
    }


def ocr(i, text, bounds, parent=None, depth=1):
    # OCR elements are inert: text role, no actions — evidence only.
    return el(i, "text", text, (), parent, depth, bounds=bounds, source="ocr")


def obs(app, elements, window_title=""):
    return {
        "id": 1,
        "timestamp": {"secs_since_epoch": 1790200000, "nanos_since_epoch": 0},
        "app": {"by": "bundle_id", "value": app},
        "pid": 1,
        "windows": [{
            "id": 100,
            "pid": 1,
            "app": app,
            "title": window_title,
            "bounds": {"x": 285.0, "y": 479.0, "w": 460.0, "h": 340.0},
            "on_screen": True,
            "layer": 0,
        }],
        "elements": elements,
        "elements_truncated": False,
        "collection_errors": 0,
        "ax_limited": True,
        "screenshot": None,
        "digest": "",
    }


def item(id, app, goal, elements, gold, window_title=""):
    return {
        "id": id,
        "goal": goal,
        "observation": obs(app, elements, window_title),
        "gold": gold,
        "source": "synthetic",
        "meta": {"app": f"vision/{app}"},
    }


def act(target, element):
    return {"type": "act", "target": target, "element": element}


def route(rtype):
    body = {"type": rtype}
    if rtype == "wait":
        body["millis"] = 500
    return {"type": "route", "route": body}


def sem(role=None, name=None, name_contains=None):
    return {
        "role": role,
        "name": name,
        "name_contains": name_contains,
        "value_contains": None,
        "identifier": None,
        "enabled": None,
        "index": None,
    }


# Minimal menu shell every AX-poor app still reports.
def menu_shell(start=1):
    return [
        el(start, "application", depth=0),
        el(start + 1, "menu_bar", depth=1, parent=start),
        el(start + 2, "menu_bar_item", "Archivo", ("press",), depth=2, parent=start + 1),
        el(start + 3, "menu", depth=3, parent=start + 2),
        el(start + 4, "menu_item", "Cerrar", ("press", "pick"), depth=4, parent=start + 3),
    ]


# 1. OCR says "Guardar" but nothing is actable → abstain, don't fabricate.
CALC_OCR_ONLY = menu_shell() + [
    ocr(10, "Guardar", (300, 520, 80, 24), parent=1),
    ocr(11, "0", (320, 560, 60, 40), parent=1),
    ocr(12, "7", (320, 610, 60, 40), parent=1),
]

# 2. A real AX menu item coexists with OCR noise → act on the AX handle.
CALC_MIXED = [
    el(1, "application", depth=0),
    el(2, "menu_bar", depth=1, parent=1),
    el(3, "menu_bar_item", "Archivo", ("press",), depth=2, parent=2),
    el(4, "menu", depth=3, parent=3),
    el(5, "menu_item", "Guardar…", ("press", "pick"), depth=4, parent=4),
    ocr(10, "128", (320, 560, 60, 40), parent=1),
    ocr(11, "+", (380, 560, 60, 40), parent=1),
    ocr(12, "Guardar", (300, 500, 70, 20), parent=1),
]

# 3. Canvas app: window full of OCR, no handles at all → abstain.
CANVAS = menu_shell() + [
    ocr(10, "Exportar", (320, 520, 90, 26), parent=1),
    ocr(11, "Capa 1", (320, 560, 80, 20), parent=1),
    ocr(12, "Capa 2", (320, 585, 80, 20), parent=1),
    ocr(13, "zoom 150%", (320, 700, 90, 18), parent=1),
]

# 4. OCR shows a spinner label — the page isn't ready → wait.
LOADING = menu_shell() + [
    ocr(10, "Cargando…", (330, 600, 100, 24), parent=1),
]

# 5. OCR-only login form — multi-step, ambiguous → escalate to a
#    larger model (route option exists for exactly this).
LOGIN = menu_shell() + [
    ocr(10, "Iniciar sesión", (320, 500, 140, 28), parent=1),
    ocr(11, "Usuario", (320, 540, 80, 22), parent=1),
    ocr(12, "Contraseña", (320, 575, 100, 22), parent=1),
    ocr(13, "¿Olvidaste tu contraseña?", (320, 640, 200, 18), parent=1),
]


ITEMS = [
    item("ocr-save-evidence-only", "Calculadora",
         "guardar el archivo", CALC_OCR_ONLY, route("abstain")),
    item("ocr-mixed-ax-wins", "Calculadora",
         "guardar el documento", CALC_MIXED,
         act(sem("menu_item", name="Guardar…"), 5)),
    item("ocr-canvas-export", "Lienzo",
         "hacer clic en Exportar", CANVAS, route("abstain")),
    item("ocr-loading-wait", "Descargas",
         "continuar al panel", LOADING, route("wait")),
    item("ocr-login-escalate", "Portal",
         "iniciar sesión con la cuenta de soporte", LOGIN,
         route("escalate_llm")),
]


def main():
    out = os.path.join(HERE, "items.jsonl")
    with open(out, "w") as f:
        for it in ITEMS:
            f.write(json.dumps(it, ensure_ascii=False) + "\n")
    print(f"wrote {len(ITEMS)} items to {out}")


if __name__ == "__main__":
    main()
