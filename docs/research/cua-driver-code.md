# cua-driver: lectura de código acotada → recetas de port a Dexter

> Fecha: 2026-09-24 · Fuente: `github.com/trycua/cua` (MIT — leer/aprender/portar
> con atribución es legal), paths `libs/cua-driver/rust/`, `contract/`, `docs/`.
> Base de citas: `https://github.com/trycua/cua/blob/main/libs/cua-driver/`.
> Mapea a candidatos del review: #1 (seam), #2 (Window), #6 (verify), #9 (tests).

## 1. macOS background input

Todo SPI privado se resuelve lazy vía `dlopen(SkyLight)+dlsym(RTLD_DEFAULT)`,
cacheado en `OnceLock<Option<Fn>>`; si falta → fallback público, nunca crash:

```rust
libc::dlopen(path.as_ptr() as *const c_char, libc::RTLD_LAZY | libc::RTLD_GLOBAL);
let ptr = unsafe { libc::dlsym(libc::RTLD_DEFAULT, name.as_ptr() as *const c_char) };
```

`rust/crates/platform-macos/src/input/skylight.rs:96-140`. Handle de
`SLEventPostToPid`: `:130-133`; `SLPSPostEventRecordTo` +
`is_focus_without_raise_available()`: `:205-235`.

Post entry: retorna `false` si el SPI falta → fallback a `CGEvent::post_to_pid`:

```rust
pub(super) fn post_to_pid(pid: pid_t, event_ptr: *mut c_void, attach_auth_message: bool) -> bool {
    let post_fn = match post_to_pid_fn() { Some(f) => f, None => return false };
```

`skylight.rs:315-330`. Teclado pasa `attach_auth_message=true` (path Chromium,
`SLSEventAuthenticationMessage` con guard macOS-14 `class_respondsToSelector`,
#1503); mouse pasa `false`: `input/keyboard.rs:591-600`, `:620-631`.

Focus-without-raise (`activate_without_raise`, portado de yabai): captura PSN
frontal → PSN target vía `SLSGetWindowOwner+SLSGetConnectionPSN` (fallback
`GetProcessForPID`) → record de 248 bytes de defocus (`buf[0x8a]=0x02`) al PSN
frontal → record de foco (`0x01` + wid LE en `0x3c..0x3f`) al PSN target.
Saltea a propósito `SLPSSetFrontProcessWithOptions` para no cerrar el gate de
user-activation de Chromium: `skylight.rs:491-555`. Clicks por píxel lo llaman y
duermen 50ms: `input/mouse.rs:421-429`.

Receta de click Chromium (5 eventos, un `click_group_id` = `subsec_nanos`, campos
f0/f1/f3/f7/f40/f51/f58/f91/f92 + `CGEventSetWindowLocation`): `mouseMoved`@target
→ down/up primer en **(-1,-1)** (f0=1/2, satisface user-activation sin tocar DOM)
→ down/up en target (f0=3/3): `mouse.rs:431-520` (off-screen en `:483`), loop de
post `:608-640` (intento SkyLight, luego **incondicional** `post_to_pid` público —
`MousePostMode::Both`, `:1473-1479`).

Clicks por elemento van directo a AX, sin eventos: `AXUIElementPerformAction`
tras re-chequeo vivo de `AXEnabled`:

```rust
let err = unsafe { perform_action(element_ptr as AXUIElementRef, ax_action) };
if err == kAXErrorSuccess { Ok(()) } else { anyhow::bail!(...) }
```

`input/ax_actions.rs:220-230`, mapa `press→AXPress/show_menu→AXShowMenu/
pick→AXPick…` en `:235-243`. AX de Electron tapado: `enable_chromium_accessibility`
setea `AXManualAccessibility=true`, fallback `AXEnhancedUserInterface`, luego
0.5s de settle de run-loop una vez por vida del pid: `ax/bindings.rs:670-700`,
`ax/enablement.rs:64-85`, `ax/tree.rs:214-223`.

**Port a Dexter.** Todo detrás del seam `ComputerDriver`: nuevo
`drivers/macos/src/background.rs` con `post_to_pid`, `activate_without_raise` y
planificador de ruta Chromium; gatear cada act background en
`crates/driver/src/lib.rs` con un trait `BackgroundGate` espejando su
`decide_background_input`. No copiar su superficie de tools — solo el transporte.

**Riesgos.** `SLEventPostToPid` / `SLPSPostEventRecordTo` /
`SLEventSetAuthenticationMessage` / números de campo de `CGEventSetWindowLocation`
son SPIs privados: rompibles en cada release de macOS, requieren ad-hoc signing +
TCC Accessibility + ScreenRecording. Dual-post (Both) puede entregar doble en
targets AppKit. El stamp de campos-f (f40 filtro de pid, f58 grupo) es
comportamiento Chromium reverse-engineereado.

## 2. Modelo element-token / stale

Token = `s{snapshot:08x}:{index}` (`element_token.rs:14-18`); `snapshot_id` de
`AtomicU32` global del proceso (`:10-12`); LRU por pid cap 8 (`:4`). Mapa de
índices por walk DFS (`ax/tree.rs:192`, `element_index` en `:492-563`),
publicado wholesale reemplazando la entrada del lane (`element_cache.rs:236-283`).
Resolve: `parse_element_args` **rehúsa `element_index` pelado**
(`snapshot_id_required`) y `snapshot_id` sin índice; desacuerdo
token/índice/snapshot/ventana → `conflicting_element_target`; snapshot id
desconocido → `stale_element_token` ("llamá get_window_state de nuevo");
fuera de rango → `invalid_element_token`: `element_token.rs:98-140`,
`element_cache.rs:512-585`. Llamadas solo-pid pasan por
`PidOnlyWindowTargetGuard`: 0 candidatos → `window_target_not_found`, 1 →
promueve a `(pid, window_id)` exacto, N → `ambiguous_window_target` +
`candidates[]` y **retorna antes de `inner.invoke`** (cero input enviado):
`cua-driver-core/src/window_target.rs:106-160`.

**Port a Dexter.** `observe()` en `crates/driver/src/lib.rs` devuelve
`ElementToken`s opacos (`snapshot_uuid:index`) y el mapa vive dentro del driver
macOS (no en core). `act()` resuelve-o-rehúsa con `stale` antes de tocar AX.
Es el trasplante de mayor valor: mata clicks TOCTOU.

**Riesgos.** Token atado al triple `(pid, window_id, snapshot_id)` — el replay
entre sesiones falla por diseño. LRU=8 churnea tokens si se alterna mucho entre
ventanas.

## 3. Taxonomía effect / escalation

Enums cerrados en el contract (wire): `ActionEffect {Confirmed, Partial,
Unverifiable, SuspectedNoop, Refused}`, `ActionEscalation{target:
Pixel|Foreground|Page|Session, reason: RouteUnavailable|DeliveryFailed|
EffectUnconfirmed|SuspectedNoop|PermissionRequired}`, `ActionResult{effect,
route, delivery?, evidence?, escalation?}` con validación
(`ConfirmedRequiresEvidence`, `RefusedCannotHaveDelivery/Evidence`):
`cua-driver-contract/src/outputs.rs:466-552`, validador `:560-610`. Verdad
interna (sin serde) + clasificador:

```rust
if confirmed && readback_available { Confirmed }
else if changed || !readback_available { Unverifiable } else { SuspectedNoop }
```

`cua-driver-core/src/action_record.rs:20-31`. Click: `suspected_noop =
!advertised.contains(ax_action)` (`platform-macos/src/tools/click.rs:1500-1502`)
→ `effect: suspected_noop + escalation{recommended: px}`; selección verificada →
`confirmed + evidence[accessibility_readback]`; si no, `unverifiable`: `:756-810`.
Refusals con forma `code + effect: refused + escalation{recommended, reason}`,
sin actuador: `tools/mod.rs:187-215`. Códigos vistos en macOS:
`background_unavailable` (clicks con modifier, `click.rs:480-500`),
`desktop_scope_disabled` (`:293-299`),
`window_not_found/owner_pid_mismatch/off_space_or_ax_unresolved/…`.

**Port a Dexter.** Copiar la *taxonomía*, no el plumbing: enums
`Effect`/`Escalation` en `crates/verify` (o core types), `confirmed⇒evidence`
forzado en construcción, y `crates/engine` trata `unverifiable` como "debe
verificar", `suspected_noop` como "re-planear ruta". Esfuerzo S.

**Riesgos.** `confirmed` solo vale lo que valga el read-back (su
`TargetBoundVerification::accepts_evidence_from` exige evidencia de la misma
ventana — portar ese check también o `confirmed` miente).

## 4. Semántica de `verify_state`

`VerifyStateInput{pid, window_id, expect: Vec<StatePredicate>[1..8] AND,
timeout_ms≤10000, stable_samples≤5}`:
`cua-driver-contract/src/verification.rs:150-180`;
`VerificationStatus{Satisfied,Unsatisfied,Unknown}` + 7 `UnknownReason`s. El tool
posee la autorización durante toda la ventana de poll; por predicado ventana XOR
elemento (ambos/ninguno → `InvalidPredicate`); `element.exists=false` rechazado
(walks no exhaustivos → ausencia improbable); contador de satisfechos
consecutivos vs `stable_samples`, si no `StabilityUnproven`:
`cua-driver-core/src/expectation.rs:224-330`, `evaluate_predicates:383-408`,
regla de elemento `:495-512`. `unknown≠success` es texto de contrato
("`satisfied` is the only successful terminal status"):
`docs/action-result-contract.md:80-85`. Ventana-vs-desktop: scope irresoluble
**rehúsa** (`window_id_not_found`, `window_owner_pid_mismatch` con remedio de
reintento) en vez de devolver el árbol vecino:
`platform-macos/src/tools/get_window_state.rs:741-800`.

**Port a Dexter.** Extender el tri-estado de `crates/verify` con outcomes por
predicado + `UnknownReason`, y la forma AND-de-≤8; `crates/engine` solo avanza
el plan con `Satisfied`. Portar `window_scope_refusal` al path `observe()` de
macOS.

**Riesgos.** La ventana de poll retiene autorización — acotarla (sus caps
10s/5-samples). Predicados de bounds necesitan tolerancia (default 1px) o el
flakiness Retina falla todo.

## 5. Formato de recording

`turn-{idx:05}/` reservado en `begin_turn` (monotónico aun desordenado):
`recording.rs:590-605`. Claves de `action.json`: `tool, arguments` (sin internos
`_`-prefijados), `result_summary, result_error, timestamp,
t_ms_from_session_start, t_start_ms_from_session_start, click_point?,
click_point_image?, action_truth?`: `:939-1040`.
`before_state.json/before.png` pre-dispatch, `after_state.json/after.png`
(+alias legacy) post-dispatch: `write_phase_artifacts:775-790`. `click.png` =
crosshair sobre la imagen **pre-acción** (nunca en turnos rehusados):
`:1045-1065`. `evidence.json` (`cua-turn-evidence/v1`) registra
captured/unavailable/not_applicable por artefacto: `:810-860`.
`replay_trajectory` re-invoca tool+args de cada `action.json` por dispatch vivo —
acciones por índice fallan (índices por snapshot; tools read-only no se graban y
el caché queda vacío); píxel/teclado replays limpios:
`recording_tools.rs:288-330` + invoke loop.

**Port a Dexter.** Adoptar el *layout* (`turn-NNNNN/action.json + before/after +
evidence.json`) para logs de trayectoria detrás de `crates/engine`; saltear su
pipeline de video/cursor/zoom. Que el índice no sobreviva es feature —
documentarlo.

**Riesgos.** `click_point` en clicks por elemento es best-effort. Grabar-mientras-
replayeas es deliberado (workflow de diff) — puede sorprender.

## 6. Gate contract-first

`cua-driver-contract` es source of truth del schema: cada input deriva
`JsonSchema` y `ToolInput::input_schema()` genera draft2020-12:
`inputs.rs:14-30`; registry `tool_contract(name)` +
`SchemaMode::{PortableSubset, CanonicalRuntime}` + `manifest()` generado:
`lib.rs:138-230`. Gate = `schema_consistency_test.rs`: (a) schemas vivos de
`tools/list` deben matchear el canon + clase de riesgo revisada + coherencia
`delivery_mode`↔capability; (b) **`portable⊆native` por OS** vía
`compatibility::schema_subset_violations` (`compatibility.rs:34-100`):
`cua-driver/tests/schema_consistency_test.rs:28-120, 119-220`. C ABI versionada
en `cua-driver-sdk/src/abi.rs:544-568`, header generado por bindgen.

**Port a Dexter.** Espejo mínimo: schemars derivados en inputs de tools en
`crates/driver` + un test que afirme schemas visibles ⊆ schemas del driver.
Saltear UniFFI/C-ABI por completo.

**Riesgos.** Su gate funciona porque un equipo dueña ambos lados; una copia sin
CI se pudre.

## 7. Modelo de sesión (liviano — NO portar)

El daemon dueña estado por sesión con `session_id` minteado por proxy; primer
dispatch la mintea implícita (`session.rs:447-500`); TTL idle 5 min (`:30`),
evicción solo con `in_flight==0`. Label público corto por call; runtime id
`__cua_runtime_<scope>:<public>`; `end_session` idempotente con cleanup diferido:
`:1510-1560`. `start_session` opcional.

**Port a Dexter.** No se necesita la capa daemon; alcanza el run-id propio de
`crates/engine` y, si acaso, un `session` string opcional por `run_step`.

## 8. Serialización de input

`AsyncMutex` por pid (`HashMap<i32, Weak<…>>`); el guard vive de gather a
restore+verify; `gate_again` re-valida otras clases de actuador bajo el mismo
lease: `platform-macos/src/background_mutation.rs:17-50`. Por qué down/drag/up
no se parten entre calls: (a) el lease + supresión/restore de foco envuelve
**una** call; (b) la coalescencia necesita un `click_group_id` compartido (f58,
minteado por call) y progresión de fase f0 (`mouse.rs:573-612`); (c) `drag_at_xy`
postea down→N interpolados→up con sleeps en una función (`mouse.rs:655-850).
Exponer `drag`/down+up solo como gestos atómicos single-call.

**Port a Dexter.** Mutex por target en el driver macOS (o `crates/driver`)
retenido durante gather→act→verify, más primitiva `drag` atómica en `act()`
(nada de tools down/up separadas). Esfuerzo S.

**Riesgos.** Granularidad por pid (no por ventana) serializa ventanas no
relacionadas de una app — correcto pero grueso.

## Prioridad de port (rankeada)

1. Taxonomía Effect + `confirmed⇒evidence` (§3) — S, sin riesgo SPI.
2. Stale model por element-token (§2) — M, fix TOCTOU core.
3. Tri-estado verify_state + unknown-reasons (§4) — S/M, extiende `crates/verify`.
4. Serialización por pid + drag atómico (§8) — S.
5. Scope refusal ventana/desktop (§4) — S, `observe()` fail-closed.
6. Layout de recording (§5) — M, solo logs/replay.
7. Ruta Chromium + focus-without-raise (§1, subset público primero) — M; set SPI
   completo — L, tras flag de capability + probe de SPI en runtime.
8. Test de subset de schemas (§6) — S, sin UniFFI/C-ABI.
9. Sesiones (§7) — no portar.

## NO portar

- Ciclo de vida de sesiones del daemon, superficie MCP de 60 tools, C ABI/UniFFI,
  renderer de video/cursor/zoom, `PidOnlyWindowTargetGuard` como decorador
  genérico (portar la *regla*, no el tipo), crates de Windows/Linux.

## Inalcanzable en esta pasada

- `_AXObserverAddNotificationAndCheckRemote`: cero hits en fuentes Rust
  listadas; solo prosa en `blog/inside-macos-window-internals.md`.
- `background_occluded` / `foreground_unsupported`: solo en
  `docs/action-support.md` (filas Windows/Linux), sin constructor en fuentes
  macOS/core traídas.
- `libs/cua-driver/swift/`: listado vacío vía API; paridad de wording del tool
  de replay sin verificar.
