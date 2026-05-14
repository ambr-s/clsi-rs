#!/bin/bash
# Bench a clsi-rs deployment. Hits /status warm + compiles a hello-world doc.
# Usage: ./bench.sh https://clsi-amber.fly.dev "$TOKEN" [N_WARM_COMPILES]
set -eu
URL="${1:?usage: bench.sh URL TOKEN [N]}"
TOKEN="${2:?need TOKEN}"
N="${3:-5}"

AUTH="Authorization: Bearer $TOKEN"
PID="000000000000000000000000aaa"   # arbitrary but matches our project_id regex

JSON=$(cat <<'EOF'
{
  "compile": {
    "options": {
      "compiler": "pdflatex",
      "timeout": 60,
      "syncType": "full",
      "syncState": "bench"
    },
    "rootResourcePath": "main.tex",
    "resources": [
      {
        "path": "main.tex",
        "content": "\\documentclass{article}\\begin{document}Hello $(date +%s)\\end{document}"
      }
    ]
  }
}
EOF
)

echo "== /status warm x3 =="
for i in 1 2 3; do
  curl -sS -o /dev/null -H "$AUTH" -w "  %{http_code}  total=%{time_total}s\n" "$URL/status"
done

echo
echo "== compile (first — may be cold) =="
curl -sS -H "$AUTH" -H "Content-Type: application/json" \
  -d "$JSON" \
  -w "\n  HTTP=%{http_code}  total=%{time_total}s\n" \
  "$URL/project/$PID/compile" | head -c 800
echo

echo
echo "== compile warm x$N =="
for i in $(seq 1 $N); do
  curl -sS -o /dev/null -H "$AUTH" -H "Content-Type: application/json" \
    -d "$JSON" \
    -w "  $i: HTTP=%{http_code}  total=%{time_total}s\n" \
    "$URL/project/$PID/compile"
done
