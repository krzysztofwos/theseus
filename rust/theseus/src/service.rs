//! Theseus's authored service implementation (L3).
//!
//! These are the operation handlers — the behavior leaves checked against the
//! generated [`TheseusService`](crate::generated::TheseusService) contract. An
//! operation without a handler here falls through to the trait's `unimplemented`
//! default, and `verify`'s coverage check reports it. This is the one file the
//! structured-edit tooling writes. The composition root and adapters live in the
//! inbound binaries (`theseus-cli`, and the agent and MCP adapters to come).

use anyhow::Context;
use theseus_model::{authored_impl_path, authored_impls, generated_files};
use theseus_modeling::{
    CoverageReport, GeneratedFile, Model, PatchOutcome, QueryOutcome, VerifyReport, apply_edits,
    coverage, describe, handler_source, query, scaffold_files, verify,
};

use crate::{
    generated::{
        CalcRequest, Ctx, ImplementRequest, PatchRequest, QueryRequest, ShowRequest, TheseusService,
    },
    session::persist,
    workspace_root,
};

#[async_trait::async_trait]
impl TheseusService for Ctx<'_> {
    async fn model(&self) -> anyhow::Result<String> {
        Ok(describe(self.model))
    }

    async fn verify(&self) -> anyhow::Result<VerifyReport> {
        // The full render and the manifest reads are compute and blocking file
        // I/O, so the check runs off the async thread and a server keeps
        // serving while it verifies.
        let model = self.model.clone();
        let report = tokio::task::spawn_blocking(move || {
            verify(
                &model,
                &workspace_root(),
                &generated_files(&model),
                &authored_impls(&model),
            )
        })
        .await?;
        Ok(report)
    }

    async fn generate(&self) -> anyhow::Result<Vec<GeneratedFile>> {
        // A crate's generated code is deferred until the crate is scaffolded, so
        // adding a crate to the model does not write into a manifest-less
        // directory and break the workspace before `scaffold` runs.
        let root = workspace_root();
        let mut written = Vec::new();
        for file in generated_files(self.model) {
            if crate_is_scaffolded(&root, &file) {
                self.workspace.write_file(&file).await?;
                written.push(file);
            }
        }
        Ok(written)
    }

    async fn scaffold(&self) -> anyhow::Result<Vec<GeneratedFile>> {
        // The skeleton files are authored leaves, so only the absent ones are
        // written. An existing file is left as the author left it.
        let root = workspace_root();
        let mut written = Vec::new();
        for file in scaffold_files(self.model) {
            if !root.join(&file.path).exists() {
                self.workspace.write_file(&file).await?;
                written.push(file);
            }
        }
        Ok(written)
    }

    async fn query(&self, request: QueryRequest) -> anyhow::Result<QueryOutcome> {
        let mut outcome = query(self.model, request.find.as_deref(), request.node.as_deref())?;
        if let Some(kind) = &request.kind {
            outcome.handles.retain(|handle| &handle.kind == kind);
        }
        Ok(outcome)
    }

    async fn coverage(&self) -> anyhow::Result<CoverageReport> {
        // The handler sources read and parse off the async thread.
        let model = self.model.clone();
        let report = tokio::task::spawn_blocking(move || {
            let root = workspace_root();
            coverage(&model, |service| -> anyhow::Result<String> {
                let path = authored_impl_path(&model, service);
                std::fs::read_to_string(root.join(&path)).with_context(|| format!("reading {path}"))
            })
        })
        .await??;
        Ok(report)
    }

    async fn show(&self, request: ShowRequest) -> anyhow::Result<String> {
        let path = handler_path(self.model, &request.method)?;
        let source = tokio::fs::read_to_string(workspace_root().join(&path))
            .await
            .with_context(|| format!("reading {path}"))?;
        Ok(handler_source(self.model, &source, &request.method)?)
    }

    async fn check(&self) -> anyhow::Result<String> {
        self.toolchain.check().await
    }

    async fn calc(&self, request: CalcRequest) -> anyhow::Result<String> {
        let operands = theseus_calculator::Operands {
            a: request.a,
            b: request.b,
        };
        match request.op.as_str() {
            "add" => self.calculator.add(operands).await,
            "subtract" => self.calculator.subtract(operands).await,
            "multiply" => self.calculator.multiply(operands).await,
            "divide" => self.calculator.divide(operands).await,
            other => anyhow::bail!(
                "unknown operator `{other}`, expected add, subtract, multiply, or divide"
            ),
        }
    }

    async fn implement(&self, request: ImplementRequest) -> anyhow::Result<String> {
        let path = handler_path(self.model, &request.method)?;
        let source = tokio::fs::read_to_string(workspace_root().join(&path))
            .await
            .with_context(|| format!("reading {path}"))?;
        let spliced = theseus_modeling::implement(
            self.model,
            &source,
            &request.method,
            &request.body,
            "crate::generated::",
        )?;
        self.workspace
            .write_file(&GeneratedFile {
                path: path.clone(),
                contents: spliced,
            })
            .await?;
        let outcome = self.toolchain.check().await?;
        Ok(format!(
            "wrote the handler for `{}` into {path}. Rebuild to load it.\n{outcome}",
            request.method
        ))
    }

    async fn patch(&self, request: PatchRequest) -> anyhow::Result<PatchOutcome> {
        if request.edit.is_empty() {
            anyhow::bail!("patch needs at least one edit");
        }
        let (outcome, proposed) = apply_edits(self.model, &request.edit);
        if request.write
            && let Some(proposed) = proposed
        {
            // Reproject every file from the proposed model — the self-model source
            // and the generated scaffolding update together. A new operation's
            // handler defaults to unimplemented until authored here, and `coverage`
            // reports what is left to write.
            persist(&proposed, self.workspace).await?;
        }
        Ok(outcome)
    }
}

/// The authored impl file holding the handler for `method`: the `service.rs` of
/// the crate the method's service lives in.
pub(crate) fn handler_path(model: &Model, method: &str) -> anyhow::Result<String> {
    let service = model
        .service_of_operation(method)
        .with_context(|| format!("no operation named `{method}`"))?;
    Ok(authored_impl_path(model, service))
}

/// Whether a generated file's crate is scaffolded — has a `Cargo.toml` on disk.
/// A crate added to the model is registered before its skeleton is written, so
/// its generated code waits for `scaffold` rather than landing in a directory
/// the workspace cannot yet build.
pub(crate) fn crate_is_scaffolded(root: &std::path::Path, file: &GeneratedFile) -> bool {
    match file
        .path
        .strip_prefix("rust/")
        .and_then(|rest| rest.split_once('/'))
    {
        Some((dir, _)) => root.join("rust").join(dir).join("Cargo.toml").exists(),
        None => true,
    }
}

#[cfg(test)]
mod tests {
    use theseus_model::theseus_model;
    use theseus_modeling::GeneratedFile;

    use crate::{
        generated::{Refused, Toolchain, Workspace, tool_catalog},
        session::Session,
    };

    /// A structured edit that adds a throwaway type, for exercising the `patch`
    /// tool. The no-op workspace discards any reprojection, so a write touches no
    /// files.
    fn probe_edit() -> serde_json::Value {
        serde_json::json!({
            "verb": "add", "parent": "model:theseus", "kind": "type",
            "name": "Probe", "attrs": { "shape": "foreign:String" }
        })
    }

    /// A workspace that writes nowhere. A read-only tool never reaches it.
    struct NoopWorkspace;

    #[async_trait::async_trait]
    impl Workspace for NoopWorkspace {
        async fn write_file(&self, _file: &GeneratedFile) -> anyhow::Result<()> {
            Ok(())
        }
    }

    /// A toolchain that reports success without running a build, so a `check`
    /// call stays in-process.
    struct StubToolchain;

    #[async_trait::async_trait]
    impl Toolchain for StubToolchain {
        async fn check(&self) -> anyhow::Result<String> {
            Ok("the workspace compiles (stub)".to_string())
        }
    }

    #[tokio::test]
    async fn the_query_tool_finds_an_operation() {
        let result = Session::new(theseus_model(), &NoopWorkspace, &StubToolchain, false)
            .call("query", &serde_json::json!({ "kind": "operation" }))
            .await
            .expect("the query tool runs");
        assert!(
            result.contains("verify"),
            "an operation handle should appear: {result}"
        );
    }

    #[tokio::test]
    async fn the_session_sees_its_own_edit() {
        let mut session = Session::new(theseus_model(), &NoopWorkspace, &StubToolchain, false);
        // An in-memory edit, no write, updates the working model.
        session
            .call("patch", &serde_json::json!({ "edit": [probe_edit()] }))
            .await
            .expect("the patch applies in memory");
        // A later call in the same session sees the new type.
        let result = session
            .call(
                "query",
                &serde_json::json!({ "node": "type:theseus:Probe" }),
            )
            .await
            .expect("the query tool runs");
        assert!(
            result.contains("Probe"),
            "the edit should be visible to a later call: {result}"
        );
    }

    #[tokio::test]
    async fn a_write_is_refused_without_the_gate() {
        let input = serde_json::json!({ "edit": [probe_edit()], "write": true });
        let error = Session::new(theseus_model(), &NoopWorkspace, &StubToolchain, false)
            .call("patch", &input)
            .await
            .expect_err("the gate refuses the write");
        assert!(
            error.downcast_ref::<Refused>().is_some(),
            "the refusal should carry the typed gate error: {error}"
        );
    }

    #[tokio::test]
    async fn a_write_is_allowed_with_the_gate() {
        // The no-op workspace discards the reprojection, so this touches no files.
        let input = serde_json::json!({ "edit": [probe_edit()], "write": true });
        let result = Session::new(theseus_model(), &NoopWorkspace, &StubToolchain, true)
            .call("patch", &input)
            .await
            .expect("the patch tool runs");
        assert!(
            result.contains(r#""ok":true"#),
            "the patch should apply: {result}"
        );
        assert!(
            result.contains("Probe"),
            "the diff should name the new type: {result}"
        );
    }

    #[tokio::test]
    async fn the_show_tool_returns_a_handler() {
        let result = Session::new(theseus_model(), &NoopWorkspace, &StubToolchain, false)
            .call("show", &serde_json::json!({ "method": "verify" }))
            .await
            .expect("the show tool runs");
        assert!(
            result.contains("fn verify"),
            "the handler source should appear: {result}"
        );
    }

    #[tokio::test]
    async fn an_implement_is_refused_without_the_gate() {
        let input = serde_json::json!({ "method": "verify", "body": "todo!()" });
        let error = Session::new(theseus_model(), &NoopWorkspace, &StubToolchain, false)
            .call("implement", &input)
            .await
            .expect_err("the gate refuses the write");
        assert!(
            error.downcast_ref::<Refused>().is_some(),
            "the refusal should carry the typed gate error: {error}"
        );
    }

    #[tokio::test]
    async fn an_implement_is_allowed_with_the_gate() {
        // The no-op workspace discards the spliced source, so this touches no files.
        let input = serde_json::json!({ "method": "verify", "body": "todo!()" });
        let result = Session::new(theseus_model(), &NoopWorkspace, &StubToolchain, true)
            .call("implement", &input)
            .await
            .expect("the implement tool runs");
        assert!(
            result.contains("wrote the handler for `verify`"),
            "the tool should report the write: {result}"
        );
        assert!(
            result.contains("the workspace compiles (stub)"),
            "the result should carry the check outcome: {result}"
        );
    }

    #[tokio::test]
    async fn the_check_tool_reports_through_the_toolchain_port() {
        let result = Session::new(theseus_model(), &NoopWorkspace, &StubToolchain, false)
            .call("check", &serde_json::json!({}))
            .await
            .expect("the check tool runs");
        assert_eq!(result, "the workspace compiles (stub)");
    }

    #[tokio::test]
    async fn an_unexposed_operation_is_not_a_tool() {
        let error = Session::new(theseus_model(), &NoopWorkspace, &StubToolchain, false)
            .call("generate", &serde_json::json!({}))
            .await
            .expect_err("an unexposed operation has no dispatch arm");
        assert!(
            error.to_string().contains("unknown tool"),
            "the dispatch should refuse it: {error}"
        );
    }

    #[tokio::test]
    async fn the_catalog_agrees_with_the_model_and_the_dispatch() {
        let model = theseus_model();
        let operations: Vec<&str> = model
            .operations()
            .iter()
            .map(|op| op.name.as_str())
            .collect();
        for tool in tool_catalog() {
            let name = tool["name"]
                .as_str()
                .expect("every catalog tool has a name");
            // Every exposed tool is a real operation of the model.
            assert!(
                operations.contains(&name),
                "catalog tool `{name}` is not a model operation"
            );
            // Every exposed tool has a dispatch arm: a bare call never falls
            // through to the unknown-tool error, though it may fail on missing input.
            let mut session = Session::new(theseus_model(), &NoopWorkspace, &StubToolchain, false);
            if let Err(error) = session.call(name, &serde_json::json!({})).await {
                assert!(
                    !error.to_string().contains("unknown tool"),
                    "catalog tool `{name}` has no dispatch arm: {error}"
                );
            }
        }
    }
}
