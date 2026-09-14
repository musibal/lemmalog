#!/usr/bin/env python3
"""Differential: production daemon (8765) vs candidate binary on a COPY (8766).

PLAN-FORK-LEMMALOG.md Fase 1 step 3. Read-only against production: the only
write target is the snapshot COPY under /tmp. Divergence = test.

  usage: differential_8766.py [--candidate BIN] [--prod 8765] [--port 8766] [--keep]
"""
import argparse, json, os, re, shutil, socket, subprocess, sys, tempfile, time
from urllib import request, error

# Heads whose goals are also asked with a bound constant (plan-fork Fase 1
# item 1): a real row, then the same row with ONE constant mutated.
BOUND_HEADS = [
    ("multi", 1),
    ("exclusive", 1),
    ("current", 3),
    ("uso_relacion", 1),
    ("evidence_count", 2),
]

PROBES = [
    # full derived state + raw base facts: identical data must derive identically
    ("current/3", "lemmalog_query", {"goal": "current(X, Y, Z)"}),
    ("edge/6", "lemmalog_query", {"goal": "edge(X, Y, Z, T1, T2, E)"}),
    ("evidence_count/2", "lemmalog_query", {"goal": "evidence_count(X, Y)"}),
    ("relacion_huerfana/1", "lemmalog_query", {"goal": "relacion_huerfana(X)"}),
    ("open_hypothesis/1", "lemmalog_query", {"goal": "open_hypothesis(X)"}),
    ("dead_end_sin_ancla/1", "lemmalog_query", {"goal": "dead_end_sin_ancla(X)"}),
    ("escalations", "lemmalog_escalations", {}),
]

def rpc(port, method, params=None, timeout=180):
    payload = {"jsonrpc": "2.0", "id": 1, "method": method}
    if params is not None:
        payload["params"] = params
    req = request.Request(
        f"http://127.0.0.1:{port}/mcp",
        data=json.dumps(payload).encode(),
        headers={"Content-Type": "application/json"},
    )
    with request.urlopen(req, timeout=timeout) as r:
        return json.loads(r.read().decode())

def esc_slot(line):
    """(subj, pred) of an escalation list line: 'idx: conflict: S --R--> ...'."""
    if "conflict: " not in line:
        return line
    rest = line.split("conflict: ", 1)[1]
    subj, tail = rest.split(" --", 1)
    return (subj, tail.split("--> ", 1)[0])

def call(port, tool, args):
    """Text of a tool result, normalized to sorted lines."""
    res = rpc(port, "tools/call", {"name": tool, "arguments": args})
    if "error" in res:
        return ["ERROR " + json.dumps(res["error"], sort_keys=True)]
    blocks = res.get("result", {}).get("content", [])
    text = "\n".join(b.get("text", "") for b in blocks)
    return sorted(text.splitlines())

NO_ANSWER = "(no answers"

def answered(lines):
    """Rows of a query result. A miss is not "zero lines": the server answers
    a miss with an explanatory line, and a ground goal that HOLDS used to
    answer with an empty body — which is exactly the false negative this
    probe pins down."""
    return [l for l in lines if l.strip() and not l.startswith(NO_ANSWER)]

def bound_literal(v):
    """Constant as the goal parser must read it back: bare when the store
    displays a number (the line protocol stores digit-only objects as Int),
    quoted otherwise (the tool schema tells models to quote constants)."""
    return v if re.fullmatch(r"-?\d+", v) else '"%s"' % v

def bound_literals(v):
    """Every spelling of a constant that must find the row: the displayed one,
    plus the quoted form of a number (how a model copies a value out of a
    result — the same fact used to answer "no such fact")."""
    return [v, '"%s"' % v] if re.fullmatch(r"-?\d+", v) else ['"%s"' % v]

def probe_bound_goals(port, heads):
    """Ground goals: for each head take the first real row of the free query
    and check that (a) the ground goal built from its values answers with at
    least one row, and (b) the same row with ONE constant mutated (`_zz`
    suffix) answers with none. Read-only. Returns (fails, detail lines)."""
    fails, lines = 0, []
    for pred, arity in heads:
        vars_ = ["X", "Y", "Z"][:arity]
        free = call(port, "lemmalog_query", {"goal": f"{pred}({', '.join(vars_)})"})
        rows = answered(free)
        row = None
        for r in rows:
            vals = [p.split("=", 1)[1] for p in r.split(", ") if "=" in p]
            if len(vals) == arity:
                row = vals
                break
        if row is None:
            lines.append(f"    {pred:<15} SKIP (no parsable free row: {len(rows)} rows)")
            continue
        bad = []
        for i in range(arity):
            for lit in bound_literals(row[i]):
                pat = [vars_[j] if j != i else lit for j in range(arity)]
                goal = f"{pred}({', '.join(pat)})"
                ground = answered(call(port, "lemmalog_query", {"goal": goal}))
                if not ground:
                    bad.append(f"ground {goal} -> NO ROW (the row exists)")
            mutated = list(row)
            mutated[i] = mutated[i] + "_zz"
            pat = [vars_[j] if j != i else bound_literal(mutated[i]) for j in range(arity)]
            goal = f"{pred}({', '.join(pat)})"
            hit = answered(call(port, "lemmalog_query", {"goal": goal}))
            if hit:
                bad.append(f"mutated {goal} -> {len(hit)} ROW(S), expected 0")
        fails += len(bad)
        lines.append(f"    {pred:<15} row={row} {'ok' if not bad else 'FAIL'}")
        for b in bad:
            lines.append(f"        {b}")
    return fails, lines

def wait_ready(port, proc, deadline=20.0):
    end = time.time() + deadline
    while time.time() < end:
        if proc.poll() is not None:
            return False
        try:
            rpc(port, "initialize", timeout=2)
            # The port can answer because a STALE server of a previous run
            # still holds it (the new one dies on AddrInUse). Only a live
            # candidate makes the verdict about this binary.
            return proc.poll() is None
        except Exception:
            time.sleep(0.2)
    return False

def main():
    ap = argparse.ArgumentParser()
    here = os.path.dirname(os.path.abspath(__file__))
    ap.add_argument("--candidate", default=os.path.join(here, "..", "target/release/lemmalog-mcp"))
    ap.add_argument("--snapshot", default=os.path.expanduser("~/.lemmalog/lemmalog-memory.snapshot"))
    ap.add_argument("--prod", type=int, default=8765)
    ap.add_argument("--port", type=int, default=8766)
    ap.add_argument("--keep", action="store_true", help="keep the snapshot copy + candidate log")
    ap.add_argument("--write-probe", action="store_true",
                    help="assert 3 values on a fresh relation against the CANDIDATE COPY and "
                         "require exactly 1 escalation (one per slot). Never touches production.")
    a = ap.parse_args()

    work = tempfile.mkdtemp(prefix="lemmalog-diff-")
    cand = os.path.join(work, "cand.snapshot")
    shutil.copy2(a.snapshot, cand)          # NEVER the real snapshot: single-writer rule
    log = open(os.path.join(work, "candidate.log"), "wb")
    env = dict(os.environ, LEMMALOG_MCP_PATH=cand, LEMMALOG_MCP_HTTP=f"127.0.0.1:{a.port}")
    proc = subprocess.Popen([a.candidate], env=env, stdout=log, stderr=subprocess.STDOUT)
    rc = 0
    try:
        if not wait_ready(a.port, proc):
            print(f"FAIL candidate never answered on {a.port} (exit={proc.poll()})")
            log.flush()
            print(open(os.path.join(work, "candidate.log")).read()[-2000:])
            return 2
        print(f"prod={a.prod} candidate={a.port} bin={a.candidate} copy={cand}")
        diverged = 0
        for label, tool, args in PROBES:
            try:
                p = call(a.prod, tool, args)
            except Exception as e:
                print(f"  {label:<22} SKIP  prod error: {e}")
                continue
            try:
                c = call(a.port, tool, args)
            except Exception as e:
                print(f"  {label:<22} DIVERGE candidate error: {e}")
                diverged += 1
                rc = 1
                continue
            if label == "escalations" and p != c:
                # the fix shrinks the queue by design: fold one line per slot
                # and purge declared relations. Expected iff every candidate
                # slot existed in prod, the queue shrank, and each surviving
                # conflict line carries the copyable remedy.
                ps, cs = {esc_slot(l) for l in p}, {esc_slot(l) for l in c}
                shrinking = cs <= ps and len(c) < len(p)
                remedied = all(
                    'fix: multi("' in l or "conflict: " not in l for l in c
                )
                if shrinking and remedied:
                    print(f"  {label:<22} QUEUE-SHRINK (intended) "
                          f"prod={len(p)} cand={len(c)}")
                    continue
            if p == c:
                print(f"  {label:<22} ok ({len(p)} lines)")
                continue
            diverged += 1
            only_p = sorted(set(p) - set(c))
            only_c = sorted(set(c) - set(p))
            print(f"  {label:<22} DIVERGE prod={len(p)} cand={len(c)} "
                  f"only_prod={len(only_p)} only_cand={len(only_c)}")
            for tag, rows in (("only_prod", only_p), ("only_cand", only_c)):
                for row in rows[:5]:
                    print(f"      {tag}: {row[:160]}")
            rc = 1
        # Bound constants, checked per port (this is NOT a prod-vs-candidate
        # diff): the candidate must answer a ground goal that holds with at
        # least one row and must not answer a mutated constant at all. A
        # pre-fix binary fails the ground half by design, so production is
        # reported for information until it is redeployed.
        cfails, clines = probe_bound_goals(a.port, BOUND_HEADS)
        print(f"  bound-goals(cand {a.port}): {'FAIL ' + str(cfails) + ' check(s)' if cfails else 'ok'}")
        for l in clines:
            print(l)
        if cfails:
            rc = 1
        try:
            pfails, _ = probe_bound_goals(a.prod, BOUND_HEADS)
            note = "ok" if not pfails else f"{pfails} check(s) — expected on a pre-fix binary"
            print(f"  bound-goals(prod {a.prod}): {note}")
        except Exception as e:
            print(f"  bound-goals(prod {a.prod}): SKIP {e}")
        if a.write_probe:
            rel, subj = "rel_sonda_diferencial", "sonda_diferencial"
            before = len(call(a.port, "lemmalog_escalations", {}))
            for v in ("valor_uno", "valor_dos", "valor_tres"):
                rpc(a.port, "tools/call", {"name": "lemmalog_observe",
                    "arguments": {"facts": f"{subj} --{rel}[1.0]--> {v}", "ts": int(time.time())}})
            after = len(call(a.port, "lemmalog_escalations", {}))
            delta = after - before
            # contract: ONE warning per (S,R) slot, not one per asserted value
            new_line = next((l for l in call(a.port, "lemmalog_escalations", {})
                             if "rel_sonda" in l), "")
            remedy_ok = 'fix: multi("rel_sonda_diferencial")' in new_line
            verdict = ("ok (one per slot)" if delta == 1
                       else f"VIOLATION ({delta} lines for 1 slot)")
            verdict += "" if remedy_ok else " + MISSING REMEDY"
            if not remedy_ok:
                rc = 1
            print(f"  write-probe:{rel:<10} escalations {before} -> {after}  delta={delta}  {verdict}")
            if delta != 1:
                rc = 1
        if proc.poll() is not None:
            print(f"FAIL candidate exited mid-run (code {proc.poll()})")
            rc = 1
        print("DIVERGED" if diverged else "NO DIVERGENCE")
    finally:
        proc.terminate()
        try:
            proc.wait(timeout=10)
        except subprocess.TimeoutExpired:
            proc.kill()
        log.close()
        if a.keep:
            print(f"kept: {work}")
        else:
            shutil.rmtree(work, ignore_errors=True)
    return rc

if __name__ == "__main__":
    sys.exit(main())
