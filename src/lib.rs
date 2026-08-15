//! CSV / delimited-text transform plugin.
//!
//! Converts between delimited text and JSON. Stateless apart from the manifest
//! — the direction + options arrive per call in `config`, so one instance
//! serves both the global transform chain (pre/post dispatch) and the pipeline
//! `plugin_transform` bridge. Pure compute; no host calls.
//!
//! - `csv_to_json`: a delimited STRING → an array of row objects (header row)
//!   or arrays (headerless). Cell values stay strings — CSV is untyped, so no
//!   lossy numeric inference is attempted.
//! - `json_to_csv`: an array of objects (→ header + rows) or arrays (→ rows)
//!   → a delimited STRING with RFC 4180 quoting.
//!
//! An optional JSON Pointer (`pointer`) selects a sub-value to transform; the
//! surrounding payload is preserved. Without it the whole value is transformed.

use mcpg_plugin_protocol::{PluginContext, PluginManifest, TransformResult, firstparty_manifest};
use mcpg_plugin_sdk::ffi::SyncTransform;
use serde::Deserialize;
use serde_json::{Map, Value};

const DEFAULT_MAX_OUTPUT_BYTES: usize = 1_048_576;

/// Which dispatch phase(s) a global transform fires on. Ignored by the
/// pipeline bridge (the host calls `transform_result` directly there).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Phase {
    Arguments,
    Result,
    #[default]
    Both,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Direction {
    CsvToJson,
    JsonToCsv,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CsvConfig {
    /// `csv_to_json` parses a delimited string; `json_to_csv` serialises an
    /// array back to delimited text.
    direction: Direction,
    /// CSV parsing: treat the first row as column names → row objects.
    /// CSV writing: emit a header row (object input only). Default `true`.
    #[serde(default = "default_true")]
    has_headers: bool,
    /// Field delimiter. Single ASCII byte (`,` `;` `\t` `|` …). Default `,`.
    #[serde(default = "default_delimiter")]
    delimiter: char,
    /// Explicit column order for `json_to_csv` over objects. When omitted, the
    /// sorted union of all object keys is used (deterministic).
    #[serde(default)]
    columns: Option<Vec<String>>,
    /// JSON Pointer (RFC 6901) to the sub-value to transform. When omitted (or
    /// `""`), the whole value is transformed.
    #[serde(default)]
    pointer: Option<String>,
    #[serde(default)]
    phase: Phase,
    #[serde(default = "default_max_output_bytes")]
    max_output_bytes: usize,
}

fn default_true() -> bool {
    true
}
fn default_delimiter() -> char {
    ','
}
fn default_max_output_bytes() -> usize {
    DEFAULT_MAX_OUTPUT_BYTES
}

pub struct CsvTransform {
    manifest: PluginManifest,
}

impl CsvTransform {
    pub fn new(_config_json: &str) -> Self {
        Self {
            manifest: firstparty_manifest! {
                id: "dev.mcpg.transform.csv",
                name: "CSV Transform",
                class: Transform,
            },
        }
    }

    fn run(&self, value: &Value, config: &Value, phase: Phase) -> TransformResult {
        let cfg: CsvConfig = match serde_json::from_value(config.clone()) {
            Ok(c) => c,
            Err(e) => {
                return TransformResult::Error {
                    message: format!("csv transform config: {e}"),
                };
            }
        };
        // Global-mode phase gating; pipeline-mode always calls transform_result.
        if cfg.phase != Phase::Both && cfg.phase != phase {
            return TransformResult::Unchanged;
        }
        let delimiter = match ascii_byte(cfg.delimiter) {
            Some(b) => b,
            None => {
                return TransformResult::Error {
                    message: format!("delimiter {:?} must be a single ASCII byte", cfg.delimiter),
                };
            }
        };

        // Resolve the sub-value to operate on (whole value when no pointer).
        let ptr = cfg.pointer.as_deref().unwrap_or("");
        let target = match value.pointer(ptr) {
            Some(t) => t,
            None => {
                return TransformResult::Error {
                    message: format!("pointer {ptr:?} not found in value"),
                };
            }
        };

        let produced = match cfg.direction {
            Direction::CsvToJson => csv_to_json(target, delimiter, cfg.has_headers),
            Direction::JsonToCsv => {
                json_to_csv(target, delimiter, cfg.has_headers, cfg.columns.as_deref())
            }
        };
        let produced = match produced {
            Ok(v) => v,
            Err(message) => return TransformResult::Error { message },
        };

        // Bound the transformed sub-value so a fan-out can't exhaust memory.
        match serde_json::to_string(&produced) {
            Ok(s) if s.len() > cfg.max_output_bytes => {
                return TransformResult::Error {
                    message: format!(
                        "csv output {} bytes exceeds max_output_bytes ({})",
                        s.len(),
                        cfg.max_output_bytes
                    ),
                };
            }
            Ok(_) => {}
            Err(e) => {
                return TransformResult::Error {
                    message: format!("output encode: {e}"),
                };
            }
        }

        // Splice back when a pointer was used; otherwise the produced value IS
        // the new whole value.
        if ptr.is_empty() {
            TransformResult::Modified { value: produced }
        } else {
            let mut out = value.clone();
            match out.pointer_mut(ptr) {
                Some(slot) => {
                    *slot = produced;
                    TransformResult::Modified { value: out }
                }
                None => TransformResult::Error {
                    message: format!("pointer {ptr:?} not assignable"),
                },
            }
        }
    }
}

impl SyncTransform for CsvTransform {
    fn manifest(&self) -> &PluginManifest {
        &self.manifest
    }

    fn transform_arguments(
        &self,
        _ctx: &PluginContext,
        arguments: &Value,
        config: &Value,
    ) -> TransformResult {
        self.run(arguments, config, Phase::Arguments)
    }

    fn transform_result(
        &self,
        _ctx: &PluginContext,
        result: &Value,
        config: &Value,
    ) -> TransformResult {
        self.run(result, config, Phase::Result)
    }
}

/// `,` → `Some(b',')`; rejects multi-byte/non-ASCII delimiters (the `csv`
/// crate's delimiter is a single `u8`).
fn ascii_byte(c: char) -> Option<u8> {
    c.is_ascii().then_some(c as u8)
}

/// Parse a delimited string into a JSON array. With headers → array of objects
/// keyed by the header row; without → array of arrays. Cells stay strings.
fn csv_to_json(target: &Value, delimiter: u8, has_headers: bool) -> Result<Value, String> {
    let text = target
        .as_str()
        .ok_or_else(|| "csv_to_json: input value must be a string".to_owned())?;
    let mut rdr = csv::ReaderBuilder::new()
        .delimiter(delimiter)
        .has_headers(has_headers)
        .flexible(true)
        .from_reader(text.as_bytes());

    let mut rows: Vec<Value> = Vec::new();
    if has_headers {
        let headers: Vec<String> = rdr
            .headers()
            .map_err(|e| format!("csv header: {e}"))?
            .iter()
            .map(ToOwned::to_owned)
            .collect();
        for rec in rdr.records() {
            let rec = rec.map_err(|e| format!("csv record: {e}"))?;
            let mut obj = Map::new();
            for (i, header) in headers.iter().enumerate() {
                let cell = rec.get(i).unwrap_or("");
                obj.insert(header.clone(), Value::String(cell.to_owned()));
            }
            rows.push(Value::Object(obj));
        }
    } else {
        for rec in rdr.records() {
            let rec = rec.map_err(|e| format!("csv record: {e}"))?;
            let cells: Vec<Value> = rec.iter().map(|c| Value::String(c.to_owned())).collect();
            rows.push(Value::Array(cells));
        }
    }
    Ok(Value::Array(rows))
}

/// Serialise a JSON array to delimited text. Array-of-objects → optional
/// header + rows (columns explicit or sorted-union); array-of-arrays → rows.
fn json_to_csv(
    target: &Value,
    delimiter: u8,
    has_headers: bool,
    columns: Option<&[String]>,
) -> Result<Value, String> {
    let arr = target
        .as_array()
        .ok_or_else(|| "json_to_csv: input value must be an array".to_owned())?;

    let mut wtr = csv::WriterBuilder::new()
        .delimiter(delimiter)
        .from_writer(Vec::new());

    // Decide shape from the first element; an empty array yields empty output.
    let object_mode = match arr.first() {
        Some(Value::Object(_)) => true,
        Some(Value::Array(_)) => false,
        Some(other) => {
            return Err(format!(
                "json_to_csv: array elements must be objects or arrays, found {}",
                kind_of(other)
            ));
        }
        None => return Ok(Value::String(String::new())),
    };

    if object_mode {
        let cols: Vec<String> = match columns {
            Some(c) => c.to_vec(),
            None => union_keys(arr)?,
        };
        if has_headers {
            wtr.write_record(&cols)
                .map_err(|e| format!("csv write header: {e}"))?;
        }
        for (i, el) in arr.iter().enumerate() {
            let obj = el
                .as_object()
                .ok_or_else(|| format!("json_to_csv: element {i} is not an object"))?;
            let row: Vec<String> = cols
                .iter()
                .map(|c| obj.get(c).map(cell_to_string).unwrap_or_default())
                .collect();
            wtr.write_record(&row)
                .map_err(|e| format!("csv write row: {e}"))?;
        }
    } else {
        for (i, el) in arr.iter().enumerate() {
            let cells = el
                .as_array()
                .ok_or_else(|| format!("json_to_csv: element {i} is not an array"))?;
            let row: Vec<String> = cells.iter().map(cell_to_string).collect();
            wtr.write_record(&row)
                .map_err(|e| format!("csv write row: {e}"))?;
        }
    }

    let bytes = wtr.into_inner().map_err(|e| format!("csv flush: {e}"))?;
    let text = String::from_utf8(bytes).map_err(|e| format!("csv output not UTF-8: {e}"))?;
    Ok(Value::String(text))
}

/// The deterministic sorted union of all object keys across the array.
fn union_keys(arr: &[Value]) -> Result<Vec<String>, String> {
    let mut set = std::collections::BTreeSet::new();
    for (i, el) in arr.iter().enumerate() {
        let obj = el
            .as_object()
            .ok_or_else(|| format!("json_to_csv: element {i} is not an object"))?;
        for k in obj.keys() {
            set.insert(k.clone());
        }
    }
    Ok(set.into_iter().collect())
}

/// Render a JSON cell for delimited output: strings verbatim, scalars via
/// their natural text, null as empty, and arrays/objects as compact JSON.
fn cell_to_string(v: &Value) -> String {
    match v {
        Value::Null => String::new(),
        Value::String(s) => s.clone(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => n.to_string(),
        other => serde_json::to_string(other).unwrap_or_default(),
    }
}

fn kind_of(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "bool",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

// cdylib export — gated so a plain workspace build emits only the rlib (no
// duplicate `mcpg_plugin_register` symbol across plugin crates).
#[cfg(any(feature = "cdylib-export", feature = "static-firstparty"))]
mcpg_plugin_sdk::declare_plugin! {
    plugin_id: "dev.mcpg.transform.csv",
    plugin_version: env!("CARGO_PKG_VERSION"),
    descriptor_yaml: include_str!("../plugin.yaml"),
    capabilities: &[],
    entities: [
        transform as xform {
            inner_name: "",
            plugin_type: CsvTransform,
            factory: |cfg: &str, _host: ::mcpg_plugin_sdk::HostHandle| CsvTransform::new(cfg),
        },
    ],
}

#[cfg(test)]
mod tests;
