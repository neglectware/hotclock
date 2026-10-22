//! Items, overrides and the materialized `item_effective` rows (architecture 3.3, 5.2).
//! `materialize` is the only writer of `item_effective`, and it gets every marker from
//! `analysis::effective::effective_markers`.

use crate::db::Library;
use crate::util::now_iso;
use crate::{LibraryError, Result};
use analysis::effective::{effective_markers, EffectiveInput, Overrides};
use analysis::env::StoredEnvelopes;
use analysis::markers::{
    flags_from_json, flags_to_json, FileFacts, Flag, LongIntroPolicy, ProfileParams,
};
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::Value;
use std::collections::BTreeSet;

#[derive(Debug, Clone, PartialEq)]
pub struct EffectiveRow {
    pub item_id: i64,
    pub cue_in: i64,
    pub attack: i64,
    pub intro: Option<i64>,
    pub segue: i64,
    pub fade_out: Option<i64>,
    pub cue_out: i64,
    pub flags: BTreeSet<Flag>,
    pub l_int_source: String,
    pub profile: String,
    pub transition_preset: Option<String>,
    pub transition_patch: String,
    pub l_int: f64,
    pub true_peak: f64,
    pub gain_offset_db: f64,
    pub revision: i64,
    /// The longest stretch below -50 dBFS between cue-in and cue-out, ms (1.5; what the station's silence
    /// detector gives the item at least, at unity gain).
    pub longest_quiet_ms: i64,
    /// The same over the whole file (what a `bounds = file` preset, `plexamp`, plays).
    pub longest_quiet_file_ms: i64,
    /// The same for a ladder of thresholds (`analysis::quiet::QuietProfile`, JSON): the station reads it at
    /// its own threshold minus the item's gain.
    pub quiet_profile: String,
}

/// A change to an item's overrides. `None` leaves a field alone; `Some(None)` clears it.
#[derive(Debug, Clone, Default)]
pub struct OverridePatch {
    pub cue_in: Option<Option<i64>>,
    pub attack: Option<Option<i64>>,
    pub intro: Option<Option<i64>>,
    pub segue: Option<Option<i64>>,
    pub fade_out: Option<Option<i64>>,
    /// The fade-out given as milliseconds before the cue-out instead (`fade_out_before_end_ms` of the API):
    /// converted by [`fade_out_at`] against the cue-out this patch produces. `Some(None)` and `Some(Some(0))`
    /// clear the fade-out. Not together with `fade_out`.
    pub fade_out_before_end: Option<Option<i64>>,
    pub cue_out: Option<Option<i64>>,
    pub gain_trim_db: Option<Option<f64>>,
    pub segue_profile: Option<Option<String>>,
    /// The whole `params` JSON object (3.3).
    pub params: Option<Option<Value>>,
}

struct Loaded {
    file_ms: i64,
    l_int: f64,
    lra: f64,
    true_peak: f64,
    stored_flags: BTreeSet<Flag>,
    env: StoredEnvelopes,
    ovr: Overrides,
    params: Value,
    segue_profile_ovr: Option<String>,
    category_profile: String,
    category_preset: Option<String>,
    category_patch: String,
    gain_offset_db: f64,
}

fn load(conn: &Connection, item_id: i64) -> Result<Loaded> {
    let row = conn
        .query_row(
            "SELECT i.file_ms, a.l_int, a.lra, a.true_peak, a.flags,
                    a.env_e10, a.env_e50, a.env_k200, a.env_k10,
                    c.segue_profile, c.transition_preset, c.transition_patch, c.gain_offset_db
             FROM items i JOIN analysis a ON a.item_id = i.id JOIN categories c ON c.id = i.category_id
             WHERE i.id = ?1 AND i.deleted_at IS NULL",
            params![item_id],
            |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, f64>(1)?,
                    r.get::<_, f64>(2)?,
                    r.get::<_, f64>(3)?,
                    r.get::<_, String>(4)?,
                    StoredEnvelopes {
                        e10: r.get(5)?,
                        e50: r.get(6)?,
                        k200: r.get(7)?,
                        k10: r.get(8)?,
                    },
                    r.get::<_, String>(9)?,
                    r.get::<_, Option<String>>(10)?,
                    r.get::<_, String>(11)?,
                    r.get::<_, f64>(12)?,
                ))
            },
        )
        .optional()?
        .ok_or(LibraryError::NoItem(item_id))?;
    #[allow(clippy::type_complexity)]
    let o: Option<(Option<String>, Option<String>, [Option<i64>; 6])> = conn
        .query_row(
            "SELECT params, segue_profile, ovr_cue_in, ovr_attack, ovr_intro, ovr_segue, ovr_fade_out, ovr_cue_out
             FROM item_overrides WHERE item_id = ?1",
            params![item_id],
            |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    [r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?, r.get(7)?],
                ))
            },
        )
        .optional()?;
    let (params_json, prof_ovr, m) = o.unwrap_or((None, None, [None; 6]));
    Ok(Loaded {
        file_ms: row.0,
        l_int: row.1,
        lra: row.2,
        true_peak: row.3,
        stored_flags: flags_from_json(&row.4),
        env: row.5,
        ovr: Overrides {
            cue_in: m[0],
            attack: m[1],
            intro: m[2],
            segue: m[3],
            fade_out: m[4],
            cue_out: m[5],
        },
        params: params_json
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or(Value::Null),
        segue_profile_ovr: prof_ovr,
        category_profile: row.6,
        category_preset: row.7,
        category_patch: row.8,
        gain_offset_db: row.9,
    })
}

/// The profile after the item's `segue_profile` and `params` (3.3).
pub fn resolve_profile(
    conn: &Connection,
    profile_name: &str,
    item_params: &Value,
) -> Result<ProfileParams> {
    let mut p = ProfileParams::builtin(profile_name).unwrap_or_else(ProfileParams::music);
    p.name = profile_name.to_string();
    let row: Option<String> = conn
        .query_row(
            "SELECT params FROM segue_profiles WHERE name = ?1",
            params![profile_name],
            |r| r.get(0),
        )
        .optional()?;
    if let Some(j) = row.and_then(|s| serde_json::from_str::<Value>(&s).ok()) {
        p.apply_json(&j);
    }
    p.apply_json(item_params);
    Ok(p)
}

fn exact_l_w(conn: &Connection, item_id: i64) -> Result<Option<(i64, i64, f64)>> {
    Ok(conn
        .query_row(
            "SELECT c_in, c_out, l_w FROM item_window_loudness WHERE item_id = ?1",
            params![item_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?)
}

/// The fade-out marker, ms from the start of the file, for a fade that starts `before_end_ms` before the cue-out
/// and lasts to it: the one conversion from "seconds before the end", which the item editor and the API use.
/// Nothing, 0 or less is no fade-out (0 s before the end is no fade, not a fade at the start of the file).
pub fn fade_out_at(cue_out: i64, before_end_ms: Option<i64>) -> Option<i64> {
    before_end_ms
        .filter(|&b| b > 0)
        .map(|b| cue_out.saturating_sub(b))
}

/// Merge two JSON objects (`b` over `a`).
fn merge_patch(a: &str, b: Option<&Value>) -> String {
    let mut base: Value = serde_json::from_str(a).unwrap_or(Value::Object(Default::default()));
    if let (Some(o), Some(Value::Object(p))) = (base.as_object_mut(), b) {
        for (k, v) in p {
            o.insert(k.clone(), v.clone());
        }
    }
    base.to_string()
}

/// The threshold `EffectiveRow::longest_quiet_ms` is shown at: the station's default silence threshold (1.5).
pub const QUIET_DISPLAY_DB: f64 = -50.0;

/// Compute the effective markers of `item_id` from what is stored and write `item_effective`
/// (revision + 1). Also bumps `meta.library_revision`.
pub fn materialize(conn: &Connection, item_id: i64) -> Result<EffectiveRow> {
    let l = load(conn, item_id)?;
    let env = l.env.load().map_err(LibraryError::Invalid)?;
    let profile_name = l
        .segue_profile_ovr
        .clone()
        .unwrap_or_else(|| l.category_profile.clone());
    let profile = resolve_profile(conn, &profile_name, &l.params)?;
    let policy = l
        .params
        .get("long_intro_policy")
        .and_then(Value::as_str)
        .and_then(LongIntroPolicy::from_name)
        .unwrap_or(LongIntroPolicy::Play);
    let base: BTreeSet<Flag> = l
        .stored_flags
        .iter()
        .copied()
        .filter(|f| {
            matches!(
                f,
                Flag::Clipped | Flag::GaplessUnknown | Flag::TruncatedFade
            )
        })
        .collect();
    let e = effective_markers(&EffectiveInput {
        facts: FileFacts {
            file_ms: l.file_ms,
            l_int: l.l_int,
            lra: l.lra,
        },
        env: &env,
        profile: &profile,
        ovr: l.ovr,
        policy,
        base_flags: &base,
        l_w_exact: exact_l_w(conn, item_id)?,
    });
    if let Some(w) = e.warning {
        tracing::warn!(item_id, "{w}: stored overrides ignored, auto values used");
    }
    let item_transition = l.params.get("transition");
    let preset = item_transition
        .and_then(|t| t.get("preset"))
        .and_then(Value::as_str)
        .map(str::to_string)
        .or(l.category_preset.clone());
    let patch = merge_patch(
        &l.category_patch,
        item_transition.and_then(|t| t.get("patch")),
    );
    let prev_rev: i64 = conn
        .query_row(
            "SELECT revision FROM item_effective WHERE item_id = ?1",
            params![item_id],
            |r| r.get(0),
        )
        .optional()?
        .unwrap_or(0);
    let quiet = analysis::quiet::QuietProfile::measure(&env.e50, e.cue_in, e.cue_out, l.file_ms);
    let row = EffectiveRow {
        item_id,
        cue_in: e.cue_in,
        attack: e.attack,
        intro: e.intro,
        segue: e.segue,
        fade_out: e.fade_out,
        cue_out: e.cue_out,
        flags: e.flags.clone(),
        l_int_source: e.l_source.name().to_string(),
        profile: profile_name,
        transition_preset: preset,
        transition_patch: patch,
        l_int: e.l_used,
        true_peak: l.true_peak,
        gain_offset_db: l.gain_offset_db,
        revision: prev_rev + 1,
        longest_quiet_ms: quiet.longest_below(QUIET_DISPLAY_DB, false),
        longest_quiet_file_ms: quiet.longest_below(QUIET_DISPLAY_DB, true),
        quiet_profile: quiet.to_json(),
    };
    conn.execute(
        "INSERT INTO item_effective(item_id, cue_in, attack, intro, segue, fade_out, cue_out, flags,
            l_int_source, profile, transition_preset, transition_patch, l_int, true_peak, gain_offset_db, revision,
            quiet_profile, longest_quiet_ms, longest_quiet_file_ms)
         VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19)
         ON CONFLICT(item_id) DO UPDATE SET cue_in=?2, attack=?3, intro=?4, segue=?5, fade_out=?6, cue_out=?7,
            flags=?8, l_int_source=?9, profile=?10, transition_preset=?11, transition_patch=?12, l_int=?13,
            true_peak=?14, gain_offset_db=?15, revision=?16, quiet_profile=?17, longest_quiet_ms=?18,
            longest_quiet_file_ms=?19",
        params![
            item_id,
            row.cue_in,
            row.attack,
            row.intro,
            row.segue,
            row.fade_out,
            row.cue_out,
            flags_to_json(&row.flags),
            row.l_int_source,
            row.profile,
            row.transition_preset,
            row.transition_patch,
            row.l_int,
            row.true_peak,
            row.gain_offset_db,
            row.revision,
            row.quiet_profile,
            row.longest_quiet_ms,
            row.longest_quiet_file_ms
        ],
    )?;
    Library::bump_revision(conn)?;
    Ok(row)
}

pub fn read_effective(conn: &Connection, item_id: i64) -> Result<EffectiveRow> {
    conn.query_row(
        "SELECT cue_in, attack, intro, segue, fade_out, cue_out, flags, l_int_source, profile,
                transition_preset, transition_patch, l_int, true_peak, gain_offset_db, revision,
                longest_quiet_ms, quiet_profile, longest_quiet_file_ms
         FROM item_effective WHERE item_id = ?1",
        params![item_id],
        |r| {
            Ok(EffectiveRow {
                item_id,
                cue_in: r.get(0)?,
                attack: r.get(1)?,
                intro: r.get(2)?,
                segue: r.get(3)?,
                fade_out: r.get(4)?,
                cue_out: r.get(5)?,
                flags: flags_from_json(&r.get::<_, String>(6)?),
                l_int_source: r.get(7)?,
                profile: r.get(8)?,
                transition_preset: r.get(9)?,
                transition_patch: r.get(10)?,
                l_int: r.get(11)?,
                true_peak: r.get(12)?,
                gain_offset_db: r.get(13)?,
                revision: r.get(14)?,
                longest_quiet_ms: r.get(15)?,
                quiet_profile: r.get(16)?,
                longest_quiet_file_ms: r.get(17)?,
            })
        },
    )
    .optional()?
    .ok_or(LibraryError::NoItem(item_id))
}

impl Library {
    pub fn item_effective(&self, item_id: i64) -> Result<EffectiveRow> {
        read_effective(&self.conn, item_id)
    }

    /// Recompute from the stored envelopes (after a profile or algorithm change).
    pub fn recompute_effective(&self, item_id: i64) -> Result<EffectiveRow> {
        materialize(&self.conn, item_id)
    }

    /// Change an item's overrides and recompute its effective markers immediately. A manual
    /// marker set in this call must lie inside the resulting cue window (3.3), and a cue pair
    /// that would leave under 250 ms is refused; stored markers that a cue move leaves outside
    /// the window are kept, ignored and flagged instead.
    pub fn set_overrides(&self, item_id: i64, patch: &OverridePatch) -> Result<EffectiveRow> {
        self.set_overrides_noted(item_id, patch).map(|(row, _)| row)
    }

    /// [`Library::set_overrides`], with the notes for the person who made the change: a fade-out that the
    /// rules moved (it cannot start before the attack, or more than 30 s before the segue) is stored where it
    /// ended up, and a note says so.
    pub fn set_overrides_noted(
        &self,
        item_id: i64,
        patch: &OverridePatch,
    ) -> Result<(EffectiveRow, Vec<String>)> {
        if patch.fade_out.is_some() && patch.fade_out_before_end.is_some() {
            return Err(LibraryError::Invalid(
                "give the fade-out either from the start (fade_out) or before the end (fade_out_before_end_ms), not both"
                    .into(),
            ));
        }
        let mut notes = vec![];
        let tx = self.conn.unchecked_transaction()?;
        let cur = load(&tx, item_id)?;
        let mut ovr = cur.ovr;
        let mut params_v = cur.params.clone();
        let mut prof = cur.segue_profile_ovr.clone();
        let mut trim: Option<Option<f64>> = None;
        macro_rules! apply {
            ($f:ident) => {
                if let Some(v) = patch.$f {
                    ovr.$f = v;
                }
            };
        }
        apply!(cue_in);
        apply!(attack);
        apply!(intro);
        apply!(segue);
        apply!(fade_out);
        apply!(cue_out);
        if let Some(p) = &patch.params {
            params_v = p.clone().unwrap_or(Value::Null);
        }
        if let Some(p) = &patch.segue_profile {
            prof = p.clone();
        }
        if let Some(t) = patch.gain_trim_db {
            trim = Some(t);
        }
        let now = now_iso();
        tx.execute(
            "INSERT INTO item_overrides(item_id, updated_at) VALUES(?1, ?2) ON CONFLICT(item_id) DO NOTHING",
            params![item_id, now],
        )?;
        tx.execute(
            "UPDATE item_overrides SET params=?2, segue_profile=?3, ovr_cue_in=?4, ovr_attack=?5, ovr_intro=?6,
               ovr_segue=?7, ovr_fade_out=?8, ovr_cue_out=?9, updated_at=?10 WHERE item_id=?1",
            params![
                item_id,
                (!params_v.is_null()).then(|| params_v.to_string()),
                prof,
                ovr.cue_in,
                ovr.attack,
                ovr.intro,
                ovr.segue,
                ovr.fade_out,
                ovr.cue_out,
                now
            ],
        )?;
        if let Some(t) = trim {
            tx.execute(
                "UPDATE item_overrides SET ovr_gain_trim_db=?2 WHERE item_id=?1",
                params![item_id, t],
            )?;
        }
        let mut row = materialize(&tx, item_id)?;
        let set_fade = |tx: &Connection, v: Option<i64>| -> Result<EffectiveRow> {
            tx.execute(
                "UPDATE item_overrides SET ovr_fade_out=?2 WHERE item_id=?1",
                params![item_id, v],
            )?;
            materialize(tx, item_id)
        };
        // A fade-out given before the end: against the cue-out this change produced, kept inside the window.
        if let Some(before) = patch.fade_out_before_end {
            let (c_in, c_out) = window_of(&tx, item_id, &row)?;
            ovr.fade_out = fade_out_at(row.cue_out, before).map(|f| f.clamp(c_in, c_out));
            row = set_fade(&tx, ovr.fade_out)?;
        }
        // A fade-out set in this call that the rules moved is stored where it ended up.
        if patch.fade_out.is_some() || patch.fade_out_before_end.is_some() {
            if let (Some(asked), Some(got)) = (ovr.fade_out, row.fade_out) {
                if asked != got {
                    notes.push(format!(
                        "The fade-out cannot start before the attack ({:.1} s) or more than 30 s before the segue \
                         ({:.1} s). It now starts at {:.1} s, {:.1} s before the end.",
                        row.attack as f64 / 1000.0,
                        row.segue as f64 / 1000.0,
                        got as f64 / 1000.0,
                        (row.cue_out - got) as f64 / 1000.0
                    ));
                    ovr.fade_out = Some(got);
                    row = set_fade(&tx, ovr.fade_out)?;
                }
            }
        }
        // Validate what this call set against the window it produced.
        if row.flags.contains(&Flag::CueOverrideInvalid) {
            return Err(LibraryError::Invalid(
                "the cue-in and cue-out leave a window under 250 ms".into(),
            ));
        }
        let (c_in, c_out) = window_of(&tx, item_id, &row)?;
        for (name, patched, value) in [
            ("attack", patch.attack, ovr.attack),
            ("segue", patch.segue, ovr.segue),
            ("intro", patch.intro, ovr.intro),
            ("fade_out", patch.fade_out, ovr.fade_out),
        ] {
            if let (Some(Some(_)), Some(v)) = (patched, value) {
                if v < c_in || v > c_out {
                    return Err(LibraryError::MarkerOutsideWindow {
                        name,
                        value: v,
                        c_in,
                        c_out,
                    });
                }
            }
        }
        tx.commit()?;
        Ok((row, notes))
    }

    /// The item's Plex-compatible ramps `(start, end)` as strings: the ones stored at import (over
    /// the file bounds), or, when a cue is overridden, ramps recomputed inside the effective cue
    /// window from the stored `K10` blocks (3.3, 2.6 `plexamp`).
    pub fn item_ramps(&self, item_id: i64) -> Result<(String, String)> {
        let l = load(&self.conn, item_id)?;
        if l.ovr.cue_in.is_none() && l.ovr.cue_out.is_none() {
            return Ok(self.conn.query_row(
                "SELECT ramp_start, ramp_end FROM analysis WHERE item_id = ?1",
                params![item_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )?);
        }
        let e = read_effective(&self.conn, item_id)?;
        let env = l.env.load().map_err(LibraryError::Invalid)?;
        let (s, en) = analysis::ramp::window_ramps(&env, e.l_int, e.cue_in, e.cue_out);
        Ok((
            analysis::ramp::format_ramp(&s),
            analysis::ramp::format_ramp(&en),
        ))
    }

    /// The exact window loudness of 3.3 (the background job): decode the stored file over the
    /// current cue window, store `L_W`, and recompute. `None` when no cue is overridden.
    pub fn compute_window_loudness(&self, item_id: i64) -> Result<Option<f64>> {
        let row = read_effective(&self.conn, item_id)?;
        if row.l_int_source == "file" {
            return Ok(None);
        }
        let (c_in, c_out) = window_of(&self.conn, item_id, &row)?;
        let store_path: String = self.conn.query_row(
            "SELECT store_path FROM items WHERE id = ?1",
            params![item_id],
            |r| r.get(0),
        )?;
        let l = analysis::analyze::window_loudness_exact(
            &self.paths.media.join(store_path),
            c_in,
            c_out,
        )
        .map_err(|e| LibraryError::Invalid(e.to_string()))?;
        self.conn.execute(
            "INSERT INTO item_window_loudness(item_id, c_in, c_out, l_w, computed_at) VALUES(?1,?2,?3,?4,?5)
             ON CONFLICT(item_id) DO UPDATE SET c_in=?2, c_out=?3, l_w=?4, computed_at=?5",
            params![item_id, c_in, c_out, l, now_iso()],
        )?;
        materialize(&self.conn, item_id)?;
        Ok(Some(l))
    }
}

/// The cue window (before the long-intro policy) that produced `row`, from the stored overrides.
fn window_of(conn: &Connection, item_id: i64, row: &EffectiveRow) -> Result<(i64, i64)> {
    let l = load(conn, item_id)?;
    let env = l.env.load().map_err(LibraryError::Invalid)?;
    let profile = resolve_profile(
        conn,
        l.segue_profile_ovr
            .as_deref()
            .unwrap_or(&l.category_profile),
        &l.params,
    )?;
    let base = BTreeSet::new();
    let e = effective_markers(&EffectiveInput {
        facts: FileFacts {
            file_ms: l.file_ms,
            l_int: l.l_int,
            lra: l.lra,
        },
        env: &env,
        profile: &profile,
        ovr: l.ovr,
        policy: LongIntroPolicy::Play,
        base_flags: &base,
        l_w_exact: exact_l_w(conn, item_id)?,
    });
    let _ = row;
    Ok(e.window)
}

#[cfg(test)]
mod tests {
    use super::fade_out_at;

    #[test]
    fn a_fade_out_before_the_end_is_converted_against_the_cue_out() {
        // 5 s before a cue-out at 208.5 s: the fade starts at 203.5 s and lasts 5 s.
        assert_eq!(fade_out_at(208_500, Some(5_000)), Some(203_500));
        assert_eq!(fade_out_at(208_500, Some(1)), Some(208_499));
        // 0 before the end is no fade, not a fade at the start of the file; nothing is no fade.
        assert_eq!(fade_out_at(208_500, Some(0)), None);
        assert_eq!(fade_out_at(208_500, None), None);
        assert_eq!(fade_out_at(208_500, Some(-3_000)), None);
        // Longer than the item: the start of the file (the window and the rules clamp it further).
        assert_eq!(fade_out_at(208_500, Some(300_000)), Some(-91_500));
    }
}
