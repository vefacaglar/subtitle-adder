#!/bin/sh
# Runs an agent definition (.claude/agents/<agent>.md) as the system prompt in pi, using DeepSeek (opencode).
# Usage: scripts/pi-agent.sh <agent> "<task>"     e.g. scripts/pi-agent.sh subtitle-engineer "..."
# Set the model with PI_MODEL (default: DeepSeek flash, thinking max).
# explorer and module-reviewer run read-only (read,bash); the others also get edit/write.
# Sessions are saved (~/.pi/agent/sessions); reopen one with `pi --session <id>`.
set -eu

agent="${1:?agent name required}"
task="${2:?task required}"
root="$(cd "$(dirname "$0")/.." && pwd)"
definition="$root/.claude/agents/$agent.md"
[ -f "$definition" ] || { echo "agent definition not found: $definition" >&2; exit 2; }

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
