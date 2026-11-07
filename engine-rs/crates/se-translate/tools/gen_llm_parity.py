"""Generates tests/fixtures/llm_parity.json from the Python engine (run from engine-rs/crates/se-translate):
../../../engine/.venv/Scripts/python.exe -I tools/gen_llm_parity.py
"""
import json
import random
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[4] / "engine" / "src"))
from subtitle_engine import translate  # noqa: E402

rng = random.Random(1234)
ALPHABET = list("abcAB xyzİıŞşçÇğüöÖ.,!?*_`=>-→⇒[]0123456789\n:)\"'ßΣς١٢") + ["  ", "=>", "**", "[1] ", "\n[2] ", "<think>", "</think>"]

def rand_text(n):
    return "".join(rng.choice(ALPHABET) for _ in range(rng.randint(0, n)))

texts = [
    "Hello.", "Where are you going?", "eu não consigo abrir minha boca daquele jeito", "Alice, Bob and Charlie.",
    "STRASSE straße", "*Merhaba* dünya", "_x_", "a `b` c", "**bold**", "2 * 3 * 4", "İstanbul'a gidiyorum.",
    "ǅemal ﬁne", "x => y", "A -> B -> C", "", "   ", "١٢٣ test", "Ⅻ roman", "²³ super",
]
pairs = [(rng.choice(texts), rng.choice(texts)) for _ in range(150)] + [(rand_text(30), rand_text(30)) for _ in range(800)]
validators = []
for s, t in pairs:
    for src, tgt in [(None, None), ("pt", "tr"), ("en", "en")]:
        validators.append({
            "source_text": s, "translated_text": t, "source": src, "target": tgt,
            "markup": translate._has_unrequested_markup(s, t),
            "clean": translate._clean_translation_pair(s, t),
            "pair": translate._has_source_translation_pair(s, t),
            "untranslated": translate._looks_untranslated(s, t, src, tgt),
        })

replies = [
    "[1] a\n[2] b", "1. a\n2) b", "[1]", "[1] x\n\n```\n[2] y", "<think>x</think>[1] a", "<THINK>x</Thought>\n1: a",
    "[١] a", "1.5 million", "  [2] b\r\n[1] a  ", "[1] a [2] b", "[1] a\x1c[2] b", "<|channel>thought\n[1] r\n<channel|>[1] ok",
    " <|channel>thoughts [1] r <channel|>[1] ok", "[0] zero", "[3] three",
] + [rand_text(40) for _ in range(500)]
parser = []
for r in replies:
    for n in (1, 2, 3):
        try:
            out = translate.parse_numbered_translations(r, n)
        except ValueError:
            continue
        parser.append({"raw": r, "n": n, "out": {str(k): v for k, v in out.items()}})

urls = [
    "192.168.1.50:1234", "http://h", "HTTP://H:1/v1", "https://opencode.ai/zen/go/v1/", "https://opencode.ai:443/zen/go/v1",
    "https://OpenCode.ai/inference/go/v1", "https://opencode.ai/inference/go/v1/chat/completions", "http://[::1]:8080",
    "http://[::1", "http://user:pw@host:80/x", "http://:80", "ftp://h", "http://h/v1?x=1", "  http://h/openai  ",
    "http://h/api/chat", "http://h/chat/completions", "host.local", "http://[fe80::1%25en0]:1/", "http://h:abc", "/",
    "http:///", "x" * 5, "http://ex ample.com", "http://exa℀mple.com", "http://[v1.fe]/", "http://[1.2.3.4]/",
]
normalize = []
for u in urls:
    try:
        out = {"ok": translate.normalize_external_chat_url(u)}
    except Exception as exc:  # noqa: BLE001
        out = {"err": str(exc)}
    normalize.append({"url": u, **out, "allowed": translate.api_key_transport_allowed(u), "host": translate._url_host(u)})

hosts = ["010.1.1.1", "0x7F.0.0.1", "0x7f.0x0.0.1", "127.0.0.01", "1.2.3", "4294967295", "4294967296", "0xffffffff",
         "0x", "0x.1", "127.", "[fd00::1]", "::ffff:10.0.0.1", "::", "0.0.0.0", "fec0::1", "FE80::1", "a.b", "cafe",
         "99999999999", " localhost ", "x.localhost.", "100.64.0.0", "100.127.255.255", "2001:db8::1", "::ffff:7f00:1"]
local = [{"host": h, "local": translate.is_local_network_host(h)} for h in hosts]

Path("tests/fixtures/llm_parity.json").write_text(
    json.dumps({"validators": validators, "parser": parser, "normalize": normalize, "local": local}, ensure_ascii=True),
    encoding="utf-8",
)
print(len(validators), len(parser), len(normalize), len(local))
