# CSV Transform — `dev.mcpg.transform.csv`

> class `transform` · `native` · package `mcpg-plugin-transform-csv` · artifact `libmcpg_plugin_transform_csv.so` · Apache-2.0

Transform plugin that converts between delimited text (CSV, TSV, semicolon-
separated, anything with a single-byte delimiter) and JSON. `csv_to_json` parses
a delimited string into an array of row objects or arrays; `json_to_csv`
serialises an array back to delimited text with RFC 4180 quoting. Both
directions are pure compute — no I/O, no host calls, no network — so one
instance serves the gateway's global transform chain and pipeline steps alike.
Reach for it when a tool speaks CSV but the MCP client wants structured rows, or
when a downstream system needs a spreadsheet-shaped payload back.

## What it does
- Parses a delimited string into an array of row objects (with a header row) or an array of arrays (headerless).
- Serialises an array of objects or arrays back to delimited text, quoting cells that contain the delimiter, quotes, or newlines.
- Keeps parsed cells as strings; CSV carries no type information, so no numeric or boolean inference is attempted.
- Pads ragged rows out to the header width, filling missing cells with `""`.
- Derives a deterministic sorted union of object keys when no explicit `columns` order is given.
- Restricts the conversion to a sub-value through an optional RFC 6901 JSON Pointer, splicing the result back and preserving the surrounding payload.
- Rejects any conversion whose serialised output exceeds `max_output_bytes`, so a wide fan-out cannot exhaust gateway memory.
- Declares no `required_capabilities` — it never calls back into the host for network, filesystem, or secret access.

## Configuration
Loaded from the flat top-level `plugins:` list. An entry there joins the global
transform chain and sees every tool call; the same registered plugin can also be
named by a pipeline `plugin_transform` step for a single binding.

```yaml
plugins:
  - id: dev.mcpg.transform.csv
    class: transform
    source: { oci: ghcr.io/mcpg-dev/source-code/plugins/transform-csv:protocol-1 }
    config:
      direction: csv_to_json
      phase: result
      pointer: /structuredContent/report
      delimiter: ";"
```

In the global chain the pre-dispatch value is the tool's `arguments` object and
the post-dispatch value is the serialised tool result — `content`, optional
`structuredContent`, `isError` — so a `phase: result` pointer starts at
`/structuredContent`.

| Field | Type | Default | Description |
|---|---|---|---|
| `direction` | `csv_to_json` \| `json_to_csv` | *(required)* | Conversion direction. |
| `has_headers` | bool | `true` | Parsing: treat the first row as column names and emit row objects. Writing: emit a header row (object input only). |
| `delimiter` | string, one ASCII character | `","` | Field delimiter, for example `";"` or `"\t"`. A multi-character or non-ASCII delimiter is rejected with an error. |
| `columns` | list of string | sorted union of all object keys | Explicit column order for `json_to_csv` over objects. |
| `pointer` | string (RFC 6901) | whole value | Convert only the sub-value at this pointer; the rest of the payload is preserved. |
| `phase` | `arguments` \| `result` \| `both` | `both` | Which dispatch phase the global chain fires this transform on. A pipeline step always dispatches through the result path, so `arguments` there turns the step into a no-op. |
| `max_output_bytes` | integer | `1048576` | Reject conversions whose serialised output exceeds this size. |

Unknown fields are rejected.

Referenced from a pipeline instead, the plugin receives the whole pipeline
context — `arguments`, `tool_name`, `steps`, and `context` — as its input value,
so a pointer addresses a prior step by id:

```yaml
mcp:
  capabilities:
    tools:
      - name: orders.report
        description: Fetch the vendor's orders and hand them back as a CSV table.
        backend:
          kind: pipeline
          steps:
            - kind: http
              id: fetch
              url: https://vendor.example.com/orders.json
            - kind: plugin_transform
              id: table
              plugin: dev.mcpg.transform.csv
              config:
                direction: json_to_csv
                pointer: /steps/fetch/output/orders
```

With a pointer the step result is that context with the pointed-at sub-value
replaced. Without one the conversion is applied to the context object itself,
which both directions reject — `csv_to_json` needs a string and `json_to_csv`
needs an array — so a pipeline step wants a pointer.

## Operations
`csv_to_json` requires the targeted value to be a string. With `has_headers:
true` the first record names the columns and every later record becomes an
object; with `has_headers: false` each record becomes an array of cells. Quoted
fields are unquoted per RFC 4180, so `"x;y"` survives a `;` delimiter intact.

`json_to_csv` requires the targeted value to be an array and decides its shape
from the first element. An array of objects produces an optional header row plus
one row per element, ordered by `columns` or by the sorted key union; an array of
arrays produces bare rows. Cells render as their natural text — strings verbatim,
numbers and booleans in their JSON form, `null` as empty, and nested arrays or
objects as compact JSON. An empty array yields an empty string.

The two directions round-trip: an array of string-valued objects converted to
CSV and back compares equal to the original.

## Observability
Every application through the global chain increments
`mcpg_transform_applies_total` (labels `plugin_id`, `phase` of `pre` or `post`,
`outcome` of `unchanged`, `modified`, or `error`) and records
`mcpg_transform_apply_ms`. A modification also emits the
`mcpg.transform.applied` audit event, which carries hashes and byte counts of
the before and after values rather than their plaintext.

A transform error is not fatal in the global chain: the gateway logs a warning
and carries the last good value forward. Inside a pipeline the same error fails
the step. Choose the wiring that matches how strict you need the conversion to be.

## Build
Default feature set is OFF (avoids `mcpg_plugin_register` linker
collisions in the workspace build); opt in to the cdylib:

```bash
cargo build -p mcpg-plugin-transform-csv --features cdylib-export --release   # → target/release/libmcpg_plugin_transform_csv.so
```

## Sign & load (production)
Sign the artifact, pin/verify via the entry's `signature:` block, and honour
revocations. See <https://mcpg.dev/docs/security/plugin-security>.

## See also
- Pipeline step reference: <https://mcpg.dev/docs/reference/pipeline-steps>
- What a plugin is and how the ABI works: <https://mcpg.dev/docs/plugins/plugins-and-protocol>
- Sibling transforms: `libs/plugins/transform/jsonata`, `libs/plugins/transform/template`, `libs/plugins/transform/xml`
- Validate rather than reshape: `libs/plugins/transform/json-schema`
