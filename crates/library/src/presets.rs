//! Processing presets (M6a): the saved settings of each effect of the broadcast chain, the way a DAW keeps
//! plug-in presets. The shipped ones come from the effect's own code and cannot be changed; the user's are
//! rows of `processing_presets`, saved, replaced, renamed, deleted, exported and imported by name.
//! Every effect (loudness now, the AGC, EQ, multiband and limiter with M6) uses the same table and calls.

use crate::LibraryError;
use rusqlite::{params, Connection};
use serde_json::{json, Value};

type Result<T> = std::result::Result<T, LibraryError>;

/// One entry of a listing.
#[derive(Debug, Clone, PartialEq)]
pub struct Preset {
    pub name: String,
    pub params: Value,
    pub description: String,
    pub builtin: bool,
}

/// The effects that have presets, and what each ships.
pub fn effects() -> &'static [&'static str] {
    &[
        "loudness",
        "chain",
        "trim",
        "highpass",
        "rotator",
        "stereo",
        "agc",
        "eq",
        "bass",
        "multiband",
        "wideband",
        "makeup",
        "clipper",
        "limiter",
    ]
}

/// The shipped presets of an effect.
pub fn shipped(effect: &str) -> Vec<Preset> {
    match effect {
        "loudness" => common::config::LOUDNESS_PRESETS
            .iter()
            .map(|p| {
                let mut c = common::config::LoudnessConfig::default();
                c.apply_preset(p.name);
                Preset {
                    name: p.name.to_string(),
                    params: c.params_json(),
                    description: p.help.to_string(),
                    builtin: true,
                }
            })
            .collect(),
        // The whole chain: one entry per shipped chain preset, holding every block's settings and switch.
        "chain" => common::chain::CHAIN_PRESETS
            .iter()
            .map(|p| Preset {
                name: p.name.to_string(),
                params: common::chain::preset_config(p.name)
                    .map(|c| c.to_json())
                    .unwrap_or(Value::Null),
                description: p.help.to_string(),
                builtin: true,
            })
            .collect(),
        // One block of the chain (without its switch).
        b if common::chain::block(b).is_some() => common::chain::effect_presets(b)
            .into_iter()
            .map(|p| Preset {
                name: p.name.to_string(),
                params: p.params,
                description: p.help.to_string(),
                builtin: true,
            })
            .collect(),
        _ => vec![],
    }
}

/// Check the settings of a preset against the effect's rules: every key known, every value in range.
pub fn validate(effect: &str, params: &Value) -> std::result::Result<(), String> {
    match effect {
        "loudness" => {
            let obj = params
                .as_object()
                .ok_or("the settings must be a JSON object")?;
            if obj.contains_key("enabled")
                || obj.contains_key("preset")
                || obj.contains_key("user_preset")
            {
                return Err(
                    "a preset holds the effect's settings, not its on/off switch or its own name"
                        .into(),
                );
            }
            let mut c = common::config::LoudnessConfig::default();
            c.apply_params(obj)?;
            match c.check().first() {
                Some((k, m)) => Err(format!("`{k}` {m}")),
                None => Ok(()),
            }
        }
        "chain" => {
            let obj = params
                .as_object()
                .ok_or("the settings must be a JSON object of blocks")?;
            if obj.contains_key("user_preset") || obj.contains_key("preset") {
                return Err("a chain preset holds the blocks' settings, not a preset name".into());
            }
            common::chain::ChainConfig::default().apply_all(params)
        }
        b if common::chain::block(b).is_some() => {
            let obj = params
                .as_object()
                .ok_or("the settings must be a JSON object")?;
            if obj.contains_key("enabled") || obj.contains_key("user_preset") {
                return Err(
                    "a preset holds the block's settings, not its on/off switch or its own name"
                        .into(),
                );
            }
            common::chain::ChainConfig::default().set_block_params(b, obj)
        }
        _ => Err(format!("no effect `{effect}` has presets")),
    }
}

fn name_ok(name: &str) -> std::result::Result<(), String> {
    let t = name.trim();
    if t.is_empty() || t.len() > 60 || t != name || name.chars().any(char::is_control) {
        return Err(
            "a preset name is 1 to 60 characters, with no leading or trailing space".into(),
        );
    }
    Ok(())
}

fn invalid(m: String) -> LibraryError {
    LibraryError::Invalid(m)
}

/// A stored preset with the keys of older releases renamed (the equalizer's fixed bands became bands 1 to 4).
fn migrated(effect: &str, params: Value) -> Value {
    match effect {
        "chain" => common::chain::migrate_chain(&params),
        b if common::chain::block(b).is_some() => match params.as_object() {
            Some(o) => Value::Object(common::chain::migrate_params(b, o)),
            None => params,
        },
        _ => params,
    }
}

/// Shipped presets first, then the user's by name.
pub fn list(conn: &Connection, effect: &str) -> Result<Vec<Preset>> {
    let mut out = shipped(effect);
    let mut st = conn.prepare(
        "SELECT name, params, description, builtin FROM processing_presets WHERE effect = ?1 ORDER BY name",
    )?;
    let rows = st.query_map([effect], |r| {
        let effect_key = effect;
        let params: Value = serde_json::from_str(&r.get::<_, String>(1)?).unwrap_or(Value::Null);
        Ok(Preset {
            name: r.get(0)?,
            params: migrated(effect_key, params),
            description: r.get(2)?,
            builtin: r.get::<_, i64>(3)? != 0,
        })
    })?;
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

pub fn get(conn: &Connection, effect: &str, name: &str) -> Result<Option<Preset>> {
    Ok(list(conn, effect)?.into_iter().find(|p| p.name == name))
}

fn is_shipped(effect: &str, name: &str) -> bool {
    shipped(effect).iter().any(|p| p.name == name)
}

/// Save (or replace) a user preset. A shipped name is refused: "save as" under another name.
pub fn put(
    conn: &Connection,
    effect: &str,
    name: &str,
    params_v: &Value,
    description: &str,
) -> Result<()> {
    name_ok(name).map_err(invalid)?;
    validate(effect, params_v).map_err(invalid)?;
    if is_shipped(effect, name) {
        return Err(invalid(format!(
            "`{name}` is a shipped preset and cannot be changed; save it under another name"
        )));
    }
    conn.execute(
        "INSERT INTO processing_presets(effect, name, params, description, builtin, updated_at)
         VALUES(?1, ?2, ?3, ?4, 0, strftime('%Y-%m-%dT%H:%M:%SZ','now'))
         ON CONFLICT(effect, name) DO UPDATE SET params = ?3, description = ?4,
            updated_at = strftime('%Y-%m-%dT%H:%M:%SZ','now')",
        params![effect, name, params_v.to_string(), description],
    )?;
    crate::Library::bump_revision(conn)?;
    Ok(())
}

/// Delete a user preset; `false` when there is none by that name. A shipped one is refused.
pub fn delete(conn: &Connection, effect: &str, name: &str) -> Result<bool> {
    if is_shipped(effect, name) {
        return Err(invalid(format!(
            "`{name}` is a shipped preset and cannot be deleted"
        )));
    }
    let n = conn.execute(
        "DELETE FROM processing_presets WHERE effect = ?1 AND name = ?2 AND builtin = 0",
        params![effect, name],
    )?;
    if n > 0 {
        crate::Library::bump_revision(conn)?;
    }
    Ok(n > 0)
}

/// Rename a user preset. Refused for a shipped one, an unknown one, or a name already taken.
pub fn rename(conn: &Connection, effect: &str, from: &str, to: &str) -> Result<()> {
    name_ok(to).map_err(invalid)?;
    if is_shipped(effect, from) {
        return Err(invalid(format!(
            "`{from}` is a shipped preset and cannot be renamed"
        )));
    }
    if is_shipped(effect, to) || get(conn, effect, to)?.is_some() {
        return Err(invalid(format!("there is already a preset named `{to}`")));
    }
    let n = conn.execute(
        "UPDATE processing_presets SET name = ?3 WHERE effect = ?1 AND name = ?2 AND builtin = 0",
        params![effect, from, to],
    )?;
    if n == 0 {
        return Err(invalid(format!("no preset `{from}`")));
    }
    crate::Library::bump_revision(conn)?;
    Ok(())
}

/// The file `Export` writes and `Import` reads: one preset, self-describing.
pub fn export(effect: &str, p: &Preset) -> Value {
    json!({"hotclock_preset": 1, "effect": effect, "name": p.name, "description": p.description, "params": p.params})
}

/// Import an exported preset: the file must name this effect, its settings must pass the effect's checks, and
/// an existing name is kept unless `overwrite` says otherwise. Returns the name it was stored under.
pub fn import(
    conn: &Connection,
    effect: &str,
    file: &Value,
    overwrite: bool,
    rename_to: Option<&str>,
) -> Result<String> {
    if file["hotclock_preset"] != 1 {
        return Err(invalid(
            "this is not a preset file (no `hotclock_preset: 1`)".into(),
        ));
    }
    if file["effect"] != effect {
        return Err(invalid(format!(
            "this preset is for effect `{}`, not `{effect}`",
            file["effect"].as_str().unwrap_or("?")
        )));
    }
    let name = rename_to
        .or(file["name"].as_str())
        .ok_or_else(|| invalid("the file has no name".into()))?;
    if !overwrite && get(conn, effect, name)?.is_some() {
        return Err(invalid(format!("there is already a preset named `{name}`")));
    }
    put(
        conn,
        effect,
        name,
        &file["params"],
        file["description"].as_str().unwrap_or(""),
    )?;
    Ok(name.to_string())
}
