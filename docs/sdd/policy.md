# SDD — dexter-policy

## Propósito

Decidir si una `ExecutionRoute` puede ejecutarse **antes** de tocar el
driver. La policy nunca depende de modelos: los modelos proponen, la
policy autoriza. En v2 la unidad de autorización es la ruta concreta —
una regla que diga `mechanism = "accessibility"` nunca puede autorizar
una ejecución por coordenadas.

## Contrato

```rust
pub enum PolicyDecision {
    Allow,
    Deny { reason: String },
    RequireApproval { reason: String },
}

impl Policy {
    // v2 — autoriza la ruta concreta (action + mechanism + tier +
    // sensitivity + resolved target). Es lo que el engine llama.
    fn evaluate_route(&self, route: &ExecutionRoute, ctx: &ActionContext) -> PolicyDecision;
    // v1 — compat sobre la acción sola; los matchers de ruta
    // (mechanism/sensitivity/target estructurado) no aplican aquí.
    fn evaluate(&self, action: &Action, ctx: &ActionContext) -> PolicyDecision;
    fn load(path: &Path) -> Result<Policy, DexterError>;        // TOML
    fn embedded() -> Policy;                                    // default
}
```

`ActionContext`: app seleccionada (selector parseado) + texto descriptivo
del target. La política no resuelve elementos — evalúa lo que el driver
declaró que ejecutará.

## Invariantes

1. **Deny-by-default**: una ruta mutante sin regla que la permita y sin
   aprobación vigente → `RequireApproval` o `Deny` (según default), nunca `Allow`.
2. Lectura (`Observe`, `Wait`) → `Allow` siempre.
3. Reglas se evalúan **en orden; primer match gana**. Sin match → defaults.
4. **Parse fail-closed**: `deny_unknown_fields` en el documento,
   `[defaults]`, cada `[[rule]]` y `[rule.target]` — una key desconocida
   o con typo es error de carga con los campos válidos listados, nunca
   una autorización más ancha de lo escrito.
5. Un matcher `mechanism`/`sensitivity`/`[rule.target]` presente exige el
   valor declarado — una ruta con `mechanism: None` (compat v1) no puede
   satisfacer un matcher de mecanismo.
6. Una aprobación está ligada al fingerprint canónico de la ruta
   (action kind, parámetros no-secretos, app, mechanism, tier,
   sensitivity, identidad estable del target, digest del payload), es
   **de un solo uso** y expira (TTL configurable, default 60s).
   `stage_grants` del engine son la excepción: el borrow de una app dura
   la sesión, porque re-activar la misma app no es una decisión nueva.
7. `RequireApproval` nunca ejecuta; la aprobación la otorga un `Approver`
   externo (humano interactivo o `--approve-all` explícito del caller).

## Formato TOML

```toml
[defaults]
mutating = "require_approval"   # allow | deny | require_approval

[[rule]]
action = "click"                # click|type_text|key|scroll|focus|set_value|
                                # invoke|drag|navigate|window|launch_app|quit_app|
                                # clipboard_read|clipboard_write|observe|wait|*
app = "Safari"                  # substring, "bundle:com.x", "pid:123", o ausente
mechanism = "accessibility"     # api|dom|accessibility|native_automation|vision|coordinates
intrusiveness = "background"    # background|visual|physical
sensitivity = "standard"        # standard|secrets|destructive
decision = "allow"              # allow|deny|require_approval
reason = "browser interaction approved for this task"

# Forma estructurada — cada campo presente exige igualdad
# case-insensitive sobre el descriptor resuelto. Ojo: [rule.target]
# abre una sub-tabla del último [[rule]] — decision/reason van antes.
[rule.target]
role = "button"
name = "Save"
identifier = "save-btn"
```

`target = "save"` sigue siendo el shorthand v1: substring
case-insensitive sobre role+name+identifier del descriptor.

## Floors que el engine aplica antes de policy

El engine sube la sensitivity de una ruta por encima de lo que el driver
declaró cuando la evidencia lo exige (secure fields, subrol `password`,
targets semánticos que resuelven a elementos sensibles, clipboard
reads/writes, `quit_app`/`window close`). Una ruta no puede volverse
menos sensible de lo que la observación indica.

## Errores

- `DexterError::PolicyDenied` cuando el motor consume un `Deny`.
- `DexterError::ApprovalRequired` cuando el caller no puede/no quiere aprobar.
- Error de parse con campos válidos listados cuando el TOML tiene keys
  desconocidas.
