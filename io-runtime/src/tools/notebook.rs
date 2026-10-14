//! Jupyter notebook (`.ipynb`) support: a text rendering for the `read` tool
//! and the `notebook_edit` tool for replacing, inserting and deleting cells.

use super::file_state::FileTracker;
use super::write::make_diff;
use super::{Tool, ToolInput, ToolOutput};
use serde_json::{json, Value};
use std::sync::Arc;

/// Longest output text shown per cell when rendering a notebook for the model.
const MAX_CELL_OUTPUT_CHARS: usize = 2000;

pub fn is_notebook(path: &std::path::Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("ipynb"))
}

pub fn parse(content: &str) -> Result<Value, String> {
    let nb: Value = serde_json::from_str(content)
        .map_err(|e| format!("not a valid notebook (bad JSON): {e}"))?;
    if !nb.get("cells").is_some_and(Value::is_array) {
        return Err("not a valid notebook: missing a \"cells\" array".to_string());
    }
    Ok(nb)
}

/// Serialize the way Jupyter does: sorted keys (serde_json's default map),
/// one-space indent, trailing newline — so saving doesn't churn diffs.
pub fn serialize(nb: &Value) -> String {
    use serde::Serialize;
    let mut out = Vec::new();
    let fmt = serde_json::ser::PrettyFormatter::with_indent(b" ");
    let mut ser = serde_json::Serializer::with_formatter(&mut out, fmt);
    nb.serialize(&mut ser)
        .expect("a JSON value always serializes");
    let mut s = String::from_utf8(out).expect("serde_json emits UTF-8");
    s.push('\n');
    s
}

/// A cell's `source`, which nbformat stores as either a string or a list of
/// lines.
fn source_text(cell: &Value) -> String {
    match cell.get("source") {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(lines)) => lines.iter().filter_map(Value::as_str).collect(),
        _ => String::new(),
    }
}

/// The list-of-lines form nbformat writes (every line but the last keeps its
/// newline).
fn source_value(text: &str) -> Value {
    Value::Array(
        text.split_inclusive('\n')
            .map(|l| Value::String(l.to_string()))
            .collect(),
    )
}

fn cell_type(cell: &Value) -> &str {
    cell.get("cell_type")
        .and_then(Value::as_str)
        .unwrap_or("code")
}

fn truncate_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let cut: String = s.chars().take(max).collect();
    format!("{cut}\n… (output truncated)")
}

/// Text of one output entry, if it has any. Rich outputs (images, HTML) are
/// noted but not inlined.
fn output_text(output: &Value) -> Option<String> {
    let join = |v: Option<&Value>| match v {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(lines)) => lines.iter().filter_map(Value::as_str).collect(),
        _ => String::new(),
    };
    match output.get("output_type").and_then(Value::as_str)? {
        "stream" => Some(join(output.get("text"))),
        "error" => Some(format!(
            "{}: {}",
            output
                .get("ename")
                .and_then(Value::as_str)
                .unwrap_or("Error"),
            output.get("evalue").and_then(Value::as_str).unwrap_or("")
        )),
        "execute_result" | "display_data" => {
            let data = output.get("data")?;
            if data.get("text/plain").is_some() {
                let mut text = join(data.get("text/plain"));
                if data.get("image/png").is_some() || data.get("image/jpeg").is_some() {
                    text.push_str("\n[image output omitted]");
                }
                Some(text)
            } else if data.get("image/png").is_some() || data.get("image/jpeg").is_some() {
                Some("[image output omitted]".to_string())
            } else {
                None
            }
        }
        _ => None,
    }
}

/// Render a notebook for the model: every cell with its index, type, id,
/// source and (for code cells) textual outputs.
pub fn render(nb: &Value) -> String {
    let cells = nb["cells"].as_array().map(Vec::as_slice).unwrap_or(&[]);
    let kernel = nb["metadata"]["kernelspec"]["name"]
        .as_str()
        .unwrap_or("unknown");
    let mut out = format!("Notebook: {} cell(s), kernel: {kernel}\n", cells.len());
    for (i, cell) in cells.iter().enumerate() {
        let id = cell
            .get("id")
            .and_then(Value::as_str)
            .map(|id| format!(" id={id}"))
            .unwrap_or_default();
        out.push_str(&format!("\n--- Cell {i} [{}]{id} ---\n", cell_type(cell)));
        out.push_str(source_text(cell).trim_end_matches('\n'));
        out.push('\n');
        if cell_type(cell) == "code" {
            let outputs: Vec<String> = cell
                .get("outputs")
                .and_then(Value::as_array)
                .map(|os| os.iter().filter_map(output_text).collect())
                .unwrap_or_default();
            let joined = outputs.join("\n");
            if !joined.trim().is_empty() {
                out.push_str("[output]\n");
                out.push_str(truncate_chars(joined.trim_end(), MAX_CELL_OUTPUT_CHARS).as_str());
                out.push('\n');
            }
        }
    }
    out
}

/// Whether new cells in this notebook need an `id` (nbformat 4.5+).
fn wants_cell_ids(nb: &Value) -> bool {
    nb["nbformat"].as_u64().unwrap_or(4) > 4
        || nb["nbformat_minor"].as_u64().unwrap_or(0) >= 5
        || nb["cells"]
            .as_array()
            .is_some_and(|cs| cs.iter().any(|c| c.get("id").is_some()))
}

fn new_cell(kind: &str, source: &str, with_id: bool) -> Value {
    let mut cell = json!({
        "cell_type": kind,
        "metadata": {},
        "source": source_value(source),
    });
    if kind == "code" {
        cell["execution_count"] = Value::Null;
        cell["outputs"] = json!([]);
    }
    if with_id {
        cell["id"] = Value::String(uuid::Uuid::new_v4().simple().to_string()[..8].to_string());
    }
    cell
}

/// Index of the cell addressed by `cell_id` (preferred) or `cell_number`.
fn locate(cells: &[Value], id: Option<&str>, number: Option<usize>) -> Result<usize, String> {
    if let Some(id) = id {
        return cells
            .iter()
            .position(|c| c.get("id").and_then(Value::as_str) == Some(id))
            .ok_or_else(|| format!("no cell with id '{id}'"));
    }
    match number {
        Some(n) if n < cells.len() => Ok(n),
        Some(n) => Err(format!(
            "cell_number {n} is out of range — the notebook has {} cell(s)",
            cells.len()
        )),
        None => Err("specify cell_id or cell_number".to_string()),
    }
}

pub struct NotebookEditTool {
    tracker: Arc<FileTracker>,
}

impl NotebookEditTool {
    pub fn new(tracker: Arc<FileTracker>) -> Self {
        Self { tracker }
    }
}

impl Default for NotebookEditTool {
    fn default() -> Self {
        Self::new(FileTracker::shared())
    }
}

#[async_trait::async_trait]
impl Tool for NotebookEditTool {
    fn name(&self) -> &str {
        "notebook_edit"
    }

    fn description(&self) -> &str {
        "Edit a Jupyter notebook (.ipynb) cell. edit_mode `replace` (default) overwrites a \
         cell's source, `insert` adds a new cell (after `cell_id`, or at index `cell_number`, or \
         at the end), `delete` removes a cell. Cells are addressed by `cell_id` or a 0-based \
         `cell_number` as shown by the read tool. The notebook must be read first. Replacing a \
         code cell clears its outputs."
    }

    fn input_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "Path to the .ipynb file" },
                "new_source": { "type": "string", "description": "New cell source (not needed for delete)" },
                "cell_id": { "type": "string", "description": "ID of the cell to edit; for insert, the new cell goes after it" },
                "cell_number": { "type": "integer", "description": "0-based cell index (used when cell_id is not given)" },
                "cell_type": { "type": "string", "enum": ["code", "markdown"], "description": "Cell type; required for insert" },
                "edit_mode": { "type": "string", "enum": ["replace", "insert", "delete"], "description": "Defaults to replace" }
            },
            "required": ["path"]
        })
    }

    async fn execute(&self, input: ToolInput) -> ToolOutput {
        let Some(path_str) = input.args.get("path").and_then(|v| v.as_str()) else {
            return ToolOutput::err("missing required argument: path");
        };
        let path = match super::resolve_safe_path(path_str) {
            Ok(p) => p,
            Err(e) => return ToolOutput::err(e),
        };
        if !is_notebook(&path) {
            return ToolOutput::err("notebook_edit only works on .ipynb files");
        }
        let mode = input
            .args
            .get("edit_mode")
            .and_then(|v| v.as_str())
            .unwrap_or("replace");
        let new_source = input.args.get("new_source").and_then(|v| v.as_str());
        let kind = input.args.get("cell_type").and_then(|v| v.as_str());
        let id = input.args.get("cell_id").and_then(|v| v.as_str());
        let number = input
            .args
            .get("cell_number")
            .and_then(|v| v.as_u64())
            .map(|n| n as usize);

        if let Some(k) = kind {
            if k != "code" && k != "markdown" {
                return ToolOutput::err("cell_type must be \"code\" or \"markdown\"");
            }
        }
        if mode != "delete" && new_source.is_none() {
            return ToolOutput::err("missing required argument: new_source");
        }

        let old_content = match std::fs::read_to_string(&path) {
            Ok(c) => c,
            Err(e) => return ToolOutput::err(format!("failed to read {}: {e}", path.display())),
        };
        if let Err(e) = self.tracker.check(&path, &old_content) {
            return ToolOutput::err(e);
        }
        let mut nb = match parse(&old_content) {
            Ok(nb) => nb,
            Err(e) => return ToolOutput::err(e),
        };
        let with_ids = wants_cell_ids(&nb);
        let cells = nb["cells"].as_array_mut().expect("validated by parse");

        let diff = match mode {
            "replace" => {
                let idx = match locate(cells, id, number) {
                    Ok(i) => i,
                    Err(e) => return ToolOutput::err(e),
                };
                let new_source = new_source.unwrap_or_default();
                let old_source = source_text(&cells[idx]);
                let old_kind = cell_type(&cells[idx]).to_string();
                let kind = kind.unwrap_or(&old_kind);
                cells[idx]["source"] = source_value(new_source);
                if kind != old_kind {
                    cells[idx]["cell_type"] = json!(kind);
                }
                if kind == "code" {
                    cells[idx]["execution_count"] = Value::Null;
                    cells[idx]["outputs"] = json!([]);
                } else if let Some(obj) = cells[idx].as_object_mut() {
                    obj.remove("execution_count");
                    obj.remove("outputs");
                }
                make_diff(
                    &old_source,
                    new_source,
                    &format!("{} (cell {idx})", path.display()),
                )
            }
            "insert" => {
                let Some(kind) = kind else {
                    return ToolOutput::err("cell_type is required when inserting a cell");
                };
                let at = if id.is_some() {
                    match locate(cells, id, None) {
                        Ok(i) => i + 1,
                        Err(e) => return ToolOutput::err(e),
                    }
                } else {
                    match number {
                        Some(n) if n <= cells.len() => n,
                        Some(n) => {
                            return ToolOutput::err(format!(
                                "cell_number {n} is out of range — the notebook has {} cell(s)",
                                cells.len()
                            ))
                        }
                        None => cells.len(),
                    }
                };
                let new_source = new_source.unwrap_or_default();
                cells.insert(at, new_cell(kind, new_source, with_ids));
                make_diff(
                    "",
                    new_source,
                    &format!("{} (new cell {at})", path.display()),
                )
            }
            "delete" => {
                let idx = match locate(cells, id, number) {
                    Ok(i) => i,
                    Err(e) => return ToolOutput::err(e),
                };
                let removed = cells.remove(idx);
                make_diff(
                    &source_text(&removed),
                    "",
                    &format!("{} (deleted cell {idx})", path.display()),
                )
            }
            other => {
                return ToolOutput::err(format!(
                    "unknown edit_mode '{other}' — use replace, insert or delete"
                ))
            }
        };

        let new_content = serialize(&nb);
        if let Err(e) = std::fs::write(&path, &new_content) {
            return ToolOutput::err(format!("failed to write {}: {e}", path.display()));
        }
        self.tracker.record(&path, &new_content);
        ToolOutput::ok(diff).with_snapshot(path.to_string_lossy().into_owned(), Some(old_content))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NB: &str = r##"{
 "cells": [
  {"cell_type": "markdown", "id": "aaaaaaaa", "metadata": {}, "source": ["# Title\n", "text"]},
  {"cell_type": "code", "id": "bbbbbbbb", "metadata": {}, "execution_count": 3,
   "outputs": [{"output_type": "stream", "name": "stdout", "text": ["hello\n"]},
               {"output_type": "execute_result", "execution_count": 3, "metadata": {},
                "data": {"text/plain": ["42"], "image/png": "AAAA"}}],
   "source": "print('hello')\n42"}
 ],
 "metadata": {"kernelspec": {"name": "python3"}},
 "nbformat": 4,
 "nbformat_minor": 5
}"##;

    fn temp_nb(content: &str) -> std::path::PathBuf {
        let p = std::env::temp_dir().join(format!("io_nb_test_{}.ipynb", uuid::Uuid::new_v4()));
        std::fs::write(&p, content).unwrap();
        p
    }

    fn call(tool: &NotebookEditTool, args: Value) -> ToolOutput {
        let args = args
            .as_object()
            .unwrap()
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        futures::executor::block_on(tool.execute(ToolInput {
            name: "notebook_edit".into(),
            args,
        }))
    }

    fn tool_for(path: &std::path::Path) -> NotebookEditTool {
        let tracker = FileTracker::shared();
        tracker.record(path, &std::fs::read_to_string(path).unwrap());
        NotebookEditTool::new(tracker)
    }

    #[test]
    fn render_shows_cells_sources_and_text_outputs() {
        let text = render(&parse(NB).unwrap());
        assert!(text.contains("2 cell(s), kernel: python3"));
        assert!(text.contains("--- Cell 0 [markdown] id=aaaaaaaa ---\n# Title\ntext"));
        assert!(text.contains("--- Cell 1 [code] id=bbbbbbbb ---\nprint('hello')\n42"));
        assert!(text.contains("[output]\nhello\n\n42\n[image output omitted]"));
    }

    #[test]
    fn replace_by_id_updates_source_and_clears_outputs() {
        let path = temp_nb(NB);
        let out = call(
            &tool_for(&path),
            json!({"path": path.to_str().unwrap(), "cell_id": "bbbbbbbb", "new_source": "x = 1\n"}),
        );
        assert!(out.success, "{}", out.data);
        let nb = parse(&std::fs::read_to_string(&path).unwrap()).unwrap();
        std::fs::remove_file(&path).ok();
        let cell = &nb["cells"][1];
        assert_eq!(source_text(cell), "x = 1\n");
        assert_eq!(cell["outputs"], json!([]));
        assert!(cell["execution_count"].is_null());
        assert!(out.data.contains("+x = 1"));
        assert!(out.pre_edit_snapshot.is_some());
    }

    #[test]
    fn insert_adds_a_cell_with_an_id_after_the_addressed_cell() {
        let path = temp_nb(NB);
        let out = call(
            &tool_for(&path),
            json!({"path": path.to_str().unwrap(), "cell_id": "aaaaaaaa",
                   "edit_mode": "insert", "cell_type": "markdown", "new_source": "notes"}),
        );
        assert!(out.success, "{}", out.data);
        let nb = parse(&std::fs::read_to_string(&path).unwrap()).unwrap();
        std::fs::remove_file(&path).ok();
        assert_eq!(nb["cells"].as_array().unwrap().len(), 3);
        assert_eq!(source_text(&nb["cells"][1]), "notes");
        assert_eq!(nb["cells"][1]["id"].as_str().unwrap().len(), 8);
        assert!(nb["cells"][1].get("outputs").is_none());
    }

    #[test]
    fn insert_without_position_appends_and_requires_a_cell_type() {
        let path = temp_nb(NB);
        let tool = tool_for(&path);
        let missing = call(
            &tool,
            json!({"path": path.to_str().unwrap(), "edit_mode": "insert", "new_source": "a"}),
        );
        assert!(!missing.success);
        assert!(missing.data.contains("cell_type is required"));

        let out = call(
            &tool,
            json!({"path": path.to_str().unwrap(), "edit_mode": "insert",
                   "cell_type": "code", "new_source": "1+1"}),
        );
        assert!(out.success, "{}", out.data);
        let nb = parse(&std::fs::read_to_string(&path).unwrap()).unwrap();
        std::fs::remove_file(&path).ok();
        assert_eq!(source_text(&nb["cells"][2]), "1+1");
        assert_eq!(nb["cells"][2]["outputs"], json!([]));
    }

    #[test]
    fn delete_removes_the_cell_and_bad_addresses_are_errors() {
        let path = temp_nb(NB);
        let tool = tool_for(&path);
        let bad = call(
            &tool,
            json!({"path": path.to_str().unwrap(), "edit_mode": "delete", "cell_number": 9}),
        );
        assert!(!bad.success);
        assert!(bad.data.contains("out of range"));
        let none = call(
            &tool,
            json!({"path": path.to_str().unwrap(), "edit_mode": "delete"}),
        );
        assert!(none.data.contains("specify cell_id or cell_number"));

        let out = call(
            &tool,
            json!({"path": path.to_str().unwrap(), "edit_mode": "delete", "cell_number": 0}),
        );
        assert!(out.success, "{}", out.data);
        let nb = parse(&std::fs::read_to_string(&path).unwrap()).unwrap();
        std::fs::remove_file(&path).ok();
        assert_eq!(nb["cells"].as_array().unwrap().len(), 1);
        assert_eq!(nb["cells"][0]["id"], "bbbbbbbb");
    }

    #[test]
    fn unread_or_externally_modified_notebooks_are_refused() {
        let path = temp_nb(NB);
        let unread = NotebookEditTool::default();
        let out = call(
            &unread,
            json!({"path": path.to_str().unwrap(), "cell_number": 0, "new_source": "x"}),
        );
        assert!(!out.success);
        assert!(out.data.contains("has not been read yet"));

        let tool = tool_for(&path);
        std::fs::write(&path, NB.replace("Title", "Changed")).unwrap();
        let out = call(
            &tool,
            json!({"path": path.to_str().unwrap(), "cell_number": 0, "new_source": "x"}),
        );
        std::fs::remove_file(&path).ok();
        assert!(!out.success);
        assert!(out.data.contains("modified since"));
    }

    #[test]
    fn rejects_non_notebooks_and_malformed_json() {
        let tool = NotebookEditTool::default();
        let out = call(&tool, json!({"path": "/tmp/x.py", "new_source": "a"}));
        assert!(out.data.contains("only works on .ipynb"));

        let path = temp_nb("{ nope");
        let tool = tool_for(&path);
        let out = call(
            &tool,
            json!({"path": path.to_str().unwrap(), "cell_number": 0, "new_source": "a"}),
        );
        std::fs::remove_file(&path).ok();
        assert!(!out.success);
        assert!(out.data.contains("not a valid notebook"));
    }

    #[test]
    fn serialization_matches_jupyter_layout() {
        let nb = parse(NB).unwrap();
        let text = serialize(&nb);
        assert!(text.ends_with("}\n"));
        assert!(text.starts_with("{\n \"cells\": ["), "{text}");
        // Round-trips losslessly.
        assert_eq!(parse(&text).unwrap(), nb);
    }

    #[test]
    fn sources_split_into_lines_keeping_newlines() {
        assert_eq!(source_value("a\nb\n"), json!(["a\n", "b\n"]),);
        assert_eq!(source_value(""), json!([]));
        assert_eq!(source_text(&json!({"source": ["a\n", "b"]})), "a\nb");
    }
}
