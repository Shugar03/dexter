# Dexter — Roadmap

> **Dexter — the hands and eyes of AI agents.**
> Runtime local, nativo y agnóstico de modelos para que agentes de IA operen
> computadoras reales de forma semántica, verificable y recuperable.

Estado: **pre-release (v0.0.x)** — construyendo el vertical slice.

Principios que no cambian en ninguna etapa:

- **semantic-first**: píxeles solo como fallback, nunca como interfaz primaria
- **verification-first**: toda acción relevante define un estado esperado y lo comprueba
- **fail-closed**: ante lo desconocido, detenerse o pedir aprobación — nunca simular éxito
- **model-agnostic / driver-agnostic**: toda dependencia externa detrás de un trait
- **local-first**: control, credenciales y datos permanecen locales por defecto

---

## Etapa 0 — Fundaciones + vertical slice (macOS)

**Objetivo:** demostrar `Observe → Act → Verify → Recover` de forma fiable sobre
una app real de macOS, con el seam de Laya presente pero no load-bearing.

**Entregables:**

- Workspace Rust: `crates/{core,driver,world-model,verify,policy,decision,engine}`, `drivers/macos`, `apps/dexter`
- `MacOSDriver`: enumeración de ventanas (CGWindowList), árbol de accesibilidad (AXUIElement), input (CGEvent), screenshots (ScreenCaptureKit/xcap), chequeo de permisos
- World Model: elementos normalizados + digest textual (diseñado como `state` para decision engines)
- Acciones semánticas: click por rol/nombre, type, key chords, scroll, focus, set-value
- Verifier: `ExpectedState` → `Verified / Failed / Uncertain` (Uncertain nunca pasa)
- Policy engine mínimo (TOML, fail-closed, `require_approval`)
- `DecisionEngine` trait con `predict(state, questions)` tipado (choice/score/noul) + `RuleBased` + cliente `LayaSidecar` (HTTP loopback, opt-in)
- `workers/laya/`: sidecar Python (uv script) exponiendo `POST /predict` sobre `laya-mlx`
- Engine: loop por pasos con retry acotado + re-observación, event log JSONL
- CLI `dexter`: `doctor`, `windows`, `observe`, `click`, `type`, `key`, `scroll`, `run`, `decide`
- Escenarios TOML de ejemplo (app controlada: TextEdit)
- CI: fmt + clippy + test + build en runner macOS
- LICENSE-MIT + LICENSE-APACHE, README

**Criterio de salida (spec §34):** Dexter detecta una ventana, inspecciona su AX
tree, representa elementos en el World Model, localiza por semántica, ejecuta,
observa el nuevo estado, verifica, informa fallo si no ocurrió, reintenta de
forma acotada — todo expuesto por CLI, con el runtime independiente de cualquier UI.

**Riesgo a medir aquí:** calidad/completitud del AX tree por app (Electron,
apps custom), fricción de permisos, límites reales del input en background.

---

## Etapa 1 — Hardening + MCP

**Objetivo:** convertir el slice en un runtime usable por agentes reales y
publicar la primera release.

- Servidor MCP (`rmcp`, stdio): `computer.observe`, `click`, `type`, `scroll`, `press`, `windows`, `execute_task` — nunca salta Policy ni Verification
- Recovery ladder pasos 1–3: retry → refresh observation → alternative semantic target
- Event log completo + recording/replay de escenarios
- Onboarding de permisos pulido (`doctor` + prompt del sistema)
- Repo público ordenado: SECURITY.md, CONTRIBUTING, issue/PR templates, changelog
- Release **v0.1.0**: binario macOS firmado + Homebrew tap, crates internos como `dexter-*` (binario `dexter` publicado como `dexter-cu`)

**Criterio de salida:** un agente MCP (Claude Code/Cursor/etc.) completa una
tarea real en una app macOS nativa, con aprobación humana en acciones sensibles.

---

## Etapa 2 — Superficie browser

**Objetivo:** DOM antes que píxeles — la superficie de mayor fidelidad.

- ~~Worker Playwright (sidecar Node)~~ → **hecho mejor**: `drivers/browser`
  habla W3C WebDriver REST directo (safaridriver built-in, chromedriver/
  geckodriver por `--browser-url`) — sin dependencia de Node
- ✅ Observación DOM normalizada al mismo `Element` (walker in-page:
  rol ARIA/tag, accessible name, bounds, acciones)
- ✅ DOM actions como `Mechanism::Dom` — background-safe real
- Pendiente: multi-tab/iframe flatten, `/actions` endpoint para casos
  que DOM-dispatch no cubre, sesiones protegidas (cookies/credenciales)

**Criterio de salida:** una tarea web completa sin usar coordenadas salvo fallback.

---

## Etapa 3 — Laya en profundidad

**Objetivo:** que la micro-decision layer demuestre valor medido, no asumido.

- ~~Eval harness~~ ✅ `crates/eval` + `dexter eval run|harvest`: replay
  offline de decision points etiquetados (goal + observación completa +
  gold teacher-authored), métricas separadas coverage/accuracy/routes —
  dataset browser de 21 páginas commiteado en `datasets/browser/`
- Candidates v2 ✅: parse verb+objeto, acciones variadas
  (click/focus/set_value/type_text), señales focused/delta/repeat
- Pendiente: dataset sobre árboles AX macOS reales (mismo harness,
  `eval harvest` ya es driver-agnóstico)
- Calibración de umbrales sobre datos reales; cascada reglas → Laya → LLM
- Usos concretos: detección de modal bloqueante, "¿la acción tuvo efecto?",
  resolución de targets ambiguos, detección de completitud
- Si los números lo justifican: fine-tune propio sobre el dataset

**Criterio de salida:** reducción medida de llamadas al LLM grande y latencia
por tarea, con accuracy reportada honestamente por tipo de pregunta.

---

## Etapa 4 — Windows

UI Automation + MSAA fallback + Win32. El premio enterprise: cubrir el
ecosistema donde hoy solo está Terminator. Electron/WPF/WinUI/legacy.

## Etapa 5 — Linux

AT-SPI + X11 primero; Wayland después, sin prometer cobertura universal.

## Etapa 6 — Desktop app + enterprise

Tauri 2 UI (sesiones, políticas, logs, replay, aprobaciones), policies
centralizadas, audit, fleet management, updates firmados.

---

## Fuera de scope (confirmado)

Modelo fundacional propio · navegador propio · OCR/VLM propio · cloud antes de
validar local · "parecer humano" · autonomía de horas · multi-agente.
