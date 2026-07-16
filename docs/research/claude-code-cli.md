# Driving Claude Code CLI (v2.1.211) as a subprocess from a Rust backend

> Researched 2026-07-16 for the Atlas build. Claims marked `uncertain`/`likely` were put
> through an adversarial verification pass; see `corrections.md` for what was refuted.

## Summary

Everything was verified by executing the local CLI at /home/alli/.local/bin/claude (v2.1.211), not from docs. Headless is `-p`/`--print`; `--output-format` is `text|json|stream-json`, and `stream-json` HARD-REQUIRES `--verbose` when combined with `-p` (errors out otherwise). stdout is clean NDJSON/JSON in all cases — every warning goes to stderr, so Atlas can parse stdout directly. Anthropic's official position (agent-sdk/overview) is that only TypeScript and Python SDKs exist and that other languages should "run the CLI programmatically with the `-p` flag and `--output-format json`" — the TS SDK itself just spawns this same binary (`pathToClaudeCodeExecutable` option), so a Rust backend shelling out is the sanctioned path, not a workaround. Two traps dominate the implementation: (1) `subtype` stays `"success"` on API/auth failures while `is_error:true` — you must branch on `is_error`/`terminal_reason`, never `subtype`; and (2) `--resume` is CWD-SCOPED — resuming from a different directory fails with a non-JSON stderr message and exit 1, so Atlas must respawn with the identical repo cwd. Auth is inherited for free from ~/.claude/.credentials.json (OAuth, apiKeySource:"none") with no API key; setting ANTHROPIC_API_KEY silently overrides the Max subscription and bills the API instead. `--mcp-config` was verified end-to-end with a custom stdio server — this is the clean way for Atlas to expose its own tools.

## Implementation notes

RECOMMENDATION: shell out to the CLI. This is Anthropic's documented guidance for non-TS/Python languages, and the TS SDK is itself a subprocess wrapper around the same binary (`pathToClaudeCodeExecutable`). Do NOT take a dependency on the unofficial Rust crates. Do NOT embed Node/the TS SDK.

CANONICAL ONE-SHOT INVOCATION (copy verbatim):
  claude -p "<prompt>" \
    --output-format stream-json --verbose \
    --permission-mode dontAsk \
    --allowedTools "Read,Glob,Grep,Edit,Bash(git diff *),Bash(cargo test *)" \
    --session-id <uuid-v4-you-generated> \
    --mcp-config '<inline json>' --strict-mcp-config \
    --max-turns 50 --max-budget-usd 5.00
with Command::current_dir(<cloned repo path>). `--verbose` is MANDATORY with stream-json+`-p`.

RUST SKELETON (tokio):
  let mut child = tokio::process::Command::new("claude")
      .current_dir(&repo_path)                 // <-- this is the ONLY way to target the clone
      .args(["-p", &prompt, "--output-format", "stream-json", "--verbose",
             "--permission-mode", "dontAsk", "--session-id", &session_uuid])
      .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped())
      .env_remove("ANTHROPIC_API_KEY")         // <-- unless you deliberately want API billing
      .kill_on_drop(true)
      .spawn()?;
  let mut lines = BufReader::new(child.stdout.take().unwrap()).lines();
  while let Some(line) = lines.next_line().await? { let ev: Event = serde_json::from_str(&line)?; ... }

DESERIALIZATION (serde):
  #[derive(Deserialize)] #[serde(tag = "type", rename_all = "snake_case")]
  enum Event {
    System(SystemEvent),        // match on inner `subtype`: init | status | api_retry | plugin_install
    Assistant(MsgEvent), User(MsgEvent),
    StreamEvent(PartialEvent),  // only with --include-partial-messages
    RateLimitEvent(serde_json::Value),
    Result(ResultEvent),
    #[serde(other)] Unknown,    // MANDATORY: the SDKMessage union has ~37 variants and grows
  }
  #[derive(Deserialize)] struct ResultEvent {
    subtype: String,            // NOT an enum — see trap below
    is_error: bool,
    result: Option<String>,     // ABSENT on error subtypes — must be Option
    #[serde(default)] errors: Vec<String>,
    session_id: String,
    num_turns: u32,
    total_cost_usd: f64,
    usage: Usage,
    #[serde(rename = "modelUsage")] model_usage: HashMap<String, ModelUsage>, // camelCase inside!
    permission_denials: Vec<PermissionDenial>,
    terminal_reason: Option<String>,
    api_error_status: Option<u16>,
    stop_reason: Option<String>,
  }
Add `#[serde(other)] Unknown` / `#[serde(flatten)] extra` liberally — treat the schema as open. Deserialize `apiKeySource` and `subtype`/`terminal_reason` as String, not strict enums (observed apiKeySource values "none"/"ANTHROPIC_API_KEY" are outside the declared union).

COMPLETION + SUCCESS DETECTION (the #1 trap):
  let ok = matches!(ev.subtype.as_str(), "success") && !ev.is_error;
`subtype == "success"` ALONE IS A BUG — auth/API failures emit subtype:"success" with is_error:true and api_error_status:401. Gate on `is_error` first, then inspect `terminal_reason` ("completed" = clean; "api_error"/"max_turns"/"budget_exhausted"/... = not). Then check `permission_denials` is empty, because a run where every tool was blocked still reports is_error:false — silently doing nothing.

ARCHITECTURE — pick one:
 (a) One-shot per turn (simplest): spawn, read to `result`, process exits. Persist session_id; next turn spawn with `--resume <id>` FROM THE SAME cwd. Pays full startup + prompt-cache warm each turn.
 (b) Long-lived process (recommended for interactive Atlas sessions): `--input-format stream-json --output-format stream-json --verbose`, write {"type":"user","message":{"role":"user","content":[{"type":"text","text":"…"}]}}\n to stdin per turn, read events until `type=="result"` — that's the TURN boundary, NOT process exit. Verified: 2 messages → 2 result events, one session_id, context retained. Process stays alive; close stdin to end.
Use (b) for live sessions, (a) for fire-and-forget jobs.

SESSION MODEL: generate a UUIDv4 in Rust and pass `--session-id` so Atlas owns the id from the start rather than scraping it back from init. Store (session_id, repo_cwd) TOGETHER in Atlas's DB — resume is cwd-scoped and WILL fail with a non-JSON stderr message + exit 1 if you resume from anywhere else. Transcripts live at ~/.claude/projects/<slugified-cwd>/<uuid>.jsonl, so cwd is effectively part of the primary key. Avoid `--continue` entirely (it latches onto whatever ran last in that directory and races across concurrent sessions). Use `--fork-session` to branch a session (e.g. "retry from here") without clobbering the original. Pass `--no-session-persistence` for ephemeral runs Atlas doesn't intend to resume.

I/O DISCIPLINE: stdout is clean JSON — parse it directly, no filtering needed. But you MUST drain stderr concurrently (spawn a second task reading it) or a chatty warning stream can fill the OS pipe buffer and deadlock the child; alternatively use Stdio::inherit()/null() for stderr if you don't need it. Capture stderr anyway: the "No conversation found with session ID" and root/sudo bypass refusal only appear there. Use `kill_on_drop(true)` plus an outer tokio::time::timeout, and keep `--max-turns`/`--max-budget-usd` as in-band safety nets (they produce clean error results rather than a hung process).

AUTH: for a single-tenant/self-hosted Atlas on this machine, spawn with a clean env and let it inherit ~/.claude/.credentials.json — no API key needed (verified apiKeySource:"none"). ALWAYS `env_remove("ANTHROPIC_API_KEY")` unless API billing is intended: if it leaks into the environment it silently overrides the Max subscription in -p mode with no prompt, moves billing to API credits, and disables claude.ai connectors. For a deployed/server Atlas, prefer explicit auth: either ANTHROPIC_API_KEY, or `claude setup-token` → inject CLAUDE_CODE_OAUTH_TOKEN (verified the var is read). Note `--bare` (the future default for -p) NEVER reads OAuth/keychain — it requires ANTHROPIC_API_KEY or an apiKeyHelper via --settings, so adopting --bare forces the API-key path. CLAUDE_CONFIG_DIR isolates config+sessions but also throws away inherited OAuth ("Not logged in · Please run /login").

PERMISSIONS: use `--permission-mode dontAsk` + an explicit `--allowedTools` allowlist. It is purpose-built for this ("never waits for input", auto-denies anything not pre-approved) and fails closed. Prefer it over `--dangerously-skip-permissions`, which fails closed only in the sense that it refuses to run as root/sudo — a real constraint if Atlas's backend runs as root in a container (use the dev-container pattern / a non-root user). Scoped rules use permission-rule syntax with the space-before-star convention: `Bash(git diff *)` not `Bash(git diff*)`. Surface `permission_denials[]` into Atlas's UI/logs so blocked work is visible rather than silently absent.

MCP (Atlas's own tools): pass `--mcp-config '<inline JSON>' --strict-mcp-config`. Inline JSON avoids temp-file lifecycle entirely (verified working). Shape: {"mcpServers":{"atlas":{"type":"stdio","command":"...","args":[...],"env":{...}}}}. Atlas can expose its own tools by running an MCP server (stdio subprocess, or http/sse pointing back at the Atlas backend — http/sse is likely a better fit for a running Rust service than spawning a stdio child per session). Tools are namespaced `mcp__atlas__<tool>` and must be listed under that exact name in --allowedTools. `--strict-mcp-config` is important: without it the operator's personal claude.ai connectors (Gmail/Drive/Calendar) leak into Atlas sessions.

COST/TOKEN ACCOUNTING: read the terminal `result` event — `total_cost_usd` for the turn, `usage` for aggregate tokens (snake_case), `modelUsage` for the per-model breakdown (camelCase fields). In streaming-input mode, SUM total_cost_usd across the per-turn result events — do not assume the last one is cumulative. Expect a stray haiku entry in modelUsage from internal calls (~$0.0006 even on a trivial prompt); it's already folded into total_cost_usd. Cache fields (cache_creation_input_tokens/cache_read_input_tokens) dominate real spend, so log them if Atlas does cost attribution.

CAPTURED ARTIFACTS (real output, for schema work):
  /tmp/claude-1000/-home-alli-Projects-Atlas/a97bfbca-9036-43f7-81ff-59bc6b0b43f3/scratchpad/cctest/stream_out.jsonl   (basic stream-json)
  /tmp/claude-1000/-home-alli-Projects-Atlas/a97bfbca-9036-43f7-81ff-59bc6b0b43f3/scratchpad/cctest/tooluse.jsonl      (tool_use + tool_result)
  /tmp/claude-1000/-home-alli-Projects-Atlas/a97bfbca-9036-43f7-81ff-59bc6b0b43f3/scratchpad/cctest/mcp_out.jsonl      (custom MCP server)
  /tmp/claude-1000/-home-alli-Projects-Atlas/a97bfbca-9036-43f7-81ff-59bc6b0b43f3/scratchpad/cctest/json_out.json      (--output-format json)
  /tmp/claude-1000/-home-alli-Projects-Atlas/a97bfbca-9036-43f7-81ff-59bc6b0b43f3/scratchpad/cctest/mt.json            (error_max_turns shape)
  /tmp/claude-1000/-home-alli-Projects-Atlas/a97bfbca-9036-43f7-81ff-59bc6b0b43f3/scratchpad/cctest/apikey_fail.json   (401 shape)
  /tmp/claude-1000/-home-alli-Projects-Atlas/a97bfbca-9036-43f7-81ff-59bc6b0b43f3/scratchpad/cctest/atlas_mcp.py       (minimal dep-free stdio MCP server, useful as a test fixture)
  /tmp/claude-1000/-home-alli-Projects-Atlas/a97bfbca-9036-43f7-81ff-59bc6b0b43f3/scratchpad/sdkpkg/package/sdk.d.ts   (AUTHORITATIVE types: SDKResultMessage @4189, SDKResultError @4169, SDKSystemMessage @4308, TerminalReason @6759, PermissionMode @2053, ApiKeySource @124)

## Facts

- **[verified]** Headless/non-interactive execution is `-p` (long form `--print`). The prompt is a positional arg (`claude -p "prompt"`) or piped via stdin (`cat x | claude -p "instruction"`). Verified: `claude -p "reply with just the word hi" --output-format text` printed exactly `hi`, exit 0.
  - Evidence: Local run of /home/alli/.local/bin/claude -p, and `claude --help`: "-p, --print  Print response and exit (useful for pipes)"
- **[verified]** `--output-format` accepts exactly `text` (default), `json` (single result object), `stream-json` (realtime NDJSON). Enum enforced by the CLI.
  - Evidence: `claude --help`: --output-format <format> ... (choices: "text", "json", "stream-json"). All three executed locally.
- **[verified]** `--output-format stream-json` with `-p` REQUIRES `--verbose`, else the CLI refuses: `Error: When using --print, --output-format=stream-json requires --verbose`. Canonical invocation: `claude -p "…" --output-format stream-json --verbose`.
  - Evidence: Ran `claude -p "…" --output-format stream-json` without --verbose; got that exact error string.
- **[verified]** stdout carries ONLY the JSON/NDJSON payload. All warnings/errors (e.g. the connectors warning, `No conversation found with session ID: …`) go to stderr. Verified by running with `2>/dev/null` (clean JSON) and `2>&1 1>/dev/null` (warning only).
  - Evidence: Local runs comparing stdout-only vs stderr-only redirection
- **[verified]** stream-json event envelope: every line is a JSON object with a `type` discriminator. Observed types in real runs: `system` (subtype `init`, `status`, plus documented `api_retry`, `plugin_install`), `rate_limit_event`, `assistant`, `user`, `stream_event` (only with --include-partial-messages), `result`. Common fields on nearly all events: `session_id`, `uuid`, and on assistant/user also `parent_tool_use_id` and `timestamp`.
  - Evidence: Captured real output at /tmp/claude-1000/-home-alli-Projects-Atlas/a97bfbca-9036-43f7-81ff-59bc6b0b43f3/scratchpad/cctest/stream_out.jsonl and tooluse.jsonl
- **[verified]** First event is `system/init`, carrying: cwd, session_id, tools[], mcp_servers[{name,status}], model, permissionMode, slash_commands[], apiKeySource, claude_code_version, output_style, agents[], skills[], plugins[{name,path}], capabilities[], uuid. Real sample: {"type":"system","subtype":"init","cwd":"…","session_id":"32ed3099-…","model":"claude-opus-4-8[1m]","permissionMode":"default","apiKeySource":"none","claude_code_version":"2.1.211",…}
  - Evidence: Real captured stream_out.jsonl line 1; matches SDKSystemMessage in sdk.d.ts:4308
- **[verified]** `assistant` events wrap a raw Anthropic API message: {"type":"assistant","message":{"model","id":"msg_…","role":"assistant","content":[{"type":"text"|"tool_use",…}],"stop_reason","usage":{…}},"parent_tool_use_id":null,"session_id","uuid","timestamp","request_id"}. Tool calls appear as content blocks {"type":"tool_use","id":"toolu_…","name":"Read","input":{…}}.
  - Evidence: Real captured tooluse.jsonl
- **[verified]** Tool RESULTS come back as `type:"user"` events (not a distinct type): {"type":"user","message":{"role":"user","content":[{"tool_use_id":"toolu_…","type":"tool_result","content":"…"}]},"tool_use_result":{…structured…},"session_id","uuid"}. Note the extra top-level `tool_use_result` field with structured detail.
  - Evidence: Real captured tooluse.jsonl from a Read tool call
- **[verified]** Terminal event is `type:"result"`. SUCCESS shape (real): {"type":"result","subtype":"success","is_error":false,"api_error_status":null,"duration_ms":1147,"duration_api_ms":2170,"ttft_ms":1108,"num_turns":1,"result":"hi","stop_reason":"end_turn","session_id":"…","total_cost_usd":0.0420225,"usage":{…},"modelUsage":{…},"permission_denials":[],"terminal_reason":"completed","uuid":"…"}
  - Evidence: Real captured stream_out.jsonl / json_out.json
- **[verified]** TRAP: on API/auth failure the CLI still emits `subtype:"success"` but sets `is_error:true`. Real output with a bogus key: {"type":"result","subtype":"success","is_error":true,"api_error_status":401,"result":"Invalid API key · Fix external API key","total_cost_usd":0,"terminal_reason":"api_error"}. Exit code 1. Branch on `is_error` + `terminal_reason`, NEVER on `subtype`.
  - Evidence: Local run with ANTHROPIC_API_KEY=sk-ant-bogus…; saved at scratchpad/cctest/apikey_fail.json
- **[verified]** Error result subtypes are exactly: `error_during_execution` | `error_max_turns` | `error_max_budget_usd` | `error_max_structured_output_retries`. On these, the `result` KEY IS ABSENT (not null) and an `errors: string[]` key is present instead. Real error_max_turns keys: [duration_api_ms, duration_ms, errors, fast_mode_state, is_error, modelUsage, num_turns, permission_denials, session_id, stop_reason, subtype, terminal_reason, total_cost_usd, type, usage, uuid] with errors=["Reached maximum number of turns (1)"]. So `result` must be Option<String> in Rust.
  - Evidence: sdk.d.ts:4169 SDKResultError; confirmed by local `--max-turns 1` run (scratchpad/cctest/mt.json)
- **[verified]** `terminal_reason` enum (authoritative): 'blocking_limit' | 'rapid_refill_breaker' | 'prompt_too_long' | 'image_error' | 'model_error' | 'api_error' | 'malformed_tool_use_exhausted' | 'aborted_streaming' | 'aborted_tools' | 'stop_hook_prevented' | 'hook_stopped' | 'tool_deferred' | 'max_turns' | 'background_requested' | 'completed' | 'budget_exhausted' | 'structured_output_retry_exhausted' | 'tool_deferred_unavailable' | 'turn_setup_failed'. Normal completion = 'completed'.
  - Evidence: @anthropic-ai/claude-agent-sdk@0.3.211 sdk.d.ts:6759 (TerminalReason)
- **[verified]** Token/cost extraction from the `result` event: `total_cost_usd` (f64, aggregate), `usage` {input_tokens, output_tokens, cache_creation_input_tokens, cache_read_input_tokens, server_tool_use{web_search_requests,web_fetch_requests}, service_tier, cache_creation{ephemeral_1h_input_tokens,ephemeral_5m_input_tokens}, iterations[]}, and `modelUsage` — a map of model-id -> {inputTokens, outputTokens, cacheReadInputTokens, cacheCreationInputTokens, webSearchRequests, costUSD, contextWindow, maxOutputTokens}. NOTE modelUsage uses camelCase while usage uses snake_case.
  - Evidence: Real json_out.json showed modelUsage keyed by "claude-haiku-4-5-20251001" and "claude-opus-4-8[1m]"; matches ModelUsage at sdk.d.ts:1234
- **[verified]** modelUsage includes models Atlas did not select — a trivial 'hi' prompt billed both claude-opus-4-8[1m] ($0.067158) AND claude-haiku-4-5-20251001 ($0.000587) for internal/background calls. Per-model cost attribution must sum modelUsage, and total_cost_usd already includes the haiku overhead.
  - Evidence: Real json_out.json from `claude -p "reply with just the word hi"`
- **[verified]** Session flags: `-r, --resume [value]` (session ID), `-c, --continue` (most recent conversation in current directory), `--session-id <uuid>` (pin a caller-chosen UUID, must be valid UUID), `--fork-session` (with --resume/--continue, mint a new session ID instead of reusing), `--no-session-persistence` (print mode only; session not saved, cannot be resumed).
  - Evidence: `claude --help`; all executed locally except --no-session-persistence
- **[verified]** Session IDs are surfaced in BOTH the `system/init` event (`session_id`) and every subsequent event including `result`. With --output-format json, read `.session_id`. Best practice for Atlas: pre-generate a UUIDv4 and pass `--session-id`, avoiding the need to parse it back — verified `--session-id 11111111-2222-3333-4444-555555555555` was honored verbatim.
  - Evidence: Local run; init and result both reported the supplied UUID
- **[verified]** Sessions PERSIST across process restarts as JSONL transcripts at ~/.claude/projects/<slugified-cwd>/<session-uuid>.jsonl (e.g. /home/alli/.claude/projects/-tmp-claude-1000-…-cctest/11111111-2222-3333-4444-555555555555.jsonl). Resume in a brand-new process correctly recalled prior context (answered 'probe.txt' about a file read by the earlier process).
  - Evidence: ls of ~/.claude/projects/…; separate `claude -p --resume <id>` process
- **[verified]** CRITICAL: `--resume <id>` is CWD-SCOPED. Resuming a session from a different directory fails: stdout EMPTY, stderr `No conversation found with session ID: <id>`, exit code 1 — and this failure is NOT JSON even with --output-format json. Atlas must respawn with the same cwd that created the session.
  - Evidence: Ran resume from scratchpad/otherdir for a session created in scratchpad/cctest; docs (headless): "session ID lookup is scoped to the current project directory and its git worktrees"
- **[verified]** `--resume` REUSES the original session_id (verified: resumed 11111111-… returned session_id 11111111-…). `--fork-session` mints a new one (verified: resuming 11111111-… with --fork-session returned session_id ed5ad269-72b2-4ca9-8ba9-8a260c6690c0).
  - Evidence: Two local runs comparing session_id in the result event
- **[verified]** `--continue` picks the most recent conversation in the CWD and continues it under that session's id — it is directory-state-dependent and races if Atlas runs concurrent sessions in one repo. Verified it silently latched onto an unrelated (errored) prior session. Prefer explicit `--resume <uuid>`.
  - Evidence: Local run: --continue resumed session 304d40de-… (the bogus-API-key run) rather than the tool session
- **[verified]** `--permission-mode` --help choices are: "acceptEdits", "auto", "bypassPermissions", "manual", "dontAsk", "plan". `default` is NOT listed in --help but IS accepted (verified, ran fine) and is the canonical config value. `manual` is an alias for `default` — passing `--permission-mode manual` reports back `permissionMode: "default"` in the init event.
  - Evidence: `claude --help`; local runs of --permission-mode default / manual / dontAsk inspecting init.permissionMode
- **[verified]** `dontAsk` is the mode designed for headless/CI: auto-DENIES every tool call that would otherwise prompt, running only `permissions.allow` matches, built-in read-only Bash commands, and PreToolUse-hook-approved calls. The session never waits for input. Verified init reports permissionMode:"dontAsk".
  - Evidence: https://code.claude.com/docs/en/permission-modes ("Allow only pre-approved tools with dontAsk mode"); local run
- **[verified]** `--allowedTools` (alias `--allowed-tools`) and `--disallowedTools` (alias `--disallowed-tools`) take a COMMA- OR SPACE-separated list, using permission rule syntax: bare tool names (`Read`, `Edit`) or scoped rules (`Bash(git diff *)`). The trailing space before `*` matters: `Bash(git diff *)` prefix-matches, whereas `Bash(git diff*)` would also match `git diff-index`. MCP tools are addressed as `mcp__<server>__<tool>`.
  - Evidence: `claude --help` ("Comma or space-separated list of tool names to allow (e.g. \"Bash(git *) Edit\")"); docs headless page; verified `--allowedTools "mcp__atlas__atlas_ping"` and `--allowedTools "Read"` locally
- **[verified]** Headless runs NEVER hang on approval. In default mode a non-pre-approved tool is auto-denied, the run completes with is_error:false, and the denial is reported in `permission_denials:[{tool_name,tool_use_id,tool_input}]`. Verified: a Bash `touch` outside scope was blocked, file was NOT created, permission_denials had one entry, yet subtype="success"/is_error=false. Atlas must inspect permission_denials to detect blocked work.
  - Evidence: Local run; SDKPermissionDenial at sdk.d.ts:4060
- *[likely]* `--dangerously-skip-permissions` bypasses ALL permission checks and is exactly equivalent to `--permission-mode bypassPermissions`. It REFUSES to start as root/sudo on Linux/macOS: "--dangerously-skip-permissions cannot be used with root/sudo privileges for security reasons" (skipped inside a recognized sandbox). Explicit ask rules and `rm -rf /`/`rm -rf ~` circuit-breakers still prompt even in this mode. A related flag `--allow-dangerously-skip-permissions` only ENABLES the mode as an option without activating it. Admins can kill it via `permissions.disableBypassPermissionsMode: "disable"`.
  - Evidence: `claude --help`; https://code.claude.com/docs/en/permission-modes. NOT executed locally — my own sandbox's auto-mode classifier blocked the attempt, and I did not work around the denial.
- **[verified]** Working directory: the CLI simply uses the spawned process's cwd — set it on the Rust Command and no flag is needed. Verified against a FRESH git clone that had never been trusted: `claude -p` read main.rs and returned 'atlas', is_error:false, with NO trust prompt. `--help` confirms: "The workspace trust dialog is skipped when Claude is run in non-interactive mode (via -p, or when stdout is not a TTY)".
  - Evidence: Created scratchpad/srcrepo, cloned to scratchpad/clonedrepo, ran claude -p with that cwd
- **[verified]** `--add-dir <directories...>` grants tool access to directories OUTSIDE cwd (space-separated/repeatable). Verified: without it, a Bash write outside cwd was denied with "Claude Code may only create or modify files in the allowed working directories for this session"; with `--add-dir /…/otherdir --allowedTools Read`, Claude read the outside file and returned its contents (secret-marker-42).
  - Evidence: Two local runs (denial then success)
- **[verified]** AUTH: a spawned subprocess inherits auth with NO API key. The CLI reads OAuth credentials from ~/.claude/.credentials.json ({claudeAiOauth:{accessToken, refreshToken, expiresAt, subscriptionType:'max', rateLimitTier}}). With no ANTHROPIC_* env vars set, runs succeeded and init reported apiKeySource:"none". `claude auth status` → {loggedIn:true, authMethod:"claude.ai", apiProvider:"firstParty", subscriptionType:"max"}.
  - Evidence: Local `claude auth status`, inspection of ~/.claude/.credentials.json key names, and successful keyless runs
- **[verified]** If ANTHROPIC_API_KEY is set it SILENTLY OVERRIDES the claude.ai subscription in -p mode (no approval prompt). init reports apiKeySource:"ANTHROPIC_API_KEY", stderr warns "claude.ai connectors are disabled because ANTHROPIC_API_KEY or another auth source is set and takes precedence over your claude.ai login", and claude.ai MCP connectors stop loading. Docs: "In non-interactive mode (-p), the key is always used when present." Billing silently moves from the Max plan to API credits.
  - Evidence: Local run with bogus ANTHROPIC_API_KEY → apiKeySource:'ANTHROPIC_API_KEY', 401; https://code.claude.com/docs/en/env-vars
- **[verified]** `CLAUDE_CODE_OAUTH_TOKEN` is recognized (71 occurrences in the 2.1.211 binary) and is the headless-friendly way to carry SUBSCRIPTION auth explicitly. Generate with `claude setup-token` ("Set up a long-lived authentication token (requires Claude subscription)"). Verified a bogus value produces "Failed to authenticate. API Error: 401 OAuth access token is invalid." (proving the var is read) while apiKeySource stays "none".
  - Evidence: Local run with CLAUDE_CONFIG_DIR isolated + bogus CLAUDE_CODE_OAUTH_TOKEN; `strings` count on the binary; `claude setup-token --help`
- **[verified]** `CLAUDE_CONFIG_DIR` relocates BOTH config and credentials AND session storage (creates <dir>/.claude.json, projects/, sessions/). Pointing it at a fresh dir loses inherited OAuth: result was is_error:true, "Not logged in · Please run /login". So per-tenant config isolation and inherited subscription auth are mutually exclusive unless Atlas also injects CLAUDE_CODE_OAUTH_TOKEN or ANTHROPIC_API_KEY.
  - Evidence: Local run with CLAUDE_CONFIG_DIR=/…/isolated_cfg
- **[verified]** The declared `ApiKeySource` type is 'user' | 'project' | 'org' | 'temporary' | 'oauth', but the CLI actually emitted "none" and "ANTHROPIC_API_KEY" — values outside the declared union. Rust must deserialize apiKeySource as a plain String, not a strict enum.
  - Evidence: sdk.d.ts:124 vs. observed init events from two local runs
- **[verified]** Official SDKs exist for TypeScript (@anthropic-ai/claude-agent-sdk, v0.3.211) and Python (claude-agent-sdk, v0.2.120, requires_python >=3.10). There is NO official Rust SDK. Anthropic's explicit guidance: "For other languages, run the CLI programmatically with the `-p` flag and `--output-format json`."
  - Evidence: npm view @anthropic-ai/claude-agent-sdk; https://pypi.org/pypi/claude-agent-sdk/json; https://code.claude.com/docs/en/agent-sdk/overview
- **[verified]** The TS SDK is itself a wrapper that spawns this same CLI binary — it exposes a `pathToClaudeCodeExecutable?: string` option and bundles a native Claude Code binary as an optional dependency. So a Rust backend shelling out to `claude -p` uses the identical transport as the official SDK; nothing is lost except in-process hooks/canUseTool callbacks.
  - Evidence: grep pathToClaudeCodeExecutable in sdk.d.ts:1691; docs note "The TypeScript SDK bundles a native Claude Code binary for your platform as an optional dependency"
- **[verified]** Third-party Rust crates exist on crates.io but are ALL unofficial: claude-agent-sdk-rust 1.0.0, claude-agent-sdk-rs 0.6.4, cc-agent-sdk 0.1.7, claude-agents-sdk 0.1.7, claude-agent-sdk 0.1.1. None are published by Anthropic.
  - Evidence: crates.io API query for 'claude agent sdk'
- **[verified]** `--mcp-config <configs...>` accepts BOTH a file path AND an inline JSON string (space-separated for multiple). Both verified end-to-end. Schema: {"mcpServers":{"atlas":{"type":"stdio","command":"python3","args":["/abs/path/server.py"],"env":{}}}}. Also supports http/sse transports.
  - Evidence: Wrote a minimal stdio MCP server (scratchpad/cctest/atlas_mcp.py) and drove it via both `--mcp-config mcp.json` and an inline JSON string
- **[verified]** MCP verified end-to-end: init reported mcp_servers:[{"name":"atlas","status":"connected"}], the tool appeared in init.tools as `mcp__atlas__atlas_ping`, Claude invoked it, and the tool_result flowed back ({"type":"text","text":"ATLAS_OK service=api build=green rev=deadbeef"}) into the final result. This is the clean path for Atlas to expose its own tools.
  - Evidence: Real captured scratchpad/cctest/mcp_out.jsonl
- **[verified]** `--strict-mcp-config` restricts the session to ONLY servers from --mcp-config, ignoring all other MCP configuration. Verified: with it, init.mcp_servers listed only "atlas"; without it, the user's claude.ai connectors (Gmail/Calendar/Drive, status "needs-auth") also appeared. Atlas should always pass this for reproducibility.
  - Evidence: `claude --help`; comparison of init.mcp_servers across local runs with/without the flag
- **[verified]** `--max-turns <n>` WORKS but is HIDDEN from `claude --help` in 2.1.211 (grep found nothing) while being documented in the CLI reference. On exhaustion: subtype="error_max_turns", is_error=true, terminal_reason="max_turns", errors=["Reached maximum number of turns (1)"], no `result` key, exit code 1. Safety-net budget flag `--max-budget-usd <amount>` (print mode only) maps to subtype error_max_budget_usd / terminal_reason budget_exhausted.
  - Evidence: Local run `--max-turns 1` on a multi-tool task (mt.json); `claude --help | grep max-turns` → no match; https://code.claude.com/docs/en/cli-reference
- **[verified]** `--input-format <format>` accepts "text" (default) or "stream-json" (realtime streaming input), print mode only. With `--input-format stream-json --output-format stream-json --verbose`, ONE long-lived process handles MULTIPLE turns: writing two NDJSON user messages to stdin produced TWO separate `result` events sharing one session_id, with context retained across them (recalled the number 77). Input line shape: {"type":"user","message":{"role":"user","content":[{"type":"text","text":"…"}]}}
  - Evidence: Local run piping two NDJSON lines into a single claude process
- **[verified]** Completion detection: in one-shot mode, the `result` event is the LAST line and the process then exits (0 on success, 1 on is_error/error subtypes). In streaming-input mode there is one `result` per turn and the process stays alive — so Atlas must treat `type=="result"` as the turn boundary, NOT process exit. Docs confirm: "The last line of the stream is a result message with the final response text, cost, and session metadata."
  - Evidence: Local one-shot and streaming-input runs; https://code.claude.com/docs/en/headless
- **[verified]** `--include-partial-messages` adds token-level `stream_event` events wrapping raw Anthropic streaming events: event.type of message_start, content_block_start, content_block_delta (delta.type="text_delta", delta.text="h"), content_block_stop, message_delta, message_stop. Also enables a `system`/`status` event (e.g. {"status":"requesting"}). Only works with --print and --output-format=stream-json.
  - Evidence: Local run with --include-partial-messages, event-type histogram
- **[verified]** `--json-schema <schema>` + `--output-format json` yields a validated `structured_output` key alongside the normal result. Verified: schema {"type":"object","properties":{"fruits":{"type":"array","items":{"type":"string"}}},"required":["fruits"]} produced structured_output={"fruits":["apple","banana"]} with result being the same JSON as a string. Invalid schema → exits with `Error: --json-schema is not a valid JSON Schema`.
  - Evidence: Local run; https://code.claude.com/docs/en/headless
- **[verified]** `--bare` is the docs-recommended mode for scripted/SDK calls ("will become the default for -p in a future release"): skips hooks, LSP, plugin sync, auto-memory, keychain reads, and CLAUDE.md auto-discovery; sets CLAUDE_CODE_SIMPLE=1. CRITICAL CAVEAT: "Anthropic auth is strictly ANTHROPIC_API_KEY or apiKeyHelper via --settings (OAuth and keychain are never read)" — so --bare is INCOMPATIBLE with inheriting the Max subscription login.
  - Evidence: `claude --help` --bare entry; https://code.claude.com/docs/en/headless ("Start faster with bare mode")
- **[verified]** Other flags useful to Atlas: `--model <model>` (alias 'opus'/'sonnet'/'fable' or full id), `--fallback-model <model>` (comma-separated, only works with --print), `--system-prompt` (replace) / `--append-system-prompt` (augment), `--agents <json>` (define custom subagents inline), `--settings <file-or-json>`, `--setting-sources <user,project,local>`, `--tools <tools...>` (restrict built-ins; "" disables all), `--include-hook-events`, `--forward-subagent-text`, `-d/--debug [filter]`, `--debug-file <path>`.
  - Evidence: `claude --help` on v2.1.211
- **[verified]** Subagent output attribution: subagent messages appear in the stream as assistant/user messages whose `parent_tool_use_id` is the spawning tool call's id; main-conversation messages carry null. By default only subagent tool_use/tool_result blocks are emitted — `--forward-subagent-text` (v2.1.211+) also emits their text/thinking blocks.
  - Evidence: https://code.claude.com/docs/en/headless; `claude --help` --forward-subagent-text; observed parent_tool_use_id:null on main-thread events locally
- *[likely]* Background Bash tasks started during `claude -p` are killed ~5s after the final result and stdin closes. Background subagents/workflows are waited for, capped at 10 minutes by default (tunable via CLAUDE_CODE_PRINT_BG_WAIT_CEILING_MS, 0 = no limit). Piped stdin is capped at 10MB (v2.1.128+) — exceed it and the CLI exits non-zero.
  - Evidence: https://code.claude.com/docs/en/headless
- *[likely]* In auto mode under -p, repeated classifier blocks ABORT the session (no user to prompt); interactively it would fall back to prompting after 3 consecutive or 20 total blocks. Also, in non-interactive/SDK sessions a sandbox network deny is cached for the REST OF THE RUN because there is no turn boundary.
  - Evidence: https://code.claude.com/docs/en/permission-modes

## Risks

- LICENSING/ToS — the biggest non-technical risk. Anthropic's Agent SDK docs state: "Unless previously approved, Anthropic does not allow third party developers to offer claude.ai login or rate limits for their products, including agents built on the Claude Agent SDK. Please use the API key authentication methods described in this document instead." If Atlas is offered to anyone other than the operator themselves, riding the inherited Max subscription is NOT permitted — Atlas must use ANTHROPIC_API_KEY (or each user's own key). The inherited-OAuth path is fine only for a personal/self-hosted single-user Atlas. This should be decided BEFORE building the auth layer, since it changes the whole design.
- `subtype: "success"` with `is_error: true` on API/auth failures is a live footgun — any implementer who matches on subtype will treat 401s and rate limits as successful empty runs. Gate on is_error + terminal_reason.
- `result` is an ABSENT KEY (not null) on all error subtypes — a non-Option String field makes serde fail to deserialize the very events that report failure, converting a clean error path into a parse crash. Same class of bug: `errors` is only present on error results (needs #[serde(default)]).
- --resume is CWD-SCOPED and its failure is NOT JSON: empty stdout, plain-text stderr, exit 1. If Atlas stores session_id without the exact repo path (or the clone gets moved/recreated at a new path, e.g. a fresh temp dir per job), every resume silently breaks. Ephemeral per-job clone directories are fundamentally incompatible with resume unless the path is stable.
- Permission denials do NOT set is_error — a run in which every tool was blocked reports success with an empty-handed answer. Atlas must inspect permission_denials[] or it will report confident non-work as success. The model also narrates the denial in prose (observed), which can read as a real answer.
- ANTHROPIC_API_KEY leaking into the backend's environment (CI, systemd unit, Docker env, .env) silently switches billing from the Max subscription to API credits with no prompt in -p mode, and disables claude.ai connectors. Explicitly env_remove it unless intended.
- --dangerously-skip-permissions REFUSES to run as root/sudo. A Rust backend in a typical Docker container runs as root by default, so this flag will fail there — use a non-root user or dontAsk mode. (I could not execute this flag locally: my own sandbox's classifier blocked it and I did not attempt to work around the denial, so its behavior is documented from --help/docs rather than observed.)
- The stream-json schema is an OPEN, fast-moving union — sdk.d.ts declares ~37 SDKMessage variants (task events, hook events, compact boundaries, notifications, refusal fallbacks...) and the CLI emits undocumented ones in practice (rate_limit_event, system/status). Any exhaustive/strict match will break on the next CLI update. Use #[serde(other)] catch-alls everywhere.
- Version coupling: findings are pinned to CLI 2.1.211. Flags move — --max-turns is already hidden from --help while still functional, --permission-mode default is accepted but undocumented in --help choices, and docs say --bare "will become the default for -p in a future release" (which would silently drop OAuth auth and CLAUDE.md discovery for Atlas). Pin/assert the CLI version at startup (init.claude_code_version) and use init.capabilities[] for feature detection rather than version-sniffing.
- If stderr is piped and never drained, a chatty child can fill the pipe buffer and deadlock. Drain it in a separate task or use Stdio::null()/inherit().
- OAuth token refresh: ~/.claude/.credentials.json holds an expiring accessToken (expiresAt) that the CLI refreshes and rewrites. Many concurrent Atlas subprocesses sharing one credentials file could race on refresh. CLAUDE_CODE_OAUTH_TOKEN (via claude setup-token) is the more robust subscription path for concurrency.
- Cost blowout: a single trivial 'hi' cost ~$0.04-0.07 on opus-4-8[1m] because of cache-creation tokens, plus an unavoidable haiku side-call. Real agentic runs on a cloned repo will be far more. --max-budget-usd and --max-turns should be non-optional in Atlas, not nice-to-haves.
- Auto mode (--permission-mode auto) behaves differently headless: repeated classifier blocks ABORT the run (no user to prompt), and a sandbox network deny is cached for the entire run since there's no turn boundary. Don't reach for auto mode as a 'fewer prompts' fix in Atlas; dontAsk + explicit allowlist is the deterministic choice.
- Without --strict-mcp-config, the operator's personal claude.ai connectors and local .mcp.json/CLAUDE.md/hooks bleed into Atlas sessions, making behavior machine-dependent and non-reproducible across deployments.
