# SDD — drivers/browser (WebDriver backend)

## Contrato

`BrowserDriver` implementa `ComputerDriver` hablando WebDriver REST
(W3C) a `safaridriver`/`chromedriver`/`geckodriver`.

- **Observe**: `execute/sync` corre un DOM walker embebido
  (`walker.rs::WALKER_JS`) que produce `Element[]` — rol ARIA explícito
  o implícito por tag, accessible name (aria-label → aria-labelledby →
  label asociado → placeholder → alt → title → texto), value, bounds
  (`getBoundingClientRect`), enabled, focused, acciones anunciadas. El
  walker guarda los nodos vivos en `window.__dexterNodes` — los
  `ElementId` son índices estables *dentro de la observación*.
- **Act**: `execute/sync` con `__dexterNodes[N]` + acción DOM pura
  (los scripts siempre empiezan `return ` — WebDriver devuelve el valor
  del return top-level; sin él los side-effects corren pero el chequeo
  de `__dexter_err` queda ciego — bug encontrado en E2E real)
  (`el.click()`, `el.focus()`, `el.value=...+input/change events`,
  `scrollIntoView`, `KeyboardEvent` dispatch) → `Mechanism::Dom`.
  Nunca coordenadas: las acciones DOM se despachan dentro de la página
  aunque la ventana esté oculta o en otro Space (background-safe real).
- **Actions tier** (input real): cuando `ctx.allow_coordinates` está
  set — el mismo flag `--coords` que habilita CGEvent en macOS —
  `Click`/`Key`/`TypeText`/`Scroll` sin target escalan de la síntesis
  DOM al endpoint W3C `POST /session/:id/actions`: pointer actions para
  clicks (con `origin` = referencia de elemento — el driver mueve al
  centro in-viewport del elemento, sin coordenadas crudas), key-source
  sequences para chords y typing por carácter, wheel source para
  scroll sin target (wheel en el centro del viewport). Es el tier que
  cubre lo que la síntesis DOM no puede: páginas que exigen
  `isTrusted`, `contenteditable`/editores ricos donde `el.value` no
  existe ni inserta texto, listeners de keydown reales, y scrolls que
  dependen de wheel events. Todo act vía `/actions` reporta
  `Mechanism::Coordinates` honestamente — es el tier de input real,
  aunque no mueva el cursor del SO (`background_input` sigue true).
  La referencia de elemento se obtiene con un probe `return el` que
  corre el mismo stale/disabled guard que `exec_on` — ningún pointer
  act pasa sobre un control deshabilitado (`Failed`, nunca éxito
  simulado). En error, `DELETE /session/:id/actions` suelta cualquier
  estado de input retenido. Sin el flag, el comportamiento DOM actual
  no cambia.
- **Targets**: `Target::Semantic` re-observa fresco + `resolve_element`
  del world-model (misma semántica fail-closed que macOS: ambiguo →
  error, cero matches → not-found). `Target::Element` valida que la
  observación siga cacheada + identidad (role+name) contra un walk
  fresco — DOM mutado → `StaleReference`.
- **Disabled guard**: todo act sobre elemento pasa por un template
  in-page que chequea `el.disabled`/`aria-disabled` antes de actuar —
  un click/set programático sobre un control deshabilitado *aterriza*
  pero no hace nada que un usuario pueda hacer, así que reporta
  `ActionResult::failure(Failed, Dom, "element is disabled")` en vez
  de simular éxito. `Target::Element` binds bypassean al generator
  (que ya salta disabled) — el guard es la última línea honesta.
- **Screenshots**: `GET /session/:id/screenshot` → PNG base64 → archivo.
- **Windows = tabs**: cada handle WebDriver del session es un `Window`
  Dexter con id estable (`handle_ids`, nunca reutilizado). Solo el tab
  activo reporta `title`/`url`/`on_screen` — leer los demás exigiría
  un switch observable, así que reportan `None`/`false` honestamente.
  `observe(scope.window)` switchea al tab pedido antes de caminar
  (acción API observable, nunca input físico); `Focus` sobre
  `Target::Window` es el switch explícito. `Target::Element` verifica
  que el tab activo sea el de la observación origen — si no, `Stale`
  con instrucción de switchear/re-observar (fail-closed cross-tab).
  `new_tab()`/`close_tab()` exponen `window/new` y `DELETE window`;
  cerrar el último tab → `false` (sin ventanas, honesto). Los tabs no se
  aplastan en un árbol: cada observación es de un solo tab.
- **Iframes**: el walker recorre `iframe.contentDocument` same-origin
  de forma transparente — el iframe aparece como `web_area` y sus
  descendientes cuelgan de él con bounds offset por el rect del frame
  (`getBoundingClientRect` dentro de un frame es relativo al frame).
  Cross-origin/no-cargados: el `web_area` sigue en el árbol y
  `collection_errors` se incrementa — lo inobservable se reporta,
  nunca se omite en silencio ni aborta la observación.
- **Capabilities**: `element_tree`, `screenshots`,
  `background_input = true` — el browser no roba cursor ni foco.
- **Navigate**: `Action::Navigate { url }` → `POST /session/:id/url`.
  Policy-gated como toda mutación (`action = "navigate"`).

## Lifecycle

`BrowserDriver::safari()` spawnea `safaridriver -p <puerto libre>`,
espera `/status` ready, y crea la sesión *lazy* en el primer observe/act.
`Drop` → `DELETE /session` + kill del proceso.

`BrowserDriver::connect(url, label)` attachea a un endpoint ya corriendo
(chromedriver:9515, geckodriver:4444, grid remoto) y abre sesión propia
— el proceso no es nuestro, no se mata.

`BrowserDriver::connect_attach(url, label)` (lo que usa el CLI
`--browser-url`) adopta la sesión viva del endpoint via `GET /sessions`
(no-W3C pero universal) — un comando one-shot puede observar/actuar la
página que el usuario ya tiene abierta. Las sesiones adoptadas no se
cierran en `Drop` (`owns_session=false`); las propias sí.

### Perfiles persistentes (sesiones protegidas)

`BrowserDriver::connect_with_profile(url, label, dir)` (CLI:
`--browser-profile <dir>` junto a `--browser-url`) crea la sesión sobre
un perfil de browser persistente — cookies y logins sobreviven entre
runs. `dir` se crea si falta y se pasa absoluto + canónico (el browser
resolvería un relativo contra el cwd del proceso driver; Firefox exige
que el directorio exista). Ruta no UTF-8 o un archivo en vez de un
directorio → `Platform` antes de tocar el endpoint.

Payload de `POST /session`:

- Sin perfil: `{"capabilities":{"alwaysMatch":{}}}` — el driver elige
  su browser (sin cambios).
- Con perfil: `alwaysMatch: {}` + un `firstMatch` por browser que
  acepta perfil. Cada driver matchea solo su entrada (W3C
  capability processing por `browserName`), así el mismo payload
  sirve contra cualquier endpoint sin adivinar el browser por URL:

| browser | driver | entrada `firstMatch` |
|---|---|---|
| Chrome/Chromium | chromedriver | `browserName: "chrome"`, `goog:chromeOptions.args: ["--user-data-dir=<dir>"]` |
| Firefox | geckodriver | `browserName: "firefox"`, `moz:firefoxOptions.args: ["-profile", "<dir>"]` |
| Safari | safaridriver | — sin capability de perfil: Safari siempre corre sobre el perfil del usuario en una ventana de automatización aislada. No matchea ninguna entrada → el endpoint rechaza la sesión (error real propagado). El CLI rechaza `--browser-profile` sin `--browser-url` antes de spawnear safaridriver. |

Con perfil la sesión *nunca* se adopta (`GET /sessions` se salta): una
sesión ajena corre sobre el perfil que eligió su creador, así que
adoptarla ignoraría el perfil en silencio. La sesión es propia y se
cierra en `Drop`; el perfil (en disco) persiste. Chrome bloquea un
`user-data-dir` en uso por otra instancia — ese error de chromedriver
llega íntegro.

Errores HTTP 4xx/5xx: el cliente lee el body WebDriver
(`{"value":{"error","message"}}`) y propaga el mensaje real — p.ej. el
"Allow remote automation" de Safari llega íntegro al usuario.

## Modelo de sesión CLI

Cada invocación de `dexter` es un proceso nuevo → la sesión de browser
es *efímera por comando*. Para flujos multi-paso usar:

- `dexter --driver browser run scenario.toml` — un proceso, sesión viva
  durante todo el scenario.
- `dexter --driver browser mcp` — sesión persistente entre tool calls.
- `dexter --driver browser task ...` — loop cerrado dentro del proceso.

`dexter --driver browser navigate <url>` solo es útil seguido de otro
comando en el mismo scenario/sesión — como comando standalone abre y
cierra la sesión al salir el proceso.

## Setup Safari (macOS)

`safaridriver` viene built-in pero requiere:

1. Safari Settings → Developer → **Allow Remote Automation**, o
2. `sudo safaridriver --enable` (una vez, pide password de admin).

Sin eso, `POST /session` devuelve http 500 con el mensaje exacto.

## Tests

- `tests/fake_webdriver.rs` — fake WebDriver HTTP en localhost (lee
  `content-length` completo; scripts grandes llegan en varios reads).
  Cubre: mapping DOM→Element, click/set_value via `__dexterNodes`,
  ambigüedad fail-closed, stale detection en DOM mutado, `Target::Point`
  → `Unsupported`, screenshot PNG, y multi-tab: ids de window estables,
  switch via `Focus{Window}` y `observe{window}`, refs stale cross-tab,
  `new_tab`/`close_tab` (incl. último tab → vacío), e `errors` de iframe
  → `collection_errors`, y el disabled guard (act sobre elemento
  `enabled:false` → `Failed`, nunca éxito simulado). Con
  `allow_coordinates`: click/key/type/scroll via `POST /actions`
  (payloads assertados en el fake) y el probe de element-ref guarda
  disabled antes del pointer act. Hermético, sin
  browser.
- `tests/safari_e2e.rs` — Safari real, gated `DEXTER_E2E_BROWSER=1`.
  data: URL → observe → click → verifica efecto DOM.

## No-goals del slice

- Leer title/url de tabs en background (exigiría switches observables;
  reportan `None` honesto).
- `Target::Point` → `Unsupported` honesto incluso con el flag — el
  browser siempre ofrece targeting semántico; un agente que insista con
  puntos está mal dirigido (pointer actions existen pero se anclan a
  elementos, nunca a coordenadas crudas).

## Failure modes

- Driver no corriendo → `DriverError::Platform` (wait_ready timeout).
- Sesión muerta (browser cerrado) → error propagado del endpoint.
- `safaridriver` sin "Allow remote automation" → mensaje real del driver
  en el error (ya propagado).
- DOM mutado entre observe y act → `StaleReference` (fail-closed).
- Elemento deshabilitado (`disabled`/`aria-disabled`) →
  `ActionResult::failure(Failed)` — el motor rutea Retry/Abstain
  en vez de creer un click que no ocurrió.
