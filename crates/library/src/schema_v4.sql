-- Schema 4 (M1.6 review, architecture 1.5): the longest quiet stretch inside each item's play window, so
-- the station's silence detector judges a deliberately quiet piece on its own content instead of a hand-set
-- `silence_ok`. `quiet_profile` is the stretch for a ladder of file-level thresholds (JSON, analysis::quiet);
-- `longest_quiet_ms` is its value at -50 dBFS, the default threshold, inside the cue window, and
-- `longest_quiet_file_ms` over the whole file (what a `bounds = file` preset such as `plexamp` plays: the
-- quiet before the cue-in is heard then), for display. All three are recomputed with the effective markers.

ALTER TABLE item_effective ADD COLUMN quiet_profile TEXT NOT NULL DEFAULT '';
ALTER TABLE item_effective ADD COLUMN longest_quiet_ms INTEGER NOT NULL DEFAULT 0;
ALTER TABLE item_effective ADD COLUMN longest_quiet_file_ms INTEGER NOT NULL DEFAULT 0;
