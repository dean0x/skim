#!/usr/bin/env bash
# Live acceptance for the PostToolUse compression hook. Manual: it needs Claude auth,
# starts real `claude -p` sessions in auto mode and spends tokens. CI does not run it.
# Env: SKIM_BIN (default target/debug/skim) CLAUDE_BIN MAX_TURNS=10 MAX_BUDGET_USD=1.00
#      LATENCY_N=50 KEEP=1 (keep the scratch dir) OFFLINE=1 (skip every claude run)
set -euo pipefail

REPO=$(cd "$(dirname "${BASH_SOURCE[0]}")/../../../.." && pwd -P)
SKIM_BIN=${SKIM_BIN:-$REPO/target/debug/skim}
CLAUDE_BIN=${CLAUDE_BIN:-claude}
MAX_TURNS=${MAX_TURNS:-10}
MAX_BUDGET_USD=${MAX_BUDGET_USD:-1.00}
LATENCY_N=${LATENCY_N:-50}
KEEP=${KEEP:-0}
OFFLINE=${OFFLINE:-0}
MARKER='[skim]'

die() { echo "error: $*" >&2; exit 2; }
for t in jq perl git cmp awk od; do command -v "$t" >/dev/null || die "missing tool: $t"; done
[ -x "$SKIM_BIN" ] || die "no skim binary at $SKIM_BIN (run: cargo build -p rskim)"
SKIM_BIN=$(cd "$(dirname "$SKIM_BIN")" && pwd -P)/$(basename "$SKIM_BIN")
if [ "$OFFLINE" != 1 ]; then
  command -v "$CLAUDE_BIN" >/dev/null || die "missing tool: $CLAUDE_BIN"
  [ -z "${ANTHROPIC_BASE_URL:-}" ] || die "ANTHROPIC_BASE_URL is set: run without a proxy"
fi

S=$(cd "$(mktemp -d)" && pwd -P)
trap 'if [ "$KEEP" = 1 ]; then echo "kept: $S"; else rm -rf "$S"; fi' EXIT
mkdir -p "$S"/{bin,raw,canary,out,cmp,proj,control,probe}
RES=$S/results.tsv; : > "$RES"

# The hook and every stub tool run with this cache dir and PATH. Nothing here sets HOME for claude.
SKIM_ENV=(SKIM_CACHE_DIR="$S/cache" SKIM_ANALYTICS_DB="$S/cache/analytics.db" SKIM_DISABLE_ANALYTICS=1
  SKIM_WRAPPERS_DIR="$S/wrappers" PATH="$S/bin:$PATH")
AGENT_ENV=(HOME="$S/home" CLAUDE_CONFIG_DIR="$S/global" CURSOR_CONFIG_DIR="$S/cursor"
  GEMINI_CONFIG_DIR="$S/gemini" COPILOT_CONFIG_DIR="$S/copilot" CODEX_CONFIG_DIR="$S/codex"
  CODEX_HOME="$S/codex" CRUSH_CONFIG_DIR="$S/crush")
skim_env() { env -u SKIM_PASSTHROUGH -u SKIM_REWRITTEN_FROM -u SKIM_SESSION_ID "${SKIM_ENV[@]}" "$@"; }

record() { printf '%s\t%s\t%s\n' "$1" "$2" "$3" >> "$RES"; }
check() { local n=$1 d=$2; shift 2; if "$@" >/dev/null 2>&1; then record PASS "$n" ""; else record FAIL "$n" "$d"; fi; }
lacks() { ! grep -qF -- "$1" "$2"; }
has_all_lines() { local l; while IFS= read -r l; do grep -qF -- "$l" "$2" || return 1; done < "$1"; }
same() {  # label expected_file actual_file: byte equality
  if cmp -s "$2" "$3"; then record PASS "$1" ""; return; fi
  record FAIL "$1" "$(cmp "$2" "$3" 2>&1 | sed -e 's/^.* differ: /differ: /' -e 's/^cmp: EOF.*/one is a prefix of the other/' | sed -n 1p); expected $(wc -c < "$2" | tr -d ' ') B, got $(wc -c < "$3" | tr -d ' ') B"
}

# ---- Fixtures: stub tools, code files, run-time canaries ----
token() { printf 'zq%s' "$(od -An -N5 -tx1 /dev/urandom | tr -d ' \n')"; }
make_stub() { printf '#!/bin/sh\ncat "%s"\nexit %s\n' "$3" "$2" > "$S/bin/$1"; chmod +x "$S/bin/$1"; }
make_py() {
  local t d i; t=$(token); echo "$t" > "$S/canary/$1"
  { echo '"""Account helpers."""'; echo 'from decimal import Decimal'
    for i in 1 2 3 4 5 6 7 8; do
      # Pseudo mode keeps comments inside function bodies: the comments sit between the functions.
      printf '\n\n# step %s: walk the rows once and keep a running total\n' "$i"
      printf '# the rate is applied last so the rounding happens a single time\n'
      if [ "$i" = 4 ]; then printf '# review note %s\n' "$t"; fi
      printf 'def step_%s(rows, rate):\n    total = Decimal(0)\n    for row in rows:\n        total += row.amount * rate\n    return total\n' "$i"
    done; } > "$S/raw/$1.out"
  for d in proj control; do mkdir -p "$S/$d/src"; cp "$S/raw/$1.out" "$S/$d/src/$1.py"; done
}
make_fixtures() {
  local t f; t=$(token); echo "$t" > "$S/canary/pytest"
  printf '%s\n' '============================= test session starts ==============================' \
    'platform darwin -- Python 3.12.1, pytest-8.4.2, pluggy-1.5.0' "rootdir: $S/proj" 'collected 14 items' '' \
    'tests/test_billing.py .......                                            [ 50%]' \
    "tests/test_$t.py .......                                            [100%]" '' \
    '============================== 14 passed in 0.12s ==============================' > "$S/raw/pytest.out"
  t=$(token); echo "$t" > "$S/canary/mypy"
  printf '%s\n' 'src/billing.py:12: error: Incompatible return value type (got "str", expected "int")  [return-value]' \
    "src/$t.py:30: error: Argument 1 to \"step\" has incompatible type \"int\"; expected \"str\"  [arg-type]" \
    'Found 2 errors in 2 files (checked 4 source files)' > "$S/raw/mypy.out"
  awk 'BEGIN { s = ""; n = 0
    while (1) { l = sprintf("test parser::tests::case_%d ... ok\n", n); if (length(s) + length(l) > 28500) break; s = s l; n++ }
    printf "\nrunning %d tests\n%s\ntest result: ok. %d passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s", n, s, n }' > "$S/raw/big.out"
  make_stub pytest 0 "$S/raw/pytest.out"; make_stub mypy 1 "$S/raw/mypy.out"; make_stub cargo 0 /dev/null
  for f in billing ledger report; do make_py "$f"; done
  git -C "$S/proj" init -q; git -C "$S/control" init -q
  jq -n --arg c "$S/cache" '{env: {SKIM_CACHE_DIR: $c, SKIM_DISABLE_ANALYTICS: "1"}}' > "$S/settings.json"
}

# ---- Expected bytes: the same hook binary, fed a built PostToolUse input ----
key_of() {
  local k
  case "$1" in
    pytest) echo pytest ;;
    mypy*) echo mypy ;;
    "cat src/"*.py) k=${1#cat src/}; echo "${k%.py}" ;;
    *) return 1 ;;
  esac
}
post_input() {  # command raw_file [permission_mode]
  jq -cn --arg cmd "$1" --rawfile out "$2" --arg mode "${3:-auto}" '{
    session_id: "00000000-0000-4000-8000-000000000001", cwd: "/work/project", permission_mode: $mode,
    hook_event_name: "PostToolUse", tool_name: "Bash", tool_input: {command: $cmd},
    tool_response: {stdout: ($out | sub("\n+$"; "")), stderr: "", interrupted: false, isImage: false, noOutputExpected: false},
    tool_use_id: "toolu_00000000000000000000000a"}'
}
hook() { skim_env "$SKIM_BIN" rewrite --hook --agent claude-code; }
view_of() {  # dir command: fill dir/exp.{out,err,txt}; fail when the hook gives no output
  local d=$1 key; mkdir -p "$d"; key=$(key_of "$2") || return 1
  post_input "$2" "$S/raw/$key.out" | hook > "$d/resp.json" 2> "$d/hook.err" || true
  [ -s "$d/resp.json" ] || return 1
  jq -j '.hookSpecificOutput.updatedToolOutput.stdout' "$d/resp.json" > "$d/exp.out"
  jq -j '.hookSpecificOutput.updatedToolOutput.stderr' "$d/resp.json" > "$d/exp.err"
  { cat "$d/exp.out"; printf '\n'; cat "$d/exp.err"; } > "$d/exp.txt"
}
check_view() {  # label dir command
  local key canary; key=$(key_of "$3"); canary=$(cat "$S/canary/$key")
  if ! view_of "$2" "$3"; then record FAIL "$1:view" "the hook gave no output for the built input"; return 1; fi
  check "$1:marker" "no skim marker line in the expected stderr" grep -qF "$MARKER" "$2/exp.err"
  check "$1:view-drops-canary" "the compressed view keeps the canary" lacks "$canary" "$2/exp.txt"
}

# ---- Install into the scratch project ----
install_hooks() {
  ( cd "$S/proj" && skim_env env "${AGENT_ENV[@]}" "$SKIM_BIN" init --project --agent claude-code --no-wrappers --no-guidance ) \
    > "$S/init.out" 2>&1 || die "skim init failed: $(tail -n 3 "$S/init.out")"
  local pid i outside; pid=$(sed -n 's/.*build started (PID \([0-9]*\)).*/\1/p' "$S/init.out")
  for i in $(seq 1 100); do kill -0 "$pid" 2>/dev/null || break; sleep 0.2; done
  outside=$(grep -oE '/[^ ()"]+' "$S/init.out" | grep -vF -e "$S/" -e "$SKIM_BIN" | tr '\n' ' ' || true)
  check init:inside-scratch "init output names paths outside the scratch dir: $outside" test -z "$outside"
  check init:settings "Pre and Post entries expected, no PostToolUseFailure" \
    jq -e '.hooks | has("PreToolUse") and has("PostToolUse") and (has("PostToolUseFailure") | not)' "$S/proj/.claude/settings.json"
  check init:pin "hook script does not pin $SKIM_BIN" grep -qF "$SKIM_BIN" "$S/proj/.claude/hooks/skim-rewrite.sh"
}

# ---- Latency of the hook process ----
PERL_LATENCY='use Time::HiRes qw(gettimeofday tv_interval); my ($bin, $in, $n) = @ARGV; my @t;
for my $i (0 .. $n) { my $t0 = [gettimeofday]; my $pid = fork(); die "fork" unless defined $pid;
  if (!$pid) { open(STDIN, "<", $in) or exit 99; open(STDOUT, ">", "/dev/null"); open(STDERR, ">", "/dev/null");
    exec {$bin} $bin, "rewrite", "--hook", "--agent", "claude-code"; exit 98 }
  waitpid($pid, 0); die "hook exit $?" if $?; push @t, tv_interval($t0) * 1000 if $i; }
@t = sort { $a <=> $b } @t; my $at = sub { my $k = int($_[0] * @t + 0.999999) - 1; $t[$k < 0 ? 0 : $k] };
printf "n=%d p50=%.1fms p95=%.1fms max=%.1fms", $n, $at->(0.5), $at->(0.95), $t[-1];'
latency() {  # label permission_mode
  local in=$S/lat-$2.json
  post_input "cargo test" "$S/raw/big.out" "$2" > "$in"
  record INFO "latency:$1" "$(skim_env perl -e "$PERL_LATENCY" "$SKIM_BIN" "$in" "$LATENCY_N") stdout=$(jq '.tool_response.stdout | length' "$in") chars; view bytes=$(hook < "$in" | wc -c | tr -d ' ')"
}

# ---- Reading a stream-json run ----
bash_ids() { jq -r 'select(.type == "assistant") | .message.content[]? | select(.type == "tool_use" and .name == "Bash") | .id' "$1"; }
cmd_of() { jq -r --arg id "$2" 'select(.type == "assistant") | .message.content[]? | select(.type == "tool_use" and .id == $id) | .input.command' "$1" | sed -n 1p; }
sub_calls() { jq -r 'select(.type == "assistant" and .parent_tool_use_id != null) | .message.content[]? | select(.type == "tool_use" and .name == "Bash") | .id' "$1"; }
max_same_message() {
  jq -s '[.[] | select(.type == "assistant") | {m: .message.id, t: [.message.content[]? | select(.type == "tool_use" and .name == "Bash") | .id]}]
    | group_by(.m) | map([.[].t[]] | unique | length) | max // 0' "$1"
}
result_of() {  # file id: the tool_result text and tool_use_result of one call
  jq -c --arg id "$2" 'limit(1; select(.type == "user") as $u | $u.message.content[]?
    | select(type == "object" and .type == "tool_result" and .tool_use_id == $id)
    | {err: (.is_error // false), tur: $u.tool_use_result,
       text: (.content | if type == "array" then map(.text // "") | join("") else (. // "") end)})' "$1"
}
count_lines() { printf '%s' "$1" | grep -c . || true; }

check_call() {  # run id command
  local run=$1 d=$S/cmp/$1-$2 key; mkdir -p "$d"
  key=$(key_of "$3") || { record FAIL "$run:command" "the model ran another command: $3"; return; }
  result_of "$S/out/$run.jsonl" "$2" > "$d/res.json"
  [ -s "$d/res.json" ] || { record FAIL "$run:$key" "no tool_result record for $2"; return; }
  jq -j '.text' "$d/res.json" > "$d/got.txt"; jq -j '(.tur | objects | .stdout) // ""' "$d/res.json" > "$d/got.out"
  jq -j '(.tur | objects | .stderr) // ""' "$d/res.json" > "$d/got.err"; jq -j '.tur | tostring' "$d/res.json" > "$d/got.tur"
  if [ "$run" = control ]; then   # no hook: the raw bytes, no skim line
    printf '%s' "$(cat "$S/raw/$key.out")" > "$d/exp.out"
    same "$run:$key:stdout" "$d/exp.out" "$d/got.out"; check "$run:$key:no-marker" "marker in a no-hook run" lacks "$MARKER" "$d/got.txt"
  elif [ "$key" = mypy ]; then   # failed run: PostToolUseFailure, the host keeps the raw text
    check "$run:$key:is-error" "a failing stub must give is_error" jq -e '.err' "$d/res.json"
    check "$run:$key:raw-text" "tool_result lacks a raw stub line" has_all_lines "$S/raw/mypy.out" "$d/got.txt"
    check "$run:$key:raw-tur" "tool_use_result lacks a raw stub line" has_all_lines "$S/raw/mypy.out" "$d/got.tur"
    check "$run:$key:no-marker" "skim line on a failed run" lacks "$MARKER" "$d/got.txt"
    check "$run:$key:canary-seen" "the raw canary is missing" grep -qF "$(cat "$S/canary/mypy")" "$d/got.txt"
  elif check_view "$run:$key" "$d" "$3"; then
    check "$run:$key:shape" "tool_use_result is not an object" jq -e '.tur | type == "object"' "$d/res.json"
    same "$run:$key:tur.stdout" "$d/exp.out" "$d/got.out"; same "$run:$key:tur.stderr" "$d/exp.err" "$d/got.err"
    same "$run:$key:tool_result" "$d/exp.txt" "$d/got.txt"
  fi
}
check_reply() {  # run compressed|raw canary...: what the model said it saw (TP-12)
  local run=$1 want=$2 c reply; shift 2
  reply=$(jq -r 'select(.type == "result") | .result // ""' "$S/out/$run.jsonl")
  if [ "$want" = raw ]; then
    for c in "$@"; do check "$run:reply-raw" "the model did not quote the raw canary" test "${reply#*"$c"}" != "$reply"; done
    check "$run:reply-no-marker" "the model quoted a skim line" test "${reply#*"$MARKER"}" = "$reply"
    return
  fi
  check "$run:reply-marker" "the model did not quote the marker" test "${reply#*"$MARKER"}" != "$reply"
  for c in "$@"; do check "$run:reply-canary" "the model knows the canary $c" test "${reply#*"$c"}" = "$reply"; done
}
verify_run() {  # run ncalls
  local run=$1 f=$S/out/$1.jsonl r ids id
  r=$(jq -c 'select(.type == "result")' "$f" | tail -n 1)
  [ -n "$r" ] || { record FAIL "$run:result" "no result record; see $S/out/$run.err"; return; }
  record INFO "$run:usage" "$(jq -r '"turns=\(.num_turns) cost=$\(.total_cost_usd) in=\(.usage.input_tokens) out=\(.usage.output_tokens) cache_read=\(.usage.cache_read_input_tokens) cache_create=\(.usage.cache_creation_input_tokens) session=\(.session_id)"' <<< "$r")"
  printf '%s\t%s\n' "$run" "$(jq -r '.session_id' <<< "$r")" >> "$S/sessions.tsv"
  check "$run:success" "result is not a success" jq -e '.subtype == "success" and (.is_error | not)' <<< "$r"
  check "$run:no-denials" "permission_denials is not empty" jq -e '.permission_denials == []' <<< "$r"
  ids=$(bash_ids "$f")
  check "$run:calls" "expected $2 Bash call(s), saw $(count_lines "$ids")" test "$(count_lines "$ids")" = "$2"
  while IFS= read -r id <&3; do
    if [ -n "$id" ]; then check_call "$run" "$id" "$(cmd_of "$f" "$id")"; fi
  done 3<<< "$ids"
}

# ---- Sessions ----
HAS_TURNS=0; HAS_BUDGET=0
claude_run() {  # name dir settings prompt
  local a=(-p --permission-mode auto --setting-sources project --settings "$3" --output-format stream-json --verbose)
  if [ "$HAS_BUDGET" = 1 ]; then a+=(--max-budget-usd "$MAX_BUDGET_USD"); fi
  if [ "$HAS_TURNS" = 1 ]; then a+=(--max-turns "$MAX_TURNS"); fi
  ( cd "$2" && printf '%s' "$4" | skim_env "$CLAUDE_BIN" "${a[@]}" ) > "$S/out/$1.jsonl" 2> "$S/out/$1.err" \
    || record INFO "$1:exit" "claude exited non-zero; see $S/out/$1.err"
}
ASK='This is a tool-output test. Run the Bash commands below exactly as written. Do not run any other command.'
P_SINGLE="$ASK

1. \`pytest\`

Reply with the exact full output text you received, and the name of every test file you saw."
P_BATCH="$ASK Issue all three in ONE message, as three parallel tool calls.

1. \`cat src/billing.py\`
2. \`cat src/ledger.py\`
3. \`pytest\`

Reply with a numbered list. For each command quote the exact output text you received. For commands 1 and 2 also list every function name and every comment line you saw, or say none."
P_SUB="This is a tool-output test. Use the Task tool to start exactly one subagent. Tell it: Run \`cat src/report.py\` with the Bash tool and nothing else, then reply with the exact output text you received and every comment line you saw, or say none. Then reply with the subagent's answer."
P_FAIL="$ASK

1. \`mypy src\`

Reply with the exact full output text you received, and say if the call failed."
live_runs() {
  local help; help=$("$CLAUDE_BIN" --help 2>&1 || true)
  if [[ $help == *--max-turns* ]]; then HAS_TURNS=1; fi
  if [[ $help == *--max-budget-usd* ]]; then HAS_BUDGET=1; fi
  record INFO claude "$("$CLAUDE_BIN" --version 2>&1 | sed -n 1p); --max-turns supported=$HAS_TURNS; --max-budget-usd supported=$HAS_BUDGET"
  claude_run single "$S/proj" "$S/settings.json" "$P_SINGLE"; verify_run single 1
  check_reply single compressed "$(cat "$S/canary/pytest")"
  claude_run batch "$S/proj" "$S/settings.json" "$P_BATCH"; verify_run batch 3
  check batch:parallel "the calls were not in one assistant message" test "$(max_same_message "$S/out/batch.jsonl")" -ge 3
  check_reply batch compressed "$(cat "$S/canary/pytest")" "$(cat "$S/canary/billing")" "$(cat "$S/canary/ledger")"
  claude_run subagent "$S/proj" "$S/settings.json" "$P_SUB"; verify_run subagent 1
  check subagent:sidechain "no Bash call with a parent_tool_use_id in the stream" test -n "$(sub_calls "$S/out/subagent.jsonl")"
  check_reply subagent compressed "$(cat "$S/canary/report")"
  claude_run failing "$S/proj" "$S/settings.json" "$P_FAIL"; verify_run failing 1
  check_reply failing raw "$(cat "$S/canary/mypy")"
  claude_run control "$S/control" "$S/settings.json" "$P_SINGLE"; verify_run control 1
}
probe_run() {  # A11: the host reaction to a PreToolUse-shaped answer on PostToolUse input; no pass or fail
  local t f=$S/out/probe.jsonl id ran=no; t=$(token)
  cat > "$S/probe/hook.sh" <<EOF
#!/bin/sh
cat > /dev/null
echo ran >> "$S/probe/ran"
echo '{"hookSpecificOutput":{"hookEventName":"PreToolUse","updatedInput":{"command":"echo rewritten-$t"}}}'
EOF
  chmod +x "$S/probe/hook.sh"
  jq -n --arg h "$S/probe/hook.sh" '{hooks: {PostToolUse: [{matcher: "Bash", hooks: [{type: "command", command: $h}]}]}}' > "$S/probe/settings.json"
  claude_run probe "$S/probe" "$S/probe/settings.json" "$ASK

1. \`echo original-$t\`

Reply with the exact output text you received."
  id=$(bash_ids "$f" | sed -n 1p)
  if [ -s "$S/probe/ran" ]; then ran=yes; fi
  record INFO probe:hook-ran "$ran"
  record INFO probe:tool_result "$(result_of "$f" "$id" | jq -c '{err, text: .text[0:160], tur: (.tur | tostring | .[0:160])}')"
  record INFO probe:system "$(jq -c 'select(.type == "system" and (.subtype | IN("init", "permission_check_status") | not))' "$f" | sed -n 1,3p | cut -c1-300 | tr '\n' ' ')"
  record INFO probe:stderr "$(head -c 300 "$S/out/probe.err" | tr '\n' ' ')"
}

print_transcript_jq() {
  echo; echo "Transcript check. Run it yourself: this script never reads ~/.claude."
  echo "Session ids:"; awk -F'\t' '{ printf "  %-9s %s\n", $1, $2 }' "$S/sessions.tsv" 2>/dev/null || true
  echo "Canaries (gone with the scratch dir unless KEEP=1):"; for f in "$S"/canary/*; do echo "  $(basename "$f") $(cat "$f")"; done
  cat <<'EOF'
SID=<session id>; CANARY=<canary>
{ find ~/.claude/projects -name "$SID.jsonl"; find ~/.claude/projects -path "*/$SID/*" -name '*.jsonl'; } | while read -r f; do echo "## $f"; jq -r --arg c "$CANARY" 'select(.type=="user") | . as $u | .message.content[]? | select(type=="object" and .type=="tool_result") | (.content | if type=="array" then map(.text // "") | join("") else (. // "") end) as $t | "== \(.tool_use_id) is_error=\(.is_error // false) text_bytes=\($t | utf8bytelength) canary=\($t | contains($c))\n" + ($t | .[0:160]) + "\n-- toolUseResult: " + ($u.toolUseResult | if type=="object" then "stdout_bytes=\((.stdout // "") | utf8bytelength) canary=\((.stdout // "") | contains($c)) stderr=\((.stderr // "") | @json)" else (tostring | .[0:120]) end)' "$f"; done
EOF
}

# ---- Main ----
make_fixtures
install_hooks
for c in pytest "cat src/billing.py" "cat src/ledger.py" "cat src/report.py"; do
  check_view "expected:$(key_of "$c")" "$S/cmp/expected-$(key_of "$c")" "$c" || true
done
record INFO latency:binary "$SKIM_BIN (a debug build runs slower than release: point SKIM_BIN at a release binary for PR numbers)"
latency default-mode default; latency auto-mode auto
if [ "$OFFLINE" = 1 ]; then record INFO live "skipped (OFFLINE=1)"
elif grep -q '^FAIL' "$RES"; then record INFO live "skipped: fix the failed set-up checks first"
else live_runs; probe_run; fi

printf '\n%-6s %-36s %s\n' STATUS CHECK DETAIL
awk -F'\t' '{ printf "%-6s %-36s %s\n", $1, $2, $3 }' "$RES"
[ "$OFFLINE" = 1 ] || print_transcript_jq
fails=$(awk -F'\t' '$1 == "FAIL" { n++ } END { print n + 0 }' "$RES")
printf '\n%s check(s) failed\n' "$fails"
[ "$fails" = 0 ]
