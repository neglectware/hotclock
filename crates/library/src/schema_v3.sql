-- Schema 3 (M1.6, architecture 1.5 and 5.2): quarantine with a reason, and the re-analysis flag.

ALTER TABLE items ADD COLUMN quarantine_reason TEXT;   -- decode | missing | truncated | silent | hash_mismatch | manual
ALTER TABLE items ADD COLUMN quarantined_at TEXT;
ALTER TABLE items ADD COLUMN analysis_failed INTEGER NOT NULL DEFAULT 0;   -- a re-analysis failed; the old analysis is kept
ALTER TABLE items ADD COLUMN analysis_error TEXT;
