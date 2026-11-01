"""Model-specific translation conventions, independent of the serving backend.

Sources:
https://ai.google.dev/gemma/docs/core/prompt-structure
https://ai.google.dev/gemma/docs/core/prompt-formatting-gemma4
https://huggingface.co/Qwen/Qwen3-4B
https://huggingface.co/Qwen/Qwen3.5-4B
https://huggingface.co/tencent/HY-MT1.5-7B
https://huggingface.co/tencent/Hunyuan-MT-7B

Names cover common Ollama tags and HF/GGUF paths. Unknown models retain the
generic subtitle prompt; no downloads or model selection changes happen here.
"""
import re
from dataclasses import dataclass


LANGUAGE_NAMES = {
    "en": "English", "tr": "Turkish", "pt": "Portuguese", "es": "Spanish",
    "ru": "Russian", "de": "German", "fr": "French", "ja": "Japanese", "zh": "Chinese",
}
_CHINESE_NAMES = {
    "en": "英语", "tr": "土耳其语", "pt": "葡萄牙语", "es": "西班牙语",
    "ru": "俄语", "de": "德语", "fr": "法语", "ja": "日语", "zh": "中文",
}


@dataclass(frozen=True)
class TranslationProfile:
    name: str = "generic"
    single_user: bool = False
    disable_thinking: bool = False
    soft_no_think: bool = False
    # Keep the conservative subtitle sampling defaults unless the model has
    # documented translation-specific recommendations (HY-MT).
    temperature: float = 0.2
    top_p: float = 0.9
    top_k: int | None = None
    repetition_penalty: float | None = None

    def chat_messages(self, system: str, user: str) -> list[dict[str, str]]:
        if self.soft_no_think:
            user += "\n/no_think"
        if self.single_user:
            return [{"role": "user", "content": f"{system}\n\n{user}"}]
        return [{"role": "system", "content": system}, {"role": "user", "content": user}]

    def generation_options(self, *, ollama: bool) -> dict:
        sampling = {"temperature": self.temperature, "top_p": self.top_p}
        # A model name does not identify an OpenAI-compatible serving backend.
        # Keep its sampler fields portable: e.g. llama.cpp's repeat_penalty and
        # vLLM's repetition_penalty are different extensions of that protocol.
        if ollama and self.top_k is not None:
            sampling["top_k"] = self.top_k
        if ollama and self.repetition_penalty is not None:
            sampling["repeat_penalty"] = self.repetition_penalty
        if ollama:
            options = {"options": {**sampling, "num_predict": 1536}}
            if self.disable_thinking:
                options["think"] = False
            return options
        options = {**sampling, "max_tokens": 1536}
        if self.disable_thinking:
            options["chat_template_kwargs"] = {"enable_thinking": False}
        return options


def profile_for_model(model: str) -> TranslationProfile:
    name = model.casefold()
    compact = re.sub(r"[-_\s]", "", name)
    if "translategemma" in compact:
        return TranslationProfile("translategemma", single_user=True)
    if "hymt1.5" in compact:
        return TranslationProfile("hy-mt1.5", single_user=True, temperature=0.7,
                                  top_p=0.6, top_k=20, repetition_penalty=1.05)
    if "hunyuanmt7b" in compact and "chimera" not in name:
        return TranslationProfile("hunyuan-mt", single_user=True, temperature=0.7,
                                  top_p=0.6, top_k=20, repetition_penalty=1.05)
    if re.search(r"gemma[-_\s]?4(?=[:/._\s-]|$)", name):
        return TranslationProfile("gemma4", disable_thinking=True)
    if re.search(r"gemma[-_\s]?(?:3n|3|2)(?=[:/._\s-]|$)", name):
        return TranslationProfile("gemma-legacy", single_user=True)
    if re.search(r"qwen[-_\s]?3\.5(?=[:/_\s-]|$)", name):
        return TranslationProfile("qwen3.5", disable_thinking=True)
    if re.search(r"qwen[-_\s]?3(?=[:/_\s-]|$)", name) and not any(
        variant in name for variant in ("thinking", "instruct", "next", "vl")
    ):
        return TranslationProfile("qwen3", disable_thinking=True, soft_no_think=True)
    return TranslationProfile()


def build_hy_mt_prompt(
    lines: list[str], target: str, source: str | None = None, *, context: list[str] | None = None,
    legacy: bool = False,
) -> str:
    """Use HY-MT's translation-only template and 1.5's optional reference context."""
    numbered = "\n".join(f"[{i}] {text}" for i, text in enumerate(lines, 1))
    if context:
        reference = "\n".join(context)
        return (
            f"{reference}\n参考上面的信息，把下面的文本翻译成{_CHINESE_NAMES[target]}，"
            f"注意不需要翻译上文，也不要额外解释：\n{numbered}"
        )
    if source == "zh" or target == "zh":
        if legacy:
            return f"把下面的文本翻译成{_CHINESE_NAMES[target]}，不要额外解释。\n\n{numbered}"
        return f"将以下文本翻译为{_CHINESE_NAMES[target]}，注意只需要输出翻译后的结果，不要额外解释：\n\n{numbered}"
    return f"Translate the following segment into {LANGUAGE_NAMES[target]}, without additional explanation.\n\n{numbered}"
