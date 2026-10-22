-- Schema 2 (M1.5, architecture 4.4, 4.5 and 5.2 after the eighth round): control blocks replace
-- rundowns, timed events, insert rules and dayparts; a playlist is an ordered list of entries
-- (items, album blocks and control blocks).

DROP TABLE IF EXISTS rundown_events;
DROP TABLE IF EXISTS rundown_schedule;
DROP TABLE IF EXISTS rundowns;
DROP TABLE IF EXISTS date_exceptions;
DROP TABLE IF EXISTS insert_rules;
DROP TABLE IF EXISTS timed_events;
DROP TABLE IF EXISTS dayparts;
DROP TABLE IF EXISTS playlist_items;

ALTER TABLE playlists ADD COLUMN no_repeat_minutes INTEGER NOT NULL DEFAULT 120;
ALTER TABLE playlists ADD COLUMN station_blocks INTEGER NOT NULL DEFAULT 1;
ALTER TABLE playlists ADD COLUMN late_policy TEXT NOT NULL DEFAULT 'run_if_within';
ALTER TABLE playlists ADD COLUMN late_window_s INTEGER NOT NULL DEFAULT 60;
ALTER TABLE playlists ADD COLUMN enabled INTEGER NOT NULL DEFAULT 1;

CREATE TABLE blocks (
  id          INTEGER PRIMARY KEY,
  station_id  TEXT REFERENCES stations(id),   -- set only for a station block (4.4); NULL inside a playlist
  name        TEXT NOT NULL DEFAULT '',
  type        TEXT NOT NULL CHECK (type IN ('start','play','start_playlist','fade_stop','stop','wait','loop','end',
                                            'insert_every','insert','osc_out','midi_out','http_request','set_gain','set_mute')),
  trigger     TEXT NOT NULL CHECK (trigger IN ('sequence','timed','standing','first')),   -- 'first' = the start block
  time_ref    TEXT CHECK (time_ref IN ('t0','absolute')),   -- timed blocks only
  offset_ms   INTEGER,                        -- t0: >= 0
  at_local    TEXT, day_offset INTEGER,       -- absolute, relative to T0's date: 'HH:MM:SS.mmm' and days after
  recurrence  TEXT,                           -- absolute, recurring: cron-like in local time, e.g. "0 * * * *"
  on_date     TEXT,                           -- absolute, one-off: ISO 8601 local date and time
  skip_dates  TEXT NOT NULL DEFAULT '[]',     -- dates on which a recurrence does not fire
  mode        TEXT,                           -- soft_then_hard | soft | hard | hard_wait (boundary blocks, 4.6)
  window_early_s INTEGER, window_late_s INTEGER,
  priority    INTEGER NOT NULL DEFAULT 100,
  params      TEXT NOT NULL DEFAULT '{}',     -- JSON per type (4.5); start blocks hold the schedule and anchor here
  late_policy TEXT NOT NULL DEFAULT 'default' CHECK (late_policy IN ('default','skip','run_now','run_if_within')),
  late_window_s INTEGER,
  enabled     INTEGER NOT NULL DEFAULT 1,
  updated_at  TEXT NOT NULL
);
CREATE INDEX blocks_station ON blocks(station_id) WHERE station_id IS NOT NULL;

CREATE TABLE playlist_entries (
  id          INTEGER PRIMARY KEY,
  playlist_id INTEGER NOT NULL REFERENCES playlists(id) ON DELETE CASCADE,
  item_id     INTEGER REFERENCES items(id),   -- a single item, or
  album_key   TEXT,                           -- an album block (3.7), or
  block_id    INTEGER REFERENCES blocks(id) ON DELETE CASCADE,   -- a control block; exactly one of the three
  continuity  TEXT NOT NULL DEFAULT 'gapless',   -- album blocks only: gapless | normal
  position    INTEGER NOT NULL,
  weight      REAL NOT NULL DEFAULT 1.0,
  CHECK ((item_id IS NOT NULL) + (album_key IS NOT NULL) + (block_id IS NOT NULL) = 1)
);
CREATE UNIQUE INDEX playlist_item_once ON playlist_entries(playlist_id, item_id) WHERE item_id IS NOT NULL;
CREATE INDEX playlist_entries_pos ON playlist_entries(playlist_id, position);
