# SDD: Loop integrity — verify-every-act + resolve detrás del seam

> Status: aprobado · 2026-09-24 (D2=max_attempts 3, D3=recording off hasta medir,
> D4=orden 2→3; D1=Effect en core::result sin objeción)
> Slices: review #6 (verify-every-act) + #1 (seam resolve) del
> `architecture-review-20260924-004327.html`, con mecanismos prestados de
> `docs/research/cua-driver-code.md` (trycua/cua, MIT).
> Extiende `docs/sdd/verify.md` — no lo contradice: el tri-estado y la
> completeness rule quedan intactos; se les suma taxonomía de efecto,
> predicados AND y `UnknownReason`.

## 1. Problem (qué y porqué)

`Engine::run_step` **sabe** verificar: con `expect` hace settle → re-observe →
`verify` → retry acotado (`crates/engine/src/lib.rs:330-428`). Pero
`run_goal`/`run_plan` lo invocan con `expect:None, max_attempts:1`, y ese camino
retorna `Done{verification:None}` (`lib.rs:366-372`). El progreso se juzga solo
por `done_when` terminal o por `world_signature`, un hash que excluye a
propósito ids, bounds y screenshots (`lib.rs:900-929).

Consecuencias verificadas en código:

- No-ops silenciosos intermedios (click absorbido, scroll sin cambio, foco
  movido) no se detectan; solo cuentan al llegar a `MaxSteps`.
- `UNCERTAIN`/`FAILED` intermedios jamás reintentan: el retry existe pero el
  loop productivo no lo usa.
- Sin pruebas por paso, **toda métrica aguas abajo miente**: eval, training rows
  (`rows_from_events` se auto-etiqueta con decisiones no verificadas), futura
  calibración. Es el prerrequisito de todo el flywheel MLOps.
- En paralelo, la semántica de resolve/stale vive triplicada (macOS
  `actions.rs:111-234`, browser `lib.rs:159-224`, sim `lib.rs:139-207`) con
  reglas de identidad divergentes: si el verify por paso exige la misma
  semántica en los tres drivers, hay que consolidarla en el mismo slice.

## 2. Justificación del orden (lente CTO)

1. **#6 primero porque la evidencia alimenta todo lo demás.** Cada paso
   verificado produce el par (antes, acción, después, veredicto) que hoy no
   existe y que necesitan eval (#7 flywheel), recording (slice 5) y la futura
   calibración de Laya. Sin esto, cualquier modelo —propio o GTA1— se evalúa
   sobre ruido.
2. **#1 en el mismo slice porque el verify necesita una sola semántica.**
   Verificar "el elemento cambió" con tres definiciones distintas de
   stale/ambiguous por plataforma es verificar tres cosas distintas. El resolve
   compartido no es refactor estético: es el cimiento del verify.
3. **Mecanismos prestados, superficie propia.** cua-driver ya resolvió en
   batalla: taxonomía Effect, stale-tokens, verify_state AND, recording
   `turn-*`, serialización por pid. Se porta el mecanismo detrás de nuestro
   seam; no su superficie (60 tools, daemon, sesiones) — eso sería un módulo
   shallow con acoplamiento de mantenimiento.
4. **SPI background-input (SkyLight) explícitamente después.** Es capacidad
   (L), no correctitud. Entra tras flag de capability + probe en runtime, con
   fallback público. Primero lo correcto, después lo invisible.
5. **La apuesta BERT queda intacta y se vuelve entrenable.** Laya no se toca en
   este plan; se le construye el suelo: labels reales por paso en vez de
   imitaciónd de reglas.

## 3. Goals / Non-goals

Goals:

- Todo `run_goal`/`run_plan` deriva un `ExpectedState` mínimo por subgoal y lo
  verifica; `UNCERTAIN` dispara re-observe/retry/escalación, nunca avance
  silencioso.
- `Effect`/`Escalation` como vocabulario común driver→engine→journal.
- Resolve/stale/ambiguous idénticos en macOS/browser/sim, detrás del seam,
  con tests de contrato herméticos (vía sim, sin permisos macOS).
- Recording por paso en layout `turn-NNNNN/` apto como futura materia prima
  de entrenamiento.

Non-goals (confirmado):

- Modelo Reflex propio, VLM cloud, grounder por coordenadas (GTA1 se benchea
  post-#6, no acá).
- Background-input por SPI privado (slice futuro, tras capability flag).
- Windows/Linux, cambios en Laya/rules, cambios en policy TOML.
- `Target::Window` split y vocabulario Drag/Hover (slices del review #2/#3,
  no de este SDD).

## 4. Contract changes

### 4.1 `Effect` + `Escalation` (nuevo, `dexter-core::result`)

```rust
enum Effect { Confirmed, Unverifiable, SuspectedNoop, Refused }
struct Escalation { target: EscalationTarget /* Px | Foreground | Page | Session */,
                    reason: EscalationReason /* RouteUnavailable | DeliveryFailed
                                               | EffectUnconfirmed | SuspectedNoop
                                               | PermissionRequired */ }
```

Reglas de construcción (validadas en test): `Confirmed` exige evidencia;
`Refused` no admite delivery ni evidencia. El driver clasifica con la regla
mínima prestada: confirmado-con-readback → `Confirmed`; cambió o sin
read-back → `Unverifiable`; si no → `SuspectedNoop`. Ver
`docs/research/cua-driver-code.md` §3.

### 4.2 `ElementToken` opaco (driver macOS; browser/sim después vía seam)

`snapshot_uuid:index`, mapa adentro del driver (no en core). `act()` con token
de snapshot viejo → `StaleReference` (fail-closed, mensaje accionable:
"re-observá"). Índice pelado sin snapshot → rechazo. Ambigüedad multi-ventana →
candidatos y **cero input enviado**. (§2 del research.)

### 4.3 `ExpectedState` derivado por subgoal (engine)

Cada subgoal deriva un delta mínimo sobre la firma anterior (no igualdad
total): típicamente 1–3 predicados (`ElementValue`, `ElementExists`,
`FocusedElement`). `Observe`/`Wait` quedan exentos (`expect:None` legítimo).
`max_attempts` default > 1; `UNCERTAIN` → re-observe (1×) → retry → escalar a
recovery ladder existente (retry → refresh → alternative target).

### 4.4 Firma enriquecida (`world_signature`)

Suma bounds cuantizados (grid 8px, tolerante a Retina) + presencia de
screenshot + conteo por rol. Nota de compat: la firma se computa en vivo por
run para detección de cambio — cambiar su forma no rompe journals viejos
(guardan el u64, no la fórmula); los tests golden que la asuman se actualizan
en el mismo commit.

### 4.5 `verify` extendido (compatible con `verify.md`)

- Outcomes por predicado + `UnknownReason` (7 razones: las de cua-driver como
  referencia, podadas a las nuestras).
- Combinador AND de ≤ 8 predicados; la lógica tri-valuada de `All`/`Any`/`Not`
  ya existente se reutiliza sin cambios.
- Solo `Satisfied` avanza el plan. La completeness rule (`elements_truncated` /
  `ax_limited` → `UNCERTAIN`) sigue mandando.

### 4.6 Seam `ComputerDriver` (profundizar, no ensanchar)

- `resolve` compartido como función provista del crate `dexter-driver`
  (identidad, stale-check, `NotFound` truncation-aware); los adapters aportan
  primitivas walk/press/dispatch.
- `observe()` rechaza scope irresoluble (ventana ajena) en vez de devolver el
  árbol vecino (scope refusal).
- Mutex por target retenido durante gather→act→verify (serialización; evita
  gestos partidos y races de foco).

### 4.7 Recording `turn-NNNNN/`

Por paso: `action.json` (tool, args sin internos, result summary/error,
timestamps relativos), `before/after_state.json`, `before/after.png` cuando hay
screenshot, `evidence.json` (captured/unavailable/not_applicable por
artefacto). Sin video/cursor/zoom. Que el índice no sobreviva al replay es
feature y se documenta.

## 5. Slices + TDD (cada uno shippable y verde por separado)

### Slice 0 — Taxonomía Effect/Escalation (S)
- **Rojo:** `crates/core/tests/contract.rs` (o `result.rs` test): construir
  `ActionResult{effect: Confirmed, evidence: None}` debe fallar (panic o
  `Result::Err` según API elegida); `Refused` con delivery debe fallar;
  clasificador sobre (changed, readback_available) → tabla de 4 casos.
- **Verde mínimo:** enums + constructor validado + clasificador puro (~60
  líneas). Sin tocar drivers ni engine.
- **Verificación:** `cargo test -p dexter-core`, clippy, fmt.

### Slice 1 — Expect derivado + retry UNCERTAIN-aware + firma (M, corazón del #6)
- **Rojo:** `crates/engine/tests/e2e.rs` (vía sim): subgoal cuyo acto es no-op
  (sim lo soporta: acto que no muta) debe terminar `Failed` con evidencia de
  verify, no `Completed` por `MaxSteps`; subgoal con cambio real completa en
  1 intento con `verification: Some(Verified)`. Golden de `world_signature`:
  mover bounds 3px no cambia la firma; 20px sí.
- **Verde mínimo:** derivación de delta (1–3 predicados) en `run_goal`,
  `max_attempts` default 2–3, rama `UNCERTAIN → re-observe → retry → escalate`,
  firma con bounds cuantizados + screenshot bit. `Observe`/`Wait` exentos.
- **Verificación:** e2e sim hermético (sin permisos macOS), `cargo test
  --workspace`, clippy `-D warnings`.

### Slice 2 — Element-token stale model en macOS (M)
- **Rojo:** `drivers/macos` tests herméticos donde haya harness (o nuevo
  `tokens.rs` test con mapa inyectado): act con token de snapshot anterior →
  `StaleReference`; índice sin snapshot → rechazo; pid multi-ventana →
  candidatos sin input.
- **Verde mínimo:** mapa snapshot en el driver + `ElementToken` opaco +
  rechazos. Sin cambiar el wire MCP todavía (mapeo interno).
- **Verificación:** tests herméticos + e2e sim intacto.

### Slice 3 — Resolve compartido detrás del seam (M, corazón del #1)
- **Rojo:** test de contrato en `crates/driver`: los tres drivers (macOS tras
  refactor, browser, sim) resuelven el mismo árbol ambiguo/stale idéntico —
  tabla paramétrica driver×caso con veredicto esperado.
- **Verde mínimo:** `resolve` provisto en `dexter-driver`; adapters delegan;
  se borra el código triplicado (deletion test: el fix vive en un lugar).
- **Verificación:** matriz de contrato verde en los tres drivers + workspace
  verde. Este slice cierra con `git log` mostrando líneas netas negativas en
  `drivers/*/actions`.

### Slice 4 — Serialización + scope refusal (S)
- **Rojo:** dos acts concurrentes al mismo target en sim no se intercalan
  (test de interleaving); `observe` con ventana de otro pid → error de scope,
  no árbol ajeno.
- **Verde mínimo:** mutex por target + refusal en `observe()`.
- **Verificación:** test de concurrencia hermético + e2e.

### Slice 5 — Recording `turn-*` (M)
- **Rojo:** tras `run_task` de 2 pasos en sim existe `turn-00000/turn-00001/`
  con `action.json + before/after_state.json + evidence.json` y schema
  validado (serde round-trip); replay documentado como no-apto para índices.
- **Verde mínimo:** writer detrás de `crates/engine` (feature-gated por env o
  config, default on en tests, off/liviano en prod hasta medir costo).
- **Verificación:** e2e + inspección manual de un directorio real.

### Después (fuera de este SDD, registrado para no perder)
- Slice 6: background-input Chromium/SPI tras capability flag + probe (L).
- Slice 7: test de subset de schemas portable⊆driver (S).
- Review #2 (Window split) y #3 (Drag/Hover): SDDs propios.

## 6. Verificación global del plan

```sh
cargo test --workspace          # hermético, sin permisos macOS
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo fmt --all
```

Más: nuevo escenario eval `datasets/scenarios/silent-noop.toml` que falle
pre-slices y pase post-slice-1 (el test rojo a nivel eval); métricas a mirar:
tasa de `MaxSteps` sin veredicto, `false_acts` en replays, líneas netas en
`drivers/*` (slice 3 debe restar).

## 7. Riesgos

- **Derivación de deltas demasiado estricta** → falsos `FAILED` en UIs con
  timing raro. Mitiga: tolerancia en bounds (grid 8px), `stable_samples`-style
  re-chequeo antes de fallar, y `Unverifiable → must-verify` en vez de fail
  directo.
- **Firma enriquecida y Retina**: bounds crudos harían flapping. Mitiga: grid
  grueso + excluir `x,y` exactos (solo celda).
- **Tokens y MCP**: el wire actual expone índices; el token vive interno hasta
  que el wire se versione. Mitiga: mapeo interno en slice 2, wire en SDD
  aparte.
- **Recording y costo**: PNGs por paso pesan. Mitiga: gate por config,
  `screenshot_out_file`-style pin a disco, medir antes de defaultear on.
- **SPIs**: ni se tocan en este plan (ver §2.4).

## 8. Decisiones (decididas 2026-09-24, valores recomendados aprobados)

- **D1.** `Effect`/`Escalation` en `dexter-core::result` (recomendado: viajan
  con `ActionResult`, leverage para MCP/CLI) vs `dexter-verify`.
- **D2. Decidido: `max_attempts` default = 3** (cubre flake de settle en
  Electron; se acepta más latencia cuando falla).
- **D3. Decidido: recording default off hasta medir** (on en tests; en prod se
  mide costo de PNGs antes de defaultear).
- **D4. Decidido: orden 2→3** (el mapa de tokens nace donde el resolve
  compartido lo va a reubicar; evita rework).

## 9. Trazabilidad

- Review: `architecture-review-20260924-004327.html` (candidatos #6 Strong,
  #1 Strong; top recommendation = este plan).
- Research: `docs/research/cua-hermes-vs-dexter.md` (qué adoptamos),
  `docs/research/cua-driver-code.md` (mecanismos §1–§8 con file:line).
- SDD previo: `docs/sdd/verify.md` (contrato base intacto).
- Invariantes AGENTS.md que este plan honra: no simular éxito (scope refusal,
  stale refusal), fail-closed, `Engine::run_step` único, `Target` serde
  untagged intacto.
