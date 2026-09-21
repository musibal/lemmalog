---
name: lemmajevgaun
description: >-
  The one MCP product for Lemmalog evidence and derivation, a fail-closed
  gate, bounded Gauntlet Loop, and JEV multi-question judgments. Use for
  durable investigations and decisions that need auditable evidence.
---

# lemmajevgaun — evidence, gate, Gauntlet, JEV

`lemmalog` is a Datalog engine exposed as MCP tools. It is your working
memory: assertions with provenance and confidence, derived consequences
computed by rules, hypotheses with lifecycles — persistent across agents
and context resets.

**The division of labor:** the engine owns state and consequence; you own
perception and choice; every choice's outcome returns to the engine.
A claim that isn't in the engine doesn't exist — nothing durable lives in
your context.

## One deployment, one MCP, one skill

`lemmajevgaun` is a single Docker service. It exposes the same HTTP MCP
endpoint to Musibañ Cloudflare, Sayago, and local development. Do not register
Lemmalog and Gatepack separately. Do not run a launchd daemon or a second
snapshot writer.

```bash
docker compose up -d --build
```

The local endpoint is `http://127.0.0.1:8765/mcp`. Containers on the same
Docker network use `http://lemmajevgaun:8765/mcp`. Persisted facts, gate
sessions, and calibration are in the Docker volume. Supply `TYPESAFE_API_KEY`
to the container; approved Gauntlet builders are the server-side JSON map
`LEMMALOG_GAUNTLET_BUILDERS_JSON`, never a command supplied by an MCP caller.

## The discipline

1. **Commit verified evidence atomically.** The agent/domain adapter turns
   confirmed sources into one semantic diff and calls
   `lemmalog_commit({ actor, evidence, observe?, retract?, ts? })`. Use an
   evidence reference for every diff, tag read-and-verified facts `[1.0]` and
   inferences `[0.4]`–`[0.7]`, and anchor sources with
   `located(Entity, "file:line")` (or another stable reference). Never call
   naked `lemmalog_observe` or `lemmalog_retract` in a normal agent flow, and
   never commit an unverified claim.
2. **Install rules when a pattern repeats.** If you ask the same shape of
   question twice, write the Datalog for it: transitive closures, guard
   tracking, status rollups, `count`/`min`/`max`/`sum` aggregates. Rules
   are experiments: one named batch per analysis idea, validated on
   install (rejections are spec feedback), backfilled against everything
   already asserted, `lemmalog_uninstall` when the idea dies.
3. **Query before re-reasoning.** Multi-hop, transitive, or
   not-X-reachable questions go through rules and `lemmalog_query` —
   never mental closure, and never re-deriving what a prior agent derived.
   For grounded answering, `lemmalog_context` retrieves the question-relevant
   facts plus their verbatim source episodes under a token budget, with an
   attribution contrast (which subjects hold facts on the topic — a
   question-mentioned party with zero topic facts is a false-premise
   signal) and, for current-state questions, the latest value per slot
   with supersessions as history — use it instead of `lemmalog_dump` when
   preparing answers; selection beats dumping.
4. **`lemmalog_why` before trusting any derived fact.** The proof tree
   shows which asserted edges carry it; a chain is only as good as its
   lowest-confidence edge. Re-verify the weakest edges against their
   `located` anchors.
5. **Hypotheses have lifecycles.** `H --hypothesis--> claim`,
   `H --status--> proposed|supported|refuted|validated` (supersedes),
   `H --evidence--> ref` (accumulates).
   Test counterfactuals with `lemmalog_what_if` — temporary facts,
   answered goal, store untouched.
6. **Correct in the same evidence diff.** When an asserted fact is
   wrong — not merely changed — include its retraction and replacement (if
   any) in one `lemmalog_commit`; it validates, derives, and persists once.
   A value that merely changed is re-asserted under the same relation; see
   "State that changes". After a context reset or another agent's turn,
   `lemmalog_changes` with your last epoch resyncs you without re-reading the
   store.
7. **Reconcile vocabulary, don't enforce it.** Name things naturally;
   when two names mean one thing, `local --alias_of[conf]--> canonical`
   via `lemmalog_canonicalize`. Conflicts surface as `alias_conflict`
   facts; they never silently merge.
8. **Decide from queries.** Derive candidate views — unexplored items,
   blocked-by-what, what-needs-attention — and choose among them. The
   queries propose; you dispose (including off-list when judgment says
   so). Then assert the decision so state stays complete.
9. **Report from the engine.** Final deliverables render from queries and
   `why` trees, not from memory. A conclusion's confidence is the product
   of its edges (the engine multiplies down the proof chain) — deep
   derivations need high-confidence inputs to stay believable.

## The decision pipeline

For a decision, do not call JEV by itself. Use these five MCP verbs in order:

1. `gate_open(case, evidence_refs)` receives one complete, compact atomic case
   `{ "id": "...", "artifact": { ... } }` and refs of the form
   `subject|relation|object`. Include the decision, constraints, acceptance
   checks, and cited evidence — not an issue transcript or unrelated history.
   It resolves every ref against the in-process Lemmalog engine.
2. `gate_ask(gate_id)` returns only unresolved or stale evidence questions.
3. `gate_answer(gate_id, ref)` re-resolves one declared ref. It never accepts
   caller-provided evidence.
4. `gate_decide(gate_id, builder, max_cycles)` injects both resolved and
   unresolved rows into the artifact and runs at most two Gauntlet revisions.
   Each cycle makes **one JEV request with the complete atomic state** and the
   complementary question cart: verdict, failed dimension, next action,
   scope, evidence, acceptance, and safety. JEV proposes; it never writes
   facts or executes business effects.
5. `gate_outcome(gate_id, was_correct, defect, evidence)` appends the observed
   result to the calibration ledger.

Hard rules only lower a result. Missing, stale, or unresolved evidence and a
`needs_work` dimension cap `pass` at `human_review`. A resolved `dead_end`
also requires review. The gate returns `facts_to_assert`; verify them and include them in a source-backed
`lemmalog_commit` if they are true.

## Schema conventions

The only shared vocabulary (everything else: invent precisely, and assert
`describes(Relation, "one-line meaning")` so others discover it):

All asserted through the line protocol (the predicate forms below are
descriptions, not assertable syntax):

```text
kernel_func --located--> vm/vm_map.c:3052     % evidence anchor (multi-valued)
works_at --describes--> person is employed at % self-documenting schema
hyp_1 --hypothesis--> claim in plain words    % lifecycle-tracked claim
hyp_1 --status--> proposed                    % supersedes on change:
                                              % proposed|supported|refuted|validated
hyp_1 --evidence--> vm_map.c:3052            % multi-valued: accumulates
decision_7 --decision--> chose scope X because Y
```

Evidence objects take a bare source reference (space-free) or a
punctuation-free phrase; spaces plus punctuation read as leaked prose
and are dropped. Symmetric quotes around subjects/objects are stripped
(`"mean field"` lands as `mean field`) — multi-word values are fine up
to 8 words; compress longer prose into a short name or split it.

## State that changes

Values, quantities, and sets evolve — assert them so the engine can
maintain them (these conventions are what the update policy and the
aggregates need):

- **Update by re-asserting the same relation.** When a value changes,
  assert the new value under the SAME relation name: the policy
  supersedes the old fact automatically. Never invent a synonym
  relation for the new value (`uses` → `switched_to`) — that leaves
  both values open, and every current-state query gets flaky. If you
  need the history, the superseded fact is still queryable by its
  validity interval.
- **Bare numbers are integers.** `launch --monthly_cost--> 120` (never
  `$120` or `120 dollars`) — digit-only objects feed `sum`/`count`
  aggregates and `<`/`>=` comparisons. Mixed forms are opaque symbols.
- **Bare dates order correctly.** `moved_on` with `YYYY-MM-DD` (or
  `YYYY-MM`) objects; derive orderings with a rule
  (`earlier(A, B) :- on(A, D1), on(B, D2), D1 < D2`) rather than
  judging from prose.
- **Evolving sets: one fact per item, plus lifecycle verbs.** Track a
  watchlist/checklist as `added(X)` per item and `watched(X)`/`done(X)`
  when consumed; current membership is then a rule —
  `pending(X) :- added(X), !watched(X).` — not something you recount.
- **Conditional preferences stay conditional.**
  `prefers_when(user, lively, with_friends)` — never assert the
  condition itself as a fact unless the source says it holds now.

## Grammar

- Bare capitalized words are **variables** — quote entity names:
  `reports_to("Alice", Y)`, never `reports_to(Alice, Y)`.
- Fact line protocol: `S --rel[conf]--> O`, one per line.
- **An asserted fact is `current(S, rel, O)`, not `rel(S, O)`.** Rule bodies
  match the triple: `reaches(X, Y) :- current(X, depends_on, Y).` Writing
  `depends_on(X, Y)` instead installs cleanly, reports a backfill count, and
  then derives nothing — the failure is silent, so check a new rule with one
  `lemmalog_query` before building on it.
- Rule syntax: `head(X, Y) :- atom(X, Y), X \= Z.` with `!atom` negation,
  `now(T)`, comparisons, arithmetic; aggregates only in heads.
- Time is bitemporal, and the two clocks are separate. `ts` on
  `lemmalog_observe` is the **valid-from** time of the facts in that call;
  `now(T)` in a rule is the reader's present, synced to the wall clock on
  every read. So backdating a batch is safe — it dates those facts without
  hiding anything asserted after them. Omit `ts` unless the facts really
  are about the past, and give one call one coherent timestamp rather than
  mixing eras in a single batch.
- Errors are actionable: every `isError` result carries the offending
  input, the reason, and a hint — fix and resend; `lemmalog_observe`
  reports dropped lines with reasons, so a zero-add result is loud, not
  silent.

## Anti-patterns

- Guesses as untagged facts (tag low confidence or don't assert).
- Batching assertions to the end (assert as you verify).
- Encoding your judgment as rules (queries inform; you decide).
- Trusting derived facts without `why`.
- Re-deriving in context what the engine already closes.
- Letting two names for one thing drift (alias them).
- Renaming a relation when its value changes (supersede, don't fork).
- Numbers or dates buried in prose objects ("about $50", "last March")
  — bare values are what the engine can aggregate and order.

## Boundary

Stays in your head: in-flight reading, semantic judgment.
Must land in the engine: conclusions, state changes, decisions — before
you move on — and dead ends most of all: a searched-and-ruled-out
avenue is the most valuable thing a future agent can inherit (`X
--dead_end--> why it failed, where confirmed`). Send one evidence-backed
semantic diff through `lemmalog_commit`; it is the single validation,
derivation, and persistence boundary.

Scope honesty: for a short task that fits one context window, working
memory in your head is cheaper — lemmalog pays when state must outlive
a window, span agents, or survive a restart. A single-session audit
with four items to track is overhead; a multi-day investigation or a
swarm reading each other's dead-ends is the payoff case.
