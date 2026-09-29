# Vocabulario de lemas de lemmalog — clasificación y organización (2026-09-29)

Higiene de lemas sobre el store real: qué nombres de relación son el vocabulario
escogido, qué es deriva del mismo lema y qué se poda.

## Fuente (reproducible)

```sh
LEMMALOG_MCP_PATH=~/.lemmalog/lemmalog-memory.snapshot \
  ./target/release/lemmalog-cli dump --pred current > /tmp/ll-current.txt     # 24.693 hechos, 3,0 s
sed -E 's/^current\(//; s/\)$//' /tmp/ll-current.txt | awk -F', ' '{print $2}' \
  | sort | uniq -c | sort -rn > /tmp/ll-vocab.tsv                             # 6.732 nombres distintos
```

Cada línea de `current(S, R, O)` tiene exactamente dos separadores `, `: el
troceado por la posición media es exacto, no heurístico.

## Método

`stanley "Clasifica y organiza el vocabulario de lemas de lemmalog" --json`,
con el workflow de repositorio `.stanley/workflows/classify-relation-names.ts`
y Jev **local** (`TYPESAFE_BASE_URL=http://127.0.0.1:11435`,
`TYPESAFE_MODEL=winnow:e4b-t08`): 40 llamadas de juicio, 0 fallos.

Reparto: el código lleva los portones duros (namespace `jv_*`/`d1_*`, nombre
negado, fecha dentro del nombre, singleton) y computa pesos; Jev juzga solo lo
semántico (`domain_relation`, `stable_concept`, `same_as`). El código deriva la
etiqueta.

## Resultado

| cajón | nombres | qué es |
|---|---:|---|
| CORE | 1.446 | vocabulario escogido |
| ALIAS_DRIFT | 3 | deriva confirmada del mismo lema |
| JUNK | 5.283 | poda |
| total | 6.732 | |

JUNK por motivo (portones de código):

| motivo | nombres |
|---|---:|
| `one-off noise` (singleton: 1 uso) | 4.732 |
| `negation-named` (`no_*`/`sin_*`) | 339 |
| fecha dentro del nombre | 138 |
| namespace de máquina (`jv_*`/`d1_*`) | 47 |

### Plan de alias (deriva confirmada)

```
status        -> estado      (conf 0.83)
estado_fase0  -> estado      (conf 0.63)
evidencia     -> evidence    (conf 0.86)
```

### Familias por población (5.146 de los 6.732 nombres son singleton)

| familia | nombres | usos |
|---|---:|---:|
| located | 5 | 2.741 |
| describes | 1 | 1.626 |
| estado | 59 | 971 |
| orphan | 5 | 954 |
| jv | 16 | 1.076 |
| d1 | 31 | 886 |

Cabeza de uso: `located` 2.720, `describes` 1.626, `orphan_audit_bulk` 875,
`estado` 645, `evidence` 514, `dead_end` 471, `es` 463, `alias_of` 327,
`status` 267, `jv_revisitado` 266, `regla` 255, `d1_coocurrencia` 226.

## Evidencia fuerte que este pase NO usó: solape de hechos

Dos nombres de relación que aparecen sobre los mismos pares `(sujeto, objeto)`
son el mismo lema, sin depender de parecido de letras ni de un modelo:

```sh
awk '{ sub(/^current\(/,""); split($0,a,", "); print a[1]"\t"a[3]"\t"a[2] }' /tmp/ll-current.txt
# agrupar por (sujeto, objeto), emitir todas las parejas, contar solapes >= 3
```

Parejas con ≥3 hechos compartidos (11 en total):

```
jv_ruido      / jv_ruido_r2                70     located         / located_at               17
jv_verdict    / jv_verdict_r2              21     d1_arista_confirmada / d1_coocurrencia     12
jv_conf       / jv_conf_r2                 15     d1_arista_worker     / d1_coocurrencia_worker 12
d1_arista_efectiva_tabla / d1_arista_tabla 15     es_bead / requiere_ok_humano                7
npm_test / typecheck                        6     bead / evidence                             3
evidence / located                          3
```

Ojo: el solape **no** respalda `status`/`estado` ni `evidencia`/`evidence` (cero
hechos compartidos). Los alias de arriba son una decisión semántica del juez, no
un hecho del store: por eso van a curador y no se asertan solos.

## Huecos declarados (lo que este pase NO prueba)

1. **No se escribió nada en el store**: es un plan, no se asertó ningún alias ni
   se retractó nada. `lemmalog_canonicalize` no se llamó.
2. **Se juzgaron 40 de 6.208 nombres juzgables** (tope de presupuesto por
   corrida). Los 1.406 CORE restantes son CORE *por defecto*, no por juicio:
   CORE está inflado y hay que medirlo antes de tomarlo como vocabulario curado.
3. **`one-off noise` es una política, no una medición**: 4.732 nombres con un
   solo uso caen a JUNK. Es defendible podar singleton, pero es una decisión de
   curaduría y debe ser explícita, no un valor por defecto silencioso.
4. El solape `(sujeto, objeto)` no alimenta todavía los candidatos del workflow:
   la recall de sinonimia sigue siendo léxica + diccionario EN/ES.
5. El prompt de extracción sigue inventando vocabulario abierto (medido:
   `src/agent.rs:152`, 5.970 nombres entonces). Clasificar después es el
   síntoma; la causa está en la extracción.

## Instalación del workflow

`.stanley/candidates/imp_6ffaae3c9c74/` → `.stanley/workflows/classify-relation-names.ts`.

`stanley --promote-candidate imp_6ffaae3c9c74` **falla**: el candidato quedó
`rejected` porque el agente de mejora, durante la misma ventana, escribió fuera
de su directorio (concretamente en `.stanley/workflows/lemmalog-vocab-triage.ts`,
un workflow paralelo que ese worker revirtió y borró). No fue un fallo de
contrato: `checks.loaded=true`, `quarantined=[]`. Se instaló a mano y se probó
con la corrida real de arriba, que es la prueba que la etiqueta `validated` no da.

---

## Poda ejecutada (2026-09-29, tarde)

Motivo: los facts estaban desactualizados y el vocabulario medía 6.732 nombres
con 4.732 de un solo uso.

Backups (en `~/.lemmalog/`):
- `backup-20260929-pre-vocab-alias.snapshot` — 9.456.393 B (antes de los 3 alias)
- `backup-20260929-pre-poda-singletons.snapshot` — 9.464.712 B (antes de la poda)

Criterio: relación con **un solo hecho** en todo el store, excluyendo
(a) cualquier nombre que lea un lote de reglas instalado (77 nombres: `ac5`,
`aplicado_a_bead`, `autoridad_hatchet`, `avanzo_a`, `backlog_medido`, `bloqueado`,
`caps_posteriores`…) y (b) 418 con caracteres inseguros para el protocolo de
`lemmalog_retract`. Ejecutado vía MCP `lemmalog_retract` + `lemmalog_save`,
10 lotes de ~500, ~8 s por lote.

| | antes | después |
|---|---:|---:|
| hechos `current` | 24.693 | **20.042** (−4.651) |
| nombres de relación | 6.732 | **2.081** (−69%) |
| snapshot | 9.464.712 B | **8.798.091 B** |
| `alias` | 18 | 18 (intacto) |
| escalaciones | 0 | 0 |

Intactos por uso: `located` 2.720, `describes` 1.626, `orphan_audit_bulk` 875,
`estado` 645, `evidence` 514, `jv_revisitado` 266, `status` 267,
`estado_fase0` 170, `d1_nodo` 140, `d1_pk` 138, `evidencia` 161.

Contabilidad exacta: 5.146 singleton = 4.651 podados + 418 formato + 77 que leen
reglas. El mismo store ya llevaba la cuenta: `uso_relacion(R, N)` quedó con
**2.081 filas**, el vocabulario superviviente. Ese lote existía desde antes: la
medición de uso estaba en el store y no se consultó primero — el pase de
clasificación con Stanley duplicó algo que ya estaba resuelto por reglas.

### Lo que NO se podó (sigue pendiente)
- 339 nombres negados (`no_*`/`sin_*`): no son basura. `sin_linter` sostiene
  `current(ingest_excel_ts, sin_linter, fuera de eslint config)`.
- 138 con fecha dentro del nombre (`filas_20260916`): notas reales fechadas.
- 47 de namespace de máquina (`jv_*` 16, `d1_*` 31): **carga estructural**.
  `b93` lee `d1_pk`, `b92`/`b94`/`b96`/`b98` declaran `multi("d1_*")`, `b1`
  declara `exclusive("estado_fase0")` y `b9` lee `estado_fase0`. Podarlos sin
  retirar antes esas reglas mata derivaciones.
- 418 singleton con `S`/`O` con espacios o caracteres que el protocolo
  `S --rel--> O` no acepta sin riesgo.

---

## Reglas de vocabulario instaladas — lote `b161`

Fuente exacta (reproducible; revertir con `lemmalog_uninstall b161`):

```
lema_poda(R) :- uso_relacion(R, N), N = 1.
lema_marginal(R) :- uso_relacion(R, N), N =< 2.
lema_estable(R) :- uso_relacion(R, N), N >= 5.
lema_colapsable(A, B, count(O)) :- lema_estable(A), lema_estable(B), current(S, A, O), current(S, B, O).
```

`uso_relacion(R, N)` ya existía en el store: N = slots `(S, O)` distintos por
nombre. El lote añade la lectura curada encima:

| predicado | filas | significado |
|---|---:|---|
| `lema_estable(R)` | **421** | N ≥ 5 — el vocabulario escogido |
| `lema_marginal(R)` | **1.276** | N ≤ 2 |
| `lema_poda(R)` | 586 → **167** | N = 1 — pendiente de poda |
| `lema_colapsable(A,B,N)` | 457 | pares con slots compartidos: evidencia para unificar |

Sintaxis real del motor (aprendida a base de sondas rechazadas):
comparación es `=<`, **no** `<=` y **no** `=<`→`<=`; **no existe operador de
desigualdad** (`!=` y `<>` se rechazan), así que `lema_colapsable` incluye los
pares consigo mismo y hay que descartarlos al consultar; los agregados
(`count(...)` en cabeza) sí funcionan. El join agregado tardó **94,5 s** de
cómputo: es caro, mantenerlo en un lote propio.

## Promover y eliminar vocabulario (operativo)

```
lemmalog_query  current(X, "mi_rel", Y)      # existe por sus hechos, no hay diccionario
lemmalog_query  lema_estable(R)              # sube a escogido al llegar a N >= 5
lemmalog_install_rules  'multi("mi_rel").'   # o exclusive("mi_rel").: política de update
lemmalog_query  lema_poda(R)                 # la lista exacta a podar
lemmalog_uninstall  b<N>                     # antes: desinstalar el lote que lee ese nombre
lemmalog_retract  'S --mi_rel--> O'          # el nombre sale del vocabulario
```

Los alias **no eliminan**: `lemmalog_canonicalize` unifica en lectura
(`current_canon`), y el slot medio solo se proyecta si se instala
`current_vocab(S, R2, O) :- current(S, R1, O), maps_to(R1, R2).` (hecho: `b158`).

## Estado final y residuo estructural

| | inicio de sesión | final |
|---|---:|---:|
| hechos `current` | 24.693 | **19.623** |
| nombres de relación | 6.732 | **1.662** |
| snapshot | 9.464.712 B | **8.777.655 B** |
| `alias` / escalaciones | 18 / 0 | 18 / 0 |

Se retractaron 5.070 hechos en dos pasadas: 4.651 relaciones de un solo uso, y
419 más que una pasada anterior había descartado por un fallo de parseo propio
(no era límite del protocolo `S --rel--> O`).

Los **167** de `lema_poda` que quedan son irreducibles desde fuera:
- **77** los lee un lote de reglas instalado (`estado_fase0` vía `b1`/`b9`,
  `d1_pk`/`d1_nodo` vía `b92`-`b98`, `backlog_medido`, `autoridad_hatchet`…).
  Podarlos sin desinstalar antes esas reglas mata derivaciones.
- **69** solo existen en aristas **cerradas** (`filas`, `tamano`, `progreso`,
  `desplegado`): 439 filas históricas, 20.506 `edge` frente a 19.623 `current`.
  `lemmalog_retract` sobre una cerrada responde `not found (no open fact
  matches)` y no la purga. El CLI no tiene `compact` ni `gc`
  (`observe|retract|query|context|why|rules|rmrules|batches|dump`).

Es decir: la limpieza por datos está agotada. Lo que queda pide **compactación
en el motor** (purgar aristas cerradas), que además aliviaría el coste de carga
de `lemmalog-src-mcz`.

## Jev y Laya en lemmalog

- **Jev** (`src/jev.rs`) es el juez: `Question::{Noul, Score, Choice}` →
  probabilidad de sí / nivel / etiqueta, con confianza y distribución, y
  `Response.model` (p. ej. `jev-1.13.0`) para reproducir la corrida. Entorno:
  `TYPESAFE_API_KEY`, `TYPESAFE_BASE_URL` (un juez local gana al hosted),
  `TYPESAFE_MODEL` (la selección del operador manda sobre cualquier default).
- **Laya (Ollaya)** es el endpoint local que contesta ese protocolo
  (`http://127.0.0.1:11435`, `winnow:e4b-t08`): no es un cliente distinto, es el
  servidor. El mismo endpoint sirve la vía generativa (`OpenAiClient` en
  `src/llm.rs`: chat/completions para extracción, `strip_think`, embeddings).
- **Dónde decide sobre el vocabulario**: `src/canonical.rs` —
  `reconcile_with_jev`, `align_pair`, `Verdict`, `route_of`; es Jev quien
  dictamina si dos nombres son el mismo (`tests/jev_test.rs`:
  `canonical_is_the_shorter_name`, `the_cut_points_follow_from_three_levels`,
  `companion_nouls_ride_along_for_the_curator`,
  `missing_companion_noul_is_an_error_not_nan`).
- **En los gates**: `src/gate.rs`, rúbrica de 4 criterios, `jev_model()` =
  `LEMMALOG_JEV_MODEL` > `TYPESAFE_MODEL` > `jev-latest`, `jev_client()` resuelve
  la base una sola vez (`LEMMALOG_JEV_BASE` > `TYPESAFE_BASE_URL` > hosted).
  Sesiones y calibración en `~/.lemmalog/gates/` y `gate-calibration.jsonl`.
- **En preguntas profundas**: `src/agent.rs::ask_deep`.

El pipeline completo del vocabulario queda así:

```
uso_relacion(R,N)  →  lema_estable / lema_poda      (selección determinista, b161)
                   →  lema_colapsable(A,B,N)        (candidatos por slots compartidos)
                   →  reconcile_with_jev            (Jev dictamina el alias)
                   →  alias asertado (read-side) + alias_conflict (freno)
```

`lema_colapsable` es la entrada nueva: convierte "CORE por defecto" en candidato
medido. El determinismo del store no se contamina porque Jev **solo propone**: no
muta el datalog, y las reglas duras no suben un veredicto.
