CREATE TABLE board_auto_activity (
 id TEXT PRIMARY KEY,
 run_id TEXT NOT NULL REFERENCES board_auto_runs(id) ON DELETE CASCADE,
 provider TEXT NOT NULL,
 message TEXT NOT NULL,
 card_key TEXT,
 session_id TEXT REFERENCES agent_sessions(id) ON DELETE SET NULL,
 created_at TEXT NOT NULL
);
CREATE INDEX board_auto_activity_run ON board_auto_activity(run_id, id);
CREATE TABLE agent_usage (
 run_id TEXT PRIMARY KEY,
 actor_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
 provider TEXT NOT NULL,
 input_tokens INTEGER,
 output_tokens INTEGER,
 cached_tokens INTEGER,
 cost_usd REAL,
 created_at TEXT NOT NULL
);
CREATE INDEX agent_usage_actor ON agent_usage(actor_id, created_at);

-- Recover reported worker usage from existing transcripts. Planner usage starts here.
WITH valid AS (
 SELECT session_id, seq, CASE WHEN json_valid(line) THEN line ELSE '{}' END AS payload
 FROM agent_session_transcript
), terminal AS (
 SELECT session_id, MAX(seq) AS seq FROM valid
 WHERE json_extract(payload, '$.type') IN ('result', 'turn.completed')
    OR json_extract(payload, '$.event') = 'result'
 GROUP BY session_id
), reports AS (
 SELECT s.id, s.started_by, s.provider, s.ended_at,
 CASE WHEN json_extract(v.payload, '$.event') = 'result'
      THEN json_extract(v.payload, '$.result') ELSE v.payload END AS payload
 FROM terminal t JOIN valid v ON v.session_id=t.session_id AND v.seq=t.seq
 JOIN agent_sessions s ON s.id=t.session_id WHERE s.started_by IS NOT NULL
)
INSERT INTO agent_usage(run_id, actor_id, provider, input_tokens, output_tokens, cached_tokens, cost_usd, created_at)
SELECT id, started_by, provider,
 json_extract(payload, '$.usage.input_tokens'),
 json_extract(payload, '$.usage.output_tokens'),
 COALESCE(json_extract(payload, '$.usage.cached_input_tokens'), json_extract(payload, '$.usage.cache_read_input_tokens'), json_extract(payload, '$.usage.cache_read_tokens')),
 CASE WHEN provider='claude' THEN json_extract(payload, '$.total_cost_usd') END,
 COALESCE(ended_at, strftime('%Y-%m-%dT%H:%M:%f+00:00','now'))
FROM reports;
