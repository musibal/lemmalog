> Procedencia de las cifras: las mediciones de tamaño del store (porcentajes
> y conteos de hechos) se tomaron contra el store externo del 2026-09-22, que
> desde entonces ha mutado; no son reproducibles tal cual desde este repo. Las
> referencias al código sí están verificadas y se citan por símbolo. Antes de
> apoyarse en un número concreto, re-medirlo.

# Análisis de velocidad, calidad y coste de lemmalog — producción (corregido)

Reemplaza un análisis anterior que confundió el harness de benchmark con el
producto. Cada afirmación lleva su medida (comando reproducible) o su
referencia exacta de código.

## 0. Corrección del análisis previo: qué estaba mal

El análisis anterior atacó el camino equivocado. Evidencia:

- **Producción = `lemmalog-mcp`**, proceso longevo, no el CLI ni el bench.
  El `State` del servidor usa `MockExtractor` — no hay extractor LLM en el
  producto (declaración de `struct State` y `main`, `src/bin/lemmalog-mcp.rs`).
- `lemmalog_observe` recibe facts **ya extraídos** en protocolo
  `S --rel[conf]--> O`: la extracción la paga el agente llamante con sus
  propios tokens (definición de `tools()` en `src/bin/lemmalog-mcp.rs`).
- Por tanto, las propuestas de "bajar tier del modelo de extracción (10-20x
  de ahorro)" y "batching de embeddings" apuntaban a `lemmalog-bench` /
  `longmemeval` (en `src/bin/lemmalog-bench.rs`, `client()` fija
  `claude-sonnet-4-6` por defecto; en `src/llm.rs`, `http_embed` hace un
  HTTP por texto), que es el harness de evaluación, no el producto. Ese ahorro no
  existe en producción.

## 1. Cómo funciona el repo en producción y quién paga cada coste

| Camino | Qué corre | Coste |
|---|---|---|
| Boot del MCP / cada invocación CLI | `AgentMemory::load`: parse del snapshot + declare + `run()` completo (el `m.engine.run()` incondicional de `AgentMemory::load`) | ~3,0 s una vez (ver §4) |
| `lemmalog_context` (por consulta) | `Retrieval::build` BM25 completo por llamada (`AgentMemory::context_for_query` → `Retrieval::build`) | ~0,4-0,5 s por consulta |
| `lemmalog_query` / `why` | solo motor Datalog | ~0,1 s |
| `lemmalog_observe`/`commit`/`retract` | maintain incremental (deltas pendientes) | milisegundos |
| `gate_decide` | 1 llamada JEV por ciclo (estado acotado a 18k chars en `choice`, `src/gate.rs`) | $ por ciclo |

La superficie de herramientas ya está presupuestada por test
(`tool_surface_stays_under_budget` en `src/bin/lemmalog-mcp.rs`:
< 4500 tokens) — ese coste de contexto ya está controlado.

## 2. Velocidad — propuestas (todas sin tocar calidad)

**P1 — Cachear `Retrieval` por época en el MCP.** Cada `lemmalog_context`
reconstruye BM25 sobre 63.415 facts + 4.448 episodios aunque nada haya
cambiado. Medido: mismo comando con y sin context (§4): 3,48 s vs 3,04 s →
el delta 0,44 s es `Retrieval::build`+`select`. Cacheando por `epoch` del
engine y reconstruyendo solo tras observar facts nuevos: ~0,45 s → <0,05 s
por consulta en el proceso longevo. Es la ganancia repetida, no la de una vez.

**P2 — Boot sin re-fixpoint.** `load()` ejecuta `run()` incondicionalmente
(el `engine.run()` incondicional de `AgentMemory::load`, `src/agent.rs`)
aunque el snapshot se guarda siempre tras `maintain()`
convergido (los saves del CLI/MCP van tras maintain: p.ej. la rama
`observe` de `main` en `src/bin/lemmalog-cli.rs`).
El fixpoint re-deriva +194.301 facts (~2,85 s de los 3,0 s; perfil en §4).
Opciones: persistir las derivadas con un flag CLEAN en el snapshot, o leer el
checkpoint binario que el propio MCP ya sabe escribir/leer
(`export_snapshot`/`restore_snapshot`, `src/bin/lemmalog-mcp.rs`).
3,0 s → ~0,2 s en cada arranque y en cada invocación de CLI.

**P3 — Costes micro del fixpoint** (camino de escritura y boot): `Ann::join`
y `plus` clonan `BTreeSet<String>` de provenance por derivación
(`Ann::join` y `plus` del semiring de anotaciones, `src/eval.rs`) — 194.301 veces por boot; `relation_keys`
clona todas las claves en vez de iterar (`relation_keys`, `src/eval.rs`).

## 3. Calidad — defecto medido en producción

Salida REAL de
`lemmalog-cli context --query "estado del proyecto lemmalog"` sobre el
snapshot vivo: el contexto se contamina con `current(estado_20260913, estado)`,
`current(estado_delsol, estado)`, `current(tiene_estado, estado)`,
`current(m365_estado, ...)` — la palabra común "estado" colisiona con nombres
de entidad existentes: `query_entities` (`src/retrieval.rs`) la mete
en `direct` y cada fact que la nombra recibe +1,5 (`select_scored`, `src/retrieval.rs`).

**Fix propuesto:** amortiguar el boost por rareza de la entidad (df: cuántos
facts la nombran), o exigir especificidad (multi-token/compuesto) para el
boost directo. Validarlo con el harness longmemeval existente antes de
fusionarlo.

## 4. Medidas — comandos y números (snapshot vivo: 63.415 FACTs, 4.448 EPs, 9,4 MB)

```
LEMMALOG_MCP_PATH=~/.lemmalog/lemmalog-memory.snapshot

# load puro (solo lectura), 3 corridas:
lemmalog-cli batches          real 3,04-3,12 s (user ~3,0 s)
# load + Retrieval::build + select:
lemmalog-cli context --query "estado del proyecto lemmalog"   real 3,48-3,52 s
# load + query Datalog pura:
lemmalog-cli query --goal 'current("lemmalog", R, O)'        real 3,03-3,17 s

# desglose del load (LEMMALOG_PROFILE_RUN=1, lemmalog-cli batches):
run stratum n=3  174 ms (+50.717)   # current, uso_relacion, relacion_nueva
run stratum n=87 949 ms (+51.259)   # doble_verdad, reaches, causes...
run stratum n=33 1053 ms (+63.198)  # open_hypothesis, desbloquea...
run stratum n=15 637 ms (+28.741)   # current_canon, accionable...
run stratum n=7   35 ms (+386)
=> fixpoint ≈ 2,85 s re-derivando 194.301 facts; parse+declare ≈ 0,15 s
```

## 5. Coste ($$) real

- **Producto:** no hay extractor LLM dentro de lemmalog; la extracción la paga
  el agente llamante. Los levers internos ya están capados por tests
  (superficie de tools < 4500 tokens). El $ medible dentro del producto son
  las llamadas JEV de `gate_decide`, por ciclo.
- **Gates JEV:** batchear estados distintos fue medido como dañino (mueve
  scores hasta 0,73 y cambia el routing; advertencia documentada en el doc de
  `JevClient::ask` (`src/jev.rs`) — no batchear. El lever legítimo es reducir ciclos de
  rework (la calibración ya existe: `~/.lemmalog/gate-calibration.jsonl`).
- **Solo aplica al harness de benchmark** (no al producto): modelo de
  extracción por defecto `claude-sonnet-4-6` (`client()`,
  `src/bin/lemmalog-bench.rs`) y embeddings un HTTP por texto
  (`OpenAiClient::http_embed`, `src/llm.rs`) — mejorar eso solo
  abarata las corridas de evaluación.