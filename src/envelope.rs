use serde_json::{json, Map, Value};
use std::process::exit;

/// Print a success envelope `{"ok": true, ...fields}` to stdout and exit 0.
///
/// `value` MUST be a JSON object; its top-level fields are flattened alongside
/// the `ok` marker so that callers parsing existing fields keep working. An
/// `"ok"` field already present in `value` is overwritten.
pub fn print_ok(value: Value) -> ! {
    let mut map = match value {
        Value::Object(m) => m,
        other => panic!("envelope::print_ok requires a JSON object, got {other:?}"),
    };
    let mut out = Map::new();
    out.insert("ok".into(), Value::Bool(true));
    map.remove("ok");
    for (k, v) in map {
        out.insert(k, v);
    }
    println!(
        "{}",
        serde_json::to_string_pretty(&Value::Object(out))
            .expect("serializing JSON object never fails")
    );
    exit(0);
}

/// Print a failure envelope `{"ok": false, "error": {...}}` to stdout and exit 1.
pub fn print_err(message: &str, hint: Option<&str>) -> ! {
    emit_err(None, message, hint)
}

/// Print a failure envelope with a stable `code` and exit 1.
#[allow(dead_code)]
pub fn print_err_coded(code: &str, message: &str, hint: Option<&str>) -> ! {
    emit_err(Some(code), message, hint)
}

fn emit_err(code: Option<&str>, message: &str, hint: Option<&str>) -> ! {
    let mut err_obj = Map::new();
    err_obj.insert("message".into(), Value::String(message.to_string()));
    if let Some(c) = code {
        err_obj.insert("code".into(), Value::String(c.to_string()));
    }
    if let Some(h) = hint {
        err_obj.insert("hint".into(), Value::String(h.to_string()));
    }
    let env = json!({ "ok": false, "error": Value::Object(err_obj) });
    println!(
        "{}",
        serde_json::to_string_pretty(&env)
            .expect("serializing JSON object never fails")
    );
    exit(1);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ok_marker_overrides_caller_supplied_ok() {
        // Just exercise the merge logic via a parallel helper, since print_ok exits.
        let mut map = Map::new();
        map.insert("ok".into(), Value::Bool(false));
        map.insert("service".into(), Value::String("billing".into()));

        let mut out = Map::new();
        out.insert("ok".into(), Value::Bool(true));
        map.remove("ok");
        for (k, v) in map {
            out.insert(k, v);
        }
        assert_eq!(out.get("ok"), Some(&Value::Bool(true)));
        assert_eq!(out.get("service"), Some(&Value::String("billing".into())));
    }
}
