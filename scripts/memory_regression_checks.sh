#!/usr/bin/env bash
set -euo pipefail

DB_PATH="${1:-$HOME/.local/share/com.agent3/agent3.db}"

if [[ ! -f "$DB_PATH" ]]; then
  echo "Database not found: $DB_PATH" >&2
  exit 1
fi

echo "[check] db: $DB_PATH"

echo

echo "[1] latest 12 episodic rows"
sqlite3 -header -column "$DB_PATH" "
SELECT datetime(created_at,'unixepoch','localtime') AS ts,
       session_id,
       role,
       substr(content,1,100) AS content_preview
FROM episodic_logs
ORDER BY created_at DESC
LIMIT 12;
"

echo

echo "[2] same-second user write bursts in last session (fragmentation signal)"
sqlite3 -header -column "$DB_PATH" "
WITH last_s AS (
  SELECT session_id
  FROM episodic_logs
  ORDER BY created_at DESC
  LIMIT 1
)
SELECT datetime(created_at,'unixepoch','localtime') AS ts,
       COUNT(*) AS user_msgs_same_sec
FROM episodic_logs
WHERE role='user'
  AND session_id=(SELECT session_id FROM last_s)
GROUP BY created_at
HAVING COUNT(*) > 1
ORDER BY created_at DESC
LIMIT 20;
"

echo

echo "[3] short user fragment ratio in last session"
sqlite3 -header -column "$DB_PATH" "
WITH last_s AS (
  SELECT session_id
  FROM episodic_logs
  ORDER BY created_at DESC
  LIMIT 1
)
SELECT COUNT(*) AS total_user,
       SUM(CASE WHEN length(trim(content)) <= 3 THEN 1 ELSE 0 END) AS len_le_3,
       ROUND(100.0*SUM(CASE WHEN length(trim(content)) <= 3 THEN 1 ELSE 0 END)/NULLIF(COUNT(*),0),2) AS pct_le_3
FROM episodic_logs
WHERE role='user'
  AND session_id=(SELECT session_id FROM last_s);
"

echo

echo "[4] blank/whitespace user rows in last session (should be 0)"
sqlite3 -header -column "$DB_PATH" "
WITH last_s AS (
  SELECT session_id
  FROM episodic_logs
  ORDER BY created_at DESC
  LIMIT 1
)
SELECT COUNT(*) AS blank_user_rows
FROM episodic_logs
WHERE role='user'
  AND session_id=(SELECT session_id FROM last_s)
  AND length(trim(content))=0;
"

echo

echo "[5] same-second assistant write bursts in last session (ResponseDone duplication signal)"
sqlite3 -header -column "$DB_PATH" "
WITH last_s AS (
  SELECT session_id
  FROM episodic_logs
  ORDER BY created_at DESC
  LIMIT 1
)
SELECT datetime(created_at,'unixepoch','localtime') AS ts,
       COUNT(*) AS assistant_msgs_same_sec
FROM episodic_logs
WHERE role='assistant'
  AND session_id=(SELECT session_id FROM last_s)
GROUP BY created_at
HAVING COUNT(*) > 1
ORDER BY created_at DESC
LIMIT 20;
"

echo

echo "[done] run this script before/after changes to compare behavior"
