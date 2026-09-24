# CUA driver + Hermes Agent vs Dexter — research comparativo

> Fecha: 2026-09-24 · Fuentes: primarias únicamente (cua.ai/docs, github.com/trycua/cua,
> hermes-agent.nousresearch.com/docs, github.com/NousResearch/hermes-agent).
> Cada claim lleva su URL. Lo no encontrado se marca explícito.

## TL;DR

- **cua-driver** (`trycua/cua`, MIT, ~26k stars, `cua-driver-rs v0.23.2`) es el driver
  de referencia: background-input real (SkyLight SPIs en macOS), percepción
  siempre-doble (árbol + screenshot), `verify_state` tri-estado, recording como
  flywheel de entrenamiento, y un ledger empírico por OS/toolkit.
- **Hermes Agent** (Nous Research, MIT) **no tiene driver propio**: su computer-use
  es un wrapper-skill sobre cua-driver + una disciplina documentada
  (verify→escalate ladder, stale-token refusal, capture_after) + un learning loop
  real (skills desde experiencia, curator, background review, FTS5).
- **El enfoque BERT de Dexter (Laya: ModernBERT + decision transformer) no existe
  en ninguno de los dos.** Ellos hacen grounding con VLM grande + SOM/índices, o
  con grounders screenshot→coordenadas (GTA1). La apuesta micro-decisión local es
  diferenciador genuino de Dexter — pero hoy corre sobre un loop que descarta
  verificación por paso y se auto-etiqueta. El BERT decide bien; el cuerpo no le
  lleva los ojos ni le anota los fallos.

---

## 1. CUA driver — lo que hace

### 1.1 Background input de verdad (no-foreground contract)
- Doctrina "best-effort background": no mover puntero real, no elevar ventanas,
  no cambiar frontmost. Ladder: 1) `element_index` → 2) `x,y` del mismo
  screenshot → 3) reintento con `delivery_mode:"foreground"`.
  https://cua.ai/docs/concepts/the-no-foreground-contract
- macOS: `SLEventPostToPid` (SkyLight SPI privado, aceptado por el filtro del
  renderer de Chrome donde `CGEvent.postToPid` se dropea), `SLPSPostEventRecordTo`
  ×2 (foco sin elevar, patrón yabai), `_AXObserverAddNotificationAndCheckRemote`
  (mantiene AX de Electron vivo tapado), decoy-click en (-1,-1) para el gate de
  user-activation de Chromium. Clicks por índice disparan `AXPerformAction`
  directo — funcionan ocultos/tapados.
  https://cua.ai/blog/inside-macos-window-internals
- Fallo estructurado, jamás silencioso: `background_unavailable` /
  `background_occluded` + `escalation:{recommended:"foreground"|"px"|"page"}`.
  Hard-fails conocidos y declarados: Blender GHOST/Unity/juegos (activación
  frontal breve), right-click sintético en Chromium coercionado a left-click.
  https://cua.ai/docs/concepts/capture-and-delivery-modalities
- Windows: UIA Invoke + `PostMessage`; modifier en background **dropeado**
  (requiere rung foreground/SendInput). Linux: AT-SPI `Action.DoAction` sin foco;
  Wayland sin raw-background universal (negación explícita, no éxito falso).
  https://cua.ai/docs/reference/cua-driver/platform-support

### 1.2 Percepción siempre-doble + verificación del driver
- `get_window_state` devuelve **árbol AX + screenshot juntos**; el viejo
  `capture_mode` se retiró. Knobs de costo: `include_screenshot:false` (re-index
  rápido), `max_elements/max_depth/query`, `screenshot_out_file`.
  https://cua.ai/docs/concepts/capture-and-delivery-modalities
- Señal degradada: `degraded:true` + `degraded_reason` (walk hecho, cero
  elementos accionables) → actuar por px sobre el mismo screenshot.
- Driver verifica lo que AX puede releer: `verified:true` solo con read-back;
  `effect: confirmed|unverifiable|suspected_noop`. `verify_state` con predicados
  AND → `satisfied|unsatisfied|unknown` (**unknown jamás es éxito**).
- Recording escribe por turno `before/after.png` + `before/after_state.json` +
  `action.json` + `click.png` + `evidence.json` (+mp4 opcional);
  `replay_trajectory` re-despacha por el mismo path (workflow de regression-diff).
  Las trazas de bench se declaran "usadas como training data" (oracle→BC).
  https://cua.ai/docs/reference/cua-driver/mcp-tools

### 1.3 Vocabulario + ledger empírico + contract-first
- 56–60 tools según OS: click, double/right/middle, drag, mouse down/up,
  parallel drag, scroll, zoom, type, press_key, hotkey, set_value, clipboard,
  invoke_menu, set_window_frame, browser typed tools, verify_state, sesiones,
  recording, health_report. https://cua.ai/docs/reference/cua-driver/mcp-tools
- `docs/action-support.md` + `test-matrix.md`: 40 celdas de evidencia por harness
  app (120 en macOS con WKWebView); cada celda prueba AX/PX × background/
  foreground; la negación solo pasa con código exacto + oráculos no-focus/
  no-z-order/no-leak.
  https://github.com/trycua/cua/blob/main/libs/cua-driver/docs/test-matrix.md
- `contract/` generado desde el crate Rust `cua-driver-contract` (source of
  truth); UniFFI genera SDKs Python/TS; gate CI `schema_consistency_test`
  (portable ⊆ nativo por OS). https://github.com/trycua/cua/blob/main/libs/cua-driver/contract/README.md
- Higiene de índices: el mapa se reemplaza por snapshot; `stale_element_token`
  → re-snapshot, nunca píxeles ciegos; pid multi-ventana ambiguo →
  `ambiguous_window_target` + candidatos, cero input enviado.

### 1.4 Policy fuera del modelo (parecido a Dexter, más capas)
- Modos fijados en arranque (el agente no los ensancha): `standard`, `bounded`
  (manifest requerido, deny-by-default), `unrestricted` (requiere flag explícito).
  Stack: invariantes duras → risk map → managed (YAML o Rego) → user → perfil →
  manifest → launch grant. https://cua.ai/docs/reference/cua-driver/permission-modes
- Telemetría default-on pero content-free (sin prompts/args/texto/screenshots).

### 1.5 Grounding: VLM grande + SOM, o grounder especialista (NO hay BERT)
- cua-agent: `ComputerAgent(model="<provider>/<name>")` sobre LiteLLM (100+
  providers); loops Generate-Execute-Repeat; `max_retries` 3.
- **GTA1** (Salesforce, Apache-2.0, Qwen2.5-VL + GRPO que premia clicks
  exitosos): screenshot→coordenadas `(x,y)`; 7B: 93.4 ScreenSpot-V2 / 55.5 Pro.
  CUA compone `"<grounding>+<planner>"` (p.ej. GTA1-7B + GPT-4o): el grounder
  emite coordenadas, el planner razona. https://cua.ai/blog/composite-agents
- Zoo: UI-TARS, OpenCUA, Holo1.5, moondream3, OmniParser (SoM pixel-detection
  para cualquier LLM). Local probado: Muse Glimmer 30B GGUF + allowlist +
  `max_elements=25` (71k→12k tokens).
- **Scorer micro-decisión tipo BERT dentro de CUA: no encontrado en fuentes
  primarias.** https://cua.ai/docs/how-to-guides/driver/run-with-local-model

---

## 2. Hermes Agent — lo que hace (wrapper + disciplina + learning loop)

### 2.1 El wrapper-skill es el producto
- Un solo `computer_use(action=…)` sobre cua-driver MCP; vocabulario propio
  (`capture/click/double/right/middle/drag/scroll/type/key/set_value/wait/
  list_apps/focus_app`), NO el crudo del driver. `#N` es el único handle;
  el wrapper mapea índice→`element_token` opaco por snapshot: click con snapshot
  viejo se rehúsa `stale` en vez de mis-clickear.
  https://raw.githubusercontent.com/NousResearch/hermes-agent/main/skills/autonomous-ai-agents/computer-use/SKILL.md
- "Click by element index is the single most important habit… Claude was trained
  on both; other models are often only reliable with indices."
- Modos: `som` (screenshot + lista indexada, default visión), `vision` (píxeles
  puros → coordenadas), `ax` (solo lista, modelos sin visión). Sin overlay
  quemado: la lista de índices ES el mapa; "ground on both and cross-check
  (the tree lies on some surfaces)".
- `capture_after=True` en todas las acciones (post-captura inline, ahorra
  round-trip). `bring_to_front` NO es propiedad de la acción: invoca una focus
  tool separada con su propia aprobación.
- `browser_*` separado: `computer_use` es desktop-only (chrome del browser:
  address bar, diálogos nativos); el DOM va por toolset aparte. Igual que la
  intuición de Dexter de separar browser/WebDriver — pero con la línea trazada
  en el contrato, no en el driver.

### 2.2 Verify→escalate ladder (la disciplina que Dexter no tiene escrita)
- Loop canónico: capture → click-por-índice → verify (re-capture o inline).
- Veredicto estructurado por acción: `effect: confirmed|unverifiable|
  suspected_noop` + `escalation:{recommended:px|foreground}` + `code`.
- Ladder (reaccionar, nunca predecir): elemento+background → verificación fresca
  ante `unverifiable` → píxel-background ante `suspected_noop`/hint/degraded →
  foreground con restore de foco → KDE/Qt verified-lost: parar tras UN round
  trip y usar terminal/archivo/DBus. Anti-patrones: no reintentar el mismo rung
  en silencio; efecto confirmado no se duplica.
- Matriz de troubleshooting síntoma→causa+remedio (9+ entradas) + `type` con
  hard-block patterns (`curl|bash`, `sudo rm -rf`, fork-bombs) + passwords solo
  por autofill del OS.

### 2.3 Approvals con granularidad bg-vs-fg
- Grants `cua:<acción>:<background|foreground>`; grant background jamás cubre
  foreground; headless (cron/unattended) se rehúsa, no se auto-aprueba; timeout
  fail-closed. Manifest bounded con apps/orígenes/tools; faltante = fallo ruidoso
  en arranque, nunca downgrade silencioso.
  https://hermes-agent.nousresearch.com/docs/user-guide/features/computer-use
  https://hermes-agent.nousresearch.com/docs/user-guide/security

### 2.4 Any-model + eficiencia de tokens
- "Works with any tool-capable model… no Anthropic-native schema": ecualizador
  SOM por índices + transporte de imagen normalizado (`image_url` OpenAI-style,
  adaptado a bloques nativos Anthropic) + fallback `auxiliary.vision` o modo
  `ax` para modelos sin visión. Local vLLM/LM Studio/Ollama soportado.
- Evicción de screenshots (~30K tokens por 20 acciones, no ~600K).

### 2.5 Learning loop real (lo que el flywheel de Dexter debería ser)
- Skills desde experiencia (`skill_manage`, `~/.hermes/skills/`): el system
  prompt pide registrar workflows no-triviales, errores/dead-ends, correcciones
  del usuario. Forma: procedimiento + comandos que funcionan + pitfalls
  (regla + mecanismo) + verificación.
- Background post-turn review fork (modelo barato `auxiliary.background_review`)
  guarda/actualiza memoria+skills; curator con prune determinístico
  (`active→stale(14d)→archived(30d)`) + consolidación LLM opt-in + telemetría de
  uso + ledger con rollback (`hermes curator … rollback`).
- Recall FTS5 sobre `state.db` sin LLM (1–2ms), `MEMORY.md/USER.md`, Honcho para
  modelado de usuario, `hermes journey` (timeline de skills+memorias).
- Los procedimientos de apps aprendidos viven en skills, no en memoria; las
  filas de troubleshooting son el corpus semilla.
  https://hermes-agent.nousresearch.com/docs/user-guide/features/skills
  https://hermes-agent.nousresearch.com/docs/user-guide/features/curator
  https://hermes-agent.nousresearch.com/docs/user-guide/features/memory
- Long-running: cron+gateway, Bot Screen (VNC por perfil con takeover lease
  humano), keep-awake.
- Grounding/VLM dedicado o scorer BERT en su path: **no encontrado en fuentes
  primarias**. Grounding = modelo de visión principal + lista SOM.

---

## 3. Qué hacen ellos que Dexter pasa por alto (rankeado por leverage)

| # | Qué | Quién | Por qué duele en Dexter | Costo |
|---|---|---|---|---|
| 1 | Percepción siempre-doble (árbol+screenshot, cross-check "the tree lies") | cua-driver | Dexter captura screenshots que jamás entran a la decisión (review #4) | M |
| 2 | Veredicto `effect/escalation/code` + ladder ordenado | ambos | `run_task` suelta el verify por paso (review #6); el ladder convierte "no pasó nada" en camino recuperable | S–M |
| 3 | Stale-token refusal (índice válido un snapshot) | Hermes | Mis-grounding silencioso en el path semántico; win fail-closed barato | S |
| 4 | `capture_after` inline + evicción de tokens (~30K/20 acts) | Hermes | Sin esto, píxeles-en-loop sale carísimo; con esto es viable | M |
| 5 | Recording `turn-*` (before/after state+png, action.json, mp4) + replay = datos de entrenamiento | cua-driver | Reemplaza rows auto-etiquetadas + harvester faltante (review #7) | M |
| 6 | Ledger empírico por OS/toolkit + test-matrix con oráculos no-focus/no-leak | cua-driver | Los fallos que deben disparar OCR/retry son hoy los menos cubiertos (review #9) | M |
| 7 | Grants bg-vs-fg separados; background jamás cubre foreground | Hermes | El tier physical de Dexter es binario; la granularidad evita pedir de más o regalar de más | S |
| 8 | `bring_to_front` como tool separada con aprobación propia | Hermes | En Dexter, foco es propiedad de la acción; separarlo hace el costo visible | S |
| 9 | Background-review fork + curator con rollback (live-fallo→skill) | Hermes | Es la mitad "self-improving" creíble; Engram ya tiene FTS5 del lado dev | M |
| 10 | Contract-first + `schema_consistency_test` CI | cua-driver | Fija la triplicación resolve+act y el drift de `Target::Window` en compile-time (review #1–2) | M |
| 11 | Troubleshooting matrix + `doctor` con health_report por check | ambos | Fallos de computer-use son ambientales; la matriz baja triage de horas a minutos | S |
| 12 | Browser/DOM como toolset separado con contrato propio | Hermes | La línea browser-vs-desktop de Dexter vive en el driver, no en el contrato | S |

## 4. El ángulo BERT (nuestra apuesta, validada por ausencia)

Ninguno tiene un scorer micro-decisión pequeño/local: CUA apuesta a
VLM-grande-o-grounder-especialista (GTA1 7B+), Hermes al modelo principal +
SOM. Implicancia: **no competir en grounding por coordenadas** (ahí ya ganan
ellos con datos y escala); el BERT de Dexter gana donde ellos son débiles —
micro-decisiones sobre estado estructurado (¿tuvo efecto? ¿ambigüedad? ¿abstener?
¿qué recovery?) con calibración y costo ~0. Pero eso exige exactamente lo que
falta: verificación por paso que genere labels reales (#2), recording como
materia prima (#5) y harvester de fallos (#9). Sin eso, Laya imita reglas con
confianza apagada.

## 5. Desconocidos abiertos (fuentes primarias mudas)

- Firmas exactas de los SPIs SkyLight (solo prosa en el blog).
- Contenido celda-por-celda de `action-support.md`; catálogo de checks de
  `health_report` y thresholds de `doctor`.
- Manejo de passwords/secretos dentro del driver (solo guías de sandbox + agent).
- Números de latencia/accuracy SOM-vs-coords por modelo; datos de entrenamiento
  de GTA1.
- Si los skill-writes del background-review produjeron alguna vez
  procedimientos de apps (sin conteos publicados).
