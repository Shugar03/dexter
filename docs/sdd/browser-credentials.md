# SDD — Credenciales de navegador protegidas

## Propósito

Completar un campo de inicio de sesión en una sesión de navegador con un
secreto local, sin exponerlo a acciones serializadas, eventos, prompts o
resultados. Los perfiles persistentes conservan cookies y logins; este
contrato permite rellenar credenciales guardadas detrás de consentimiento
explícito de policy.

## Almacén local

`--credentials <file>` carga un TOML local. El archivo guarda únicamente
metadatos y el nombre de una variable de entorno para la contraseña; nunca
contiene el secreto:

```toml
[[credential]]
name = "github"
origin = "https://github.com"
username = "alice"
password_env = "DEXTER_CRED_GITHUB"
```

El cargador rechaza enlaces simbólicos, archivos no regulares, archivos
escribibles por grupo u otros usuarios, claves desconocidas y nombres
duplicados. Cada entrada debe configurar `username`, `password_env`, o
ambos. La contraseña se lee de la variable de entorno al ejecutar la
acción; una variable ausente o vacía produce un error sin revelar su
contenido.

Los orígenes se normalizan como `location.origin`: esquema y host en
minúsculas y puerto predeterminado omitido. Se acepta HTTPS; HTTP solo
para `localhost`, `127.0.0.1` y `[::1]`. Userinfo, query, fragment y
caminos distintos de vacío o `/` se rechazan.

## Flujo y consentimiento

`Action::FillCredential` contiene el target, el nombre de la credencial y
`CredentialField` (`username` o `password`). Nunca contiene el secreto.
El engine evalúa policy antes de resolver el almacén. La policy embedded
deniega esta acción; una regla comodín `action = "*"` no la cubre, y aun
`mutating = "allow"` no basta. Solo una regla explícita
`action = "fill_credential"` puede permitirla, exigir aprobación o
denegarla. `approve_all`/`--approve-all` no concede esta acción: una regla
`require_approval` necesita una aprobación real, vinculada al fingerprint
y de un solo uso.

Tras la autorización, el engine resuelve el secreto y llama únicamente a
`ComputerDriver::fill_secret`. El método predeterminado de los drivers se
niega; el navegador es el único que puede demostrar el origen y escribir
el valor dentro de una misma ejecución DOM. Los demás drivers nunca
reciben la credencial.

El navegador resuelve el target como `SetValue` y, dentro de la página,
comprueba en orden que el nodo sigue vivo, que `location.origin` coincide
con el origen esperado y que el campo está habilitado. Password solo
admite `INPUT[type=password]`; username solo admite `INPUT` de tipo
`text`, `email`, `tel` o vacío. No se permite poner una contraseña en un
campo de texto: el walker solo redacta valores de `type=password`, por lo
que un valor en otro tipo podría aparecer en la siguiente observación.
Tras las comprobaciones, el navegador enfoca el nodo, asigna el valor y
despacha `input` y `change`.

El secreto se elimina de los detalles del resultado y de los errores
antes de devolver o journalizar el estado. Nunca se incluye en la acción,
el fingerprint, el journal ni la respuesta de MCP.

## Límites

- Los secretos viven solo en variables de entorno; no se leen desde
  perfiles, políticas, escenarios o argumentos de línea de comandos.
- Las credenciales solo se usan con el origen exacto configurado; un
  cambio de origen, nodo obsoleto, campo deshabilitado o tipo incorrecto
  falla sin escribir.
- `Action::FillCredential` es una directiva del engine. La llamada
  directa a `act()` y los drivers no-browser fallan cerrados.
- Los perfiles de navegador pueden persistir cookies y logins, pero no
  relajan la policy ni sustituyen el consentimiento explícito.
