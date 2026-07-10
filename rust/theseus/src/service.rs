//! Theseus's authored service implementation (L3).
//!
//! These are the operation handlers — the behavior leaves checked against the
//! generated [`TheseusService`](crate::generated::TheseusService) contract. An
//! operation without a handler here falls through to the trait's `unimplemented`
//! default, and `verify`'s coverage check reports it. This is the one file the
//! structured-edit tooling writes. The composition root and adapters live in the
//! inbound binaries (`theseus-cli`, and the agent and MCP adapters to come).

use anyhow::Context;
use theseus_model::{
    adapter_impl_path, authored_impl_path, authored_impls, checkpoint_expectations,
    checkpoint_paths, crate_is_scaffolded, generated_files, inbound_adapter_impl_path,
    interior_impls,
};
use theseus_modeling::{
    CoverageReport, GeneratedFile, Model, PatchOutcome, QueryOutcome, VerifyReport, apply_edits,
    coverage, describe, handler_source, query, scaffold_files, verify,
};

use crate::{
    CheckReport, CheckpointSnapshotRequest, CheckpointStateRequest, ExpectedFile, ExpectedFileSet,
    ImplementResult, MutationFile, PendingMutation,
    generated::{
        CalcRequest, Ctx, ImplementRequest, PatchRequest, QueryRequest, ShowRequest,
        TheseusService, Toolchain, Workspace,
    },
    workspace_root,
};

#[async_trait::async_trait]
impl TheseusService for Ctx<'_> {
    async fn lint(&self) -> anyhow::Result<crate::CheckReport> {
        self.toolchain.lint().await
    }

    async fn read(&self, request: crate::generated::ReadRequest) -> anyhow::Result<String> {
        let path = rooted(&request.path)?;
        let text = tokio::fs::read_to_string(&path)
            .await
            .with_context(|| format!("reading {}", request.path))?;
        Ok(crate::head(&text))
    }

    async fn search(&self, request: crate::generated::SearchRequest) -> anyhow::Result<String> {
        let base = rooted(request.path.as_deref().unwrap_or(""))?;
        let root = rooted("")?;
        let pattern = request.pattern;
        tokio::task::spawn_blocking(move || {
            let mut hits = Vec::new();
            search_tree(&base, &root, &pattern, &mut hits)?;
            if hits.is_empty() {
                return Ok(format!("no lines contain `{pattern}`"));
            }
            let total = hits.len();
            hits.truncate(SEARCH_CAP);
            let mut out = hits.join("\n");
            if total > SEARCH_CAP {
                out.push_str(&format!(
                    "\n… truncated ({} more line(s))",
                    total - SEARCH_CAP
                ));
            }
            Ok(out)
        })
        .await?
    }

    async fn list(&self, request: crate::generated::ListRequest) -> anyhow::Result<String> {
        let dir = rooted(request.path.as_deref().unwrap_or(""))?;
        let mut entries = Vec::new();
        let mut reader = tokio::fs::read_dir(&dir).await?;
        while let Some(entry) = reader.next_entry().await? {
            let mut name = entry.file_name().to_string_lossy().into_owned();
            if entry.file_type().await?.is_dir() {
                name.push('/');
            }
            entries.push(name);
        }
        entries.sort();
        Ok(entries.join("\n"))
    }

    async fn restart(&self) -> anyhow::Result<()> {
        // The rebuild and the resume belong to the inbound above. The service's
        // share of a restart is proving the tree compiles before the handoff.
        let report = self.toolchain.check().await?;
        anyhow::ensure!(report.ok, "restart refused: {}", report.detail);
        Ok(())
    }

    async fn diff(&self, request: crate::generated::SnapshotRef) -> anyhow::Result<String> {
        self.checkpoint
            .diff(&checkpoint_state_request(self.model, request.reference)?)
            .await
    }

    async fn rollback(&self, request: crate::generated::SnapshotRef) -> anyhow::Result<String> {
        Ok(self
            .checkpoint
            .restore(&checkpoint_state_request(self.model, request.reference)?)
            .await?
            .detail)
    }

    async fn snapshot(&self, request: crate::generated::SnapshotRequest) -> anyhow::Result<String> {
        Ok(self
            .checkpoint
            .snapshot(&checkpoint_snapshot_request(self.model, request.label)?)
            .await?
            .reference)
    }

    async fn release(&self, request: crate::generated::SnapshotRef) -> anyhow::Result<String> {
        self.checkpoint.release(&request.reference).await
    }

    async fn prune(&self, request: crate::generated::SnapshotRetention) -> anyhow::Result<String> {
        self.checkpoint.prune(&request).await
    }

    async fn test(&self) -> anyhow::Result<crate::CheckReport> {
        self.toolchain.test().await
    }

    async fn model(&self) -> anyhow::Result<String> {
        Ok(describe(self.model))
    }

    async fn verify(&self) -> anyhow::Result<VerifyReport> {
        // The full render and the manifest reads are compute and blocking file
        // I/O, so the check runs off the async thread and a server keeps
        // serving while it verifies.
        let model = self.model.clone();
        let report = tokio::task::spawn_blocking(move || {
            let generated = generated_files(&model)?;
            Ok::<_, theseus_modeling::RenderError>(verify(
                &model,
                &workspace_root(),
                &generated,
                &authored_impls(&model),
                &interior_impls(&model),
            ))
        })
        .await??;
        Ok(report)
    }

    async fn generate(&self) -> anyhow::Result<Vec<GeneratedFile>> {
        generate_model(self.model, self.model, self.workspace, self.toolchain).await
    }

    async fn scaffold(&self) -> anyhow::Result<Vec<GeneratedFile>> {
        scaffold_model(self.model, self.model, self.workspace, self.toolchain).await
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
        match &request.port {
            Some(port) => {
                let path = adapter_path(self.model, port)?;
                let source = tokio::fs::read_to_string(workspace_root().join(&path))
                    .await
                    .with_context(|| format!("reading {path}"))?;
                Ok(theseus_modeling::adapter_source(
                    self.model,
                    &source,
                    port,
                    &request.method,
                    request.adapter.as_deref(),
                )?)
            }
            None => {
                let path = handler_path(self.model, &request.method)?;
                let source = tokio::fs::read_to_string(workspace_root().join(&path))
                    .await
                    .with_context(|| format!("reading {path}"))?;
                Ok(handler_source(self.model, &source, &request.method)?)
            }
        }
    }

    async fn check(&self) -> anyhow::Result<crate::CheckReport> {
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

    async fn implement(&self, request: ImplementRequest) -> anyhow::Result<ImplementResult> {
        implement_model(
            self.model,
            self.model,
            request,
            self.workspace,
            self.toolchain,
        )
        .await
    }

    async fn patch(&self, request: PatchRequest) -> anyhow::Result<PatchOutcome> {
        let (outcome, proposed) = apply_patch(self.model, &request)?;
        if request.write
            && let Some(proposed) = proposed
        {
            // Reproject every file from the proposed model — the self-model source
            // and the generated scaffolding update together. A new operation's
            // handler defaults to unimplemented until authored here, and `coverage`
            // reports what is left to write.
            persist_model(self.model, &proposed, self.workspace, self.toolchain).await?;
        }
        Ok(outcome)
    }
}

enum TransactionCheck {
    Committed(CheckReport),
    RolledBack(CheckReport),
}

/// The generated projection that exists for a model in the current workspace.
/// Crates without a manifest are deferred until `scaffold` creates them.
pub(crate) fn projected_files(model: &Model) -> anyhow::Result<Vec<GeneratedFile>> {
    let root = workspace_root();
    Ok(generated_files(model)?
        .into_iter()
        .filter(|file| crate_is_scaffolded(&root, file))
        .collect())
}

pub(crate) fn projected_expectations(model: &Model) -> anyhow::Result<ExpectedFileSet> {
    Ok(checkpoint_expectations(&workspace_root(), model)?)
}

pub(crate) fn checkpoint_snapshot_request(
    model: &Model,
    label: String,
) -> anyhow::Result<CheckpointSnapshotRequest> {
    Ok(CheckpointSnapshotRequest {
        label,
        expected: projected_expectations(model)?,
        owned_paths: checkpoint_paths(model)?,
        model: model.clone(),
    })
}

pub(crate) fn checkpoint_state_request(
    model: &Model,
    reference: String,
) -> anyhow::Result<CheckpointStateRequest> {
    Ok(CheckpointStateRequest {
        reference,
        expected: projected_expectations(model)?,
        owned_paths: checkpoint_paths(model)?,
        model: model.clone(),
    })
}

async fn begin_mutation(
    expected: &Model,
    desired: &Model,
    workspace: &dyn Workspace,
) -> anyhow::Result<PendingMutation> {
    let mut expected = projected_expectations(expected)?;
    for file in generated_files(desired)? {
        if !expected.iter().any(|entry| entry.path == file.path) {
            expected.push(ExpectedFile {
                path: file.path,
                contents: None,
            });
        }
    }
    workspace.begin_mutation(&expected).await
}

async fn finish_mutation(
    mutation: PendingMutation,
    toolchain: &dyn Toolchain,
) -> anyhow::Result<TransactionCheck> {
    let report = match toolchain.check_mutation().await {
        Ok(report) => report,
        Err(primary) => {
            return match mutation.rollback() {
                Ok(()) => Err(primary.context("the mutation was rolled back after check failed")),
                Err(rollback) => Err(anyhow::anyhow!(
                    "workspace check could not run: {primary}; rollback also failed: {rollback}"
                )),
            };
        }
    };
    if report.ok {
        mutation.commit()?;
        Ok(TransactionCheck::Committed(report))
    } else {
        mutation.rollback()?;
        Ok(TransactionCheck::RolledBack(report))
    }
}

fn validation_failed(report: &CheckReport) -> anyhow::Error {
    anyhow::anyhow!(
        "mutation failed its compile gate and was rolled back: {}",
        report.detail
    )
}

pub(crate) async fn generate_model(
    model: &Model,
    expected: &Model,
    workspace: &dyn Workspace,
    toolchain: &dyn Toolchain,
) -> anyhow::Result<Vec<GeneratedFile>> {
    let files = projected_files(model)?;
    let mut mutation = begin_mutation(expected, model, workspace).await?;
    let mut changes = mutation_changes(expected, model, files.clone())?;
    protect_cargo_lock(mutation.as_ref(), &mut changes).await?;
    mutation.apply(&changes).await?;
    match finish_mutation(mutation, toolchain).await? {
        TransactionCheck::Committed(_) => Ok(files),
        TransactionCheck::RolledBack(report) => Err(validation_failed(&report)),
    }
}

pub(crate) async fn scaffold_model(
    model: &Model,
    expected: &Model,
    workspace: &dyn Workspace,
    toolchain: &dyn Toolchain,
) -> anyhow::Result<Vec<GeneratedFile>> {
    let mut mutation = begin_mutation(expected, model, workspace).await?;
    let mut files = Vec::new();
    for file in scaffold_files(model) {
        if !mutation.exists(&file.path).await? {
            files.push(file);
        }
    }
    let mut writes = files.clone();
    for generated in generated_files(model)? {
        let Some(manifest) = crate_manifest_for(&generated) else {
            writes.push(generated);
            continue;
        };
        let scaffolded_now = mutation.exists(&manifest).await?;
        let scaffolded_by_batch = files.iter().any(|file| file.path == manifest);
        if scaffolded_now || scaffolded_by_batch {
            writes.push(generated);
        }
    }
    let mut changes = mutation_changes(expected, model, writes)?;
    protect_cargo_lock(mutation.as_ref(), &mut changes).await?;
    mutation.apply(&changes).await?;
    match finish_mutation(mutation, toolchain).await? {
        TransactionCheck::Committed(_) => Ok(files),
        TransactionCheck::RolledBack(report) => Err(validation_failed(&report)),
    }
}

fn crate_manifest_for(file: &GeneratedFile) -> Option<String> {
    let rest = file.path.strip_prefix("rust/")?;
    let (directory, _) = rest.split_once('/')?;
    Some(format!("rust/{directory}/Cargo.toml"))
}

async fn protect_cargo_lock(
    mutation: &dyn crate::WorkspaceMutation,
    files: &mut Vec<MutationFile>,
) -> anyhow::Result<()> {
    let contents = if mutation.exists("Cargo.lock").await? {
        Some(mutation.read_to_string("Cargo.lock").await?)
    } else {
        None
    };
    files.push(match contents {
        Some(contents) => MutationFile::text("Cargo.lock", contents),
        None => MutationFile::absent("Cargo.lock"),
    });
    Ok(())
}

fn mutation_changes(
    expected: &Model,
    desired: &Model,
    writes: Vec<GeneratedFile>,
) -> anyhow::Result<Vec<MutationFile>> {
    let desired_paths: std::collections::HashSet<String> = generated_files(desired)?
        .into_iter()
        .map(|file| file.path)
        .collect();
    let mut changes: Vec<MutationFile> = writes.into_iter().map(mutation_file).collect();
    for previous in generated_files(expected)? {
        if !desired_paths.contains(&previous.path)
            && !changes.iter().any(|change| change.path == previous.path)
        {
            changes.push(MutationFile::absent(previous.path));
        }
    }
    Ok(changes)
}

pub(crate) async fn implement_model(
    model: &Model,
    expected: &Model,
    request: ImplementRequest,
    workspace: &dyn Workspace,
    toolchain: &dyn Toolchain,
) -> anyhow::Result<ImplementResult> {
    let mut mutation = begin_mutation(expected, model, workspace).await?;
    let (wrote, path, spliced) = match &request.port {
        Some(port) => {
            let path = adapter_path(model, port)?;
            let source = mutation.read_to_string(&path).await?;
            let spliced = theseus_modeling::implement_adapter(
                model,
                &source,
                port,
                &request.method,
                request.adapter.as_deref(),
                &request.body,
                "crate::generated::",
            )?;
            (
                format!("the adapter method `{port}.{}`", request.method),
                path,
                spliced,
            )
        }
        None => {
            let path = handler_path(model, &request.method)?;
            let source = mutation.read_to_string(&path).await?;
            let spliced = theseus_modeling::implement(
                model,
                &source,
                &request.method,
                &request.body,
                "crate::generated::",
            )?;
            (
                format!("the handler for `{}`", request.method),
                path,
                spliced,
            )
        }
    };
    let writes = vec![GeneratedFile {
        path: path.clone(),
        contents: spliced,
    }];
    let mut changes: Vec<MutationFile> = writes.into_iter().map(mutation_file).collect();
    protect_cargo_lock(mutation.as_ref(), &mut changes).await?;
    mutation.apply(&changes).await?;
    match finish_mutation(mutation, toolchain).await? {
        TransactionCheck::Committed(check) => Ok(ImplementResult {
            applied: true,
            path: path.clone(),
            detail: format!("wrote {wrote} into {path}. Rebuild to load it."),
            check,
        }),
        TransactionCheck::RolledBack(check) => Ok(ImplementResult {
            applied: false,
            path: path.clone(),
            detail: format!("did not write {wrote} into {path}; the compile gate rolled it back"),
            check,
        }),
    }
}

fn mutation_file(file: GeneratedFile) -> MutationFile {
    MutationFile::text(file.path, file.contents)
}

pub(crate) async fn persist_model(
    expected: &Model,
    proposed: &Model,
    workspace: &dyn Workspace,
    toolchain: &dyn Toolchain,
) -> anyhow::Result<CheckReport> {
    let files = projected_files(proposed)?;
    let mut mutation = begin_mutation(expected, proposed, workspace).await?;
    let mut changes = mutation_changes(expected, proposed, files)?;
    protect_cargo_lock(mutation.as_ref(), &mut changes).await?;
    mutation.apply(&changes).await?;
    match finish_mutation(mutation, toolchain).await? {
        TransactionCheck::Committed(report) => Ok(report),
        TransactionCheck::RolledBack(report) => Err(validation_failed(&report)),
    }
}

/// Apply a patch request to a model: the one place the at-least-one-edit rule
/// and the edit application live, shared by the trait handler and the session.
pub(crate) fn apply_patch(
    model: &Model,
    request: &PatchRequest,
) -> anyhow::Result<(PatchOutcome, Option<Model>)> {
    if request.edit.is_empty() {
        anyhow::bail!("patch needs at least one edit");
    }
    Ok(apply_edits(model, &request.edit))
}

/// The most search hits a result carries, so a broad pattern stays readable.
const SEARCH_CAP: usize = 200;

/// A workspace path, resolved and proven to stay inside the workspace. Every
/// read surface goes through this guard — the operations cross every
/// transport, so a wire caller is held to the same boundary as the loop.
fn rooted(path: &str) -> anyhow::Result<std::path::PathBuf> {
    let root = workspace_root()
        .canonicalize()
        .context("resolving the workspace root")?;
    let resolved = root
        .join(path)
        .canonicalize()
        .with_context(|| format!("no such path: {path}"))?;
    anyhow::ensure!(
        resolved.starts_with(&root),
        "`{path}` escapes the workspace"
    );
    Ok(resolved)
}

/// Collect `path:line: text` hits for every line containing `pattern` under
/// `base`, skipping build trees, version control, and files that are not text.
fn search_tree(
    base: &std::path::Path,
    root: &std::path::Path,
    pattern: &str,
    hits: &mut Vec<String>,
) -> anyhow::Result<()> {
    let mut entries: Vec<_> = std::fs::read_dir(base)?.collect::<Result<_, _>>()?;
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        if hits.len() >= SEARCH_CAP {
            return Ok(());
        }
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        let file_type = entry.file_type()?;
        if file_type.is_symlink() {
            continue;
        }
        if file_type.is_dir() {
            if matches!(
                name.as_str(),
                ".git" | "target" | ".theseus" | ".trunk" | "node_modules"
            ) {
                continue;
            }
            search_tree(&path, root, pattern, hits)?;
            continue;
        }
        if !file_type.is_file() {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let rel = path.strip_prefix(root).unwrap_or(&path).display();
        for (number, line) in text.lines().enumerate() {
            if line.contains(pattern) {
                hits.push(format!("{rel}:{}: {}", number + 1, line.trim()));
                if hits.len() >= SEARCH_CAP {
                    return Ok(());
                }
            }
        }
    }
    Ok(())
}

/// The authored adapters file holding the impls for `port`: the `lib.rs` of the
/// crate whose service carries the port.
pub(crate) fn adapter_path(model: &Model, port: &str) -> anyhow::Result<String> {
    if let Some(service) = model.service_of_port(port) {
        return Ok(adapter_impl_path(model, service));
    }
    let inbound = model
        .inbound_of_port(port)
        .with_context(|| format!("no port named `{port}`"))?;
    Ok(inbound_adapter_impl_path(model, inbound))
}

/// The authored impl file holding the handler for `method`: the `service.rs` of
/// the crate the method's service lives in.
pub(crate) fn handler_path(model: &Model, method: &str) -> anyhow::Result<String> {
    let service = model
        .service_of_operation(method)
        .with_context(|| format!("no operation named `{method}`"))?;
    Ok(authored_impl_path(model, service))
}

#[cfg(test)]
mod tests {
    use std::{
        collections::HashMap,
        sync::{
            Arc, Mutex,
            atomic::{AtomicU64, Ordering},
        },
    };

    use theseus_model::theseus_model;

    use super::{SEARCH_CAP, search_tree};
    use crate::{
        generated::{Refused, TheseusService as _, Toolchain, Workspace, tool_catalog},
        session::Session,
    };

    static NEXT_SEARCH_DIRECTORY: AtomicU64 = AtomicU64::new(0);

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

    struct NoopMutation;

    #[async_trait::async_trait]
    impl crate::WorkspaceMutation for NoopMutation {
        async fn read_to_string(&self, path: &str) -> Result<String, crate::MutationError> {
            std::fs::read_to_string(crate::workspace_root().join(path)).map_err(|source| {
                crate::MutationError::Io {
                    operation: "reading test workspace file",
                    path: crate::workspace_root().join(path),
                    source,
                }
            })
        }

        async fn exists(&self, path: &str) -> Result<bool, crate::MutationError> {
            Ok(crate::workspace_root().join(path).is_file())
        }

        async fn apply(
            &mut self,
            _files: &[crate::MutationFile],
        ) -> Result<(), crate::MutationError> {
            Ok(())
        }

        fn commit(self: Box<Self>) -> Result<(), crate::MutationError> {
            Ok(())
        }

        fn rollback(self: Box<Self>) -> Result<(), crate::MutationError> {
            Ok(())
        }
    }

    #[async_trait::async_trait]
    impl Workspace for NoopWorkspace {
        async fn begin_mutation(
            &self,
            _expected: &crate::ExpectedFileSet,
        ) -> anyhow::Result<crate::PendingMutation> {
            Ok(Box::new(NoopMutation))
        }
    }

    #[derive(Default)]
    struct MutationRecording {
        expected: Vec<crate::ExpectedFile>,
        applied: Vec<crate::MutationFile>,
        commits: usize,
        rollbacks: usize,
    }

    struct RecordingWorkspace(Arc<Mutex<MutationRecording>>);

    struct RecordingMutation(Arc<Mutex<MutationRecording>>);

    #[async_trait::async_trait]
    impl crate::WorkspaceMutation for RecordingMutation {
        async fn read_to_string(&self, path: &str) -> Result<String, crate::MutationError> {
            std::fs::read_to_string(crate::workspace_root().join(path)).map_err(|source| {
                crate::MutationError::Io {
                    operation: "reading recorded workspace file",
                    path: crate::workspace_root().join(path),
                    source,
                }
            })
        }

        async fn exists(&self, path: &str) -> Result<bool, crate::MutationError> {
            if path == "Cargo.lock" {
                return Ok(false);
            }
            Ok(crate::workspace_root().join(path).is_file())
        }

        async fn apply(
            &mut self,
            files: &[crate::MutationFile],
        ) -> Result<(), crate::MutationError> {
            self.0.lock().unwrap().applied = files.to_vec();
            Ok(())
        }

        fn commit(self: Box<Self>) -> Result<(), crate::MutationError> {
            self.0.lock().unwrap().commits += 1;
            Ok(())
        }

        fn rollback(self: Box<Self>) -> Result<(), crate::MutationError> {
            self.0.lock().unwrap().rollbacks += 1;
            Ok(())
        }
    }

    #[async_trait::async_trait]
    impl Workspace for RecordingWorkspace {
        async fn begin_mutation(
            &self,
            expected: &crate::ExpectedFileSet,
        ) -> anyhow::Result<crate::PendingMutation> {
            self.0.lock().unwrap().expected = expected.clone();
            Ok(Box::new(RecordingMutation(Arc::clone(&self.0))))
        }
    }

    /// A checkpoint on its trait defaults: a test that never snapshots needs
    /// no history, and one that calls it reads the typed unimplemented error.
    struct StubCheckpoint;

    #[async_trait::async_trait]
    impl crate::generated::Checkpoint for StubCheckpoint {}

    #[derive(Default)]
    struct CheckpointRecording {
        snapshots: usize,
        restores: Vec<String>,
        models: HashMap<String, theseus_modeling::Model>,
    }

    struct RecordingCheckpoint(Arc<Mutex<CheckpointRecording>>);

    #[async_trait::async_trait]
    impl crate::generated::Checkpoint for RecordingCheckpoint {
        async fn snapshot(
            &self,
            request: &crate::CheckpointSnapshotRequest,
        ) -> anyhow::Result<crate::CheckpointSnapshot> {
            let mut recording = self.0.lock().unwrap();
            recording.snapshots += 1;
            let reference = format!("snapshot-{}", recording.snapshots);
            recording
                .models
                .insert(reference.clone(), request.model.clone());
            Ok(crate::CheckpointSnapshot { reference })
        }

        async fn restore(
            &self,
            request: &crate::CheckpointStateRequest,
        ) -> anyhow::Result<crate::CheckpointRestore> {
            let mut recording = self.0.lock().unwrap();
            recording.restores.push(request.reference.clone());
            let model = recording
                .models
                .get(&request.reference)
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("unknown snapshot"))?;
            Ok(crate::CheckpointRestore {
                detail: format!("restored {}", request.reference),
                model,
            })
        }
    }

    /// A toolchain that reports success without running a build, so a `check`
    /// call stays in-process.
    struct StubToolchain;

    #[async_trait::async_trait]
    impl Toolchain for StubToolchain {
        async fn check(&self) -> anyhow::Result<crate::CheckReport> {
            Ok(crate::CheckReport::success("the workspace compiles (stub)"))
        }

        async fn check_mutation(&self) -> anyhow::Result<crate::CheckReport> {
            self.check().await
        }
    }

    struct FailingToolchain;

    #[async_trait::async_trait]
    impl Toolchain for FailingToolchain {
        async fn check(&self) -> anyhow::Result<crate::CheckReport> {
            Ok(crate::CheckReport::failure(
                "the workspace does not compile",
            ))
        }

        async fn check_mutation(&self) -> anyhow::Result<crate::CheckReport> {
            self.check().await
        }
    }

    #[tokio::test]
    async fn restart_refuses_a_failed_compile_report() {
        let model = theseus_model();
        let ctx = crate::Ctx {
            model: &model,
            workspace: &NoopWorkspace,
            checkpoint: &StubCheckpoint,
            calculator: &theseus_calculator::Calculator,
            toolchain: &FailingToolchain,
        };
        let error = ctx
            .restart()
            .await
            .expect_err("restart must not accept a failed compile check");
        assert!(error.to_string().contains("does not compile"), "{error}");
    }

    #[tokio::test]
    async fn the_query_tool_finds_an_operation() {
        let result = Session::new(
            theseus_model(),
            &NoopWorkspace,
            &StubCheckpoint,
            &theseus_calculator::Calculator,
            &StubToolchain,
            false,
        )
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
        let mut session = Session::new(
            theseus_model(),
            &NoopWorkspace,
            &StubCheckpoint,
            &theseus_calculator::Calculator,
            &StubToolchain,
            false,
        );
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
    async fn a_write_uses_the_persisted_revision_and_commits_the_whole_working_model() {
        let recording = Arc::new(Mutex::new(MutationRecording::default()));
        let workspace = RecordingWorkspace(Arc::clone(&recording));
        let mut session = Session::new(
            theseus_model(),
            &workspace,
            &StubCheckpoint,
            &theseus_calculator::Calculator,
            &StubToolchain,
            true,
        );
        session
            .call(
                "patch",
                &serde_json::json!({ "edit": [{
                    "verb": "add", "parent": "model:theseus", "kind": "type",
                    "name": "DryProbe", "attrs": { "shape": "foreign:String" }
                }] }),
            )
            .await
            .expect("the dry edit applies");
        session
            .call(
                "patch",
                &serde_json::json!({
                    "edit": [{
                        "verb": "add", "parent": "model:theseus", "kind": "type",
                        "name": "WrittenProbe", "attrs": { "shape": "foreign:String" }
                    }],
                    "write": true
                }),
            )
            .await
            .expect("the written edit commits");

        let recording = recording.lock().unwrap();
        let expected_model = recording
            .expected
            .iter()
            .find(|file| file.path == theseus_model::SELF_MODEL_PATH)
            .and_then(|file| file.contents.as_deref())
            .expect("the persisted self-model is expected");
        assert!(!expected_model.contains("DryProbe"));
        assert!(!expected_model.contains("WrittenProbe"));
        let applied_model = recording
            .applied
            .iter()
            .find(|file| file.path == theseus_model::SELF_MODEL_PATH)
            .and_then(|file| file.text_contents())
            .expect("the proposed self-model is applied");
        assert!(applied_model.contains("DryProbe"));
        assert!(applied_model.contains("WrittenProbe"));
        assert!(
            recording
                .applied
                .iter()
                .any(|file| file.path == "Cargo.lock" && file.is_absent())
        );
        assert_eq!(recording.commits, 1);
        assert_eq!(recording.rollbacks, 0);
        drop(recording);

        let state = session.into_state();
        assert_eq!(state.working, state.persisted);
        assert!(state.persisted.type_def("DryProbe").is_some());
        assert!(state.persisted.type_def("WrittenProbe").is_some());
    }

    #[tokio::test]
    async fn rollback_restores_both_session_models_and_the_next_expected_revision() {
        let mutation = Arc::new(Mutex::new(MutationRecording::default()));
        let workspace = RecordingWorkspace(Arc::clone(&mutation));
        let checkpoint = Arc::new(Mutex::new(CheckpointRecording::default()));
        let checkpoint_adapter = RecordingCheckpoint(Arc::clone(&checkpoint));
        let mut session = Session::new(
            theseus_model(),
            &workspace,
            &checkpoint_adapter,
            &theseus_calculator::Calculator,
            &StubToolchain,
            true,
        );
        let reference = session
            .call("snapshot", &serde_json::json!({ "label": "before edits" }))
            .await
            .expect("the session snapshot succeeds");
        assert_eq!(reference, "snapshot-1");

        session
            .call(
                "patch",
                &serde_json::json!({ "edit": [{
                    "verb": "add", "parent": "model:theseus", "kind": "type",
                    "name": "DryAfterSnapshot", "attrs": { "shape": "foreign:String" }
                }] }),
            )
            .await
            .expect("the dry edit applies");
        session
            .call(
                "patch",
                &serde_json::json!({
                    "edit": [{
                        "verb": "add", "parent": "model:theseus", "kind": "type",
                        "name": "WrittenAfterSnapshot", "attrs": { "shape": "foreign:String" }
                    }],
                    "write": true
                }),
            )
            .await
            .expect("the written edit commits");
        session
            .call("rollback", &serde_json::json!({ "reference": reference }))
            .await
            .expect("the known snapshot restores");

        session
            .call(
                "patch",
                &serde_json::json!({
                    "edit": [{
                        "verb": "add", "parent": "model:theseus", "kind": "type",
                        "name": "AfterRollback", "attrs": { "shape": "foreign:String" }
                    }],
                    "write": true
                }),
            )
            .await
            .expect("a write after rollback uses the restored revision");

        let state = session.into_state();
        assert!(state.working.type_def("DryAfterSnapshot").is_none());
        assert!(state.working.type_def("WrittenAfterSnapshot").is_none());
        assert!(state.persisted.type_def("AfterRollback").is_some());
        let mutation = mutation.lock().unwrap();
        let expected_model = mutation
            .expected
            .iter()
            .find(|file| file.path == theseus_model::SELF_MODEL_PATH)
            .and_then(|file| file.contents.as_deref())
            .expect("the restored self-model is expected");
        assert!(!expected_model.contains("DryAfterSnapshot"));
        assert!(!expected_model.contains("WrittenAfterSnapshot"));
        let applied_model = mutation
            .applied
            .iter()
            .find(|file| file.path == theseus_model::SELF_MODEL_PATH)
            .and_then(|file| file.text_contents())
            .expect("the post-rollback self-model is applied");
        assert!(applied_model.contains("AfterRollback"));
        assert!(!applied_model.contains("DryAfterSnapshot"));
        assert!(!applied_model.contains("WrittenAfterSnapshot"));
        assert_eq!(checkpoint.lock().unwrap().restores, ["snapshot-1"]);
    }

    #[tokio::test]
    async fn snapshot_model_metadata_survives_session_reconstruction() {
        let mutation = Arc::new(Mutex::new(MutationRecording::default()));
        let workspace = RecordingWorkspace(Arc::clone(&mutation));
        let checkpoint = Arc::new(Mutex::new(CheckpointRecording::default()));
        let checkpoint_adapter = RecordingCheckpoint(Arc::clone(&checkpoint));
        let mut first = Session::new(
            theseus_model(),
            &workspace,
            &checkpoint_adapter,
            &theseus_calculator::Calculator,
            &StubToolchain,
            true,
        );
        let reference = first
            .call(
                "snapshot",
                &serde_json::json!({ "label": "before restart" }),
            )
            .await
            .expect("the first session snapshots its model");
        first
            .call(
                "patch",
                &serde_json::json!({
                    "edit": [{
                        "verb": "add", "parent": "model:theseus", "kind": "type",
                        "name": "AfterRestart", "attrs": { "shape": "foreign:String" }
                    }],
                    "write": true
                }),
            )
            .await
            .expect("the later model is persisted");
        let later = first.into_state().persisted;
        assert!(later.type_def("AfterRestart").is_some());

        let mut resumed = Session::new(
            later,
            &workspace,
            &checkpoint_adapter,
            &theseus_calculator::Calculator,
            &StubToolchain,
            true,
        );
        resumed
            .call("rollback", &serde_json::json!({ "reference": reference }))
            .await
            .expect("the reconstructed session restores durable metadata");

        let restored = resumed.into_state();
        assert!(restored.working.type_def("AfterRestart").is_none());
        assert_eq!(restored.working, restored.persisted);
        assert_eq!(checkpoint.lock().unwrap().restores, ["snapshot-1"]);
    }

    #[tokio::test]
    async fn a_failed_implement_check_rolls_back_and_reports_not_applied() {
        let recording = Arc::new(Mutex::new(MutationRecording::default()));
        let workspace = RecordingWorkspace(Arc::clone(&recording));
        let mut session = Session::new(
            theseus_model(),
            &workspace,
            &StubCheckpoint,
            &theseus_calculator::Calculator,
            &FailingToolchain,
            true,
        );
        let result = session
            .call(
                "implement",
                &serde_json::json!({
                    "method": "calc",
                    "body": "Ok(\"replacement\".to_string())"
                }),
            )
            .await
            .expect("a failed compile check is a structured implement result");
        let result: crate::ImplementResult = serde_json::from_str(&result).unwrap();
        assert!(!result.applied);
        assert!(!result.check.ok);
        let recording = recording.lock().unwrap();
        assert_eq!(recording.commits, 0);
        assert_eq!(recording.rollbacks, 1);
    }

    #[tokio::test]
    async fn a_write_is_refused_without_the_gate() {
        let input = serde_json::json!({ "edit": [probe_edit()], "write": true });
        let error = Session::new(
            theseus_model(),
            &NoopWorkspace,
            &StubCheckpoint,
            &theseus_calculator::Calculator,
            &StubToolchain,
            false,
        )
        .call("patch", &input)
        .await
        .expect_err("the gate refuses the write");
        assert!(
            error.downcast_ref::<Refused>().is_some(),
            "the refusal should carry the typed gate error: {error}"
        );
    }

    #[tokio::test]
    async fn every_checkpoint_mutation_is_refused_without_the_gate() {
        let calls = [
            ("snapshot", serde_json::json!({ "label": "read only" })),
            (
                "rollback",
                serde_json::json!({ "reference": "snapshot-id" }),
            ),
            ("release", serde_json::json!({ "reference": "snapshot-id" })),
            ("prune", serde_json::json!({ "keep": 3 })),
            ("diff", serde_json::json!({ "reference": "snapshot-id" })),
        ];

        for (name, input) in calls {
            let outcome = Session::new(
                theseus_model(),
                &NoopWorkspace,
                &StubCheckpoint,
                &theseus_calculator::Calculator,
                &StubToolchain,
                false,
            )
            .call(name, &input)
            .await;
            let error = match outcome {
                Ok(result) => panic!("{name} bypassed its write gate: {result}"),
                Err(error) => error,
            };
            assert!(
                error.downcast_ref::<Refused>().is_some(),
                "{name} should carry the typed gate error: {error}"
            );
        }
    }

    #[test]
    fn prune_tool_schema_matches_the_unsigned_contract() {
        let prune = crate::tool_catalog()
            .into_iter()
            .find(|tool| tool["name"] == "prune")
            .expect("prune is exposed as an agent tool");
        let keep = &prune["input_schema"]["properties"]["keep"];

        assert_eq!(keep["type"], "integer");
        assert_eq!(keep["minimum"], 0);
        assert_eq!(keep["maximum"], u32::MAX);
    }

    #[tokio::test]
    async fn a_write_is_allowed_with_the_gate() {
        // The no-op workspace discards the reprojection, so this touches no files.
        let input = serde_json::json!({ "edit": [probe_edit()], "write": true });
        let result = Session::new(
            theseus_model(),
            &NoopWorkspace,
            &StubCheckpoint,
            &theseus_calculator::Calculator,
            &StubToolchain,
            true,
        )
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
        let result = Session::new(
            theseus_model(),
            &NoopWorkspace,
            &StubCheckpoint,
            &theseus_calculator::Calculator,
            &StubToolchain,
            false,
        )
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
        let error = Session::new(
            theseus_model(),
            &NoopWorkspace,
            &StubCheckpoint,
            &theseus_calculator::Calculator,
            &StubToolchain,
            false,
        )
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
        let result = Session::new(
            theseus_model(),
            &NoopWorkspace,
            &StubCheckpoint,
            &theseus_calculator::Calculator,
            &StubToolchain,
            true,
        )
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
        let result = Session::new(
            theseus_model(),
            &NoopWorkspace,
            &StubCheckpoint,
            &theseus_calculator::Calculator,
            &StubToolchain,
            false,
        )
        .call("check", &serde_json::json!({}))
        .await
        .expect("the check tool runs");
        let report: crate::CheckReport =
            serde_json::from_str(&result).expect("the tool returns a structured check report");
        assert!(report.ok);
        assert_eq!(report.detail, "the workspace compiles (stub)");
    }

    #[tokio::test]
    async fn the_read_tool_reads_a_workspace_file() {
        let result = Session::new(
            theseus_model(),
            &NoopWorkspace,
            &StubCheckpoint,
            &theseus_calculator::Calculator,
            &StubToolchain,
            false,
        )
        .call("read", &serde_json::json!({ "path": "Cargo.toml" }))
        .await
        .expect("the read tool runs");
        assert!(result.contains("[workspace]"), "{result}");
    }

    #[tokio::test]
    async fn the_read_tool_refuses_an_escape() {
        let error = Session::new(
            theseus_model(),
            &NoopWorkspace,
            &StubCheckpoint,
            &theseus_calculator::Calculator,
            &StubToolchain,
            false,
        )
        .call("read", &serde_json::json!({ "path": "../" }))
        .await
        .expect_err("a path above the workspace is refused");
        assert!(
            error.to_string().contains("escapes the workspace"),
            "{error}"
        );
    }

    #[tokio::test]
    async fn the_search_tool_reports_path_and_line() {
        let result = Session::new(
            theseus_model(),
            &NoopWorkspace,
            &StubCheckpoint,
            &theseus_calculator::Calculator,
            &StubToolchain,
            false,
        )
        .call(
            "search",
            &serde_json::json!({ "pattern": "theseus-kernel", "path": "rust/kernel" }),
        )
        .await
        .expect("the search tool runs");
        assert!(result.contains("rust/kernel/Cargo.toml:"), "{result}");
    }

    #[cfg(unix)]
    #[test]
    fn workspace_search_skips_symlinks_and_stops_at_its_cap() {
        use std::os::unix::fs::symlink;

        let nonce = NEXT_SEARCH_DIRECTORY.fetch_add(1, Ordering::Relaxed);
        let root =
            std::env::temp_dir().join(format!("theseus-search-{}-{nonce}", std::process::id()));
        let outside = root.with_extension("outside");
        std::fs::create_dir(&root).unwrap();
        std::fs::write(&outside, "outside secret").unwrap();
        symlink(&outside, root.join("linked.txt")).unwrap();
        let lines = std::iter::repeat_n("match", SEARCH_CAP + 20).collect::<Vec<_>>();
        std::fs::write(root.join("many.txt"), lines.join("\n")).unwrap();

        let mut hits = Vec::new();
        search_tree(&root, &root, "match", &mut hits).unwrap();
        assert_eq!(hits.len(), SEARCH_CAP);
        hits.clear();
        search_tree(&root, &root, "outside secret", &mut hits).unwrap();
        assert!(hits.is_empty());

        std::fs::remove_dir_all(root).ok();
        std::fs::remove_file(outside).ok();
    }

    #[tokio::test]
    async fn the_show_tool_reads_a_port_adapter_method() {
        let result = Session::new(
            theseus_model(),
            &NoopWorkspace,
            &StubCheckpoint,
            &theseus_calculator::Calculator,
            &StubToolchain,
            false,
        )
        .call(
            "show",
            &serde_json::json!({ "method": "check", "port": "toolchain" }),
        )
        .await
        .expect("the show tool runs");
        assert!(
            result.contains("async fn check"),
            "the adapter source should appear: {result}"
        );
        assert!(
            result.contains("cargo"),
            "the authored adapter body should appear: {result}"
        );
    }

    #[tokio::test]
    async fn the_show_tool_reads_an_inbound_interior_adapter() {
        let result = Session::new(
            theseus_model(),
            &NoopWorkspace,
            &StubCheckpoint,
            &theseus_calculator::Calculator,
            &StubToolchain,
            false,
        )
        .call(
            "show",
            &serde_json::json!({ "method": "complete", "port": "llm", "adapter": "OfflineLlm" }),
        )
        .await
        .expect("the show tool reads the interior adapter");
        assert!(
            result.contains("async fn complete"),
            "the adapter source should appear: {result}"
        );
    }

    #[tokio::test]
    async fn an_adapter_implement_is_refused_without_the_gate() {
        let input = serde_json::json!({
            "method": "check", "port": "toolchain", "body": "todo!()"
        });
        let error = Session::new(
            theseus_model(),
            &NoopWorkspace,
            &StubCheckpoint,
            &theseus_calculator::Calculator,
            &StubToolchain,
            false,
        )
        .call("implement", &input)
        .await
        .expect_err("the gate refuses the write");
        assert!(
            error.downcast_ref::<Refused>().is_some(),
            "the refusal should carry the typed gate error: {error}"
        );
    }

    #[tokio::test]
    async fn an_adapter_implement_writes_through_the_gate() {
        // The no-op workspace discards the spliced source, so this touches no files.
        let input = serde_json::json!({
            "method": "check", "port": "toolchain", "body": "todo!()"
        });
        let result = Session::new(
            theseus_model(),
            &NoopWorkspace,
            &StubCheckpoint,
            &theseus_calculator::Calculator,
            &StubToolchain,
            true,
        )
        .call("implement", &input)
        .await
        .expect("the implement tool runs");
        assert!(
            result.contains("wrote the adapter method `toolchain.check`"),
            "the tool should report the adapter write: {result}"
        );
    }

    #[tokio::test]
    async fn an_unexposed_operation_is_not_a_tool() {
        let error = Session::new(
            theseus_model(),
            &NoopWorkspace,
            &StubCheckpoint,
            &theseus_calculator::Calculator,
            &StubToolchain,
            false,
        )
        .call("calc", &serde_json::json!({}))
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
            let mut session = Session::new(
                theseus_model(),
                &NoopWorkspace,
                &StubCheckpoint,
                &theseus_calculator::Calculator,
                &StubToolchain,
                false,
            );
            if let Err(error) = session.call(name, &serde_json::json!({})).await {
                assert!(
                    !error.to_string().contains("unknown tool"),
                    "catalog tool `{name}` has no dispatch arm: {error}"
                );
            }
        }
    }
}
