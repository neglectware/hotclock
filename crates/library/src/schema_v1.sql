
CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);   -- schema_version, created_at

CREATE TABLE segue_profiles (
  name        TEXT PRIMARY KEY,           -- music, liner, ambient, longform, ...
  params      TEXT NOT NULL               -- JSON, section 3.4
);

CREATE TABLE transition_presets (      -- 2.6: basic, plexamp, sam, fm, none, plus the owner's own
  name        TEXT PRIMARY KEY,
  params      TEXT NOT NULL,             -- JSON, the parameter table of 2.6
  description TEXT NOT NULL DEFAULT '',  -- one sentence on what it sounds like
  builtin     INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE categories (
  id             INTEGER PRIMARY KEY,
  name           TEXT NOT NULL UNIQUE,
  kind           TEXT NOT NULL CHECK (kind IN ('music','liner','id','promo','show','commercial','bed','fill','emergency','other')),
  segue_profile  TEXT NOT NULL REFERENCES segue_profiles(name),
  transition_preset TEXT REFERENCES transition_presets(name),   -- NULL = inherit the station's (2.6)
  transition_patch  TEXT NOT NULL DEFAULT '{}',                  -- JSON patch, applied as caps for lead/tail (2.6)
  gain_offset_db REAL NOT NULL DEFAULT 0,
  color          TEXT
);

CREATE TABLE items (
  id              INTEGER PRIMARY KEY,
  category_id     INTEGER NOT NULL REFERENCES categories(id),
  title           TEXT NOT NULL,
  artist          TEXT NOT NULL DEFAULT '',
  album           TEXT NOT NULL DEFAULT '',
  year            INTEGER,
  album_artist    TEXT NOT NULL DEFAULT '',
  disc_no         INTEGER,                -- NULL = no tag (treated as disc 1)
  track_no        INTEGER,                -- NULL = no tag: never album-adjacent
  track_total     INTEGER,
  album_key       TEXT NOT NULL DEFAULT '',   -- 3.7; '' when the item has no album tag
  art_sha         TEXT REFERENCES artwork(sha256),   -- NULL = no artwork
  sha256          TEXT NOT NULL UNIQUE,
  store_path      TEXT NOT NULL,          -- relative: ab/abcdef....flac
  orig_filename   TEXT NOT NULL,
  format          TEXT NOT NULL,          -- flac, mp3, aac, vorbis, wav, alac
  sample_rate     INTEGER NOT NULL,
  channels        INTEGER NOT NULL,
  file_ms         INTEGER NOT NULL,
  file_bytes      INTEGER NOT NULL,
  enabled         INTEGER NOT NULL DEFAULT 1,
  quarantined     INTEGER NOT NULL DEFAULT 0,
  playback_errors INTEGER NOT NULL DEFAULT 0,
  silence_ok      INTEGER NOT NULL DEFAULT 0,
  notes           TEXT NOT NULL DEFAULT '',
  source          TEXT NOT NULL DEFAULT '', -- where it came from (upload, inbox, plex:<ratingKey>)
  imported_at     TEXT NOT NULL,
  updated_at      TEXT NOT NULL,
  deleted_at      TEXT                      -- soft delete; file moves to .trash
);
CREATE INDEX items_cat ON items(category_id) WHERE deleted_at IS NULL;
CREATE INDEX items_artist ON items(artist);
CREATE INDEX items_album ON items(album_key, disc_no, track_no) WHERE album_key <> '';

CREATE TABLE artwork (
  sha256      TEXT PRIMARY KEY,           -- of the source image bytes; shared by every track of an album
  origin      TEXT NOT NULL,              -- embedded | folder | manual
  src_w INTEGER NOT NULL, src_h INTEGER NOT NULL,
  bytes_600   INTEGER NOT NULL, bytes_144 INTEGER NOT NULL
);

CREATE TABLE analysis (
  item_id        INTEGER PRIMARY KEY REFERENCES items(id) ON DELETE CASCADE,
  algo_version   INTEGER NOT NULL,
  l_int          REAL NOT NULL, lra REAL NOT NULL, true_peak REAL NOT NULL, sample_peak REAL NOT NULL,
  auto_cue_in    INTEGER NOT NULL, auto_attack INTEGER NOT NULL, auto_segue INTEGER NOT NULL,
  auto_cue_out   INTEGER NOT NULL, auto_intro INTEGER,
  flags          TEXT NOT NULL DEFAULT '[]',   -- JSON array of flag names
  env_e10        BLOB NOT NULL,   -- zstd(i16 dB*10, 10 ms hop)
  env_e50        BLOB NOT NULL,   -- zstd(i16 dB*10, 10 ms hop)
  env_k200       BLOB NOT NULL,   -- zstd(i16 dB*10, 20 ms hop)
  env_k10        BLOB NOT NULL,   -- zstd(i16 dB*10, 10 ms blocks): K-weighted block loudness; source of the ramps and of window loudness (M1.3)
  wave           BLOB NOT NULL,   -- zstd(i8 min,max per channel, 20 ms)
  ramp_start     TEXT NOT NULL,   -- Plex-style 'dB seconds;' list over the file bounds, ascending dB from -51.00, 7 to 14 points (2.6)
  ramp_end       TEXT NOT NULL,   -- same, seconds measured back from the file end
  analyzed_at    TEXT NOT NULL
);

CREATE TABLE item_overrides (
  item_id        INTEGER PRIMARY KEY REFERENCES items(id) ON DELETE CASCADE,
  params         TEXT,            -- JSON parameter overrides, 3.3; the key "transition" holds a preset name and/or a patch (2.6)
  segue_profile  TEXT REFERENCES segue_profiles(name),
  ovr_cue_in INTEGER, ovr_attack INTEGER, ovr_intro INTEGER, ovr_segue INTEGER,
  ovr_fade_out INTEGER, ovr_cue_out INTEGER,
  ovr_gain_trim_db REAL,
  lead_max_ms INTEGER, tail_max_ms INTEGER,
  updated_at TEXT NOT NULL
);

-- markers recomputed from params are materialized here by the library service on every change,
-- so stations never run the recompute themselves.
CREATE TABLE item_effective (
  item_id    INTEGER PRIMARY KEY REFERENCES items(id) ON DELETE CASCADE,
  cue_in INTEGER NOT NULL, attack INTEGER NOT NULL, intro INTEGER, segue INTEGER NOT NULL,
  fade_out INTEGER, cue_out INTEGER NOT NULL,
  flags TEXT NOT NULL DEFAULT '[]',   -- flags recomputed inside the cue window (3.3)
  l_int_source TEXT NOT NULL DEFAULT 'file',   -- file | window | window_estimate
  profile TEXT NOT NULL,
  transition_preset TEXT,                          -- the category's preset name, or NULL to inherit the station's (2.6)
  transition_patch TEXT NOT NULL DEFAULT '{}',     -- category patch merged with the item's; stations add the station preset and any queue-entry override
  start_type TEXT NOT NULL DEFAULT '', end_type TEXT NOT NULL DEFAULT '',   -- M9 context-aware transitions (2.6); '' until then
  l_int REAL NOT NULL, true_peak REAL NOT NULL, gain_offset_db REAL NOT NULL,
  revision INTEGER NOT NULL        -- bumped on every change; stations sync by revision
);

CREATE TABLE tags (id INTEGER PRIMARY KEY, name TEXT NOT NULL UNIQUE);
CREATE TABLE item_tags (item_id INTEGER REFERENCES items(id) ON DELETE CASCADE,
                        tag_id INTEGER REFERENCES tags(id) ON DELETE CASCADE,
                        PRIMARY KEY (item_id, tag_id));

CREATE TABLE stations (id TEXT PRIMARY KEY, display_name TEXT NOT NULL);  -- mirrors /etc config names

CREATE TABLE playlists (
  id           INTEGER PRIMARY KEY,
  name         TEXT NOT NULL UNIQUE,
  station_id   TEXT REFERENCES stations(id),   -- NULL = usable by any station
  kind         TEXT NOT NULL CHECK (kind IN ('static','smart')),
  filter       TEXT,                           -- JSON for smart playlists
  mode         TEXT NOT NULL DEFAULT 'shuffle' CHECK (mode IN ('shuffle','sequential')),
  separation   TEXT NOT NULL DEFAULT '{}',     -- JSON: sep_item_minutes, sep_artist_minutes, ...
  updated_at   TEXT NOT NULL
);
CREATE TABLE playlist_items (
  id          INTEGER PRIMARY KEY,
  playlist_id INTEGER NOT NULL REFERENCES playlists(id) ON DELETE CASCADE,
  item_id     INTEGER REFERENCES items(id),   -- a single item, or
  album_key   TEXT,                           -- an album block (3.7), exactly one of the two
  continuity  TEXT NOT NULL DEFAULT 'gapless',   -- album blocks only: gapless | normal
  position    INTEGER NOT NULL,
  weight      REAL NOT NULL DEFAULT 1.0,
  CHECK ((item_id IS NULL) <> (album_key IS NULL))
);
CREATE UNIQUE INDEX playlist_item_once ON playlist_items(playlist_id, item_id) WHERE item_id IS NOT NULL;

CREATE TABLE insert_rules (
  id INTEGER PRIMARY KEY, station_id TEXT NOT NULL REFERENCES stations(id),
  name TEXT NOT NULL, playlist_id INTEGER REFERENCES playlists(id),   -- NULL = any playlist
  daypart_id INTEGER REFERENCES dayparts(id),
  every_n INTEGER NOT NULL, count_kind TEXT NOT NULL DEFAULT 'music',
  source_category_id INTEGER REFERENCES categories(id),
  source_playlist_id INTEGER REFERENCES playlists(id),
  position TEXT NOT NULL DEFAULT 'between',       -- between | over_intro_if_fits
  min_gap_minutes INTEGER NOT NULL DEFAULT 0,
  resets_on TEXT NOT NULL DEFAULT '["id"]',
  priority INTEGER NOT NULL DEFAULT 100, enabled INTEGER NOT NULL DEFAULT 1
);

CREATE TABLE timed_events (
  id INTEGER PRIMARY KEY, station_id TEXT NOT NULL REFERENCES stations(id),
  name TEXT NOT NULL,
  cron TEXT,                      -- "0 * * * *" in local time; NULL for one-shot
  at TEXT,                        -- one-shot, ISO 8601 local
  action TEXT NOT NULL,           -- JSON: {"type":"play_from_category","category_id":3} ...
  mode TEXT NOT NULL DEFAULT 'soft_then_hard',
  window_early_s INTEGER NOT NULL DEFAULT 60, window_late_s INTEGER NOT NULL DEFAULT 30,
  priority INTEGER NOT NULL DEFAULT 100, enabled INTEGER NOT NULL DEFAULT 1,
  created_at TEXT NOT NULL
);

CREATE TABLE dayparts (
  id INTEGER PRIMARY KEY, station_id TEXT NOT NULL REFERENCES stations(id),
  name TEXT NOT NULL, days_mask INTEGER NOT NULL,     -- bit 0 = Monday
  start_min INTEGER NOT NULL, end_min INTEGER NOT NULL,   -- local minutes after midnight
  playlist_id INTEGER NOT NULL REFERENCES playlists(id),
  clock_id INTEGER, priority INTEGER NOT NULL DEFAULT 100
);
CREATE TABLE date_exceptions (
  id INTEGER PRIMARY KEY, station_id TEXT NOT NULL, date TEXT NOT NULL,
  daypart_id INTEGER NOT NULL REFERENCES dayparts(id)
);

CREATE TABLE transition_matrix (       -- optional, M9 (2.6): deterministic context-aware transitions
  station_id TEXT NOT NULL REFERENCES stations(id),
  end_type   TEXT NOT NULL CHECK (end_type IN ('cold_end','fade','long_ambient_tail')),
  start_type TEXT NOT NULL CHECK (start_type IN ('cold_start','soft_pickup','long_quiet_intro')),
  preset     TEXT REFERENCES transition_presets(name),
  patch      TEXT NOT NULL DEFAULT '{}',
  PRIMARY KEY (station_id, end_type, start_type)
);

CREATE TABLE rundowns (                -- 4.9
  id INTEGER PRIMARY KEY, station_id TEXT NOT NULL REFERENCES stations(id),
  name TEXT NOT NULL,
  anchor TEXT NOT NULL CHECK (anchor IN ('absolute','manual')),
  arm_window_min INTEGER NOT NULL DEFAULT 60,   -- hand starts this close to (or after) a nominal time consume that occurrence
  fallback_start INTEGER NOT NULL DEFAULT 0,    -- manual only: off by default, never used by eve-monday
  fallback_at TEXT, fallback_grace_s INTEGER,   -- start at the nominal time plus grace if nobody has started it
  late_policy TEXT NOT NULL DEFAULT 'run_if_within',   -- default for events whose own policy is 'default'
  late_window_s INTEGER NOT NULL DEFAULT 60,
  enabled INTEGER NOT NULL DEFAULT 1, notes TEXT NOT NULL DEFAULT '', updated_at TEXT NOT NULL
);
CREATE TABLE rundown_schedule (        -- recurrences and one-off dates (NULL anchor rundowns have none)
  id INTEGER PRIMARY KEY, rundown_id INTEGER NOT NULL REFERENCES rundowns(id) ON DELETE CASCADE,
  cron TEXT,                           -- "45 18 * * 1", local time
  at TEXT,                             -- one-off, ISO 8601 local, e.g. an extra Thursday show
  enabled INTEGER NOT NULL DEFAULT 1,
  CHECK ((cron IS NULL) <> (at IS NULL))
);
CREATE TABLE rundown_events (
  id INTEGER PRIMARY KEY, rundown_id INTEGER NOT NULL REFERENCES rundowns(id) ON DELETE CASCADE,
  position INTEGER NOT NULL, name TEXT NOT NULL,
  time_ref TEXT NOT NULL CHECK (time_ref IN ('t0','absolute')),
  offset_ms INTEGER,                   -- t0: >= 0
  at_local TEXT, day_offset INTEGER NOT NULL DEFAULT 0,   -- absolute: 'HH:MM:SS.mmm' and days after the occurrence date
  type TEXT NOT NULL,                  -- start_playlist, switch_playlist, fade_stop, stop, play_item, osc_out, midi_out, http_request, set_gain, set_mute, end_rundown
  params TEXT NOT NULL DEFAULT '{}',   -- JSON, 4.9
  late_policy TEXT NOT NULL DEFAULT 'default' CHECK (late_policy IN ('default','skip','run_now','run_if_within')),
  late_window_s INTEGER, enabled INTEGER NOT NULL DEFAULT 1
);

CREATE TABLE import_jobs (
  id INTEGER PRIMARY KEY, source TEXT NOT NULL, status TEXT NOT NULL,  -- queued|running|done|failed|duplicate
  item_id INTEGER REFERENCES items(id), error TEXT, created_at TEXT NOT NULL, finished_at TEXT
);

CREATE TABLE audit (             -- every edit through the UI or API
  id INTEGER PRIMARY KEY, at TEXT NOT NULL, actor TEXT NOT NULL,   -- client IP or token name
  entity TEXT NOT NULL, entity_id TEXT NOT NULL, diff TEXT NOT NULL
);

-- clocks (milestone M8)
CREATE TABLE clocks (id INTEGER PRIMARY KEY, name TEXT NOT NULL UNIQUE);
CREATE TABLE clock_slots (
  clock_id INTEGER REFERENCES clocks(id) ON DELETE CASCADE, position INTEGER NOT NULL,
  minute_target INTEGER,            -- NULL = relative
  source TEXT NOT NULL,             -- JSON: category, playlist or item
  time_type TEXT NOT NULL DEFAULT 'relative',   -- relative | soft | hard
  PRIMARY KEY (clock_id, position)
);

-- M1.3 addition: the exact window loudness L_W of 3.3, keyed by the window it was computed for.
CREATE TABLE item_window_loudness (
  item_id  INTEGER PRIMARY KEY REFERENCES items(id) ON DELETE CASCADE,
  c_in     INTEGER NOT NULL, c_out INTEGER NOT NULL,
  l_w      REAL NOT NULL,
  computed_at TEXT NOT NULL
);

