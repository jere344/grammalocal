"""JSON-lines worker wrapping Grammalecte (French grammar + spelling).

Protocol (one JSON object per line on stdin/stdout):
  startup  -> {"ready": true}
  request  -> {"id": N, "text": "..."}
           -> {"id": N, "errors": [{start, end, kind, rule_id, message, suggestions}]}
  request  -> {"id": N, "cmd": "rss"}
           -> {"id": N, "rss_kb": K}
  failure  -> {"id": N, "error": "..."}

Offsets are Python code-point indices into the request text.
Run as: python3 -I grammalecte_worker.py [VENDOR_DIR]
Vendor dir falls back to the GRAMMALOCAL_GRAMMALECTE env var.
"""
import json
import os
import sys

MAX_SUGGESTIONS = 5

# Typographic apostrophe rules (straight -> curly) are out of scope for this
# checker: they fire on every apostrophe and drown out real errors.
APOS_RULE_MARKER = "apostrophe_typographique"


def emit(obj):
    out.write(json.dumps(obj, ensure_ascii=False) + "\n")
    out.flush()


def rss_kb():
    try:
        with open("/proc/self/status", encoding="utf-8") as f:
            for line in f:
                if line.startswith("VmRSS:"):
                    return int(line.split()[1])
    except OSError:
        pass
    return None


def spell_suggestions(checker, word):
    # suggest() yields lists in batches; flatten lazily and stop at the cap.
    result = []
    for batch in checker.suggest(word, nSuggLimit=MAX_SUGGESTIONS):
        for s in batch:
            if s not in result:
                result.append(s)
            if len(result) >= MAX_SUGGESTIONS:
                return result
    return result


def grammar_options(gc_options):
    # Passing dOptions replaces Grammalecte's defaults, so start from a full copy.
    opts = gc_options.getOptions()
    opts["apos"] = False
    # Typographic glyphs (curly quotes, …) and non-breaking spaces (before ! ? : ;,
    # inside « », units, large numbers): nobody types these by hand, pure noise.
    for opt in ("typo", "nbsp", "unit", "num"):
        opts[opt] = False
    opts["idrule"] = True  # include sRuleId so rule ids can be filtered
    return opts


def check(gc, spell, opts, text):
    grammar_gen, spelling_gen = gc.getParagraphErrors(text, dOptions=opts, bSpellSugg=False)
    errors = []
    for e in list(grammar_gen):
        if APOS_RULE_MARKER in e.get("sRuleId", ""):
            continue
        errors.append({
            "start": e["nStart"],
            "end": e["nEnd"],
            "kind": e.get("sType", "grammar"),
            "rule_id": e.get("sRuleId", ""),
            "message": e.get("sMessage", ""),
            "suggestions": list(e.get("aSuggestions", []))[:MAX_SUGGESTIONS],
        })
    for tok in list(spelling_gen):
        errors.append({
            "start": tok["nStart"],
            "end": tok["nEnd"],
            "kind": "spelling",
            "rule_id": "spelling",
            "message": "Mot inconnu",
            # Suggestions are computed only for flagged words, on demand.
            "suggestions": spell_suggestions(spell, tok["sValue"]),
        })
    errors.sort(key=lambda e: (e["start"], e["end"]))
    return errors


def main():
    global out
    out = sys.stdout
    # Keep stray prints from Grammalecte off the protocol channel.
    sys.stdout = sys.stderr

    vendor = sys.argv[1] if len(sys.argv) > 1 else os.environ.get("GRAMMALOCAL_GRAMMALECTE")
    if not vendor:
        print("no Grammalecte vendor dir given", file=sys.stderr)
        return 2
    sys.path.insert(0, vendor)

    from grammalecte.grammar_checker import GrammarChecker
    from grammalecte.fr import gc_options
    gc = GrammarChecker("fr")
    opts = grammar_options(gc_options)
    spell = gc.getSpellChecker() if hasattr(gc, "getSpellChecker") else gc.oSpellChecker

    emit({"ready": True})

    for line in sys.stdin:
        line = line.strip()
        if not line:
            continue
        req_id = None
        try:
            req = json.loads(line)
            req_id = req.get("id")
            if req.get("cmd") == "rss":
                emit({"id": req_id, "rss_kb": rss_kb()})
            elif req.get("cmd") == "is_word":
                # Batched dictionary lookup: {"cmd":"is_word","words":[...]} -> {"valid":[bool,...]}.
                # A single {"word":"..."} is accepted too and answered with {"valid":bool}.
                if "words" in req:
                    valid = [bool(spell.isValid(str(w))) for w in req["words"]]
                    emit({"id": req_id, "valid": valid})
                else:
                    emit({"id": req_id, "valid": bool(spell.isValid(str(req["word"])))})
            else:
                emit({"id": req_id, "errors": check(gc, spell, opts, req["text"])})
        except Exception as exc:  # keep the worker alive on bad input
            emit({"id": req_id, "error": f"{type(exc).__name__}: {exc}"})
    return 0


if __name__ == "__main__":
    sys.exit(main())
