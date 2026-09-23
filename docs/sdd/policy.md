# SDD — dexter-policy

## Propósito

Decidir si una `Action` puede ejecutarse **antes** de tocar el driver.
La policy nunca depende de modelos: los modelos proponen, la policy autoriza.

## Contrato

```rust
pub enum PolicyDecision {
    Allow,
    Deny { reason: String },
    RequireApproval { reason: String },
}

impl Policy {
    fn evaluate(&self, action: &Action, ctx: &ActionContext) -> PolicyDecision;
    fn load(path: &Path) -> Result<Policy, DexterError>;        // TOML
    fn embedded() -> Policy;                                    // default
}
```

`ActionContext`: app seleccionada (selector parseado) + texto descriptivo
del target. La política no resuelve elementos — evalúa intención.

## Invariantes

1. **Deny-by-default**: una acción mutante sin regla que la permita y sin
   aprobación vigente → `RequireApproval` o `Deny` (según default), nunca `Allow`.
2. Lectura (`Observe`, `Wait`) → `Allow` siempre.
3. Acciones mutantes: `Click`, `TypeText`, `Key`, `Scroll`, `Focus`, `SetValue`.
4. Reglas se evalúan **en orden; primer match gana**. Sin match → defaults.
5. Un TOML malformado → error explícito, nunca default-permisivo silencioso.
6. Una aprobación está ligada al fingerprint `(action, app)`, es **de un
   solo uso** y expira (TTL configurable, default 60s).
7. `RequireApproval` nunca ejecuta; la aprobación la otorga un `Approver`
   externo (humano interactivo o `--yes` explícito del caller).

## Formato TOML

```toml
[defaults]
mutating = "require_approval"   # allow | deny | require_approval

[[rule]]
action = "click"                # click|type_text|key|scroll|focus|set_value|*
app = "Safari"                  # substring, "bundle:com.x", "pid:123", o ausente
decision = "allow"
reason = "browser interaction approved for this task"
```

## Errores

- `DexterError::PolicyDenied` cuando el motor consume un `Deny`.
- `DexterError::ApprovalRequired` cuando el caller no puede/no quiere aprobar.
