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
- **Targets**: `Target::Semantic` re-observa fresco + `resolve_element`
  del world-model (misma semántica fail-closed que macOS: ambiguo →
  error, cero matches → not-found). `Target::Element` valida que la
  observación siga cacheada + identidad (role+name) contra un walk
  fresco — DOM mutado → `StaleReference`.
- **Screenshots**: `GET /session/:id/screenshot` → PNG base64 → archivo.
- **Windows**: la sesión es una "ventana" (title + url reportados).
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
  → `Unsupported`, screenshot PNG. Hermético, sin browser.
- `tests/safari_e2e.rs` — Safari real, gated `DEXTER_E2E_BROWSER=1`.
  data: URL → observe → click → verifica efecto DOM.

## No-goals del slice

- Múltiples tabs/frames (se reporta la ventana activa; iframe flatten
  después).
- Endpoint WebDriver `/actions` (pointer/teclado físico del driver) —
  las acciones DOM cubren el caso real sin coordenadas.
- `Target::Point` → `Unsupported` honesto (el browser no necesita
  coordenadas; un agente que insista con puntos está mal dirigido).

## Failure modes

- Driver no corriendo → `DriverError::Platform` (wait_ready timeout).
- Sesión muerta (browser cerrado) → error propagado del endpoint.
- `safaridriver` sin "Allow remote automation" → mensaje real del driver
  en el error (ya propagado).
- DOM mutado entre observe y act → `StaleReference` (fail-closed).
