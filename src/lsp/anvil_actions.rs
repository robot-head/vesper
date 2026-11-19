//! *Vesper Lens* code actions for Vesper Anvil (`docs/rfcs/vesper-anvil.md`, Phase 4).
//!
//! Produces pure LSP `CodeAction` JSON payloads (`vesper.code-action/1`) for:
//! 1. Running an individual `@test` function via `vesper.anvilRun`, and
//! 2. Pinning a failing property counterexample directly above the `@test`
//!    function as `@test_case(<args>)`.

use serde_json::{Value as Json, json};

/// LSP command identifier for running a single `@test` in Vesper Anvil.
pub const ANVIL_RUN_COMMAND: &str = "vesper.anvilRun";

/// Schema identifier shared with [`crate::lsp::actions`].
pub const CODE_ACTION_SCHEMA: &str = "vesper.code-action/1";

/// Build LSP `CodeAction` JSON objects for an Anvil `@test` function at
/// `fn_line` (0-indexed) in `uri`.
///
/// Always includes a `"Run `@test` in Vesper Anvil"` command action
/// (`vesper.anvilRun`). When `failing_args` is `Some(args)`, also includes a
/// 1-click `"Pin counterexample as `@test_case(...)`"` quickfix edit inserting
/// `@test_case(<args>)\n` directly above the `@test` function.
pub fn anvil_code_actions(
    uri: &str,
    source: &str,
    fn_name: &str,
    fn_line: u32,
    failing_args: Option<&str>,
) -> Vec<Json> {
    let mut actions = Vec::with_capacity(2);

    let run_title = "Run `@test` in Vesper Anvil";
    actions.push(json!({
        "title": run_title,
        "kind": "source",
        "command": {
            "title": run_title,
            "command": ANVIL_RUN_COMMAND,
            "arguments": [uri, fn_name],
        },
        "data": {
            "schema": CODE_ACTION_SCHEMA,
            "action": "anvil.run",
            "uri": uri,
            "function": fn_name,
            "line": fn_line,
        },
    }));

    if let Some(raw_args) = failing_args {
        let args = raw_args.trim();
        let lines: Vec<&str> = source.lines().collect();
        let insert_line = (fn_line as usize).min(lines.len()) as u32;
        let indent: String = lines
            .get(insert_line as usize)
            .map(|line| {
                line.chars()
                    .take_while(|c| *c == ' ' || *c == '\t')
                    .collect()
            })
            .unwrap_or_default();
        let new_text = format!("{indent}@test_case({args})\n");

        let mut changes = serde_json::Map::new();
        changes.insert(
            uri.to_string(),
            json!([
                {
                    "range": {
                        "start": { "line": insert_line, "character": 0 },
                        "end": { "line": insert_line, "character": 0 },
                    },
                    "newText": new_text,
                }
            ]),
        );

        actions.push(json!({
            "title": "Pin counterexample as `@test_case(...)`",
            "kind": "quickfix",
            "edit": {
                "changes": Json::Object(changes),
            },
            "data": {
                "schema": CODE_ACTION_SCHEMA,
                "action": "anvil.pin_test_case",
                "uri": uri,
                "function": fn_name,
                "line": insert_line,
                "args": args,
            },
        }));
    }

    actions
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn emits_run_action_without_failing_args() {
        let uri = "file:///workspace/math.ves";
        let src = "@test\nfn test_add() -> bool {\n    1 + 1 == 2\n}\n";
        let actions = anvil_code_actions(uri, src, "test_add", 0, None);
        assert_eq!(actions.len(), 1);
        assert_eq!(actions[0]["title"], "Run `@test` in Vesper Anvil");
        assert_eq!(actions[0]["command"]["command"], ANVIL_RUN_COMMAND);
        assert_eq!(actions[0]["command"]["arguments"][0], uri);
        assert_eq!(actions[0]["command"]["arguments"][1], "test_add");
        assert_eq!(actions[0]["data"]["schema"], CODE_ACTION_SCHEMA);
    }

    #[test]
    fn emits_pin_counterexample_edit_preserving_indentation() {
        let uri = "file:///workspace/prop.ves";
        let src = "    @test(cases = 100)\n    fn prop_small(x: u64) -> bool {\n        x < 10\n    }\n";
        let actions = anvil_code_actions(uri, src, "prop_small", 0, Some("10"));
        assert_eq!(actions.len(), 2);

        let pin = &actions[1];
        assert_eq!(pin["title"], "Pin counterexample as `@test_case(...)`");
        assert_eq!(pin["kind"], "quickfix");
        assert_eq!(pin["data"]["schema"], CODE_ACTION_SCHEMA);
        assert_eq!(pin["data"]["action"], "anvil.pin_test_case");

        let edits = pin["edit"]["changes"][uri]
            .as_array()
            .expect("edits array for uri");
        assert_eq!(edits.len(), 1);
        assert_eq!(edits[0]["range"]["start"]["line"], 0);
        assert_eq!(edits[0]["range"]["start"]["character"], 0);
        assert_eq!(edits[0]["range"]["end"]["line"], 0);
        assert_eq!(edits[0]["range"]["end"]["character"], 0);
        assert_eq!(edits[0]["newText"], "    @test_case(10)\n");
    }

    #[test]
    fn clamps_out_of_bounds_line_safely() {
        let uri = "file:///workspace/empty.ves";
        let actions = anvil_code_actions(uri, "", "test_empty", 99, Some("0, true"));
        assert_eq!(actions.len(), 2);
        let edits = actions[1]["edit"]["changes"][uri].as_array().unwrap();
        assert_eq!(edits[0]["range"]["start"]["line"], 0);
        assert_eq!(edits[0]["newText"], "@test_case(0, true)\n");
    }
}
