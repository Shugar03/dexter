#!/usr/bin/env python3
"""Generate datasets/sim/items.jsonl — a synthetic-app domain for the
cross-app generalization matrix.

These are NOT harvested: they are hand-authored desktop-app worlds
(installer wizard, media player, file manager) serialized as frozen
EvalItems. The point is a surface the models never saw in training —
a different UI idiom than web pages (browser) and macOS menu-driven
apps (macos) — so leave-one-domain-out measures real transfer.

Goals are in Spanish to match the machine locale and the other macOS
items. Every act gold is verified by `eval run`/`eval matrix` resolving
the declared target (the harness fails if it does not resolve).

Regenerate deliberately:  python3 datasets/sim/gen.py
"""

import json
import os

HERE = os.path.dirname(os.path.abspath(__file__))


def el(i, role, name=None, actions=(), parent=None, depth=0, enabled=True,
       value=None, bounds=(0.0, 0.0, 0.0, 0.0), focused=False):
    x, y, w, h = bounds
    return {
        "id": i,
        "parent": parent,
        "depth": depth,
        "role": role,
        "raw_role": f"SIM{role.upper()}",
        "subrole": None,
        "name": name,
        "value": value,
        "bounds": {"x": x, "y": y, "w": w, "h": h},
        "enabled": enabled,
        "focused": focused,
        "actions": list(actions),
        "identifier": None,
        "source": "dom",
    }


def obs(app, elements, window_title):
    return {
        "id": 1,
        "timestamp": {"secs_since_epoch": 1790200000, "nanos_since_epoch": 0},
        "app": {"by": "name", "value": app},
        "pid": 1,
        "windows": [{
            "id": 1,
            "pid": 1,
            "app": app,
            "title": window_title,
            "bounds": {"x": 100.0, "y": 100.0, "w": 640.0, "h": 480.0},
            "on_screen": True,
            "layer": 0,
        }],
        "elements": elements,
        "elements_truncated": False,
        "collection_errors": 0,
        "ax_limited": False,
        "screenshot": None,
        "digest": "",
    }


def item(id, app, goal, elements, gold, window_title):
    return {
        "id": id,
        "goal": goal,
        "observation": obs(app, elements, window_title),
        "gold": gold,
        "source": "synthetic",
        "meta": {"app": f"sim/{app}"},
    }


def act(target, element):
    return {"type": "act", "target": target, "element": element}


def route(rtype):
    # Route variants carry their params — wait needs millis.
    body = {"type": rtype}
    if rtype == "wait":
        body["millis"] = 500
    return {"type": "route", "route": body}


def sem(role=None, name=None, name_contains=None, enabled=None):
    return {
        "role": role,
        "name": name,
        "name_contains": name_contains,
        "value_contains": None,
        "identifier": None,
        "enabled": enabled,
        "index": None,
    }


PRESS = ("press",)

# --- Installer wizard -------------------------------------------------
# "Siguiente" is DISABLED until the license check box is ticked — the
# naive "continue" pick is the disabled button; gold is the checkbox.
WIZARD = [
    el(1, "window", "Asistente de instalación", (), depth=0),
    el(2, "group", depth=1, parent=1),
    el(3, "static_text", "Lee el acuerdo de licencia para continuar", depth=2, parent=2),
    el(4, "scroll_area", depth=2, parent=2, bounds=(120, 150, 480, 200)),
    el(5, "text_area", depth=3, parent=4, value="LICENCIA..."),
    el(6, "check_box", "Acepto los términos de la licencia", PRESS, depth=2, parent=2),
    el(7, "button", "Atrás", PRESS, depth=2, parent=2, enabled=False),
    el(8, "button", "Siguiente", PRESS, depth=2, parent=2, enabled=False),
    el(9, "button", "Cancelar", PRESS, depth=2, parent=2),
]

# --- Media player -----------------------------------------------------
PLAYER = [
    el(1, "window", "Reproductor", (), depth=0),
    el(2, "toolbar", depth=1, parent=1),
    el(3, "button", "Anterior", PRESS, depth=2, parent=2),
    el(4, "button", "Pausar", PRESS, depth=2, parent=2),
    el(5, "button", "Siguiente", PRESS, depth=2, parent=2),
    el(6, "slider", "Volumen", ("set_value", "increment", "decrement"), depth=2,
       parent=2, value="40"),
    el(7, "static_text", "Sonando: Medianoche en Oslo — 02:41", depth=2, parent=2),
    el(8, "list", "Lista de reproducción", depth=1, parent=1),
    el(9, "row", "Medianoche en Oslo", ("press",), depth=2, parent=8),
    el(10, "row", "Valles de niebla", ("press",), depth=2, parent=8),
    el(11, "row", "Tren nocturno", ("press",), depth=2, parent=8),
]

# --- File manager -----------------------------------------------------
FILES = [
    el(1, "window", "Archivos", (), depth=0),
    el(2, "toolbar", depth=1, parent=1),
    el(3, "button", "Atrás", PRESS, depth=2, parent=2),
    el(4, "button", "Nueva carpeta", PRESS, depth=2, parent=2),
    el(5, "text_field", "Buscar", ("set_value", "focus"), depth=2, parent=2),
    el(6, "outline", "Favoritos", depth=1, parent=1),
    el(7, "row", "Documentos", ("press",), depth=2, parent=6),
    el(8, "row", "Descargas", ("press",), depth=2, parent=6),
    el(9, "table", depth=1, parent=1),
    el(10, "row", "factura_marzo.pdf", ("press",), depth=2, parent=9),
    el(11, "row", "notas_viaje.txt", ("press",), depth=2, parent=9),
    el(12, "row", "presupuesto.numbers", ("press",), depth=2, parent=9),
]

# --- Download in progress (route gold: wait) --------------------------
DOWNLOAD = [
    el(1, "window", "Descargas", (), depth=0),
    el(2, "group", depth=1, parent=1),
    el(3, "static_text", "Descargando actualización…", depth=2, parent=2),
    el(4, "progress_indicator", depth=2, parent=2, value="62"),
    el(5, "button", "Cancelar", PRESS, depth=2, parent=2),
]

# --- Admin panel absent (route gold: abstain) -------------------------
NOTES = [
    el(1, "window", "Bloc", (), depth=0),
    el(2, "toolbar", depth=1, parent=1),
    el(3, "button", "Nueva nota", PRESS, depth=2, parent=2),
    el(4, "text_field", "Buscar", ("set_value", "focus"), depth=2, parent=2),
    el(5, "text_area", depth=1, parent=1, value="lista del super"),
]


ITEMS = [
    item("wizard-accept-license", "Instalador",
         "aceptar los términos para continuar la instalación",
         WIZARD, act(sem("check_box", name_contains="licencia"), 6),
         "Asistente de instalación"),
    item("wizard-cancel", "Instalador", "cancelar la instalación",
         WIZARD, act(sem("button", name="Cancelar"), 9),
         "Asistente de instalación"),
    item("player-pause", "Reproductor", "pausar la reproducción",
         PLAYER, act(sem("button", name="Pausar"), 4), "Reproductor"),
    item("player-volume", "Reproductor", "subir el volumen",
         PLAYER, act(sem("slider", name="Volumen"), 6), "Reproductor"),
    item("player-pick-track", "Reproductor", "reproducir tren nocturno",
         PLAYER, act(sem("row", name_contains="Tren nocturno"), 11),
         "Reproductor"),
    item("files-open-txt", "Archivos", "abrir el archivo de notas de viaje",
         FILES, act(sem("row", name_contains="notas_viaje"), 11), "Archivos"),
    item("files-search", "Archivos", "buscar en los archivos",
         FILES, act(sem("text_field", name="Buscar"), 5), "Archivos"),
    item("files-new-folder", "Archivos", "crear una carpeta nueva",
         FILES, act(sem("button", name="Nueva carpeta"), 4), "Archivos"),
    item("download-wait", "Descargas", "esperar a que termine la descarga",
         DOWNLOAD, route("wait"), "Descargas"),
    item("notes-admin-absent", "Bloc", "abrir el panel de administración",
         NOTES, route("abstain"), "Bloc"),
    item("notes-new", "Bloc", "crear una nota nueva",
         NOTES, act(sem("button", name="Nueva nota"), 3), "Bloc"),
]


def main():
    out = os.path.join(HERE, "items.jsonl")
    with open(out, "w") as f:
        for it in ITEMS:
            f.write(json.dumps(it, ensure_ascii=False) + "\n")
    print(f"wrote {len(ITEMS)} items to {out}")


if __name__ == "__main__":
    main()
