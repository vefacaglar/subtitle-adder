"""Generates se-core JSON golden fixtures from CPython's json/float repr."""
import json, random, struct, sys
out_dir = sys.argv[1]
rng = random.Random(20261107)
floats = [0.0, -0.0, 1.0, 0.1, 0.42, 1e-05, 1e16, 1e15, 9999999999999998.0, 1.5e-07, 123456789.123,
          0.0001, 0.00009999, 5e-324, 1.7976931348623157e308, 2.2250738585072014e-308, 0.32000000000000006,
          0.7200000000000001, 1/3, 2/3, 100.0, 1e22, 1e23, 12345678901234567890.0]
for _ in range(3000):
    bits = rng.getrandbits(64)
    f = struct.unpack('<d', struct.pack('<Q', bits))[0]
    if f != f or f in (float('inf'), float('-inf')):
        continue
    floats.append(f)
for _ in range(2000):
    floats.append(round(rng.uniform(-1e4, 1e4), rng.randint(0, 6)))
    floats.append(rng.uniform(0, 7200))
    floats.append(10.0 ** rng.randint(-30, 30) * rng.random())
with open(f"{out_dir}/float_repr.txt", "w", encoding="utf-8", newline="\n") as fh:
    for f in floats:
        fh.write(f"{struct.unpack('<Q', struct.pack('<d', f))[0]:016x} {f!r}\n")

rounds = []
for _ in range(3000):
    x = rng.uniform(-100, 100)
    n = rng.randint(0, 6)
    rounds.append((x, n))
for k in range(0, 4000):
    rounds.append((k / 16000.0, 3))
    rounds.append((k * 0.0005, 3))
with open(f"{out_dir}/round.txt", "w", encoding="utf-8", newline="\n") as fh:
    for x, n in rounds:
        r = round(x, n)
        fh.write(f"{struct.unpack('<Q', struct.pack('<d', x))[0]:016x} {n} {struct.unpack('<Q', struct.pack('<d', r))[0]:016x}\n")

docs = [
    '{"event": "stage", "stage": "x", "fraction": null}',
    r'[{"id": 1, "start": 0, "end": 1.0, "text": "D\u00fczenlendi.", "words": [], "speaker": null}]',
    r'{"a": "\u0000\u001f\b\f\n\r\t\"\\/", "b": "\u007f\u0080\u00a0\u2028\u2029\ud83d\ude00\u00e9\u0130"}',
    '{"n": [1E3, 1.50, -0, 0.0, 1e-7, 1e400, -1e400, 123456789012345678901234567890, NaN, Infinity, -Infinity]}',
    '{"nested": {"x": [[], {}, [1, [2, {"y": true}]]], "z": false}, "": "empty key"}',
    '{"a": 1, "b": 2, "a": 3}',
    r'"just a string \u3053\u3093\u306b\u3061\u306f"',
    '[]', '{}', '5', '-12.5e+3', 'null',
]
cases = []
for d in docs:
    v = json.loads(d)
    cases.append({"doc": d,
                  "compact": json.dumps(v, ensure_ascii=False),
                  "ascii": json.dumps(v),
                  "indent2": json.dumps(v, ensure_ascii=False, indent=2),
                  "sorted_indent1": json.dumps(v, sort_keys=True, indent=1)})
bad = ['', ' ', '[1,]', '{"a":1,}', '[1 2]', '{"a" 1}', '{"a":}', '{1:2}', '[] x', '\n\n  "abc', '"a\nb"', r'"\x"',
       r'"\u12"', '\ufeff[]', '[', '{', '{"a"', '{"a":1', '[1,', '-', '01', '1.', '1e', 'nul', 'tru', '[nan]',
       '{"a": [1, 2,\n 3,, 4]}', '"abc' + '\\','[\r\n1\r\n,]']
errors = []
for d in bad:
    try:
        json.loads(d)
        errors.append({"doc": d, "error": None})
    except json.JSONDecodeError as e:
        errors.append({"doc": d, "error": str(e)})
with open(f"{out_dir}/json_cases.json", "w", encoding="utf-8", newline="\n") as fh:
    json.dump({"cases": cases, "errors": errors}, fh, ensure_ascii=True, indent=1)
