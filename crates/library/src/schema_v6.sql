-- Processing presets (M6a): the user's saved settings of each effect of the broadcast chain, one row per
-- (effect, name). The shipped presets live in code, are read-only and are listed beside these; `builtin` is
-- for effects whose shipped presets are seeded as rows (none yet). A whole-chain preset (M6) is a row of effect
-- `chain` whose params name one preset per effect.
CREATE TABLE IF NOT EXISTS processing_presets (
  effect TEXT NOT NULL,
  name TEXT NOT NULL,
  params TEXT NOT NULL,                -- JSON, validated by the effect's own rules before it is stored
  description TEXT NOT NULL DEFAULT '',
  builtin INTEGER NOT NULL DEFAULT 0,
  updated_at TEXT NOT NULL DEFAULT '',
  PRIMARY KEY (effect, name)
);
