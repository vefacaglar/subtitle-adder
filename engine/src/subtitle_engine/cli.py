"""Motor CLI. `--json` ile stdout'a satır satır JSON olay yazar (native uygulamalar bunu okur).

Olaylar:
  {"event": "stage", "stage": "extracting_audio|downloading_model|loading_model|transcribing|segmenting|translating", "fraction": 0.42|null}
  {"event": "models", "models": [{"id", "kind", "backend", "languages", "size_mb", "disk_mb", "speed", "accuracy", "license", "downloaded"}]}
  {"event": "deleted", "id": "small", "freed_mb": 465}
  {"event": "result", "language": "tr", "duration": 12.3, "audio": "...", "outputs": [...], "sentences": [...]}
  {"event": "translation", "language": "es", "sentences": [...]}
  {"event": "error", "message": "..."}
"""
import argparse
import json
import sys
from pathlib import Path

from . import models, pipeline, subtitles
from .models import KATALOG, Sentence, cached_disk_mb, delete_model, disk_mb, is_downloaded
from .sentences import SegmenterConfig

PROTOCOL_VERSION = 1


def _emit(payload: dict) -> None:
    sys.stdout.write(json.dumps(payload, ensure_ascii=False) + "\n")
    sys.stdout.flush()


def _transcribe(args: argparse.Namespace) -> int:
    def on_stage(stage: str, fraction: float | None) -> None:
        if args.json:
            _emit({"event": "stage", "stage": stage, "fraction": fraction})
        elif fraction is None:
            print(f"… {stage}", file=sys.stderr)

    def factory():
        from .transcribe import create_transcriber

        return create_transcriber(args.model)

    result = pipeline.run(
        args.video.resolve(),
        factory,
        args.out_dir.resolve(),
        language=args.language,
        model=args.model,
        formats=tuple(args.format),
        config=SegmenterConfig(max_chars=args.max_chars, max_duration=args.max_duration),
        on_stage=on_stage,
    )
    if args.json:
        _emit({
            "event": "result",
            "language": result.language,
            "duration": result.duration,
            "audio": str(result.audio),
            "outputs": [str(p) for p in result.outputs],
            "sentences": [s.to_dict() for s in result.sentences],
        })
    else:
        print(f"dil: {result.language} | {len(result.sentences)} altyazı satırı")
        for path in result.outputs:
            print(f"-> {path}")
    return 0


def _render(args: argparse.Namespace) -> int:
    """stdin'den cümle JSON'u okur (uygulamada düzenlenmiş hali), istenen formatta yazar."""
    sentences = [Sentence.from_dict(d) for d in json.load(sys.stdin)]
    if args.out:
        subtitles.write(sentences, args.out, args.format)
    else:
        sys.stdout.write(subtitles.render(sentences, args.format))
    return 0


def _translate(args: argparse.Namespace) -> int:
    """stdin'deki cümle JSON'unu yerel modelle çevirir; satır başına bir cümle."""
    from .translate import check_request, create_translator

    def on_stage(stage: str, fraction: float | None) -> None:
        if args.json:
            _emit({"event": "stage", "stage": stage, "fraction": fraction})
        elif fraction is None:
            print(f"… {stage}", file=sys.stderr)

    # Dil/model hatası indirmeye başlamadan, Türkçe `error` olayı olarak dönsün.
    check_request(args.model, args.to, args.source)
    sentences = [Sentence.from_dict(d) for d in json.load(sys.stdin)]
    if models.needs_download(args.model):
        on_stage("downloading_model", 0.0)
        models.ensure_model(args.model, on_progress=lambda f: on_stage("downloading_model", f))

    on_stage("loading_model", None)
    translator = create_translator(args.model)

    on_stage("translating", 0.0)
    translated = translator.translate(
        sentences,
        target=args.to,
        source=args.source,
        on_progress=lambda f: on_stage("translating", f),
    )
    if args.json:
        _emit({
            "event": "translation",
            "language": args.to,
            "sentences": [s.to_dict() for s in translated],
        })
    else:
        sys.stdout.write(subtitles.render(translated, "srt"))
    return 0


def _models(args: argparse.Namespace) -> int:
    """Model kataloğunu ve yerel önbellek durumunu listeler (ağa çıkmaz)."""
    sizes = cached_disk_mb()
    rows = [
        {
            "id": info.id,
            "kind": info.kind,
            "backend": info.backend,
            "languages": list(info.languages) if info.languages is not None else None,
            "size_mb": info.size_mb,
            "disk_mb": disk_mb(info.id, sizes),
            "speed": info.speed,
            "accuracy": info.accuracy,
            "license": info.license,
            "downloaded": is_downloaded(info.id),
        }
        for info in KATALOG
    ]
    if args.json:
        _emit({"event": "models", "models": rows})
        return 0
    print(
        f"{'model':<16}{'tür':>14}{'boyut':>10}{'disk':>10}"
        f"{'hız':>6}{'doğruluk':>10}{'lisans':>14}{'önbellek':>10}"
    )
    for row in rows:
        disk = "—" if row["disk_mb"] is None else f"{row['disk_mb']} MB"
        cached = "var" if row["downloaded"] else "yok"
        print(
            f"{row['id']:<16}{row['kind']:>14}{row['size_mb']:>7} MB{disk:>10}"
            f"{row['speed']:>6}{row['accuracy']:>10}{row['license']:>14}{cached:>10}"
        )
    return 0


def _delete_model(args: argparse.Namespace) -> int:
    """İndirilmiş bir modelin önbellekteki dosyalarını siler."""
    freed = delete_model(args.id)
    if args.json:
        _emit({"event": "deleted", "id": args.id, "freed_mb": freed})
    else:
        print(f"{args.id} silindi, {freed} MB boşaldı")
    return 0


def build_parser() -> argparse.ArgumentParser:
    p = argparse.ArgumentParser(prog="subtitle-engine")
    p.add_argument("--version", action="version", version=f"%(prog)s protocol {PROTOCOL_VERSION}")
    sub = p.add_subparsers(dest="command", required=True)

    t = sub.add_parser("transcribe", help="videodan altyazı üret")
    t.add_argument("video", type=Path)
    t.add_argument("-o", "--out-dir", type=Path, default=Path("output"))
    t.add_argument("-l", "--language", default=None, help="ör. tr, en (boşsa otomatik algılanır)")
    t.add_argument("-m", "--model", default="small", help="model kimliği (models listeler); diğer adlar faster-whisper'a gider")
    t.add_argument("-f", "--format", nargs="+", choices=subtitles.FORMATS, default=["srt"])
    t.add_argument("--max-chars", type=int, default=84)
    t.add_argument("--max-duration", type=float, default=6.0)
    t.add_argument("--json", action="store_true", help="stdout'a JSON olay akışı yaz")
    t.set_defaults(handler=_transcribe)

    tr = sub.add_parser("translate", help="stdin'deki cümle JSON'unu yerel modelle çevir")
    tr.add_argument("--to", required=True, help="hedef dil (ör. tr, en, es, pt, ru)")
    tr.add_argument("--from", dest="source", default=None, help="kaynak dil; NLLB'de zorunlu")
    tr.add_argument("-m", "--model", default="madlad-3b", help="çeviri modeli (models listeler)")
    tr.add_argument("--json", action="store_true", help="stdout'a JSON olay akışı yaz")
    tr.set_defaults(handler=_translate)

    r = sub.add_parser("render", help="stdin'deki cümle JSON'unu altyazı formatına çevir")
    r.add_argument("-f", "--format", choices=subtitles.FORMATS, default="srt")
    r.add_argument("-o", "--out", type=Path, default=None)
    r.set_defaults(handler=_render)

    ms = sub.add_parser("models", help="model kataloğunu ve indirme durumunu listele")
    ms.add_argument("--json", action="store_true", help="tek satır JSON yaz")
    ms.set_defaults(handler=_models)

    dm = sub.add_parser("delete-model", help="indirilmiş modeli önbellekten sil")
    dm.add_argument("id", help="model kimliği (models listeler)")
    dm.add_argument("--json", action="store_true", help="tek satır JSON yaz")
    dm.set_defaults(handler=_delete_model)
    return p


def _force_utf8() -> None:
    """Windows'ta borular ANSI kod sayfası kullanır; protokol UTF-8'dir."""
    for stream in (sys.stdin, sys.stdout, sys.stderr):
        try:
            stream.reconfigure(encoding="utf-8")
        except (AttributeError, ValueError, OSError):
            pass  # akış değiştirilmiş (test, StringIO) ya da yeniden yapılandırılamıyor


def main(argv: list[str] | None = None) -> int:
    _force_utf8()
    args = build_parser().parse_args(argv)
    try:
        return args.handler(args)
    except Exception as exc:  # uygulama her hatayı tek tip olay olarak alır
        if getattr(args, "json", False):
            _emit({"event": "error", "message": str(exc)})
        else:
            print(f"hata: {exc}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main())
