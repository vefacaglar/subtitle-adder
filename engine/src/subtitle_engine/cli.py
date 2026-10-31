"""Motor CLI. `--json` ile stdout'a satır satır JSON olay yazar (native uygulamalar bunu okur).

Olaylar:
  {"event": "stage", "stage": "extracting_audio|downloading_model|loading_model|transcribing|segmenting", "fraction": 0.42|null}
  {"event": "models", "models": [{"id", "size_mb", "speed", "accuracy", "downloaded"}]}
  {"event": "result", "language": "tr", "duration": 12.3, "audio": "...", "outputs": [...], "sentences": [...]}
  {"event": "error", "message": "..."}
"""
import argparse
import json
import sys
from pathlib import Path

from . import pipeline, subtitles
from .models import KATALOG, Sentence, is_downloaded
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
        from .transcribe import FasterWhisperTranscriber

        return FasterWhisperTranscriber(args.model)

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


def _models(args: argparse.Namespace) -> int:
    """Model kataloğunu ve yerel önbellek durumunu listeler (ağa çıkmaz)."""
    rows = [
        {
            "id": info.id,
            "size_mb": info.size_mb,
            "speed": info.speed,
            "accuracy": info.accuracy,
            "downloaded": is_downloaded(info.id),
        }
        for info in KATALOG
    ]
    if args.json:
        _emit({"event": "models", "models": rows})
        return 0
    print(f"{'model':<16}{'boyut':>10}{'hız':>6}{'doğruluk':>10}{'önbellek':>10}")
    for row in rows:
        cached = "var" if row["downloaded"] else "yok"
        print(f"{row['id']:<16}{row['size_mb']:>7} MB{row['speed']:>6}{row['accuracy']:>10}{cached:>10}")
    return 0


def build_parser() -> argparse.ArgumentParser:
    p = argparse.ArgumentParser(prog="subtitle-engine")
    p.add_argument("--version", action="version", version=f"%(prog)s protocol {PROTOCOL_VERSION}")
    sub = p.add_subparsers(dest="command", required=True)

    t = sub.add_parser("transcribe", help="videodan altyazı üret")
    t.add_argument("video", type=Path)
    t.add_argument("-o", "--out-dir", type=Path, default=Path("output"))
    t.add_argument("-l", "--language", default=None, help="ör. tr, en (boşsa otomatik algılanır)")
    t.add_argument("-m", "--model", default="small", help="katalog modeli (models komutu listeler) ya da tiny/turbo gibi faster-whisper adı")
    t.add_argument("-f", "--format", nargs="+", choices=subtitles.FORMATS, default=["srt"])
    t.add_argument("--max-chars", type=int, default=84)
    t.add_argument("--max-duration", type=float, default=6.0)
    t.add_argument("--json", action="store_true", help="stdout'a JSON olay akışı yaz")
    t.set_defaults(handler=_transcribe)

    r = sub.add_parser("render", help="stdin'deki cümle JSON'unu altyazı formatına çevir")
    r.add_argument("-f", "--format", choices=subtitles.FORMATS, default="srt")
    r.add_argument("-o", "--out", type=Path, default=None)
    r.set_defaults(handler=_render)

    ms = sub.add_parser("models", help="model kataloğunu ve indirme durumunu listele")
    ms.add_argument("--json", action="store_true", help="tek satır JSON yaz")
    ms.set_defaults(handler=_models)
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
