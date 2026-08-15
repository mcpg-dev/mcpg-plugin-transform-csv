use mcpg_plugin_protocol::{PluginContext, PluginIdentity, TransformResult};
use mcpg_plugin_sdk::ffi::SyncTransform;
use serde_json::json;

use super::CsvTransform;

fn ctx() -> PluginContext {
    PluginContext {
        request_id: "t".into(),
        session_id: None,
        tool_name: "x".into(),
        surface: "tool".into(),
        identity: PluginIdentity {
            kind: "anonymous".into(),
            trust_level: "unauthenticated".into(),
            subject_id: None,
            auth_provider: None,
            issuer: None,
            roles: Vec::new(),
            groups: Vec::new(),
            scopes: Vec::new(),
            attributes: Default::default(),
        },
        transport: "http".into(),
    }
}

fn modified(r: TransformResult) -> serde_json::Value {
    match r {
        TransformResult::Modified { value } => value,
        other => panic!("expected Modified, got {other:?}"),
    }
}

fn error_msg(r: TransformResult) -> String {
    match r {
        TransformResult::Error { message } => message,
        other => panic!("expected Error, got {other:?}"),
    }
}

// ── csv_to_json ─────────────────────────────────────────────────────────────

#[test]
fn csv_to_json_with_headers_yields_row_objects() {
    let p = CsvTransform::new("{}");
    let cfg = json!({ "direction": "csv_to_json" });
    let input = json!("name,age\r\nalice,30\r\nbob,25\r\n");
    let out = modified(p.transform_result(&ctx(), &input, &cfg));
    assert_eq!(
        out,
        json!([
            { "name": "alice", "age": "30" },
            { "name": "bob", "age": "25" },
        ])
    );
}

#[test]
fn csv_to_json_headerless_yields_arrays() {
    let p = CsvTransform::new("{}");
    let cfg = json!({ "direction": "csv_to_json", "has_headers": false });
    let input = json!("alice,30\nbob,25\n");
    let out = modified(p.transform_result(&ctx(), &input, &cfg));
    assert_eq!(out, json!([["alice", "30"], ["bob", "25"]]));
}

#[test]
fn csv_to_json_honours_custom_delimiter_and_quoting() {
    let p = CsvTransform::new("{}");
    let cfg = json!({ "direction": "csv_to_json", "delimiter": ";" });
    // A quoted field contains the delimiter and a comma — RFC 4180 unquoting.
    let input = json!("a;b\r\n\"x;y\";\"has,comma\"\r\n");
    let out = modified(p.transform_result(&ctx(), &input, &cfg));
    assert_eq!(out, json!([{ "a": "x;y", "b": "has,comma" }]));
}

#[test]
fn csv_to_json_ragged_row_pads_missing_cells() {
    let p = CsvTransform::new("{}");
    let cfg = json!({ "direction": "csv_to_json" });
    let input = json!("a,b,c\r\n1,2\r\n");
    let out = modified(p.transform_result(&ctx(), &input, &cfg));
    assert_eq!(out, json!([{ "a": "1", "b": "2", "c": "" }]));
}

#[test]
fn csv_to_json_rejects_non_string_input() {
    let p = CsvTransform::new("{}");
    let cfg = json!({ "direction": "csv_to_json" });
    let msg = error_msg(p.transform_result(&ctx(), &json!({ "not": "a string" }), &cfg));
    assert!(msg.contains("must be a string"), "{msg}");
}

// ── json_to_csv ─────────────────────────────────────────────────────────────

#[test]
fn json_to_csv_objects_with_explicit_columns() {
    let p = CsvTransform::new("{}");
    let cfg = json!({ "direction": "json_to_csv", "columns": ["name", "age"] });
    let input = json!([{ "age": "30", "name": "alice" }, { "name": "bob", "age": "25" }]);
    let out = modified(p.transform_result(&ctx(), &input, &cfg));
    assert_eq!(out, json!("name,age\nalice,30\nbob,25\n"));
}

#[test]
fn json_to_csv_objects_derive_sorted_union_columns() {
    let p = CsvTransform::new("{}");
    let cfg = json!({ "direction": "json_to_csv" });
    // Second row has an extra key → union {age,city,name} sorted.
    let input = json!([{ "name": "alice", "age": "30" }, { "name": "bob", "city": "NYC" }]);
    let out = modified(p.transform_result(&ctx(), &input, &cfg));
    assert_eq!(out, json!("age,city,name\n30,,alice\n,NYC,bob\n"));
}

#[test]
fn json_to_csv_quotes_cells_with_delimiter_and_renders_scalars() {
    let p = CsvTransform::new("{}");
    let cfg = json!({ "direction": "json_to_csv", "columns": ["s", "n", "b", "z"] });
    let input = json!([{ "s": "has,comma", "n": 42, "b": true, "z": null }]);
    let out = modified(p.transform_result(&ctx(), &input, &cfg));
    assert_eq!(out, json!("s,n,b,z\n\"has,comma\",42,true,\n"));
}

#[test]
fn json_to_csv_arrays_without_header() {
    let p = CsvTransform::new("{}");
    let cfg = json!({ "direction": "json_to_csv" });
    let input = json!([["a", "b"], ["c", "d"]]);
    let out = modified(p.transform_result(&ctx(), &input, &cfg));
    assert_eq!(out, json!("a,b\nc,d\n"));
}

#[test]
fn json_to_csv_objects_can_suppress_header() {
    let p = CsvTransform::new("{}");
    let cfg = json!({ "direction": "json_to_csv", "columns": ["x"], "has_headers": false });
    let input = json!([{ "x": "1" }, { "x": "2" }]);
    let out = modified(p.transform_result(&ctx(), &input, &cfg));
    assert_eq!(out, json!("1\n2\n"));
}

#[test]
fn json_to_csv_empty_array_is_empty_string() {
    let p = CsvTransform::new("{}");
    let cfg = json!({ "direction": "json_to_csv" });
    let out = modified(p.transform_result(&ctx(), &json!([]), &cfg));
    assert_eq!(out, json!(""));
}

#[test]
fn json_to_csv_rejects_non_array_input() {
    let p = CsvTransform::new("{}");
    let cfg = json!({ "direction": "json_to_csv" });
    let msg = error_msg(p.transform_result(&ctx(), &json!("nope"), &cfg));
    assert!(msg.contains("must be an array"), "{msg}");
}

// ── round-trip ──────────────────────────────────────────────────────────────

#[test]
fn round_trips_objects_through_csv_and_back() {
    let p = CsvTransform::new("{}");
    let rows = json!([{ "a": "1", "b": "2" }, { "a": "3", "b": "4" }]);
    let to_csv =
        modified(p.transform_result(&ctx(), &rows, &json!({ "direction": "json_to_csv" })));
    let back =
        modified(p.transform_result(&ctx(), &to_csv, &json!({ "direction": "csv_to_json" })));
    assert_eq!(back, rows);
}

// ── pointer targeting ───────────────────────────────────────────────────────

#[test]
fn pointer_transforms_subfield_and_preserves_rest() {
    let p = CsvTransform::new("{}");
    let cfg = json!({ "direction": "csv_to_json", "pointer": "/data" });
    let input = json!({ "data": "a,b\r\n1,2\r\n", "meta": { "rows": 1 } });
    let out = modified(p.transform_result(&ctx(), &input, &cfg));
    assert_eq!(
        out,
        json!({ "data": [{ "a": "1", "b": "2" }], "meta": { "rows": 1 } })
    );
}

#[test]
fn pointer_not_found_is_error() {
    let p = CsvTransform::new("{}");
    let cfg = json!({ "direction": "csv_to_json", "pointer": "/missing" });
    let msg = error_msg(p.transform_result(&ctx(), &json!({ "data": "a\n1\n" }), &cfg));
    assert!(msg.contains("not found"), "{msg}");
}

// ── phase gating ────────────────────────────────────────────────────────────

#[test]
fn phase_result_skips_arguments_phase() {
    let p = CsvTransform::new("{}");
    let cfg = json!({ "direction": "csv_to_json", "phase": "result" });
    // transform_arguments fires the Arguments phase → gated out → Unchanged.
    assert!(matches!(
        p.transform_arguments(&ctx(), &json!("a\n1\n"), &cfg),
        TransformResult::Unchanged
    ));
    // transform_result fires the Result phase → runs.
    assert!(matches!(
        p.transform_result(&ctx(), &json!("a\n1\n"), &cfg),
        TransformResult::Modified { .. }
    ));
}

// ── guards ──────────────────────────────────────────────────────────────────

#[test]
fn rejects_unknown_config_field() {
    let p = CsvTransform::new("{}");
    let cfg = json!({ "direction": "csv_to_json", "bogus": true });
    let msg = error_msg(p.transform_result(&ctx(), &json!("a\n1\n"), &cfg));
    assert!(msg.contains("config"), "{msg}");
}

#[test]
fn rejects_non_ascii_delimiter() {
    let p = CsvTransform::new("{}");
    let cfg = json!({ "direction": "csv_to_json", "delimiter": "€" });
    let msg = error_msg(p.transform_result(&ctx(), &json!("a\n1\n"), &cfg));
    assert!(msg.contains("ASCII"), "{msg}");
}

#[test]
fn enforces_max_output_bytes() {
    let p = CsvTransform::new("{}");
    let cfg = json!({ "direction": "csv_to_json", "max_output_bytes": 4 });
    let msg = error_msg(p.transform_result(&ctx(), &json!("name,age\r\nalice,30\r\n"), &cfg));
    assert!(msg.contains("max_output_bytes"), "{msg}");
}
