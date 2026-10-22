-- Notifications (architecture 12.1). Passwords are never stored here: they live in secrets.toml.
CREATE TABLE IF NOT EXISTS notify_channels (
  id INTEGER PRIMARY KEY,
  name TEXT NOT NULL UNIQUE,
  kind TEXT NOT NULL DEFAULT 'smtp',
  config TEXT NOT NULL DEFAULT '{}',   -- JSON: host, port, tls (starttls|tls|none), username, from, to[]; no password
  enabled INTEGER NOT NULL DEFAULT 1,
  last_test_at TEXT, last_test_result TEXT
);
CREATE TABLE IF NOT EXISTS notify_rules (
  code TEXT PRIMARY KEY,
  enabled INTEGER NOT NULL DEFAULT 1,
  severity TEXT,                       -- override, NULL = the alert's own
  channels TEXT NOT NULL DEFAULT '[]', -- JSON array of channel ids, empty = every enabled channel
  delivery TEXT NOT NULL DEFAULT 'immediate' CHECK (delivery IN ('immediate','hourly_digest','daily_digest','off')),
  rate_limit_min INTEGER NOT NULL DEFAULT 60,
  notify_on_clear INTEGER NOT NULL DEFAULT 0
);
-- What the notifier has seen and what is waiting for a digest.
CREATE TABLE IF NOT EXISTS notify_cursor (station TEXT PRIMARY KEY, last_alert_id INTEGER NOT NULL DEFAULT 0);
CREATE TABLE IF NOT EXISTS notify_queue (
  id INTEGER PRIMARY KEY, at TEXT NOT NULL, station TEXT NOT NULL, code TEXT NOT NULL,
  severity TEXT NOT NULL, message TEXT NOT NULL, cleared INTEGER NOT NULL DEFAULT 0,
  bucket TEXT NOT NULL   -- hourly_digest | daily_digest
);
CREATE TABLE IF NOT EXISTS notify_log (
  id INTEGER PRIMARY KEY, at TEXT NOT NULL, station TEXT NOT NULL, code TEXT NOT NULL,
  channel_id INTEGER, result TEXT NOT NULL, kind TEXT NOT NULL   -- immediate | digest | test | suppressed
);
CREATE TABLE IF NOT EXISTS display_view (id INTEGER PRIMARY KEY CHECK (id = 1), view TEXT NOT NULL DEFAULT 'single',
  station TEXT, rotate_s INTEGER NOT NULL DEFAULT 20);
INSERT OR IGNORE INTO display_view(id) VALUES (1);
