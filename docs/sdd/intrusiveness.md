# SDD — intrusiveness & presence

## Propósito

Dexter actúa sobre la computadora **sin secuestrarla**. Dos garantías:

1. **Intrusiveness** — cada `Action` tiene un nivel de intrusión derivado de
   su target, y el nivel `physical` está gated aparte del approval normal:
   ningún driver puede mover el cursor real del usuario sin consentimiento
   explícito.
2. **Presence** — el journal expone lo que un overlay necesita para
   renderizar la presencia del agente (bounds del target + estado), así el
   usuario *ve* dónde trabaja Dexter aunque su mouse nunca se mueva.

## Taxonomía

`Intrusiveness` se deriva de `Action` — nunca lo declara el modelo:

| Intrusiveness | Acciones | Efecto sobre el usuario |
|---|---|---|
| `background` | `Click`/`SetValue`/`Scroll`/`TypeText` sobre `Element`/`Semantic`/`Focused`; `Observe`, `Wait` | Ninguno — mutación semántica (DOM, AX). El cursor físico no se mueve, el foco no se roba |
| `visual` | `Navigate`, `Focus`, targets `Window` | El usuario *ve* cambios (ventana sube, navegación) pero no pierde input |
| `physical` | `Target::Point`, `Key`, `TypeText`/`Scroll`/`Click` sin target resoluble a elemento | Mueve o captura el puntero/teclado reales (CGEvent). **Tier invasivo** |

Regla de derivación: el nivel lo decide el *peor* target de la acción —
un `Click{Point}` es `physical` aunque el mismo verbo sobre `Element` sea
`background`.

## Contrato de policy

```toml
[defaults]
mutating = "require_approval"
physical = "deny"        # default: deny — CGEvent nunca es silencioso

[[rule]]
action = "click"
intrusiveness = "physical"   # matcher opcional: background|visual|physical
decision = "allow"
reason = "physical input explicitly approved"
```

Invariantes nuevas (se suman a las de policy.md):

8. Si ninguna regla matchea y la acción es `physical` → `defaults.physical`
   (default **deny**), no `defaults.mutating`. `--approve-all` no cubre
   physical: la aprobación batch aprueba mutaciones semánticas, no el
   control del puntero.
9. `intrusiveness` en una regla restringe el match — una regla
   `action="click"` sin matcher sigue aplicando a clicks de cualquier
   nivel (compat hacia atrás); con matcher, solo a ese nivel.
10. El driver-side `allow_coordinates` permanece como segunda línea:
    policy puede denegar lo que el flag permitiría, nunca al revés.

## Contrato de presence (journal)

`ActionProposed` en `run_task` lleva, además de lo actual:

```json
{
  "action": {...}, "app": ..., "fingerprint": "...",
  "intrusiveness": "background",
  "target_bounds": {"x": 12.0, "y": 40.0, "w": 96.0, "h": 22.0}
}
```

`target_bounds` se resuelve del `Element`/`Semantic`/`Focused` contra la
observación viva; `null` cuando no hay rect (p.ej. `Point` ya trae coords,
`Navigate` no tiene target). Un overlay (proceso separado) tail-ea el
journal JSONL y dibuja un cursor etiquetado en esas bounds — la ventana
es click-through (`ignoresMouseEvents`), jamás captura input.

`PolicyChecked` registra `intrusiveness` para auditoría.

## Errores

- `PolicyDenied` con reason `physical input not permitted` cuando
  `defaults.physical = deny` frena una acción.
- Un `intrusiveness` inválido en una regla → `InvalidInput` al parsear
  (fail-closed, consistente con `action` desconocido).

## Out of scope

- El binario overlay (`dexter-overlay`) se implementa como proceso
  consumidor del journal — este SDD define su contrato de entrada.
- Isolated desktops / virtual displays (solución de aislamiento más
  fuerte, orthogonal — un driver futuro declararía
  `background_input: true` igual que el actual).
