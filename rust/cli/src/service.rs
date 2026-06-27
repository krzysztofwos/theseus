//! Theseus's authored service implementation (L3).
//!
//! These are the operation handlers — the behavior leaves checked against the
//! generated [`TheseusService`](crate::generated::TheseusService) contract. An
//! operation without a handler here falls through to the trait's `unimplemented`
//! default, and `verify`'s coverage check reports it. This is the one file the
//! structured-edit tooling writes. The composition root and adapters in
//! [`main`](crate) stay hand-written.

use anyhow::Context;
use theseus_model::{authored_impl_path, authored_impls, generated_files};
use theseus_modeling::{
    CoverageReport, Edit, GeneratedFile, PatchOutcome, QueryOutcome, VerifyReport, apply_edit,
    apply_edits, coverage, describe, handler_source, model_hash, query, scaffold_files, verify,
};

use crate::generated::{
    CalcRequest, ChatRequest, Ctx, ImplementRequest, PatchRequest, QueryRequest, ShowRequest,
    TheseusService,
};
use crate::workspace_root;

impl TheseusService for Ctx<'_> {
    fn chat(&self, request: ChatRequest) -> anyhow::Result<String> {
        run_agent(self, &request.message, request.allow_writes)
    }

    fn model(&self) -> anyhow::Result<String> {
        Ok(describe(self.model))
    }

    fn verify(&self) -> anyhow::Result<VerifyReport> {
        Ok(verify(
            self.model,
            &workspace_root(),
            &generated_files(self.model),
            &authored_impls(self.model),
        ))
    }

    fn generate(&self) -> anyhow::Result<Vec<GeneratedFile>> {
        // A crate's generated code is deferred until the crate is scaffolded, so
        // adding a crate to the model does not write into a manifest-less
        // directory and break the workspace before `scaffold` runs.
        let root = workspace_root();
        let mut written = Vec::new();
        for file in generated_files(self.model) {
            if crate_is_scaffolded(&root, &file) {
                self.workspace.write_file(&file)?;
                written.push(file);
            }
        }
        Ok(written)
    }

    fn scaffold(&self) -> anyhow::Result<Vec<GeneratedFile>> {
        // The skeleton files are authored leaves, so only the absent ones are
        // written. An existing file is left as the author left it.
        let root = workspace_root();
        let mut written = Vec::new();
        for file in scaffold_files(self.model) {
            if !root.join(&file.path).exists() {
                self.workspace.write_file(&file)?;
                written.push(file);
            }
        }
        Ok(written)
    }

    fn query(&self, request: QueryRequest) -> anyhow::Result<QueryOutcome> {
        let mut outcome = query(self.model, request.find.as_deref(), request.node.as_deref())?;
        if let Some(kind) = &request.kind {
            outcome.handles.retain(|handle| &handle.kind == kind);
        }
        Ok(outcome)
    }

    fn coverage(&self) -> anyhow::Result<CoverageReport> {
        let root = workspace_root();
        Ok(coverage(self.model, |service| -> anyhow::Result<String> {
            let path = authored_impl_path(self.model, service);
            std::fs::read_to_string(root.join(&path)).with_context(|| format!("reading {path}"))
        })?)
    }

    fn show(&self, request: ShowRequest) -> anyhow::Result<String> {
        let path = self.handler_path(&request.method)?;
        let source = std::fs::read_to_string(workspace_root().join(&path))
            .with_context(|| format!("reading {path}"))?;
        Ok(handler_source(self.model, &source, &request.method)?)
    }

    fn calc(&self, request: CalcRequest) -> anyhow::Result<String> {
        let operands = theseus_calculator::Operands {
            a: request.a,
            b: request.b,
        };
        match request.op.as_str() {
            "add" => self.calculator.add(operands),
            "subtract" => self.calculator.subtract(operands),
            "multiply" => self.calculator.multiply(operands),
            "divide" => self.calculator.divide(operands),
            other => anyhow::bail!(
                "unknown operator `{other}`, expected add, subtract, multiply, or divide"
            ),
        }
    }

    fn implement(&self, request: ImplementRequest) -> anyhow::Result<String> {
        let base = model_hash(self.model);
        if base != request.expect_model_hash {
            anyhow::bail!(
                "stale model hash: expected `{}`, current is `{base}`; run `theseus query`",
                request.expect_model_hash
            );
        }
        let body = resolve_body(&request)?;
        let path = self.handler_path(&request.method)?;
        let source = std::fs::read_to_string(workspace_root().join(&path))
            .with_context(|| format!("reading {path}"))?;
        let spliced = theseus_modeling::implement(
            self.model,
            &source,
            &request.method,
            &body,
            "crate::generated::",
        )?;
        self.workspace.write_file(&GeneratedFile {
            path: path.clone(),
            contents: spliced,
        })?;
        Ok(format!(
            "wrote the handler for `{}` into {path}. Rebuild to load it",
            request.method
        ))
    }

    fn patch(&self, request: PatchRequest) -> anyhow::Result<PatchOutcome> {
        let (outcome, proposed) = if request.edit.is_empty() {
            let edit = build_edit(&request)?;
            apply_edit(self.model, &edit, &request.expect_model_hash)
        } else {
            let edits = request
                .edit
                .iter()
                .map(|spec| parse_edit_spec(spec))
                .collect::<anyhow::Result<Vec<_>>>()?;
            apply_edits(self.model, &edits, &request.expect_model_hash)
        };
        if request.write
            && let Some(proposed) = proposed
        {
            // Reproject every file from the proposed model — the self-model source
            // and the generated scaffolding update together. A new operation's
            // handler defaults to unimplemented until authored here, and `coverage`
            // reports what is left to write. A crate's generated code is deferred
            // until the crate is scaffolded.
            let root = workspace_root();
            for file in generated_files(&proposed) {
                if crate_is_scaffolded(&root, &file) {
                    self.workspace.write_file(&file)?;
                }
            }
        }
        Ok(outcome)
    }
}

impl Ctx<'_> {
    /// The authored impl file holding the handler for `method`: the `service.rs`
    /// of the crate the method's service lives in.
    fn handler_path(&self, method: &str) -> anyhow::Result<String> {
        let service = self
            .model
            .service_of_operation(method)
            .with_context(|| format!("no operation named `{method}`"))?;
        Ok(authored_impl_path(self.model, service))
    }
}

// ============================================================================
// The agent loop — the `chat` handler's behavior. The model drives Theseus's own
// read-only operations as tools, so the loop closes onto the model it inspects.
// ============================================================================

/// The most model turns the loop runs before giving up, a guard against a model
/// that never answers.
const MAX_TURNS: usize = 8;

/// The framing handed to the model: the tool surface and the reply protocol. The
/// offline stub ignores it. A real model adapter relies on it.
const SYSTEM: &str = "You are Theseus, inspecting and editing your own model by \
calling tools. Reply with exactly one JSON object: {\"tool\": name, \"input\": {…}} \
to call a tool, or {\"answer\": text} to finish. Tools: model (no input), query \
(input: find, node, kind, each an optional string), verify (no input), coverage \
(no input), patch (input: edit, a list of `verb|target|key=value` strings, and \
write, a bool — applying a write needs the chat to permit it).";

/// The result fed back when a write tool runs without the permission gate.
const WRITE_REFUSED: &str =
    "writes are not permitted; rerun chat with --allow-writes to apply this edit";

/// The model's next move, parsed from one completion.
enum AgentAction {
    /// Call a tool with a JSON input object.
    Tool {
        name: String,
        input: serde_json::Value,
    },
    /// Finish, answering the user.
    Answer(String),
}

/// Run the agent loop: the model drives Theseus's own operations as tools. The
/// transcript opens with the framing and the user's message. Each turn the model
/// either calls a tool, whose result is appended, or answers and ends. A mutating
/// tool runs only when `allow_writes` permits it.
fn run_agent(ctx: &Ctx<'_>, message: &str, allow_writes: bool) -> anyhow::Result<String> {
    let mut transcript = format!("{SYSTEM}\n\nUser: {message}\n");
    for _ in 0..MAX_TURNS {
        let reply = ctx.llm.complete(&transcript)?;
        match parse_action(&reply)? {
            AgentAction::Answer(answer) => return Ok(answer),
            AgentAction::Tool { name, input } => {
                let result = run_tool(ctx, &name, &input, allow_writes)?;
                transcript.push_str(&format!("Assistant: call {name}\nTool result: {result}\n"));
            }
        }
    }
    anyhow::bail!("the agent did not answer within {MAX_TURNS} turns")
}

/// Parse one completion into the next action. The model replies with a single
/// JSON object: a tool call or an answer.
fn parse_action(reply: &str) -> anyhow::Result<AgentAction> {
    let value: serde_json::Value = serde_json::from_str(reply.trim())
        .with_context(|| format!("the model reply was not JSON: {reply}"))?;
    if let Some(answer) = value.get("answer").and_then(serde_json::Value::as_str) {
        return Ok(AgentAction::Answer(answer.to_string()));
    }
    if let Some(name) = value.get("tool").and_then(serde_json::Value::as_str) {
        let input = value
            .get("input")
            .cloned()
            .unwrap_or(serde_json::Value::Null);
        return Ok(AgentAction::Tool {
            name: name.to_string(),
            input,
        });
    }
    anyhow::bail!("the model reply was neither a tool call nor an answer: {reply}")
}

/// Run one tool — one of Theseus's own operations — and return its result as a
/// JSON string to feed back to the model. The tool surface is Theseus's own
/// operations, so the loop edits the model it inspects. A `patch` that writes is
/// refused unless `allow_writes` permits it.
fn run_tool(
    ctx: &Ctx<'_>,
    name: &str,
    input: &serde_json::Value,
    allow_writes: bool,
) -> anyhow::Result<String> {
    match name {
        "model" => ctx.model(),
        "verify" => Ok(serde_json::to_string(&ctx.verify()?)?),
        "coverage" => Ok(serde_json::to_string(&ctx.coverage()?)?),
        "query" => {
            let field = |key: &str| {
                input
                    .get(key)
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_string)
            };
            let request = QueryRequest {
                find: field("find"),
                node: field("node"),
                kind: field("kind"),
            };
            Ok(serde_json::to_string(&ctx.query(request)?)?)
        }
        "patch" => {
            let request = patch_request(input, model_hash(ctx.model));
            if request.write && !allow_writes {
                return Ok(WRITE_REFUSED.to_string());
            }
            Ok(serde_json::to_string(&ctx.patch(request)?)?)
        }
        other => {
            anyhow::bail!("unknown tool `{other}`; tools are model, query, verify, coverage, patch")
        }
    }
}

/// Build a patch request from the model's JSON tool input, stamping the current
/// model hash so the model need not track it.
fn patch_request(input: &serde_json::Value, model_hash: String) -> PatchRequest {
    let field = |key: &str| {
        input
            .get(key)
            .and_then(serde_json::Value::as_str)
            .map(str::to_string)
    };
    let list = |key: &str| {
        input
            .get(key)
            .and_then(serde_json::Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(serde_json::Value::as_str)
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default()
    };
    PatchRequest {
        verb: field("verb"),
        target: field("target"),
        kind: field("kind"),
        name: field("name"),
        to: field("to"),
        set: list("set"),
        edit: list("edit"),
        write: input
            .get("write")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false),
        expect_model_hash: model_hash,
    }
}

/// Whether a generated file's crate is scaffolded — has a `Cargo.toml` on disk.
/// A crate added to the model is registered before its skeleton is written, so
/// its generated code waits for `scaffold` rather than landing in a directory
/// the workspace cannot yet build.
fn crate_is_scaffolded(root: &std::path::Path, file: &GeneratedFile) -> bool {
    match file
        .path
        .strip_prefix("rust/")
        .and_then(|rest| rest.split_once('/'))
    {
        Some((dir, _)) => root.join("rust").join(dir).join("Cargo.toml").exists(),
        None => true,
    }
}

/// The handler body for an implement request: read from `--body-file` (or stdin
/// when `-`) if given, otherwise the inline `--body`.
fn resolve_body(request: &ImplementRequest) -> anyhow::Result<String> {
    match &request.body_file {
        Some(path) if path == "-" => {
            std::io::read_to_string(std::io::stdin()).context("reading the body from stdin")
        }
        Some(path) => {
            std::fs::read_to_string(path).with_context(|| format!("reading the body from {path}"))
        }
        None => request
            .body
            .clone()
            .context("implement needs --body or --body-file"),
    }
}

/// Build the structured [`Edit`] from a parsed patch request — the inbound
/// adapter's wire-to-domain conversion for the verb vocabulary.
fn build_edit(request: &PatchRequest) -> anyhow::Result<Edit> {
    let verb = request
        .verb
        .as_deref()
        .context("patch needs --verb or --edit")?;
    let target = request.target.clone().context("patch needs --target")?;
    make_edit(
        verb,
        target,
        request.kind.clone(),
        request.to.clone(),
        request.name.clone(),
        parse_assignments(&request.set)?,
    )
}

/// Parse one batch edit spec, `verb|target|key=value|…`, into an [`Edit`]. The
/// keys `kind`, `name`, and `to` set the matching fields. The rest are scalar
/// assignments. A pipe never appears in a value, so it is the field separator.
fn parse_edit_spec(spec: &str) -> anyhow::Result<Edit> {
    let mut parts = spec.split('|');
    let verb = parts.next().unwrap_or_default().trim();
    let target = parts
        .next()
        .context("edit spec must be `verb|target|…`")?
        .trim()
        .to_string();
    let (mut kind, mut name, mut to, mut attrs) = (None, None, None, Vec::new());
    for part in parts {
        let (key, value) = part
            .split_once('=')
            .with_context(|| format!("edit field `{part}` must be key=value"))?;
        match key.trim() {
            "kind" => kind = Some(value.to_string()),
            "name" => name = Some(value.to_string()),
            "to" => to = Some(value.to_string()),
            key => attrs.push((key.to_string(), value.to_string())),
        }
    }
    make_edit(verb, target, kind, to, name, attrs)
}

/// Assemble an [`Edit`] from a verb and its parts. A missing part the verb needs
/// is a usage error.
fn make_edit(
    verb: &str,
    target: String,
    kind: Option<String>,
    to: Option<String>,
    name: Option<String>,
    attrs: Vec<(String, String)>,
) -> anyhow::Result<Edit> {
    match verb {
        "add" => Ok(Edit::Add {
            parent: target,
            kind: kind.context("add needs a kind")?,
            name: name.context("add needs a name")?,
            attrs,
        }),
        "remove" => Ok(Edit::Remove { target }),
        "rename" => Ok(Edit::Rename {
            target,
            to: to.context("rename needs a new name")?,
        }),
        "set" => Ok(Edit::Set { target, attrs }),
        other => anyhow::bail!("unknown verb `{other}`; expected add, remove, rename, or set"),
    }
}

/// Parse `--set key=value` assignments into attribute pairs. The first `=`
/// separates the key, so a value may itself contain `=`.
fn parse_assignments(set: &[String]) -> anyhow::Result<Vec<(String, String)>> {
    set.iter()
        .map(|pair| {
            let (key, value) = pair
                .split_once('=')
                .with_context(|| format!("assignment `{pair}` must be key=value"))?;
            Ok((key.trim().to_string(), value.to_string()))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::collections::VecDeque;

    use anyhow::Context;
    use theseus_model::theseus_model;
    use theseus_modeling::GeneratedFile;

    use super::{WRITE_REFUSED, run_tool};
    use crate::generated::{ChatRequest, Ctx, Llm, TheseusService, Workspace};

    /// An edit that adds a throwaway type, for exercising the `patch` tool. The
    /// no-op workspace discards any reprojection, so a write touches no files.
    const PROBE_EDIT: &str = "add|model:theseus|kind=type|name=Probe|shape=foreign:String";

    /// A model that replays a fixed script of completions, so the loop runs with
    /// no network.
    struct ScriptedLlm {
        replies: RefCell<VecDeque<String>>,
    }

    impl ScriptedLlm {
        fn new(replies: impl IntoIterator<Item = &'static str>) -> Self {
            Self {
                replies: RefCell::new(replies.into_iter().map(str::to_string).collect()),
            }
        }
    }

    impl Llm for ScriptedLlm {
        fn complete(&self, _transcript: &str) -> anyhow::Result<String> {
            self.replies
                .borrow_mut()
                .pop_front()
                .context("the scripted model ran out of replies")
        }
    }

    /// A workspace that writes nowhere. The loop's read-only tools never reach it.
    struct NoopWorkspace;

    impl Workspace for NoopWorkspace {
        fn write_file(&self, _file: &GeneratedFile) -> anyhow::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn the_loop_calls_a_tool_then_answers() {
        let model = theseus_model();
        let workspace = NoopWorkspace;
        let calculator = theseus_calculator::Calculator;
        let llm = ScriptedLlm::new([
            r#"{"tool": "query", "input": {"kind": "operation"}}"#,
            r#"{"answer": "Theseus exposes a chat operation."}"#,
        ]);
        let ctx = Ctx {
            model: &model,
            workspace: &workspace,
            calculator: &calculator,
            llm: &llm,
        };
        let reply = ctx
            .chat(ChatRequest {
                message: "What can you do?".to_string(),
                allow_writes: false,
            })
            .expect("the loop answers");
        assert_eq!(reply, "Theseus exposes a chat operation.");
    }

    #[test]
    fn the_query_tool_finds_the_chat_operation() {
        let model = theseus_model();
        let workspace = NoopWorkspace;
        let calculator = theseus_calculator::Calculator;
        let llm = ScriptedLlm::new(["unused"]);
        let ctx = Ctx {
            model: &model,
            workspace: &workspace,
            calculator: &calculator,
            llm: &llm,
        };
        let result = run_tool(
            &ctx,
            "query",
            &serde_json::json!({ "kind": "operation" }),
            false,
        )
        .expect("the query tool runs");
        assert!(
            result.contains("chat"),
            "the chat operation handle should appear: {result}"
        );
    }

    #[test]
    fn a_write_is_refused_without_the_gate() {
        let model = theseus_model();
        let workspace = NoopWorkspace;
        let calculator = theseus_calculator::Calculator;
        let llm = ScriptedLlm::new(["unused"]);
        let ctx = Ctx {
            model: &model,
            workspace: &workspace,
            calculator: &calculator,
            llm: &llm,
        };
        let input = serde_json::json!({ "edit": [PROBE_EDIT], "write": true });
        let result = run_tool(&ctx, "patch", &input, false).expect("the tool returns a result");
        assert_eq!(result, WRITE_REFUSED);
    }

    #[test]
    fn a_write_is_allowed_with_the_gate() {
        let model = theseus_model();
        let workspace = NoopWorkspace;
        let calculator = theseus_calculator::Calculator;
        let llm = ScriptedLlm::new(["unused"]);
        let ctx = Ctx {
            model: &model,
            workspace: &workspace,
            calculator: &calculator,
            llm: &llm,
        };
        let input = serde_json::json!({ "edit": [PROBE_EDIT], "write": true });
        let result = run_tool(&ctx, "patch", &input, true).expect("the patch tool runs");
        assert!(
            result.contains(r#""ok":true"#),
            "the patch should apply: {result}"
        );
        assert!(
            result.contains("Probe"),
            "the diff should name the new type: {result}"
        );
    }
}
