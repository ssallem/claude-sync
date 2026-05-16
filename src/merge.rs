use std::collections::BTreeSet;
use std::path::Path;

use anyhow::Context;
use serde_json::{Map, Value};

/// One unresolved location in a JSON deep merge, addressed JSON-pointer style.
#[derive(Debug, Clone)]
pub struct ConflictPath {
    pub path: Vec<String>,
    pub ours: Value,
    pub theirs: Value,
}

/// Result of a 3-way deep merge. `Clean` means nothing required human input.
#[derive(Debug, Clone)]
pub enum MergeOutcome {
    Clean(Value),
    Conflict {
        merged: Value,
        conflicts: Vec<ConflictPath>,
    },
}

/// 3-way deep merge of JSON values. Object-typed branches recurse key by key;
/// arrays and scalars are merged whole — see module-level notes on why.
pub fn deep_merge(base: &Value, ours: &Value, theirs: &Value) -> MergeOutcome {
    let mut conflicts = Vec::new();
    let mut path = Vec::new();
    let merged = merge_node(&mut path, &mut conflicts, base, ours, theirs);
    if conflicts.is_empty() {
        MergeOutcome::Clean(merged)
    } else {
        MergeOutcome::Conflict { merged, conflicts }
    }
}

/// Persist a merge outcome. Conflicts get appended under a top-level
/// `_conflicts` key so users can grep for one literal string to find unresolved
/// merges, then strip the key and push the file back when done.
pub fn write_with_conflict_markers(path: &Path, outcome: &MergeOutcome) -> anyhow::Result<()> {
    let value = match outcome {
        MergeOutcome::Clean(v) => v.clone(),
        MergeOutcome::Conflict { merged, conflicts } => decorate_with_conflicts(merged, conflicts),
    };
    let text = serde_json::to_string_pretty(&value).context("serialize merged JSON")?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("create dir for {}", path.display()))?;
    }
    std::fs::write(path, text).with_context(|| format!("write {}", path.display()))?;
    Ok(())
}

fn merge_node(
    path: &mut Vec<String>,
    conflicts: &mut Vec<ConflictPath>,
    base: &Value,
    ours: &Value,
    theirs: &Value,
) -> Value {
    // No divergence — both sides agree, take it.
    if ours == theirs {
        return ours.clone();
    }
    // One side untouched relative to base — the other side wins by default.
    if ours == base {
        return theirs.clone();
    }
    if theirs == base {
        return ours.clone();
    }
    // Both diverged. Recurse for objects; arrays/scalars are atomic conflicts.
    if matches!(ours, Value::Object(_)) && matches!(theirs, Value::Object(_)) {
        return merge_objects(path, conflicts, base, ours, theirs);
    }
    conflicts.push(ConflictPath {
        path: path.clone(),
        ours: ours.clone(),
        theirs: theirs.clone(),
    });
    // Pick ours as placeholder so downstream tools see *something* parseable.
    ours.clone()
}

fn merge_objects(
    path: &mut Vec<String>,
    conflicts: &mut Vec<ConflictPath>,
    base: &Value,
    ours: &Value,
    theirs: &Value,
) -> Value {
    let empty = Map::new();
    let base_obj = base.as_object().unwrap_or(&empty);
    let ours_obj = ours.as_object().unwrap_or(&empty);
    let theirs_obj = theirs.as_object().unwrap_or(&empty);

    let mut keys: BTreeSet<&String> = BTreeSet::new();
    keys.extend(base_obj.keys());
    keys.extend(ours_obj.keys());
    keys.extend(theirs_obj.keys());

    let mut out = Map::new();
    for key in keys {
        path.push(key.clone());
        if let Some(value) = merge_object_key(path, conflicts, base_obj, ours_obj, theirs_obj, key)
        {
            out.insert(key.clone(), value);
        }
        path.pop();
    }
    Value::Object(out)
}

/// Returns `None` when the key should be omitted (e.g. both sides deleted it).
fn merge_object_key(
    path: &mut Vec<String>,
    conflicts: &mut Vec<ConflictPath>,
    base: &Map<String, Value>,
    ours: &Map<String, Value>,
    theirs: &Map<String, Value>,
    key: &str,
) -> Option<Value> {
    let null = Value::Null;
    let b = base.get(key).unwrap_or(&null);
    match (ours.get(key), theirs.get(key)) {
        (None, None) => None,
        (Some(o), None) => resolve_delete_conflict(path, conflicts, b, o, true),
        (None, Some(t)) => resolve_delete_conflict(path, conflicts, b, t, false),
        (Some(o), Some(t)) => Some(merge_node(path, conflicts, b, o, t)),
    }
}

/// One side kept the key, the other removed it. If the keeper didn't modify
/// from base, honor the deletion silently; otherwise it's a modify/delete
/// conflict and we keep the side that still has a value (with a record).
fn resolve_delete_conflict(
    path: &[String],
    conflicts: &mut Vec<ConflictPath>,
    base: &Value,
    kept: &Value,
    ours_is_kept: bool,
) -> Option<Value> {
    if kept == base {
        return None;
    }
    let (ours, theirs) = if ours_is_kept {
        (kept.clone(), Value::Null)
    } else {
        (Value::Null, kept.clone())
    };
    conflicts.push(ConflictPath {
        path: path.to_vec(),
        ours,
        theirs,
    });
    Some(kept.clone())
}

fn decorate_with_conflicts(merged: &Value, conflicts: &[ConflictPath]) -> Value {
    let mut value = merged.clone();
    let map = match &mut value {
        Value::Object(m) => m,
        // Non-object roots can't host the `_conflicts` sidecar, so wrap them.
        _ => {
            let mut m = Map::new();
            m.insert("_value".to_string(), merged.clone());
            value = Value::Object(m);
            value.as_object_mut().expect("just inserted")
        }
    };
    let arr: Vec<Value> = conflicts.iter().map(conflict_to_json).collect();
    map.insert("_conflicts".to_string(), Value::Array(arr));
    value
}

fn conflict_to_json(c: &ConflictPath) -> Value {
    let mut entry = Map::new();
    let path: Vec<Value> = c.path.iter().map(|s| Value::String(s.clone())).collect();
    entry.insert("path".to_string(), Value::Array(path));
    entry.insert("ours".to_string(), c.ours.clone());
    entry.insert("theirs".to_string(), c.theirs.clone());
    Value::Object(entry)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn clean_when_only_one_side_changes() {
        let base = json!({"a": 1, "b": 2});
        let ours = json!({"a": 1, "b": 2});
        let theirs = json!({"a": 1, "b": 99});
        match deep_merge(&base, &ours, &theirs) {
            MergeOutcome::Clean(v) => assert_eq!(v, json!({"a": 1, "b": 99})),
            other => panic!("expected clean, got {other:?}"),
        }
    }

    #[test]
    fn conflict_on_dual_scalar_change() {
        let base = json!({"font": 12});
        let ours = json!({"font": 14});
        let theirs = json!({"font": 16});
        match deep_merge(&base, &ours, &theirs) {
            MergeOutcome::Conflict { conflicts, .. } => {
                assert_eq!(conflicts.len(), 1);
                assert_eq!(conflicts[0].path, vec!["font".to_string()]);
            }
            other => panic!("expected conflict, got {other:?}"),
        }
    }

    #[test]
    fn nested_object_recurses() {
        let base = json!({"editor": {"font": 12, "tabs": 4}});
        let ours = json!({"editor": {"font": 14, "tabs": 4}});
        let theirs = json!({"editor": {"font": 12, "tabs": 2}});
        match deep_merge(&base, &ours, &theirs) {
            MergeOutcome::Clean(v) => {
                assert_eq!(v, json!({"editor": {"font": 14, "tabs": 2}}))
            }
            other => panic!("expected clean, got {other:?}"),
        }
    }

    #[test]
    fn array_diff_is_a_conflict() {
        let base = json!({"xs": [1, 2]});
        let ours = json!({"xs": [1, 2, 3]});
        let theirs = json!({"xs": [1, 2, 4]});
        match deep_merge(&base, &ours, &theirs) {
            MergeOutcome::Conflict { conflicts, .. } => {
                assert_eq!(conflicts[0].path, vec!["xs".to_string()])
            }
            other => panic!("expected conflict, got {other:?}"),
        }
    }
}
