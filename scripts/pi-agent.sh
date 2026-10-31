#!/bin/sh
# .claude/agents/<ajan>.md tanımını pi'ye sistem promptu olarak verip DeepSeek (opencode) ile çalıştırır.
# Kullanım: scripts/pi-agent.sh <ajan> "<görev>"     ör. scripts/pi-agent.sh subtitle-engineer "..."
# Model PI_MODEL ile değişir (varsayılan: DeepSeek flash, thinking max).
# explorer ve module-reviewer salt okunur çalışır (read,bash); diğerleri edit/write da alır.
# Oturumlar kaydedilir (~/.pi/agent/sessions); `pi --session <id>` ile açılır.
set -eu

agent="${1:?ajan adı gerekli}"
task="${2:?görev gerekli}"
root="$(cd "$(dirname "$0")/.." && pwd)"
definition="$root/.claude/agents/$agent.md"
[ -f "$definition" ] || { echo "ajan tanımı yok: $definition" >&2; exit 2; }

model="${PI_MODEL:-opencode-go/deepseek-v4.1-flash:max}"
case "$agent" in
  explorer|module-reviewer) tools="read,bash" ;;
  *)                        tools="read,bash,edit,write" ;;
esac

cd "$root"
exec pi -p \
  --model "$model" \
  --tools "$tools" \
  --append-system-prompt "$definition" \
  --name "subtitle-adder:$agent" \
  "$task" < /dev/null
