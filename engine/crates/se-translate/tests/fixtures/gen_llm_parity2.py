"""Generates llm_parity2.json from the Python engine: URL rules, `.local` resolution, batching, checkpoint
names, profile reprs and generation options, inline-markup and parser fuzz.

Run with the engine venv: engine/.venv/Scripts/python.exe gen_llm_parity2.py
"""
import itertools
import json
import random
import socket
from pathlib import Path
from unittest import mock

from subtitle_engine import translate as tr
from subtitle_engine.models import ModelError, Sentence
from subtitle_engine.translation_profiles import profile_for_model

rng = random.Random(7)
out: dict = {}

# --- URL rules ---------------------------------------------------------------------------------------
SCHEMES = ["", "http://", "https://", "HTTP://", "HtTpS://", "ftp://", "http:/", "https:"]
HOSTS = ["opencode.ai", "OpenCode.AI", "api.deepseek.com", "192.168.1.5", "10.0.0.1", "8.8.8.8", "[::1]",
         "[fe80::1%eth0]", "[2001:db8::1]", "box.local", "Box.Local.", "localhost", "srv", "a.b", "user:pw@10.0.0.2",
         "user@evil.com", "127.1", "0x7f.1", "100.64.3.4", "172.32.0.1", "", "[::ffff:192.168.0.1]", "host:abc",
         "host:99999", "h_ost", "ex ample.com"]
PORTS = ["", ":1234", ":11434", ":443", ":0"]
PATHS = ["", "/", "//", "/v1", "/v1/", "/openai", "/OpenAI", "/v1/chat/completions", "/chat/completions",
         "/api/chat", "/zen/go/v1", "/zen/go/v1/chat/completions", "/inference/go/v1", "/inference/go/openai/v1/",
         "/inference/go/openai/v1/chat/completions", "/x/v1", "/v1?x=1", "/v1#f", "/api/chat/", ";p"]
urls = set()
for s, h, p, pa in itertools.product(SCHEMES, HOSTS, PORTS, PATHS):
    if rng.random() < 0.08:
        urls.add(s + h + p + pa)
urls |= {"  http://x  ", "http://", "https://", "///", "http:///v1", " ", "http://[::1", "opencode.ai/zen/go/v1",
         "https://opencode.ai/zen/go/v1", "https://opencode.ai/zen/go/v1?q", "https://opencode.ai:443/zen/go/v1"}
def safe(fn, *a, **k):
    try:
        return fn(*a, **k)
    except ValueError:
        return "ValueError"


rows = []
for u in sorted(urls):
    row = {"url": u, "host": tr._url_host(u), "allowed": tr.api_key_transport_allowed(u),
           "opencode": safe(tr._is_opencode_go_url, u), "models": safe(tr._external_models_url, u)}
    try:
        row["ok"] = tr.normalize_external_chat_url(u)
    except ModelError as e:
        row["err"] = str(e)
    except Exception as e:  # noqa: BLE001
        row["exc"] = type(e).__name__
    if "ok" in row:
        row["norm_models"] = tr._external_models_url(row["ok"])
        row["norm_host"] = tr._url_host(row["ok"])
        row["norm_local"] = tr.is_local_network_host(row["norm_host"])
        with mock.patch.object(socket, "getaddrinfo", return_value=[(2, 1, 6, "", ("10.1.2.3", 0))]):
            row["resolved"] = tr._resolve_lan_url(row["ok"], allow_scan=False)
        with mock.patch.object(socket, "getaddrinfo", side_effect=OSError("x")), \
                mock.patch.object(tr, "_discover_lan_server_ip", return_value="192.168.7.7"):
            row["scanned"] = tr._resolve_lan_url(row["ok"], allow_scan=True)
            row["unscanned"] = tr._resolve_lan_url(row["ok"], allow_scan=False)
    rows.append(row)
out["urls"] = rows

# --- batching ----------------------------------------------------------------------------------------
ENDINGS = ["", ".", "!", "?", "。", "”", ".”", "?)", "...", ",", " .", ".  ", "»", ".'"]
batches = []
for case in range(400):
    n = rng.randint(1, 30)
    t = 0.0
    sents = []
    for i in range(n):
        start = round(t + rng.choice([0, 0.1, 0.5, 1.19, 1.2, 1.21, 3]), 2)
        end = round(start + rng.random() * 2, 2)
        t = end
        sents.append(Sentence(id=i, start=start, end=end, text="w" + rng.choice(ENDINGS),
                              words=[], speaker=rng.choice([None, 0, 0, 0, 1])))
    limit = rng.choice([1, 2, 3, 6, 6, 6, 7, 16])
    pending = list(enumerate(sents))
    got = [[s, s + len(b)] for s, b in tr._translation_batches(pending, limit)]
    batches.append({"limit": limit, "sentences": [x.to_dict() for x in sents], "ranges": got})
out["batches"] = batches

# --- checkpoint names --------------------------------------------------------------------------------
ck = []
for case in range(60):
    sents = [Sentence(id=rng.randint(-3, 99), start=rng.choice([0, 1, 0.5, 1e-7, 12345678901234567.0, 3.0]),
                      end=rng.choice([2, 2.25, 1e16, 0.1]), text=rng.choice([" a ", "çay", "x\n", ""]), words=[],
                      speaker=rng.choice([None, 0, 3])) for _ in range(rng.randint(0, 4))]
    model = rng.choice(["qwen3.5-4b", "madlad-3b"])
    target = rng.choice(["tr", "en"])
    source = rng.choice([None, "", "en"])
    prompt = rng.choice(["", "p", "ünï\ncode"])
    with mock.patch.dict("os.environ", {}, clear=False):
        import os
        os.environ.pop(tr.EXTERNAL_LLM_URL_ENV_VAR, None)
        os.environ.pop(tr.EXTERNAL_LLM_MODEL_ENV_VAR, None)
        name = tr._temp_checkpoint_path(model, target, source, sents, prompt).name
    ck.append({"model": model, "target": target, "source": source, "prompt": prompt,
               "sentences": [s.to_dict() for s in sents], "name": name})
out["checkpoints"] = ck

# --- profiles ----------------------------------------------------------------------------------------
prof = []
for name in ["", "gemma-4-e4b", "qwen3.5-9b", "qwen3:8b", "translategemma:4b", "hy-mt1.5-7b", "Hunyuan-MT-7B",
             "gemma3:12b", "deepseek-v4-pro"]:
    p = profile_for_model(name)
    prof.append({"model": name, "repr": str(p), "openai": json.dumps(p.generation_options(ollama=False)),
                 "ollama": json.dumps(p.generation_options(ollama=True)),
                 "messages": json.dumps(p.chat_messages("S", "U"), ensure_ascii=False)})
out["profiles"] = prof

# --- markup / parser fuzz ----------------------------------------------------------------------------
ALPHA = ["*", "**", "_", "__", "`", " ", "a", "ş", "1", "\n", ".", "\t", "x_y", "́", "²"]
mk = []
for _ in range(4000):
    s = "".join(rng.choice(ALPHA) for _ in range(rng.randint(0, 9)))
    mk.append([s, bool(tr._INLINE_MARKUP_RE.search(s))])
out["markup"] = mk
PA = ["[1]", "[2]", "1.", "2)", "3:", " ", "\n", "\r\n", "x", "```", "<think>", "</think>", "[٣]", " ",
      "<|channel>thought", "<channel|>", "[0]", "10.", "\x0b", "[1 ]", "1 .", "ok"]
pr = []
for _ in range(4000):
    s = "".join(rng.choice(PA) for _ in range(rng.randint(0, 8)))
    n = rng.randint(1, 4)
    pr.append({"raw": s, "n": n, "out": {str(k): v for k, v in tr.parse_numbered_translations(s, n).items()}})
out["parser"] = pr

Path(__file__).with_name("llm_parity2.json").write_text(json.dumps(out, ensure_ascii=False), encoding="utf-8")
print({k: len(v) for k, v in out.items()})
