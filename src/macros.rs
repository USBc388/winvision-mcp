//! Macro engine: replayable sequences of UI/system steps.
//!
//! A macro is an array of step objects (JSON). Supported step actions:
//!   hotkey | type | click | double_click | right_click | move | drag |
//!   scroll | wait | focus | launch | ocr_click | clipboard_set
//!
//! Named macros are stored as JSON files in
//! `%USERPROFILE%\.winvision-mcp\macros\<name>.json` so the agent (or a
//! human) can reuse one-off sequences as durable system workflows.

use std::path::PathBuf;

use serde_json::Value;

/// Actions a macro step can perform, with their required parameters.
pub const KNOWN_ACTIONS: &[(&str, &[&str])] = &[
    ("hotkey", &["keys"]),
    ("type", &["text"]),
    ("click", &["x", "y"]),
    ("double_click", &["x", "y"]),
    ("right_click", &["x", "y"]),
    ("move", &["x", "y"]),
    ("drag", &["x", "y", "to_x", "to_y"]),
    ("scroll", &["delta"]),
    ("wait", &["ms"]),
    ("focus", &["target"]),
    ("launch", &["command"]),
    ("ocr_click", &["text_query"]),
    ("clipboard_set", &["text"]),
];

pub fn is_known_action(action: &str) -> bool {
    KNOWN_ACTIONS.iter().any(|(a, _)| *a == action)
}

/// Parse and validate the `steps` argument (must be a JSON array of
/// objects with a known `action` and its required parameters).
pub fn parse_steps(steps: &Value) -> Result<Vec<Value>, String> {
    let items = steps
        .as_array()
        .ok_or("steps must be a JSON array of step objects")?;
    if items.is_empty() {
        return Err("steps array is empty".into());
    }
    if items.len() > 50 {
        return Err(format!(
            "too many steps ({}) — max 50 per macro",
            items.len()
        ));
    }
    for (i, s) in items.iter().enumerate() {
        validate_step(i, s)?;
    }
    Ok(items.clone())
}

/// Validate one step: known action + required params present.
pub fn validate_step(index: usize, step: &Value) -> Result<(), String> {
    let obj = step
        .as_object()
        .ok_or_else(|| format!("step {} is not an object", index))?;
    let action = obj
        .get("action")
        .and_then(|v| v.as_str())
        .ok_or_else(|| format!("step {} has no 'action' string", index))?;
    let known = KNOWN_ACTIONS
        .iter()
        .find(|(a, _)| *a == action)
        .ok_or_else(|| {
            let names: Vec<&str> = KNOWN_ACTIONS.iter().map(|(a, _)| *a).collect();
            format!(
                "step {} has unknown action '{}' (known: {})",
                index,
                action,
                names.join(", ")
            )
        })?;
    for req in known.1 {
        if obj.get(*req).is_none() {
            return Err(format!(
                "step {} ('{}') is missing required parameter '{}'",
                index, action, req
            ));
        }
    }
    Ok(())
}

/// Get a string param from a step.
pub fn step_str<'a>(step: &'a Value, key: &str) -> Option<&'a str> {
    step.get(key).and_then(|v| v.as_str())
}

/// Get an i64 param from a step.
pub fn step_i64(step: &Value, key: &str) -> Option<i64> {
    step.get(key).and_then(|v| v.as_i64())
}

// ---- named macro storage ----

pub fn macros_dir() -> PathBuf {
    let home = std::env::var("USERPROFILE").unwrap_or_else(|_| ".".into());
    PathBuf::from(home)
        .join(".winvision-mcp")
        .join("macros")
}

/// Sanitize a macro name: only [a-z A-Z 0-9 _ -] allowed (no traversal).
pub fn sanitize_name(name: &str) -> Result<String, String> {
    let cleaned: String = name.trim().chars().filter(|c| !c.is_control()).collect();
    let ok = !cleaned.is_empty()
        && cleaned.len() <= 64
        && cleaned
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == ' ');
    if !ok {
        return Err(format!(
            "invalid macro name '{name}' — use 1-64 chars: letters, digits, '_', '-', spaces"
        ));
    }
    Ok(cleaned.replace(' ', "-").to_lowercase())
}

fn macro_path(name: &str) -> Result<PathBuf, String> {
    let safe = sanitize_name(name)?;
    Ok(macros_dir().join(format!("{safe}.json")))
}

/// Save a macro (steps array) under a name. Returns the file path.
pub fn save_macro(name: &str, steps: &[Value]) -> Result<String, String> {
    let path = macro_path(name)?;
    let dir = path
        .parent()
        .ok_or("cannot resolve macro directory")?
        .to_path_buf();
    std::fs::create_dir_all(&dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    let doc = serde_json::json!({
        "name": sanitize_name(name)?,
        "steps": steps,
        "saved_at": chrono_iso(),
    });
    std::fs::write(
        &path,
        serde_json::to_string_pretty(&doc).map_err(|e| e.to_string())?,
    )
    .map_err(|e| format!("write failed: {e}"))?;
    Ok(path.display().to_string())
}

/// Load a saved macro's steps by name.
pub fn load_macro(name: &str) -> Result<Vec<Value>, String> {
    let path = macro_path(name)?;
    let raw = std::fs::read_to_string(&path)
        .map_err(|_| format!("macro '{name}' not found ({})", path.display()))?;
    let doc: Value = serde_json::from_str(&raw).map_err(|e| format!("corrupt macro file: {e}"))?;
    let steps = doc
        .get("steps")
        .cloned()
        .ok_or("corrupt macro file: no steps")?;
    parse_steps(&steps)
}

/// List saved macros: (name, step count, modified unix ms).
pub fn list_macros() -> Vec<(String, usize, u64)> {
    let dir = macros_dir();
    let mut out = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&dir) {
        for e in entries.flatten() {
            let p = e.path();
            if p.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            let name = p
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("?")
                .to_string();
            let (count, modified) = (|| -> Option<(usize, u64)> {
                let doc: Value =
                    serde_json::from_str(&std::fs::read_to_string(&p).ok()?).ok()?;
                let count = doc.get("steps")?.as_array()?.len();
                let modified = e.metadata().ok()?.modified().ok()?;
                let ms = modified
                    .duration_since(std::time::UNIX_EPOCH)
                    .ok()?
                    .as_millis() as u64;
                Some((count, ms))
            })()
            .unwrap_or((0, 0));
            out.push((name, count, modified));
        }
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

/// Delete a saved macro by name.
pub fn delete_macro(name: &str) -> Result<String, String> {
    let path = macro_path(name)?;
    if !path.exists() {
        return Err(format!("macro '{name}' not found"));
    }
    std::fs::remove_file(&path).map_err(|e| format!("delete failed: {e}"))?;
    Ok(path.display().to_string())
}

fn chrono_iso() -> String {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs().to_string())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parse_steps_valid() {
        let steps = json!([
            {"action": "focus", "target": "notepad"},
            {"action": "click", "x": 100, "y": 200},
            {"action": "type", "text": "hello"},
            {"action": "wait", "ms": 300},
            {"action": "hotkey", "keys": "Ctrl+s"}
        ]);
        let parsed = parse_steps(&steps).expect("valid");
        assert_eq!(parsed.len(), 5);
    }

    #[test]
    fn parse_steps_rejects_unknown_action_and_missing_params() {
        assert!(parse_steps(&json!([{"action": "fly", "x": 1}])).is_err());
        assert!(parse_steps(&json!([{"action": "click"}])).is_err()); // no x,y
        assert!(parse_steps(&json!([{"action": "type"}])).is_err()); // no text
        assert!(parse_steps(&json!([{"not_a_step": 1}])).is_err());
        assert!(parse_steps(&json!([])).is_err()); // empty
        assert!(parse_steps(&json!("nope")).is_err()); // not an array
    }

    #[test]
    fn parse_steps_rejects_more_than_50() {
        let many: Vec<Value> = (0..51)
            .map(|i| json!({"action": "wait", "ms": 10 + i}))
            .collect();
        let err = parse_steps(&json!(many)).unwrap_err();
        assert!(err.contains("max 50"), "{err}");
    }

    #[test]
    fn sanitize_name_blocks_traversal() {
        assert!(sanitize_name("../evil").is_err());
        assert!(sanitize_name("").is_err());
        assert!(sanitize_name("ok_name-1").is_ok());
        assert_eq!(sanitize_name("My Macro 2").unwrap(), "my-macro-2");
    }

    #[test]
    fn known_actions_cover_documented_set() {
        for a in [
            "hotkey", "type", "click", "double_click", "right_click", "move", "drag", "scroll",
            "wait", "focus", "launch", "ocr_click", "clipboard_set",
        ] {
            assert!(is_known_action(a), "missing {a}");
        }
        assert!(!is_known_action("nope"));
    }

    #[test]
    fn save_load_delete_roundtrip() {
        let name = format!("test-macro-{}", std::process::id());
        let steps = json!([{"action": "wait", "ms": 50}]);
        let path = save_macro(&name, steps.as_array().unwrap()).expect("save");
        assert!(std::path::Path::new(&path).exists());
        let loaded = load_macro(&name).expect("load");
        assert_eq!(loaded.len(), 1);
        let listed = list_macros();
        assert!(listed.iter().any(|(n, c, _)| *n == sanitize_name(&name).unwrap() && *c == 1));
        let deleted = delete_macro(&name).expect("delete");
        assert!(!std::path::Path::new(&deleted).exists());
        assert!(load_macro(&name).is_err());
    }
}
