# se-core golden fixtures

Generated from CPython 3.11 (`engine/.venv`), the Python engine's interpreter:

* `float_repr.txt`: `<f64 bits hex> <repr(x)>` per line (random bit patterns plus edge cases).
* `round.txt`: `<x bits hex> <ndigits> <round(x, ndigits) bits hex>` per line.
* `json_cases.json`: `cases` (document, `json.dumps` compact / ensure_ascii / indent=2 / sort_keys+indent=1)
  and `errors` (document, `str(JSONDecodeError)` or null).

The generator is a short script using only `json`, `random(20261107)` and `struct`; rerun it with
`engine/.venv/Scripts/python.exe -I -X utf8 gen_json_golden.py <this folder>` if the cases change.
