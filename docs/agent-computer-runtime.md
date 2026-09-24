# Dexter
## Agent Computer Runtime — Especificación técnica y roadmap

**Estado:** propuesta técnica v0.2  
**Nombre del proyecto:** Dexter  
**Objetivo:** construir un runtime local, nativo, agnóstico de modelos y orientado a alta fiabilidad para que agentes de IA puedan operar navegadores, aplicaciones desktop y sistemas legacy de forma semántica, verificable y recuperable.

> **Dexter — the hands and eyes of AI agents.**

---

# 1. Resumen ejecutivo

Dexter no debe plantearse como "otro modelo que mueve el mouse". Debe ser un **Agent Computer Runtime**: una capa de ejecución que recibe intenciones de agentes/LLMs y las convierte en acciones sobre computadoras reales.

La tesis central es:

> **El LLM decide qué quiere conseguir; Dexter decide cómo interactuar con la computadora, ejecuta la acción, comprueba el resultado y se recupera si falla.**

Dexter debe priorizar las superficies de interacción más fiables:

```text
API
  ↓
DOM / Playwright
  ↓
Accessibility Tree
  ↓
UI Automation nativa
  ↓
Vision / OCR
  ↓
Mouse + Keyboard por coordenadas
```

El sistema debe ser:

- Rust-first
- local-first
- semantic-first
- verification-first
- model-agnostic
- driver-agnostic
- fail-closed

Dexter no intenta reemplazar a los modelos fundacionales. Tampoco intenta ser solamente un "Computer Use API". Su objetivo es convertirse en la **capa de ejecución física/digital de los agentes**.

---

# 2. Objetivos

## Objetivos principales

1. Permitir que distintos LLMs controlen la misma computadora mediante una API común.
2. Soportar web, desktop y aplicaciones legacy.
3. Evitar depender exclusivamente de screenshots.
4. Reducir el número de inferencias necesarias del LLM.
5. Utilizar modelos rápidos especializados —por ejemplo Jev/Laya— para microdecisiones cuando corresponda.
6. Verificar que las acciones realmente tuvieron el efecto esperado.
7. Recuperarse automáticamente ante errores.
8. Permitir ejecución en background cuando el sistema operativo y la aplicación lo permitan.
9. Mantener las políticas de seguridad fuera del control del LLM.
10. Ser multiplataforma y extensible.
11. Ser suficientemente ligero para instalarse en una computadora empresarial.
12. Exponer MCP, SDK y CLI para integrarse con distintos agentes.
13. Permitir que el mismo runtime sea utilizado por agentes de diferentes proveedores.
14. Crear una abstracción unificada sobre APIs, browsers, accessibility, visión y input nativo.

## No objetivos iniciales

- Entrenar un modelo fundacional propio.
- Crear un navegador propio.
- Crear un OCR propio.
- Crear un VLM propio.
- Construir una plataforma cloud antes de validar el runtime local.
- Intentar resolver todos los problemas de Linux/Wayland en la primera versión.
- Reemplazar herramientas maduras como Playwright.
- Simular movimientos humanos artificiales solamente para "parecer humano".

---

# 3. Concepto del producto

La metáfora central de Dexter es:

```text
                     AGENT
                       │
                    INTENT
                       │
                       ▼
                  ┌─────────┐
                  │ DEXTER  │
                  └────┬────┘
                       │
             ┌─────────┴─────────┐
             ▼                   ▼
           EYES                 HANDS
             │                   │
      DOM / A11y / Vision   Mouse / Keyboard
             │                   │
             └─────────┬─────────┘
                       ▼
                    COMPUTER
```

El agente aporta razonamiento de alto nivel.

Dexter aporta:

- percepción;
- interacción;
- memoria operacional;
- verificación;
- recuperación;
- políticas;
- ejecución.

---

# 4. Arquitectura conceptual

```text
                         ┌───────────────┐
                         │      LLM      │
                         │ GPT/Claude/...│
                         └───────┬───────┘
                                 │
                              Intent
                                 │
                                 ▼
┌───────────────────────────────────────────────────┐
│                     DEXTER                        │
│                AGENT COMPUTER RUNTIME             │
│                                                   │
│ Planner                                            │
│ Task Engine                                        │
│ World Model                                        │
│ Memory                                             │
│ Policy                                             │
│ Recovery                                           │
│ Verification                                       │
│ Observability                                      │
│                                                   │
│                 Action Router                      │
└──────────────────────┬────────────────────────────┘
                       │
                       ▼
                Decision Engine
                ┌──────────────┐
                │ Jev / Laya   │
                └──────┬───────┘
                       │
          ┌────────────┼──────────────┐
          ▼            ▼              ▼
        API        Playwright      Desktop
                                   Driver
                                      │
                      ┌───────────────┼───────────────┐
                      ▼               ▼               ▼
                     AX              UIA            AT-SPI
                      │               │               │
                      └───────────────┼───────────────┘
                                      │
                              Vision / OCR
                                      │
                                      ▼
                                   ACTION
                                      │
                                      ▼
                                  VERIFY
                                      │
                               ┌──────┴──────┐
                               ▼             ▼
                           SUCCESS        RECOVERY
```

---

# 5. Principios de diseño

## 5.1 Semantic-first

Nunca utilizar píxeles si existe una representación estructurada fiable.

Preferido:

```text
button(name="Responder")
```

sobre:

```text
click(x=742, y=512)
```

Las coordenadas son fallback, no la interfaz primaria.

## 5.2 Deterministic-first

Si una operación puede resolverse de forma determinista, no debe utilizarse un LLM.

Ejemplo:

```text
Esperar a que una página termine de cargar
```

debe ser responsabilidad del runtime.

## 5.3 Verify-every-important-action

Toda acción relevante debe poder definir un estado esperado y comprobarlo.

```text
ACTION
  ↓
EXPECTED STATE
  ↓
OBSERVE
  ↓
ASSERT
```

## 5.4 Model-agnostic

Dexter no debe depender de un único proveedor.

Debe aceptar:

- OpenAI
- Anthropic
- Google
- modelos locales
- otros proveedores
- modelos especializados como Jev/Laya

## 5.5 Driver-agnostic

El runtime debe abstraer la computadora.

```text
ComputerDriver
├── NativeDriver
├── CuaDriver
├── BrowserDriver
├── RemoteDriver
└── MockDriver
```

## 5.6 Local-first

El control de la computadora debe permanecer local por defecto.

El cloud puede utilizarse para inferencia si el usuario lo permite, pero el runtime y las credenciales deben permanecer bajo control local.

## 5.7 Fail closed

Ante una situación desconocida, el sistema debe detenerse o pedir intervención antes que ejecutar una acción peligrosa.

---

# 6. Stack recomendado

| Componente | Tecnología |
|---|---|
| Runtime | **Rust** |
| Async runtime | **Tokio** |
| Desktop UI | **Tauri 2 + TypeScript** |
| Frontend | TypeScript + React + Vite |
| Browser automation | **Playwright** |
| Native automation | Rust |
| macOS | Accessibility / AXUIElement + APIs nativas |
| Windows | UI Automation + MSAA + Win32 |
| Linux | AT-SPI + X11 inicialmente |
| Decision layer | Jev / Laya mediante adapters |
| LLM | Provider-agnostic |
| Vision | Provider-agnostic |
| IPC | Protobuf + Unix sockets / Named Pipes |
| Storage | SQLite |
| Logging | `tracing` |
| Metrics | OpenTelemetry |
| Protocol | MCP |
| Configuration | TOML |
| Packaging | Instaladores nativos |
| CI | GitHub Actions |
| Testing | Rust unit/integration + desktop E2E |

### Nota sobre Playwright

No es recomendable crear un motor de browser automation propio.

Puede utilizarse un worker independiente de Playwright para conservar el ecosistema maduro de Node/TypeScript, mientras el runtime permanece en Rust.

La pureza de "100% Rust" no debe convertirse en una restricción que empeore la fiabilidad del producto.

---

# 7. Estructura del proyecto

```text
dexter/
├── crates/
│   ├── runtime/
│   ├── task-engine/
│   ├── world-model/
│   ├── action-router/
│   ├── verifier/
│   ├── recovery/
│   ├── policy/
│   ├── memory/
│   ├── decision-engine/
│   ├── computer-driver/
│   ├── accessibility/
│   ├── vision/
│   ├── protocol/
│   └── observability/
│
├── drivers/
│   ├── macos/
│   ├── windows/
│   └── linux/
│
├── workers/
│   └── playwright/
│
├── apps/
│   ├── daemon/
│   └── desktop/
│
├── schemas/
│
├── tests/
│   ├── unit/
│   ├── integration/
│   ├── desktop/
│   └── scenarios/
│
└── docs/
```

---

# 8. Core abstractions

## 8.1 ComputerDriver

```rust
trait ComputerDriver {
    fn observe(&self) -> Result<Observation>;
    fn screenshot(&self, target: Target) -> Result<Image>;
    fn click(&self, target: Target) -> Result<ActionResult>;
    fn type_text(&self, text: &str) -> Result<ActionResult>;
    fn key(&self, key: Key) -> Result<ActionResult>;
    fn scroll(&self, delta: Scroll) -> Result<ActionResult>;
    fn focus(&self, target: Target) -> Result<ActionResult>;
    fn windows(&self) -> Result<Vec<Window>>;
}
```

## 8.2 DecisionEngine

```rust
trait DecisionEngine {
    fn decide(
        &self,
        observation: &Observation,
        task: &Task,
        candidates: &[Action]
    ) -> Result<Decision>;
}
```

Implementaciones:

```text
Jev
Laya
RuleBased
LocalModel
RemoteModel
```

## 8.3 VisionProvider

```rust
trait VisionProvider {
    async fn analyze(
        &self,
        image: Image,
        query: VisionQuery
    ) -> Result<VisionResult>;
}
```

## 8.4 Verifier

```rust
trait Verifier {
    async fn verify(
        &self,
        expected: &ExpectedState,
        observation: &Observation
    ) -> Result<Verification>;
}
```

---

# 9. World Model

El World Model normaliza:

```text
Screenshot
DOM
Accessibility Tree
OCR
Window state
Application state
Cursor
URL
Focused element
Recent actions
Task state
```

Ejemplo:

```json
{
  "element_id": "e_483",
  "role": "button",
  "name": "Responder",
  "bounds": [820, 431, 120, 42],
  "enabled": true,
  "visible": true,
  "actions": ["click"],
  "source": "accessibility"
}
```

La misma interfaz puede provenir de:

- macOS Accessibility;
- Windows UI Automation;
- Linux AT-SPI;
- DOM;
- OCR;
- visión.

El agente no debería tener que conocer el origen.

---

# 10. Action Router

El Action Router decide el mecanismo de ejecución.

Prioridad propuesta:

```text
1. API
2. Browser DOM / Playwright
3. Accessibility semantic action
4. Native UI automation
5. Vision target
6. Coordinate input
```

Ejemplo:

```text
Target: "Responder"

¿Existe API?
  └─ no

¿Existe DOM?
  └─ sí → Playwright

Si no:

¿Existe Accessibility?
  └─ sí → semantic click

Si no:

¿Vision puede localizar?
  └─ sí → vision click

Si no:

→ coordinate click
```

---

# 11. Micro-decision layer

Jev/Laya no deben ser el cerebro completo.

Su función es resolver decisiones rápidas:

- seleccionar un elemento;
- elegir entre acciones candidatas;
- detectar si el objetivo fue alcanzado;
- decidir si continuar;
- decidir si reintentar;
- detectar estados conocidos;
- estimar confidence.

Ejemplo:

```text
Candidates:
A = Reviews
B = Analytics
C = Messages
D = Settings

Decision:
A = 0.97
```

El runtime puede ejecutar A sin llamar nuevamente al LLM grande.

---

# 12. Planner

El LLM grande transforma:

```text
"Respondé las reviews de los últimos 15 días."
```

en una tarea estructurada:

```text
Task
├── navigate_to_reviews
├── identify_target_reviews
├── generate_response
├── submit_response
└── verify_submission
```

El planner no debe ejecutar acciones directamente.

---

# 13. Task Engine

Cada tarea debe ser una máquina de estados.

```text
PENDING
  ↓
RUNNING
  ↓
OBSERVING
  ↓
PLANNING
  ↓
ACTING
  ↓
VERIFYING
  ├── SUCCESS
  ├── RETRY
  ├── RECOVERY
  └── HUMAN_REQUIRED
```

Debe existir un límite de:

- acciones;
- tiempo;
- reintentos;
- coste de inferencia.

Esto evita loops infinitos.

---

# 14. Verification Engine

Ejemplo:

```text
Action:
click("Publicar")

Expected:
reply.exists == true
reply.text == generated_text
```

El resultado debe clasificarse como:

```text
VERIFIED
FAILED
UNCERTAIN
```

`UNCERTAIN` nunca debe tratarse automáticamente como `VERIFIED`.

---

# 15. Recovery Engine

Orden recomendado:

```text
1. retry
2. refresh observation
3. alternative semantic target
4. alternative driver
5. vision fallback
6. rollback
7. replan
8. LLM recovery
9. human intervention
```

No utilizar el LLM como primer mecanismo de recuperación.

---

# 16. Background execution

Objetivo:

```text
Usuario
  ├── Window A → uso normal
  │
  └── Agent Window B → automatización
```

El runtime debe intentar:

- no secuestrar el cursor físico;
- no cambiar el foco innecesariamente;
- trabajar sobre ventanas aisladas cuando sea posible;
- utilizar mecanismos de background del sistema operativo.

Pero debe asumir que **no todas las aplicaciones permiten interacción background**.

El driver debe devolver estados explícitos:

```text
SUCCESS
FOREGROUND_REQUIRED
UNSUPPORTED
PERMISSION_DENIED
TIMEOUT
FAILED
```

Nunca simular éxito.

---

# 17. Synthetic cursor

Cada sesión de agente puede tener un cursor lógico:

```text
Agent A → cursor A
Agent B → cursor B
User    → physical cursor
```

Esto permite futuras capacidades multi-agent.

No debe implementarse antes de estabilizar el driver básico.

---

# 18. Policy Engine

Las políticas son una capa independiente del LLM.

Ejemplo:

```yaml
navigation:
  allowed: true

read:
  allowed: true

publish:
  allowed: true
  require_approval: true

delete:
  allowed: false

purchase:
  allowed: false
```

El LLM nunca puede modificar sus propias políticas.

La policy debe ejecutarse antes de una acción sensible.

---

# 19. Seguridad

El runtime tiene acceso potencial a:

- pantalla;
- teclado;
- navegador;
- sesiones;
- contraseñas;
- correo;
- sistemas empresariales;
- datos personales.

Por defecto:

- no almacenar passwords;
- no almacenar cookies;
- no almacenar clipboard;
- no almacenar texto sensible;
- redacción automática de secretos;
- logs con IDs, no contenido sensible;
- almacenamiento local cifrado cuando corresponda;
- permisos mínimos;
- acciones destructivas protegidas.

Utilizar el keychain/credential manager nativo del sistema operativo.

---

# 20. Event log

Eventos mínimos:

```text
ObservationCreated
ActionProposed
PolicyChecked
ActionExecuted
ActionFailed
StateChanged
VerificationPassed
VerificationFailed
RecoveryStarted
RecoveryCompleted
HumanApprovalRequired
TaskCompleted
TaskFailed
```

Esto permite auditoría y debugging.

---

# 21. Recording y Replay

Cada ejecución debería poder generar un trace:

```text
Task
 ├── Observation
 ├── Decision
 ├── Action
 ├── Observation
 ├── Verification
 ├── Recovery
 └── Result
```

Debe ser posible reproducir escenarios para testing.

Las grabaciones deben poder anonimizarse.

---

# 22. MCP / SDK / CLI

Interfaces públicas:

```text
MCP
Rust SDK
CLI
Local IPC
```

Ejemplo conceptual:

```bash
dexter run \
  --task "Open the hotel PMS and check today's arrivals"
```

MCP (herramientas reales implementadas — ver `docs/for-agents.md`):

```text
dexter_observe
dexter_map
dexter_candidates
dexter_act
dexter_verify
dexter_task
dexter_journal
dexter_cancel
dexter_status
dexter_grant
```

El MCP no debe saltarse Policy Engine ni Verification — toda activación
visible (stage borrow) también pasa por policy.

---

# 23. Desktop UI

Tauri + TypeScript.

Debe servir para:

- sesiones;
- tareas;
- permisos;
- políticas;
- logs;
- estado de drivers;
- debugging;
- replay;
- aprobación humana.

No debe contener lógica crítica del runtime.

La aplicación debe poder cerrarse mientras el daemon continúa funcionando.

---

# 24. Observabilidad

Métricas importantes:

```text
task_success_rate
verification_success_rate
recovery_rate
human_intervention_rate
actions_per_task
llm_calls_per_task
decision_latency
action_latency
task_latency
driver_failures
accessibility_failures
vision_fallback_rate
```

El objetivo no es solamente "que funcione".

Hay que saber **por qué funciona o falla**.

---

# 25. Testing

## Unit tests

- state machine;
- policy;
- router;
- verifier;
- recovery;
- serialization.

## Integration tests

- driver;
- accessibility;
- IPC;
- Playwright;
- MCP.

## Desktop E2E

Escenarios controlados:

```text
Open app
Find button
Type text
Submit
Verify
```

## Failure tests

Simular:

- popup inesperado;
- UI modificada;
- botón deshabilitado;
- app congelada;
- sesión expirada;
- permiso denegado;
- network timeout;
- DOM cambiado;
- accessibility tree incompleto.

## Benchmark

Medir:

```text
Success rate
Latency
Cost
Actions
Recovery
Human intervention
```

---

# 26. Riesgos técnicos

## Muy alto

### Accessibility inconsistente

Aplicaciones pueden:

- ocultar elementos;
- exponer árboles incompletos;
- bloquear consultas;
- usar canvas;
- implementar controles custom.

### Background automation

No es universal.

### Seguridad

Un runtime con control de teclado/mouse es una superficie de ataque extremadamente poderosa.

### Reliability del LLM

El modelo puede interpretar mal una interfaz.

### Vision fallback

Es más lento y menos determinista.

---

## Alto

### Windows

La diversidad de frameworks hace que sea una de las partes más complejas.

### Linux

Wayland limita capacidades que eran habituales en X11.

### macOS

Permisos y APIs privadas/semiprivadas pueden afectar mantenimiento y distribución.

### Browser drift

Las interfaces web cambian constantemente.

---

## Medio

### Jev/Laya

Son tecnologías jóvenes y no deben convertirse en dependencias irreemplazables.

### Playwright Rust ecosystem

Menos maduro que Node/TypeScript.

---

# 27. Estrategia de dependencia

Todas las tecnologías externas importantes deben estar detrás de interfaces.

```text
DecisionEngine
VisionProvider
ComputerDriver
BrowserDriver
MemoryStore
PolicyStore
LLMProvider
```

Esto permite reemplazar:

```text
Jev → otro modelo
Playwright → otro browser driver
OpenAI → otro LLM
Cua → native driver
SQLite → otro storage
```

sin reescribir el runtime.

---

# 28. Roadmap

## Fase 1 — MVP técnico

Plataforma:

**macOS**

Features:

- Rust daemon;
- screenshot;
- window enumeration;
- accessibility tree;
- mouse;
- keyboard;
- semantic target;
- basic verifier;
- CLI;
- MCP.

Objetivo:

> Un agente puede realizar tareas sencillas en una computadora real.

---

## Fase 2 — Browser

Agregar:

- Playwright;
- DOM;
- locator;
- action routing;
- browser sessions;
- cookies/sessions protegidas;
- semantic browser observation.

Objetivo:

> Preferir DOM antes que pixels.

---

## Fase 3 — Reliability

Agregar:

- state machine;
- verifier;
- retry;
- recovery;
- checkpoints;
- replay;
- metrics.

Objetivo:

> Convertir una demo en un runtime confiable.

---

## Fase 4 — Jev/Laya

Agregar:

- DecisionEngine;
- candidate actions;
- confidence;
- completion detection;
- micro-action loop.

Objetivo:

> Reducir latencia, coste y llamadas al LLM.

---

## Fase 5 — Windows

Agregar:

- UI Automation;
- MSAA fallback;
- Win32;
- Electron;
- WPF;
- WinUI;
- legacy desktop.

Objetivo:

> Cubrir el principal ecosistema empresarial.

---

## Fase 6 — Linux

Primero:

- X11;
- AT-SPI.

Después:

- Wayland.

No prometer cobertura universal inicialmente.

---

## Fase 7 — Enterprise

- fleet management;
- RBAC;
- remote monitoring;
- signed updates;
- centralized policies;
- audit;
- organization-level controls.

---

# 29. MVP recomendado

No intentar construir todo.

El primer milestone debería ser:

```text
Rust daemon
+
macOS Accessibility
+
Screenshot
+
Mouse/Keyboard
+
World Model
+
Semantic actions
+
Verifier
+
MCP
+
CLI
```

Demo objetivo:

> "Abrí Safari, navegá a una página, encontrá un elemento, interactuá con él y verificá el resultado."

Después:

> "Realizá una tarea web completa sin utilizar coordenadas salvo como fallback."

Después:

> "Recuperate de un popup inesperado."

Después:

> "Ejecutá la misma tarea mientras el usuario sigue utilizando la computadora."

---

# 30. Qué NO intentar demostrar en el MVP

No empezar con:

- autonomía de 8 horas;
- múltiples agentes;
- todos los sistemas operativos;
- Wayland;
- Computer Vision avanzada;
- aprendizaje automático propio;
- remote control;
- fleet management;
- enterprise cloud.

Primero hay que demostrar:

> **Observe → Act → Verify → Recover**

de forma extremadamente fiable.

---

# 31. Criterio de éxito

El proyecto no debe evaluarse por:

> "La IA pudo hacer click."

Debe evaluarse por:

```text
¿Puede completar tareas reales?

¿Puede detectar que falló?

¿Puede recuperarse?

¿Puede evitar acciones peligrosas?

¿Puede trabajar sin secuestrar al usuario?

¿Puede utilizar la mejor interfaz disponible?

¿Puede cambiar de estrategia?

¿Puede funcionar con distintos LLMs?

¿Podemos explicar por qué hizo cada acción?
```

---

# 32. Veredicto técnico

La propuesta recomendada es:

> **Dexter = Rust-first Agent Computer Runtime + Tauri + TypeScript + Playwright + Native Accessibility + Vision fallback + Jev/Laya Decision Engine + LLM Planner + Verification + Recovery + Policy.**

No se debe competir inicialmente con los proveedores de modelos ni intentar reemplazar los drivers maduros.

La diferenciación debe estar en la **orquestación de múltiples mecanismos de interacción**.

La arquitectura objetivo es:

```text
                  INTENT
                    │
                    ▼
                   LLM
                    │
                    ▼
                 PLANNER
                    │
                    ▼
               WORLD MODEL
                    │
                    ▼
              ACTION ROUTER
                    │
       ┌────────────┼─────────────┐
       ▼            ▼             ▼
      API         DOM/A11y      Vision
       │            │             │
       └────────────┼─────────────┘
                    ▼
              JEV / LAYA
                    │
                    ▼
                DRIVER
                    │
                    ▼
                COMPUTER
                    │
                    ▼
               OBSERVATION
                    │
                    ▼
                VERIFIER
                    │
          ┌─────────┴─────────┐
          ▼                   ▼
       SUCCESS              RECOVERY
                                │
                                ▼
                              LLM
```

## Principio rector

**El LLM piensa.  
Dexter decide cómo ejecutar.  
El driver controla la máquina.  
El verifier comprueba la realidad.  
El recovery corrige los errores.  
La policy pone los límites.**

---

# 33. Primer objetivo de implementación para el agente de coding

El agente de coding debe comenzar por construir **un vertical slice funcional**, no por implementar toda la arquitectura.

Orden:

1. Crear workspace Rust.
2. Crear daemon `dexter`.
3. Implementar `ComputerDriver` trait.
4. Implementar backend macOS.
5. Obtener ventanas.
6. Obtener Accessibility Tree.
7. Capturar screenshot.
8. Normalizar elementos al World Model.
9. Implementar semantic click.
10. Implementar keyboard/type.
11. Implementar Observation → Action → Verification loop.
12. Exponerlo mediante CLI.
13. Exponer las mismas capacidades mediante MCP.
14. Crear tests reales contra aplicaciones controladas.
15. Implementar Playwright como segundo driver.
16. Implementar Action Router.
17. Implementar Recovery.
18. Recién entonces conectar Jev/Laya.

No implementar Jev/Laya primero.

Primero hay que demostrar que **Dexter puede observar, actuar y verificar de forma determinista**.

---

# 34. Definición de "Done" para el primer milestone

El milestone 1 se considera terminado solamente cuando Dexter puede:

```text
1. detectar una ventana;
2. inspeccionar su Accessibility Tree;
3. representar sus elementos en un World Model;
4. localizar un elemento por semántica;
5. ejecutar una acción;
6. observar el nuevo estado;
7. verificar que el estado esperado ocurrió;
8. informar failure si no ocurrió;
9. reintentar de forma limitada;
10. exponer todo el proceso mediante CLI y MCP;
11. mantener el runtime funcionando aunque la UI de Dexter esté cerrada.
```

La primera implementación debe priorizar **fiabilidad y observabilidad sobre autonomía**.

El objetivo de Dexter no es parecer humano.

El objetivo es que un agente pueda **operar computadoras reales con la robustez de un sistema de automatización y la flexibilidad de un agente inteligente**.
