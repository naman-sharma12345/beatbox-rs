#!/usr/bin/env python3
"""Write SUMMARY.md for a beats run (see produce_beats.sh).

The two trap beats are renamed to their blind labels (trap_A / trap_B) so the
summary never says which run produced which file."""
import glob
import json
import os
import shutil
import sys

out, tag = sys.argv[1], sys.argv[2]


def load(p, d=None):
    try:
        return json.load(open(p))
    except Exception:
        return d


sess = load(os.path.join(out, "blind_session.json"), {}) or {}
sid = sess.get("id", "")
key = load(os.path.join(out, "blind", f"{sid}.key.json"), {}) or {}
label_of = {v.get("name"): k for k, v in (key.get("mapping") or {}).items()}
rename = {}
for src, lab in label_of.items():  # trap_1 -> trap_A
    rename[src] = f"trap_{lab}"
for src, dst in rename.items():
    for ext in ["", ".mp3", ".result.json", ".wall", ".loud"]:
        a = os.path.join(out, f"{src}_{tag}{ext}")
        b = os.path.join(out, f"{dst}_{tag}{ext}")
        if os.path.exists(a):
            shutil.move(a, b)

names = [rename.get(n, n) for n in ["trap_1", "trap_2", "desi_hiphop", "boom_bap", "drill"]]
names = sorted(names[:2]) + names[2:]
rows, details = [], []
R = {}
for n in names:
    r = load(os.path.join(out, f"{n}_{tag}.result.json"))
    if not r:
        continue
    R[n] = r
    plan = r["plan"]
    loud = (open(os.path.join(out, f"{n}_{tag}.loud")).read().split() if os.path.exists(os.path.join(out, f"{n}_{tag}.loud")) else ["n/a", "n/a"])
    wall = float(open(os.path.join(out, f"{n}_{tag}.wall")).read()) if os.path.exists(os.path.join(out, f"{n}_{tag}.wall")) else 0
    pr = r.get("progressions", {})
    prog = f"{pr.get('verse')} / {pr.get('hook')}" + (f" / bridge {pr['bridge']}" if pr.get("bridge") else "") + (f" ({pr['color']})" if pr.get("color") not in (None, "", "as_written") else "")
    wc = ", ".join(f"{w['name']} [{w['role']}]" for w in r.get("wildcards", []))
    nov = r.get("novelty") or {}
    near = nov.get("final_nearest") or nov.get("nearest") or {}
    nd = (near.get("distance") or {}).get("total") if isinstance(near, dict) else None
    rows.append(f"| {n} | {r['seed']} | {r['method']} | {r['bpm']} | {r['key']} | {prog} | {wc} | {loud[0]} | {loud[1]} | {wall:.0f} | {r['score']} | {nd if nd is not None else 'n/a'} ({near.get('label', '-') if isinstance(near, dict) else '-'}) | {len(nov.get('regenerated') or [])} |")
    d = r.get("direction") or {}
    secs = " > ".join(f"{s['name']}({s['bars']}{'; ' + ','.join(s['tags']) if s['tags'] else ''})" for s in r.get("sections", []))
    dev = "; ".join(f"{s['name']}: {'+'.join(s['development'])}" for s in r.get("sections", []))
    details.append(
        f"### {n}\n\n- **Direction**: {d.get('identity')} (energy {d.get('energy', 0):.2f})\n"
        f"- **Drums**: {r.get('groove')}\n- **Swing**: {plan.get('swing')}, 808: {plan.get('bass_mode')} (glide {plan['knobs']['glide']:.2f}), chords: {plan.get('harmony_style') or 'playbook'}, counter: {plan.get('counter_mode')}\n"
        f"- **Motif**: {[m['deg'] for m in plan['motif']]} - development {dev}\n- **Sections**: {secs}\n"
        f"- **Palette**: {', '.join(f'{k}={v}' for k, v in plan['palette'].items())}{' (kit ' + plan['sample_kit'] + ')' if plan.get('sample_kit') else ''}\n"
        + "".join(f"- **Wildcard** {w['name']} [{w['role']}] on {w['target']}: {w['why']}\n" for w in r.get("wildcards", []))
        + f"- **Reproduce**: {plan['provenance'].get('reproduce')}\n"
    )

L = []
L.append(f"# Beatbox {tag} beats\n")
L.append("**Listen blind first.** The two trap beats are loudness-matched in `blind/" + sid + "/A.wav` and `B.wav` (no names, seeds or configs). "
         "Rate them with the MCP tool `blind_ab_rate` (or just tell Hark which you prefer and why, per groove / identity / development / memorability / emotion / production) before reading the A/B section below. "
         "**Novelty scores only show the beats are not repeats of each other or of s8; they are not a measure of quality. Naman's blind ratings are the quality test.**\n")
L.append("Seeds are fresh per beat (OS entropy + time) and recorded in each plan; passing the same seed back reproduces the beat exactly.\n")
L.append("| beat | seed | method | BPM | key | progression (verse / hook) | wildcards [role] | LUFS | dBTP | wall s | guardrail score | nearest earlier beat (distance) | regenerated |")
L.append("|---|---|---|---|---|---|---|---|---|---|---|---|---|")
L += rows
L.append("")
L += details
# novelty matrix
m = load(os.path.join(out, "novelty_matrix.json"), {}) or {}
mx = m.get("matrix") or {}
if mx:
    labs = [l for l in mx["labels"]]
    for src, dst in rename.items():
        labs = [x.replace(f"{src}_{tag}", f"{dst}_{tag}") for x in labs]
    L.append("\n## Novelty matrix (total distance, 0 = identical, 1 = nothing shared)\n")
    L.append("Components: rhythm (genre conventions masked), melody (transposition-invariant contour), harmony (key-relative bass roots), arrangement, palette, tempo/key, audio summary. s8 beats were bit-identical to s7.\n")
    L.append("| | " + " | ".join(labs) + " |")
    L.append("|---" * (len(labs) + 1) + "|")
    for lab, row in zip(labs, mx["total"]):
        L.append(f"| {lab} | " + " | ".join(f"{x:.2f}" for x in row) + " |")
    L.append("")
    # component detail for the two traps
    try:
        ia = next(i for i, x in enumerate(labs) if x.startswith("trap_A"))
        ib = next(i for i, x in enumerate(labs) if x.startswith("trap_B"))
        c = mx["components"][ia][ib]
        L.append(f"trap A vs trap B components: {json.dumps(c)}\n")
        for i, x in enumerate(labs):
            if x.startswith("trap_s8") or x.startswith("trap_") and "s8" in x:
                L.append(f"trap A vs {x}: {json.dumps(mx['components'][ia][i])}; trap B vs {x}: {json.dumps(mx['components'][ib][i])}\n")
    except StopIteration:
        pass

# blind A/B differences
A, B = R.get("trap_A"), R.get("trap_B")
if A and B:
    pa, pb = A["plan"], B["plan"]

    def dec(p, what):
        for d in p.get("decisions", []):
            if d["what"] == what:
                return f"{d['choice']} - {d['why']}"
        return "n/a"

    L.append("\n## Blind A/B: how trap A and trap B differ\n")
    L.append("Same brief and genre, different seeds. Read after listening.\n")
    L.append("| | trap A | trap B |\n|---|---|---|")
    L.append(f"| direction | {A['direction']['identity']} | {B['direction']['identity']} |")
    L.append(f"| method | {A['method']} | {B['method']} |")
    L.append(f"| groove | {A['groove']} | {B['groove']} |")
    L.append(f"| tempo / swing | {A['bpm']} BPM, swing {pa['swing']} | {B['bpm']} BPM, swing {pb['swing']} |")
    L.append(f"| melodic identity | {dec(pa, 'motif')} | {dec(pb, 'motif')} |")
    L.append(f"| harmony | {A['key']}: {A['progressions']} | {B['key']}: {B['progressions']} |")
    L.append(f"| sound design | {', '.join(f'{k}={v}' for k, v in pa['palette'].items())}; 808 {pa['bass_mode']}; chords {pa.get('harmony_style') or 'playbook'}; {dec(pa, 'mix character')} | {', '.join(f'{k}={v}' for k, v in pb['palette'].items())}; 808 {pb['bass_mode']}; chords {pb.get('harmony_style') or 'playbook'}; {dec(pb, 'mix character')} |")
    L.append(f"| arrangement | {dec(pa, 'arrangement form')} | {dec(pb, 'arrangement form')} |")
    L.append(f"| wildcards | {'; '.join(w['name'] + ' [' + w['role'] + ']: ' + w['why'] for w in A['wildcards'])} | {'; '.join(w['name'] + ' [' + w['role'] + ']: ' + w['why'] for w in B['wildcards'])} |")
    L.append("")
open(os.path.join(out, "SUMMARY.md"), "w").write("\n".join(L) + "\n")
