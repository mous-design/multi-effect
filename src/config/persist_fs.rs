use std::path::Path;
use std::fs::{read_to_string, write, rename};
use anyhow::{Result,Context};
use serde_json::Value;


/// Round a new copy of the json, with float values to 3 decimal places (removes floating-point noise).
fn round_floats(json: &Value) -> Value {
    match json {
        Value::Number(n) if n.is_f64() => {
            let rounded = (n.as_f64().unwrap() * 1000.0).round() / 1000.0;
            Value::from(rounded)
        },
        Value::Array(arr) => arr.into_iter().map(round_floats).collect(),
        Value::Object(obj) => Value::Object(
            obj.iter().map(|(k, v)| (k.clone(), round_floats(v))).collect()
        ),
        other => other.clone()
    }
}
/// Convert float JSON numbers that are whole numbers back to integers (e.g. 1.0 → 1).
/// Repairs config files that were incorrectly serialized with integer fields as floats.
fn whole_floats_to_int(json: &Value) -> Value {
    match json {
        Value::Number(n) if n.is_f64() => {
            let f = n.as_f64().unwrap();
            if f.fract() == 0.0 {
                Value::from(f as i64)
            } else {
                json.clone() // preserve float as-is
            }
        },
        Value::Array(arr) => arr.into_iter().map(whole_floats_to_int).collect(),
        Value::Object(obj) => Value::Object(
            obj.iter().map(|(k, v)| (k.clone(), whole_floats_to_int(v))).collect()
        ),
        other => other.clone()
    }
}

/// Strip derived fields from a JSON tree before persisting. Recursively
/// removes keys that hold data master can always recompute from canonical /
/// overrides — namely `params_info`. Keeps disk minimal; the in-memory and
/// wire shapes still carry the derived values.
pub fn strip_derived(json: &mut Value) {
    const DERIVED: &[&str] = &["params_info"];
    match json {
        Value::Object(map) => {
            for &k in DERIVED { map.remove(k); }
            for v in map.values_mut() { strip_derived(v); }
        },
        Value::Array(arr) => arr.iter_mut().for_each(strip_derived),
        _ => {},
    }
}

pub fn load(path: &Path) -> Result<Value> {
    let content = read_to_string(path)
        .with_context(|| format!("cannot read config file '{}'", path.display()))?;
    let v: Value = serde_json::from_str(&content)?;
   Ok(whole_floats_to_int(&v))
}

pub fn persist(json: &Value, path: &Path) -> Result<()> {
    let tmp = path.with_extension(
        format!("tmp.{}", path.extension().and_then(|e| e.to_str()).unwrap_or("tmp"))
    );
    let v = round_floats(json);
    let content = serde_json::to_string_pretty(&v)?;
    write(&tmp, &content)?;
    rename(&tmp, path)?;
    Ok(())
}
