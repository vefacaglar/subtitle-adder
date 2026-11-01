"""Engine CLI. With `--json` it writes one JSON event per line to stdout (the native apps read it).

Events:
  {"event": "stage", "stage": "extracting_audio|downloading_model|loading_model|transcribing|diarizing|segmenting|translating", "fraction": 0.42|null}
  {"event": "models", "models": [{"id", "kind", "backend", "languages", "size_mb", "disk_mb", "speed", "accuracy", "license", "downloaded"}]}
  {"event": "deleted", "id": "small", "freed_mb": 465}
  {"event": "result", "language": "tr", "duration": 12.3, "audio": "...", "outputs": [...], "sentences": [...]}
  {"event": "translation", "language": "es", "sentences": [...]}
  {"event": "error", "message": "..."}
"""
import argparse
import json
import os
import sys
from pathlib import Path

from . import cuda, i18n, models, pipeline, subtitles
from .models import KATALOG, Sentence, cached_disk_mb, delete_model, disk_mb, is_downloaded
from .i18n import t
from .sentences import SegmenterConfig

PROTOCOL_VERSION = 1


def _emit(payload: dict) -> None:
    sys.stdout.write(json.dumps(payload, ensure_ascii=False) + "\n")
    sys.stdout.flush()


def _transcribe(args: argparse.Namespace) -> int:
    if args.speakers is not None and not args.diarize:
        raise ValueError(t("speakers_needs_diarize"))
    def on_stage(stage: str, fraction: float | None) -> None:
        if args.json:
            _emit({"event": "stage", "stage": stage, "fraction": fraction})
        elif fraction is None:
            print(f"… {stage}", file=sys.stderr)

    def factory():
        from .transcribe import create_transcriber

        return create_transcriber(args.model)

    import inspect

    run_kwargs: dict = {
        "language": args.language,
        "model": args.model,
        "formats": tuple(args.format),
        "config": SegmenterConfig(max_chars=args.max_chars, max_duration=args.max_duration),
        "on_stage": on_stage,
        "diarize": args.diarize,
        "num_speakers": args.speakers,
    }
    if args.json and "on_partial" in inspect.signature(pipeline.run).parameters:
        run_kwargs["on_partial"] = lambda partial, lang: _emit(
            {
                "event": "partial",
                "language": lang,
                "sentences": [s.to_dict() for s in partial],
            }
        )

    result = pipeline.run(
        args.video.resolve(),
        factory,
        args.out_dir.resolve(),
        **run_kwargs,
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
        print(t("summary", language=result.language, count=len(result.sentences)))
        for path in result.outputs:
            print(f"-> {path}")
    return 0


def _render(args: argparse.Namespace) -> int:
    """Reads sentence JSON from stdin (as edited in the app) and writes it in the requested format."""
    sentences = [Sentence.from_dict(d) for d in json.load(sys.stdin)]
    if args.out:
        subtitles.write(sentences, args.out, args.format)
    else:
        sys.stdout.write(subtitles.render(sentences, args.format))
    return 0


def _translate(args: argparse.Namespace) -> int:
    """Translates the sentence JSON on stdin with a local model; one sentence per line."""
    from .translate import check_request, create_translator

    def on_stage(stage: str, fraction: float | None) -> None:
        if args.json:
            _emit({"event": "stage", "stage": stage, "fraction": fraction})
        elif fraction is None:
            print(f"… {stage}", file=sys.stderr)

    # A language/model problem must surface as a (localized) `error` event before any download starts.
    info = check_request(args.model, args.to, args.source)
    sentences = [Sentence.from_dict(d) for d in json.load(sys.stdin)]
    if models.require_download(args.model):
        on_stage("downloading_model", 0.0)
        models.ensure_model(args.model, on_progress=lambda f: on_stage("downloading_model", f))
    if info.backend == "llama":
        from . import llama

        if llama.require_llama_download():
            on_stage("downloading_model", 0.0)
            llama.ensure_llama(on_progress=lambda f: on_stage("downloading_model", f))
    elif cuda.needs_cuda_download():
        on_stage("downloading_model", 0.0)
        cuda.ensure_cuda(on_progress=lambda f: on_stage("downloading_model", f))
    models.go_offline()

    on_stage("loading_model", None)
    translator = create_translator(args.model)
    try:
        on_stage("translating", 0.0)
        import inspect

        translate_kwargs: dict = {
            "target": args.to,
            "source": args.source,
            "on_progress": lambda f: on_stage("translating", f),
        }
        if args.json and "on_batch" in inspect.signature(translator.translate).parameters:
            translate_kwargs["on_batch"] = lambda partial: _emit(
                {
                    "event": "translation",
                    "language": args.to,
                    "sentences": [s.to_dict() for s in partial],
                }
            )
        translated = translator.translate(sentences, **translate_kwargs)
    finally:
        close = getattr(translator, "close", None)
        if callable(close):
            close()
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
    """Lists the model catalog and local cache status (no network)."""
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
        f"{t('col_model'):<22}{t('col_kind'):>14}{t('col_size'):>10}{t('col_disk'):>10}"
        f"{t('col_speed'):>6}{t('col_accuracy'):>10}{t('col_license'):>20}{t('col_cache'):>10}"
    )
    for row in rows:
        disk = "—" if row["disk_mb"] is None else f"{row['disk_mb']} MB"
        cached = t("cached_yes") if row["downloaded"] else t("cached_no")
        print(
            f"{row['id']:<22}{row['kind']:>14}{row['size_mb']:>7} MB{disk:>10}"
            f"{row['speed']:>6}{row['accuracy']:>10}{row['license']:>20}{cached:>10}"
        )
    return 0


def _delete_model(args: argparse.Namespace) -> int:
    """Deletes the cached files of a downloaded model."""
    freed = delete_model(args.id)
    if args.json:
        _emit({"event": "deleted", "id": args.id, "freed_mb": freed})
    else:
        print(t("deleted", id=args.id, freed=freed))
    return 0


_LANG_HELP = f"language of user-facing messages (overrides {i18n.ENV_VAR}; anything but en means tr)"


def build_parser() -> argparse.ArgumentParser:
    p = argparse.ArgumentParser(prog="subtitle-engine")
    p.add_argument("--lang", default=None, metavar="tr|en", help=_LANG_HELP)
    p.add_argument(
        "--offline", action="store_true",
        help="never use the network, not even to download models (also SUBTITLE_ENGINE_OFFLINE=1)",
    )
    # Same option after the subcommand; separate dest so a missing sub-level value cannot clobber the global one.
    lang_parent = argparse.ArgumentParser(add_help=False)
    lang_parent.add_argument("--lang", dest="sub_lang", default=None, metavar="tr|en", help=_LANG_HELP)
    p.add_argument("--version", action="version", version=f"%(prog)s protocol {PROTOCOL_VERSION}")
    sub = p.add_subparsers(dest="command", required=True)

    t = sub.add_parser("transcribe", parents=[lang_parent], help="generate subtitles from a video")
    t.add_argument("video", type=Path)
    t.add_argument("-o", "--out-dir", type=Path, default=Path("output"))
    t.add_argument("-l", "--language", default=None, help="e.g. tr, en (auto-detected if omitted)")
    t.add_argument("-m", "--model", default="small", help="model id (see the models command); other names are passed to faster-whisper")
    t.add_argument("-f", "--format", nargs="+", choices=subtitles.FORMATS, default=["srt"])
    t.add_argument("--max-chars", type=int, default=84)
    t.add_argument("--max-duration", type=float, default=6.0)
    t.add_argument("--diarize", action="store_true", help="separate speakers (local); adds a speaker to each sentence")
    t.add_argument("--speakers", type=int, default=None, help="number of speakers (only with --diarize; auto if omitted)")
    t.add_argument("--json", action="store_true", help="write a JSON event stream to stdout")
    t.set_defaults(handler=_transcribe)

    tr = sub.add_parser("translate", parents=[lang_parent], help="translate the sentence JSON on stdin with a local model")
    tr.add_argument("--to", required=True, help="target language (e.g. tr, en, es, pt, ru)")
    tr.add_argument("--from", dest="source", default=None, help="source language; required for NLLB")
    tr.add_argument("-m", "--model", default="madlad-3b", help="translation model (see the models command)")
    tr.add_argument("--json", action="store_true", help="write a JSON event stream to stdout")
    tr.set_defaults(handler=_translate)

    r = sub.add_parser("render", parents=[lang_parent], help="convert the sentence JSON on stdin to a subtitle format")
    r.add_argument("-f", "--format", choices=subtitles.FORMATS, default="srt")
    r.add_argument("-o", "--out", type=Path, default=None)
    r.set_defaults(handler=_render)

    ms = sub.add_parser("models", parents=[lang_parent], help="list the model catalog and download status")
    ms.add_argument("--json", action="store_true", help="write a single JSON line")
    ms.set_defaults(handler=_models)

    dm = sub.add_parser("delete-model", parents=[lang_parent], help="delete a downloaded model from the cache")
    dm.add_argument("id", help="model id (see the models command)")
    dm.add_argument("--json", action="store_true", help="write a single JSON line")
    dm.set_defaults(handler=_delete_model)
    return p


def _force_utf8() -> None:
    """Windows pipes default to the ANSI code page and CRLF; the protocol is UTF-8 with LF."""
    for stream in (sys.stdin, sys.stdout, sys.stderr):
        try:
            if stream is sys.stdin:
                stream.reconfigure(encoding="utf-8")
            else:
                stream.reconfigure(encoding="utf-8", newline="\n")
        except (AttributeError, ValueError, OSError):
            pass  # stream was replaced (test, StringIO) or cannot be reconfigured


def main(argv: list[str] | None = None) -> int:
    _force_utf8()
    args = build_parser().parse_args(argv)
    i18n.set_language(args.sub_lang or args.lang)  # None keeps the SUBTITLE_ENGINE_LANG environment variable in charge
    if args.offline:
        os.environ[models.OFFLINE_ENV_VAR] = "1"
    # Only transcribe and translate may download; everything else is local from the start.
    if models.offline_requested() or args.handler not in (_transcribe, _translate):
        models.go_offline()
    try:
        return args.handler(args)
    except Exception as exc:  # the app receives every failure as a single kind of event
        if getattr(args, "json", False):
            _emit({"event": "error", "message": str(exc)})
        else:
            print(f"{t('error_prefix')}: {exc}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main())
