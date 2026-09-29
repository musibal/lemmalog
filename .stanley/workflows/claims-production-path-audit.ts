import { readFile, readdir, stat } from "node:fs/promises";
import { isAbsolute, join, normalize, relative, sep } from "node:path";

/**
 * Repo workflow: audita un conjunto de afirmaciones (un análisis) sobre ESTE sistema
 * y separa lo que es camino de PRODUCCIÓN de lo que es harness de benchmark/tests,
 * exigiendo evidencia reproducible por afirmación.
 *
 * Nace de un fallo medido (2026-09-29): un análisis que proponía "bajar el tier del
 * modelo de extracción" y "batching de embeddings" anclado en `lemmalog-bench` /
 * `longmemeval`, cuando el producto (`lemmajevgaun-mcp`, bin de `src/bin/lemmalog-mcp.rs`)
 * no usa ningún extractor LLM: su `State` construye `MockExtractor`
 * (`src/bin/lemmalog-mcp.rs:38-39`) y `lemmalog_observe` recibe facts ya extraídos.
 * El harness no es el producto, y una librería (`src/llm.rs`) no es un entry point.
 *
 * Código hace: partir el texto en afirmaciones, extraer rutas citadas, comprobar que
 * existen, resolverlas contra los binarios reales del manifiesto y comprobar si la
 * afirmación nombra un binario. Jev juzga una propiedad por afirmación. El veredicto
 * lo deriva el código, nunca el modelo.
 */
export default async ({ root }) => {
  const entries = await discoverEntries(root);
  return {
    id: "claims_production_path_audit",
    // Metadatos de routing: Jev lee cada campo JSON para decidir si elige este workflow.
    instructions:
      "Use when the user supplies an analysis, claim set, or report about how THIS system behaves — " +
      "performance, speed, quality, cost, bottlenecks — and wants each claim separated into production " +
      "runtime vs benchmark/test/example harness, and checked for an entry point and reproducible evidence. " +
      "Not for finding code, not for reviewing a diff, not for checking a change against acceptance " +
      "criteria, not for triaging failures or comments, and not for running tests.",
    examples: [
      "Audita este análisis de velocidad y coste: marca qué afirma del producto y qué del harness",
      "¿Qué afirmaciones de este informe no tienen evidencia reproducible?",
    ],
    // Solo tiene sentido con un texto de análisis suministrado.
    available: ({ input }) => input === "text",

    async run({ request, input, judge, log }) {
      const text = typeof input === "string" ? input : input === undefined ? "" : JSON.stringify(input);
      const claims = extractClaims(text).slice(0, 24);
      log.info("claims extracted", {
        count: claims.length,
        production: entries.production,
        harness: entries.harness,
      });

      const results = [];
      let judgeFailures = 0;

      for (const [index, claim] of claims.entries()) {
        // Evidencia determinista: rutas citadas, existencia real, entry point y harness.
        const cited = [];
        for (const path of citedPaths(claim)) cited.push(await resolveCited(root, path, entries));
        const citesMissing = cited.some((entry) => !entry.exists);
        const citesHarness = cited.some((entry) => entry.exists && entry.harness);
        const citesProductionBin = cited.some((entry) => entry.exists && !entry.harness && entry.entry);

        const verdict = await judge({
          scope: `claim ${index + 1}`,
          state: {
            request,
            claim,
            cited_paths: cited,
            entry_points: { production: entries.production, harness: entries.harness },
          },
          questions: {
            path: {
              type: "choice",
              instructions:
                "Which path of the repository does `state.claim` describe? " +
                "`state.cited_paths` lists the paths the claim references and whether each is a harness path " +
                "or a binary. Product = the binary/process actually run in production. Harness = benchmark, " +
                "example or test-only code that is not what runs in production. A library module cited without " +
                "naming the binary that runs it does not establish either path.",
              criteria: {
                production_runtime: "Describes the binary or server actually run in production.",
                benchmark_or_harness:
                  "Describes benchmark, evaluation, example or script code that is not the production runtime.",
                tests_or_examples: "Describes only tests or examples.",
                docs_or_plan: "Describes documentation, a plan, or a proposal with no runtime path.",
                cannot_tell: "The claim alone does not say which path it describes.",
              },
            },
            production_impact: {
              type: "noul",
              instructions:
                "Does `state.claim` assert a concrete effect on the SPEED, QUALITY or COST of the shipped " +
                "system in production? A claim that only describes a benchmark, a test, or an example does not.",
            },
            names_entry_point: {
              type: "noul",
              instructions:
                "Does `state.claim` name the specific binary, process or harness it measured or refers to " +
                "(for example a name listed in `state.entry_points`), rather than only a library module or a file path?",
            },
            evidence: {
              type: "noul",
              instructions:
                "Does `state.claim` carry reproducible evidence — a command with a measured number, or an exact " +
                "`file:line` reference to this repository — rather than a general assertion or an unsourced number?",
            },
          },
        });

        // Fail-closed: un juez caído no aprueba nada.
        if (!verdict.ok) {
          judgeFailures += 1;
          results.push({ index: index + 1, claim, label: "unjudged", reason: verdict.reason, cited });
          continue;
        }

        const answer = (name) => verdict.answers[name];
        const pickChoice = (name) => {
          const value = answer(name);
          return value && value.type === "choice" ? value.choice : "cannot_tell";
        };
        const pickProbability = (name) => {
          const value = answer(name);
          return value && value.type === "noul" ? value.probability : 0;
        };

        const pathChoice = pickChoice("path");
        const impact = pickProbability("production_impact");
        const namesEntry = pickProbability("names_entry_point");
        const evidence = pickProbability("evidence");

        // Código deriva la etiqueta; el modelo solo aporta probabilidades.
        let label;
        let reason;
        if (citesMissing) {
          label = "bad_reference";
          reason = "cita una ruta que no existe en el repo";
        } else if (impact >= 0.6 && (citesHarness || pathChoice === "benchmark_or_harness" || pathChoice === "tests_or_examples")) {
          label = "mislabelled";
          reason = "afirma impacto en el producto pero la evidencia es harness";
        } else if (pathChoice === "benchmark_or_harness" || pathChoice === "tests_or_examples") {
          label = "harness_only";
          reason = "describe el harness, no el producto";
        } else if (impact >= 0.6 && namesEntry < 0.6 && !citesProductionBin) {
          label = "no_entry_point";
          reason =
            "afirma impacto en el producto sin nombrar el binario ni citar una ruta de producción (p.ej. solo una librería): no se sabe qué se midió";
        } else if (evidence < 0.6) {
          label = "unproven";
          reason = "sin comando medido ni file:line verificable";
        } else {
          label = "ok";
          reason = "afirma del producto con evidencia";
        }

        results.push({
          index: index + 1,
          label,
          reason,
          path: pathChoice,
          impact_probability: round(impact),
          evidence_probability: round(evidence),
          names_entry_point: round(namesEntry),
          claim,
          cited,
        });
      }

      const counts = results.reduce((acc, row) => {
        acc[row.label] = (acc[row.label] ?? 0) + 1;
        return acc;
      }, {});
      const flagged = results.filter((row) =>
        ["mislabelled", "bad_reference", "no_entry_point", "unproven", "unjudged"].includes(row.label),
      );

      const lines = [
        `auditoría de afirmaciones: ${results.length} afirmación(es) · ${flagged.length} señalada(s)` +
          (judgeFailures > 0 ? ` · ${judgeFailures} sin juzgar (juez caído)` : ""),
        Object.entries(counts)
          .map(([label, n]) => `${label}=${n}`)
          .join("  "),
        "",
        ...results.map((row) => {
          const detail = `path=${row.path} impact=${row.impact_probability} evidence=${row.evidence_probability} entry=${row.names_entry_point}`;
          return `${row.label.padEnd(14)} [${detail}] ${truncate(row.claim, 140)}`;
        }),
      ];

      return {
        status: "complete",
        output: {
          text: lines.join("\n"),
          data: {
            counts,
            entry_points: { production: entries.production, harness: entries.harness },
            findings: flagged.map((row) => ({
              claim: row.claim,
              label: row.label,
              reason: row.reason,
              cited: row.cited,
            })),
            notChecked: [
              "solo se auditan afirmaciones con ≥ 24 caracteres (viñetas, filas de tabla o frases)",
              claims.length === 24 ? "se truncó a 24 afirmaciones" : "no se truncaron afirmaciones",
              "la existencia de rutas se comprueba con stat; no se lee su contenido",
              "los entry points salen del manifiesto (Cargo.toml) y de src/bin; fuera de ahí no se resuelve el binario",
            ],
          },
        },
      };
    },
  };
};

function countClaims(text) {
  return extractClaims(text).length;
}

/**
 * Entry points reales del repo: `[[bin]]` es producción; `[[bench]]`, `[[example]]` y `[[test]]`
 * son harness. Un bin cuyo nombre parece de benchmark tambien cuenta como harness. Si no hay
 * `[[bin]]` explícitos, se listan `src/bin/*.rs`.
 */
async function discoverEntries(root) {
  const production = new Set();
  const harness = new Set();
  const byFile = new Map();
  let manifest = "";
  try {
    manifest = await readFile(join(root, "Cargo.toml"), "utf8");
  } catch {
    manifest = "";
  }
  const kind = { bin: "production", bench: "harness", example: "harness", test: "harness" };
  let section = null;
  let name = null;
  let path = null;
  const flush = () => {
    if (!section) return;
    const looksHarness = /bench|example|fixture|fake/i.test(name ?? "");
    const isHarness = kind[section] === "harness" || (kind[section] === "production" && looksHarness);
    const entryName = name ?? "(unnamed)";
    (isHarness ? harness : production).add(entryName);
    if (path) byFile.set(normalize(path.replace(/^\.\//, "")), { name: entryName, harness: isHarness });
    section = null;
    name = null;
    path = null;
  };
  for (const raw of manifest.split(/\r?\n/)) {
    const line = raw.trim();
    const header = line.match(/^\[\[(bin|bench|example|test)\]\]$/);
    if (header) {
      flush();
      section = header[1];
      continue;
    }
    if (/^\[/.test(line)) {
      flush();
      continue;
    }
    if (!section) continue;
    const nameMatch = line.match(/^name\s*=\s*"([^"]+)"/);
    if (nameMatch) name = nameMatch[1];
    const pathMatch = line.match(/^path\s*=\s*"([^"]+)"/);
    if (pathMatch) path = pathMatch[1];
  }
  flush();
  if (production.size === 0) {
    try {
      for (const file of await readdir(join(root, "src/bin"))) {
        if (!file.endsWith(".rs")) continue;
        production.add(file);
        byFile.set(`src/bin/${file}`, { name: file, harness: /bench|example/i.test(file) });
      }
    } catch {
      // sin src/bin y sin manifiesto: no hay entry points que resolver
    }
  }
  return { production: [...production].sort(), harness: [...harness].sort(), byFile };
}

/**
 * Afirmaciones, en orden de preferencia: viñetas y listas numeradas, luego filas de tabla (sin la
 * cabecera), y solo si hay pocas, frases de la prosa. Las líneas dentro de bloques de código son
 * evidencia (comandos), no afirmaciones: se ignoran. Una línea suelta de un párrafo envuelto NO es
 * una afirmación — partía la prosa en trozos y los marcaba como "sin evidencia".
 */
function extractClaims(text) {
  const bullets = [];
  const rows = [];
  const prose = [];
  const fence = /^\s*```/;
  let inFence = false;
  let inTable = false;
  for (const raw of text.split(/\r?\n/)) {
    const line = raw.trim();
    if (fence.test(line)) {
      inFence = !inFence;
      continue;
    }
    if (inFence || !line) continue;
    if (/^#{1,6}\s/.test(line)) continue;
    const bullet = line.match(/^(?:[-*+]|\d+[.)])\s+(.+)$/);
    if (bullet) {
      inTable = false;
      const claim = clean(bullet[1]);
      if (claim.length >= 24) bullets.push(claim);
      continue;
    }
    if (line.startsWith("|")) {
      if (/^\|?[\s:|-]*-{3,}/.test(line)) continue; // separador de tabla
      const cells = line
        .replace(/^\|/, "")
        .replace(/\|$/, "")
        .split("|")
        .map(clean)
        .filter(Boolean);
      if (!inTable) {
        // Primera fila de una tabla: es cabecera, no afirmación.
        inTable = true;
        continue;
      }
      const claim = cells.join(" — ");
      if (claim.length >= 24) rows.push(claim);
      continue;
    }
    inTable = false;
    prose.push(clean(line));
  }
  const bulletish = dedupe([...bullets, ...rows]);
  if (bulletish.length >= 3) return bulletish;
  // Sin viñetas suficientes: frases de la prosa, nunca líneas físicas.
  const sentences = dedupe(
    prose
      .join(" ")
      .split(/(?<=[.!?;])\s+/)
      .map(clean),
  ).filter((sentence) => sentence.length >= 40);
  return dedupe([...bulletish, ...sentences]);
}

function clean(value) {
  return value.replace(/\*\*/g, "").replace(/`/g, "").replace(/\s+/g, " ").trim();
}

function dedupe(values) {
  const seen = new Set();
  const out = [];
  for (const value of values) {
    if (!value || seen.has(value)) continue;
    seen.add(value);
    out.push(value);
  }
  return out;
}

/** Rutas de repositorio citadas en una afirmación, en orden y sin duplicados. */
function citedPaths(claim) {
  const matches = claim.match(/[\w.-]+(?:\/[\w.-]+)+\.[a-z]{1,4}(?::\d+(?:-\d+)?)?/gi) ?? [];
  const out = [];
  for (const match of matches) {
    const path = normalize(match.replace(/:\d+(?:-\d+)?$/, ""));
    if (!out.includes(path)) out.push(path);
  }
  return out;
}

/** Resuelve una ruta citada: existe, es harness y a qué binario pertenece (si pertenece a alguno). */
async function resolveCited(root, path, entries) {
  const exists = await existsUnder(root, path);
  const entry = entries.byFile.get(path) ?? null;
  const harnessPath = /^(?:tests?|benches|examples|benchmarks)\//.test(path);
  const harnessName =
    /(bench|longmemeval|loss_analysis|memeval_adapter|differential_|fake|mock|fixture|example)/i.test(path);
  return {
    path,
    exists,
    harness: entry ? entry.harness : harnessPath || harnessName,
    entry: entry ? entry.name : null,
  };
}

/** Existencia bajo root, sin escapar el repo. */
async function existsUnder(root, path) {
  if (isAbsolute(path) || path.split(/[/\\]/).includes("..")) return false;
  const full = join(root, path);
  const rel = relative(root, full);
  if (rel === "" || rel.startsWith(`..${sep}`) || isAbsolute(rel)) return false;
  try {
    return (await stat(full)).isFile();
  } catch {
    return false;
  }
}

const round = (value) => Math.round(value * 100) / 100;
const truncate = (value, max) => (value.length <= max ? value : `${value.slice(0, max - 1)}…`);
