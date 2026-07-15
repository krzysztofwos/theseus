//! Theseus's authored service implementation (L3).
//!
//! These are the operation handlers — the behavior leaves checked against the
//! generated [`TheseusService`](crate::generated::TheseusService) contract. An
//! operation without a handler here falls through to the trait's `unimplemented`
//! default, and `verify`'s coverage check reports it. This is the one file the
//! structured-edit tooling writes. The composition root and adapters live in the
//! inbound binaries (`theseus-cli`, and the agent and MCP adapters to come).

use anyhow::Context;
use theseus_modeling::{
    CoverageReport, GeneratedFile, Model, PatchOutcome, QueryOutcome, RustItemEdit, RustItemMode,
    VerifyReport, apply_edits, coverage, describe, edit_rust_item as splice_rust_item,
    handler_source, query, rust_source_revision, scaffold_files, verify,
};

use crate::{
    CheckReport, CheckpointSnapshotRequest, CheckpointStateRequest, ExpectedFile, ExpectedFileSet,
    ImplementResult, MutationFile, PendingMutation, ProjectContext, RustItemResult, SourceDocument,
    generated::{
        CalcRequest, Checkpoint, Ctx, ImplementRequest, PatchRequest, QueryRequest,
        RustItemRequest, ShowRequest, TheseusService, Toolchain, Workspace,
    },
};

#[derive(Debug, thiserror::Error)]
enum BrowsePathError {
    #[error("path {path:?} is a directory; call `list` with {repair}")]
    ReadDirectory { path: String, repair: String },
    #[error("path {path:?} is a file; call `read` with {repair}")]
    ListFile { path: String, repair: String },
}

pub(crate) async fn ensure_workspace_toolchain_project(
    project: &ProjectContext,
    workspace: &dyn Workspace,
    toolchain: &dyn Toolchain,
) -> anyhow::Result<()> {
    project.ensure_same_project(&workspace.context().await?)?;
    project.ensure_same_project(&toolchain.context().await?)?;
    Ok(())
}

pub(crate) async fn ensure_toolchain_project(
    project: &ProjectContext,
    toolchain: &dyn Toolchain,
) -> anyhow::Result<()> {
    project.ensure_same_project(&toolchain.context().await?)?;
    Ok(())
}

pub(crate) async fn ensure_checkpoint_project(
    project: &ProjectContext,
    checkpoint: &dyn Checkpoint,
) -> anyhow::Result<()> {
    project.ensure_same_project(&checkpoint.context().await?)?;
    Ok(())
}

#[async_trait::async_trait]
impl TheseusService for Ctx<'_> {
    async fn explain(&self, request: crate::generated::ExplainRequest) -> anyhow::Result<String> {
        match request.code.as_deref() {
            None => {
                let listing = explain_catalog::CODES
                    .iter()
                    .map(|entry| format!("  {:8} {}", entry.code, entry.message))
                    .collect::<Vec<_>>()
                    .join("\n");
                Ok(format!(
                    "Harness diagnostic codes — call with `code: <name>` for the full entry:\n\n{listing}\n\nModel edit refusals carry their own `PATCH0xx` codes, returned inline by `patch`."
                ))
            }
            Some(name) => match explain_catalog::get(name) {
                Some(entry) => Ok(format!(
                    "{}\n\n{}\n\nnext:   {}\nsafety: {}",
                    entry.code, entry.message, entry.help, entry.safety
                )),
                None => {
                    let known = explain_catalog::CODES
                        .iter()
                        .map(|entry| entry.code)
                        .collect::<Vec<_>>()
                        .join(", ");
                    anyhow::bail!(
                        "unknown diagnostic code {name:?}; call `explain` bare for the list. Known codes: {known}"
                    )
                }
            },
        }
    }

    async fn skills(&self, request: crate::generated::SkillsRequest) -> anyhow::Result<String> {
        use theseus_modeling::model_hash;
        let hash = model_hash(self.model);
        let version_header = format!("model-hash: {hash}\n\n");
        match request.topic.as_deref() {
            None => {
                let listing = skills_catalog::TOPICS
                    .iter()
                    .map(|t| format!("  {t}"))
                    .collect::<Vec<_>>()
                    .join("\n");
                Ok(format!(
                    "{version_header}Available topics — call with `topic: <name>` to fetch one:\n\n{listing}\n\nFetch `workflow` once per session to learn gate trust."
                ))
            }
            Some("model") => {
                let theseus_service = self.model.service_named("Theseus");
                let ops_section = if let Some(svc) = theseus_service {
                    let op_lines = svc
                        .operations
                        .iter()
                        .map(|op| format!("  {:20} — {}", op.name, op.summary))
                        .collect::<Vec<_>>()
                        .join("\n");
                    format!(
                        "## model\n\nWorking with the self-model:\n\n- **query** — list handles (kind: operation, type, port); filter with `find` or `node`.\n- **patch** — edit the model. Verbs: `add`, `remove`, `rename`, `set`.\n  - Operations: attrs `summary`, `request`, `response`, `uses`, `tool`.\n  - Set `tool` to a description string to expose an op to agents; omit to keep CLI-only.\n  - `uses` declares port dependencies; `verify` checks the handler reaches exactly those.\n  - `write: true` reprojects under a compile gate; `write: false` is a dry run.\n- **generate** — refresh generated.rs after a model change.\n- **verify** — check workspace conformance after model changes.\n- **coverage** — list operations with no authored handler.\n\nCurrent Theseus operations ({count}):\n\n{op_lines}\n",
                        count = svc.operations.len()
                    )
                } else {
                    "## model\n\n(Theseus service not found in model — re-query.)\n".to_string()
                };
                Ok(format!("{version_header}{ops_section}"))
            }
            Some(name) => {
                if let Some(body) = skills_catalog::get(name) {
                    Ok(format!("{version_header}{body}"))
                } else {
                    let known = skills_catalog::TOPICS.join(", ");
                    Err(anyhow::anyhow!(
                        "unknown topic {name:?}; available topics: {known}"
                    ))
                }
            }
        }
    }

    async fn drive(&self, request: crate::generated::DriveRequest) -> anyhow::Result<String> {
        let input = match request.input.as_deref() {
            Some(text) => serde_json::from_str(text)
                .context("parsing `input` as a JSON object of field values")?,
            None => serde_json::Value::Null,
        };
        let invocation = theseus_modeling::cli_invocation(self.model, &request.operation, &input)?;
        self.toolchain.drive(&invocation).await
    }

    async fn lint(&self) -> anyhow::Result<crate::CheckReport> {
        let project = self.project.context().await?;
        ensure_toolchain_project(&project, self.toolchain).await?;
        self.toolchain.lint().await
    }

    async fn read(&self, request: crate::generated::ReadRequest) -> anyhow::Result<SourceDocument> {
        let project = self.project.context().await?;
        let path = project.resolve_existing(&request.path)?;
        let metadata = tokio::fs::metadata(&path)
            .await
            .with_context(|| format!("inspecting {}", request.path))?;
        if metadata.is_dir() {
            let repair = serde_json::json!({ "path": &request.path }).to_string();
            return Err(BrowsePathError::ReadDirectory {
                path: request.path,
                repair,
            }
            .into());
        }
        let text = tokio::fs::read_to_string(&path)
            .await
            .with_context(|| format!("reading {}", request.path))?;
        Ok(SourceDocument::new(request.path, &text))
    }

    async fn search(&self, request: crate::generated::SearchRequest) -> anyhow::Result<String> {
        let project = self.project.context().await?;
        let base = project.resolve_existing(request.path.as_deref().unwrap_or(""))?;
        let root = project.root().to_path_buf();
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
        let project = self.project.context().await?;
        let requested = request.path.unwrap_or_default();
        let dir = project.resolve_existing(&requested)?;
        let metadata = tokio::fs::metadata(&dir)
            .await
            .with_context(|| format!("inspecting {requested}"))?;
        if metadata.is_file() {
            let repair = serde_json::json!({ "path": &requested }).to_string();
            return Err(BrowsePathError::ListFile {
                path: requested,
                repair,
            }
            .into());
        }
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

    async fn edit_rust_item(&self, request: RustItemRequest) -> anyhow::Result<RustItemResult> {
        let project = self.project.context().await?;
        edit_rust_item_model(
            &project,
            self.model,
            self.model,
            request,
            self.workspace,
            self.toolchain,
        )
        .await
    }

    async fn restart(&self) -> anyhow::Result<()> {
        // The rebuild and the resume belong to the inbound above. The service's
        // share of a restart is proving the tree compiles before the handoff.
        let project = self.project.context().await?;
        ensure_toolchain_project(&project, self.toolchain).await?;
        let report = self.toolchain.check().await?;
        anyhow::ensure!(report.ok, "restart refused: {}", report.detail);
        Ok(())
    }

    async fn diff(&self, request: crate::generated::SnapshotRef) -> anyhow::Result<String> {
        let project = self.project.context().await?;
        ensure_checkpoint_project(&project, self.checkpoint).await?;
        self.checkpoint
            .diff(&checkpoint_state_request(
                &project,
                self.model,
                request.reference,
            )?)
            .await
    }

    async fn rollback(&self, request: crate::generated::SnapshotRef) -> anyhow::Result<String> {
        let project = self.project.context().await?;
        ensure_checkpoint_project(&project, self.checkpoint).await?;
        Ok(self
            .checkpoint
            .restore(&checkpoint_state_request(
                &project,
                self.model,
                request.reference,
            )?)
            .await?
            .detail)
    }

    async fn snapshot(&self, request: crate::generated::SnapshotRequest) -> anyhow::Result<String> {
        let project = self.project.context().await?;
        ensure_checkpoint_project(&project, self.checkpoint).await?;
        Ok(self
            .checkpoint
            .snapshot(&checkpoint_snapshot_request(
                &project,
                self.model,
                request.label,
            )?)
            .await?
            .reference)
    }

    async fn release(&self, request: crate::generated::SnapshotRef) -> anyhow::Result<String> {
        let project = self.project.context().await?;
        ensure_checkpoint_project(&project, self.checkpoint).await?;
        self.checkpoint.release(&request.reference).await
    }

    async fn prune(&self, request: crate::generated::SnapshotRetention) -> anyhow::Result<String> {
        let project = self.project.context().await?;
        ensure_checkpoint_project(&project, self.checkpoint).await?;
        self.checkpoint.prune(&request).await
    }

    async fn test(&self) -> anyhow::Result<crate::CheckReport> {
        let project = self.project.context().await?;
        ensure_toolchain_project(&project, self.toolchain).await?;
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
        let project = self.project.context().await?;
        let report = tokio::task::spawn_blocking(move || -> anyhow::Result<_> {
            let generated = project.generated_files(&model)?;
            let authored = project.authored_impls(&model)?;
            let interiors = project.interior_impls(&model)?;
            Ok(verify(
                &model,
                project.root(),
                &generated,
                &authored,
                &interiors,
            ))
        })
        .await??;
        Ok(report)
    }

    async fn generate(&self) -> anyhow::Result<Vec<GeneratedFile>> {
        let project = self.project.context().await?;
        ensure_workspace_toolchain_project(&project, self.workspace, self.toolchain).await?;
        generate_model(
            &project,
            self.model,
            self.model,
            self.workspace,
            self.toolchain,
        )
        .await
    }

    async fn scaffold(&self) -> anyhow::Result<Vec<GeneratedFile>> {
        let project = self.project.context().await?;
        ensure_workspace_toolchain_project(&project, self.workspace, self.toolchain).await?;
        scaffold_model(
            &project,
            self.model,
            self.model,
            self.workspace,
            self.toolchain,
        )
        .await
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
        let project = self.project.context().await?;
        let report = tokio::task::spawn_blocking(move || {
            coverage(&model, |service| -> anyhow::Result<String> {
                let path = project.authored_impl_path(&model, service)?;
                std::fs::read_to_string(project.root().join(&path))
                    .with_context(|| format!("reading {path}"))
            })
        })
        .await??;
        Ok(report)
    }

    async fn show(&self, request: ShowRequest) -> anyhow::Result<String> {
        let project = self.project.context().await?;
        match &request.port {
            Some(port) => {
                let path = adapter_path(&project, self.model, port)?;
                let source = tokio::fs::read_to_string(project.root().join(&path))
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
                let path = handler_path(&project, self.model, &request.method)?;
                let source = tokio::fs::read_to_string(project.root().join(&path))
                    .await
                    .with_context(|| format!("reading {path}"))?;
                Ok(handler_source(self.model, &source, &request.method)?)
            }
        }
    }

    async fn check(&self) -> anyhow::Result<crate::CheckReport> {
        let project = self.project.context().await?;
        ensure_toolchain_project(&project, self.toolchain).await?;
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
        let project = self.project.context().await?;
        ensure_workspace_toolchain_project(&project, self.workspace, self.toolchain).await?;
        implement_model(
            &project,
            self.model,
            self.model,
            request,
            self.workspace,
            self.toolchain,
        )
        .await
    }

    async fn patch(&self, request: PatchRequest) -> anyhow::Result<PatchOutcome> {
        let project = self.project.context().await?;
        let (outcome, proposed) = apply_patch(self.model, &request)?;
        if request.write
            && let Some(proposed) = proposed
        {
            ensure_workspace_toolchain_project(&project, self.workspace, self.toolchain).await?;
            // Reproject every file from the proposed model — the self-model source
            // and the generated scaffolding update together. A new operation's
            // handler defaults to unimplemented until authored here, and `coverage`
            // reports what is left to write.
            persist_model(
                &project,
                self.model,
                &proposed,
                self.workspace,
                self.toolchain,
            )
            .await?;
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
pub(crate) fn projected_files(
    project: &ProjectContext,
    model: &Model,
) -> anyhow::Result<Vec<GeneratedFile>> {
    Ok(project.projected_files(model)?)
}

pub(crate) fn projected_expectations(
    project: &ProjectContext,
    model: &Model,
) -> anyhow::Result<ExpectedFileSet> {
    Ok(project.expected_files(model)?)
}

pub(crate) fn checkpoint_snapshot_request(
    project: &ProjectContext,
    model: &Model,
    label: String,
) -> anyhow::Result<CheckpointSnapshotRequest> {
    Ok(CheckpointSnapshotRequest {
        label,
        expected: projected_expectations(project, model)?,
        owned_paths: project.owned_paths(model)?,
        model: model.clone(),
        project: project.descriptor(),
    })
}

pub(crate) fn checkpoint_state_request(
    project: &ProjectContext,
    model: &Model,
    reference: String,
) -> anyhow::Result<CheckpointStateRequest> {
    Ok(CheckpointStateRequest {
        reference,
        expected: projected_expectations(project, model)?,
        owned_paths: project.owned_paths(model)?,
        model: model.clone(),
        project: project.descriptor(),
    })
}

async fn begin_mutation(
    project: &ProjectContext,
    expected: &Model,
    desired: &Model,
    workspace: &dyn Workspace,
) -> anyhow::Result<PendingMutation> {
    let mut expected = projected_expectations(project, expected)?;
    for file in project.generated_files(desired)? {
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
    project: &ProjectContext,
    model: &Model,
    expected: &Model,
    workspace: &dyn Workspace,
    toolchain: &dyn Toolchain,
) -> anyhow::Result<Vec<GeneratedFile>> {
    ensure_workspace_toolchain_project(project, workspace, toolchain).await?;
    let files = projected_files(project, model)?;
    let mut mutation = begin_mutation(project, expected, model, workspace).await?;
    let mut changes = mutation_changes(project, expected, model, files.clone())?;
    protect_cargo_lock(mutation.as_ref(), &mut changes).await?;
    mutation.apply(&changes).await?;
    match finish_mutation(mutation, toolchain).await? {
        TransactionCheck::Committed(_) => Ok(files),
        TransactionCheck::RolledBack(report) => Err(validation_failed(&report)),
    }
}

pub(crate) async fn scaffold_model(
    project: &ProjectContext,
    model: &Model,
    expected: &Model,
    workspace: &dyn Workspace,
    toolchain: &dyn Toolchain,
) -> anyhow::Result<Vec<GeneratedFile>> {
    ensure_workspace_toolchain_project(project, workspace, toolchain).await?;
    let mut mutation = begin_mutation(project, expected, model, workspace).await?;
    let mut files = Vec::new();
    for file in scaffold_files(model) {
        if !mutation.exists(&file.path).await? {
            files.push(file);
        }
    }
    let mut writes = files.clone();
    for generated in project.generated_files(model)? {
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
    let mut changes = mutation_changes(project, expected, model, writes)?;
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
    project: &ProjectContext,
    expected: &Model,
    desired: &Model,
    writes: Vec<GeneratedFile>,
) -> anyhow::Result<Vec<MutationFile>> {
    let desired_paths: std::collections::HashSet<String> = project
        .generated_files(desired)?
        .into_iter()
        .map(|file| file.path)
        .collect();
    let mut changes: Vec<MutationFile> = writes.into_iter().map(mutation_file).collect();
    for previous in project.generated_files(expected)? {
        if !desired_paths.contains(&previous.path)
            && !changes.iter().any(|change| change.path == previous.path)
        {
            changes.push(MutationFile::absent(previous.path));
        }
    }
    Ok(changes)
}

pub(crate) async fn implement_model(
    project: &ProjectContext,
    model: &Model,
    expected: &Model,
    request: ImplementRequest,
    workspace: &dyn Workspace,
    toolchain: &dyn Toolchain,
) -> anyhow::Result<ImplementResult> {
    ensure_workspace_toolchain_project(project, workspace, toolchain).await?;
    let mut mutation = begin_mutation(project, expected, model, workspace).await?;
    let (wrote, path, spliced) = match &request.port {
        Some(port) => {
            let path = adapter_path(project, model, port)?;
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
            let path = handler_path(project, model, &request.method)?;
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

pub(crate) async fn edit_rust_item_model(
    project: &ProjectContext,
    model: &Model,
    expected: &Model,
    request: RustItemRequest,
    workspace: &dyn Workspace,
    toolchain: &dyn Toolchain,
) -> anyhow::Result<RustItemResult> {
    ensure_workspace_toolchain_project(project, workspace, toolchain).await?;
    project
        .layout()
        .authorize_authored_rust_path(model, &request.path)?;
    let mut mutation = begin_mutation(project, expected, model, workspace).await?;
    let source = mutation.read_to_string(&request.path).await?;
    let previous_revision = rust_source_revision(&source);
    let edited = splice_rust_item(
        &source,
        &request.revision,
        &RustItemEdit {
            mode: if request.replace {
                RustItemMode::Replace
            } else {
                RustItemMode::Insert
            },
            item: request.item,
        },
    )?;
    let item = edited.identity;
    let new_revision = edited.new_revision;
    let mut changes = vec![MutationFile::text(request.path.clone(), edited.source)];
    protect_cargo_lock(mutation.as_ref(), &mut changes).await?;
    mutation.apply(&changes).await?;
    match finish_mutation(mutation, toolchain).await? {
        TransactionCheck::Committed(check) => Ok(RustItemResult {
            applied: true,
            path: request.path.clone(),
            item: item.clone(),
            revision: new_revision,
            detail: format!("wrote `{item}` into {}", request.path),
            check,
        }),
        TransactionCheck::RolledBack(check) => Ok(RustItemResult {
            applied: false,
            path: request.path.clone(),
            item: item.clone(),
            revision: previous_revision,
            detail: format!(
                "did not write `{item}` into {}; the all-target compile gate rolled it back",
                request.path
            ),
            check,
        }),
    }
}

fn mutation_file(file: GeneratedFile) -> MutationFile {
    MutationFile::text(file.path, file.contents)
}

pub(crate) async fn persist_model(
    project: &ProjectContext,
    expected: &Model,
    proposed: &Model,
    workspace: &dyn Workspace,
    toolchain: &dyn Toolchain,
) -> anyhow::Result<CheckReport> {
    ensure_workspace_toolchain_project(project, workspace, toolchain).await?;
    let files = projected_files(project, proposed)?;
    let mut mutation = begin_mutation(project, expected, proposed, workspace).await?;
    let mut changes = mutation_changes(project, expected, proposed, files)?;
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
pub(crate) fn adapter_path(
    project: &ProjectContext,
    model: &Model,
    port: &str,
) -> anyhow::Result<String> {
    if let Some(service) = model.service_of_port(port) {
        return Ok(project.adapter_impl_path(model, service)?);
    }
    let inbound = model
        .inbound_of_port(port)
        .with_context(|| format!("no port named `{port}`"))?;
    Ok(project.inbound_adapter_impl_path(model, inbound)?)
}

/// The authored impl file holding the handler for `method`: the `service.rs` of
/// the crate the method's service lives in.
pub(crate) fn handler_path(
    project: &ProjectContext,
    model: &Model,
    method: &str,
) -> anyhow::Result<String> {
    let service = model
        .service_of_operation(method)
        .with_context(|| format!("no operation named `{method}`"))?;
    Ok(project.authored_impl_path(model, service)?)
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
    use theseus_modeling::Model;

    use super::{SEARCH_CAP, search_tree};
    use crate::{
        generated::{Refused, TheseusService as _, Toolchain, Workspace, tool_catalog},
        session::Session,
    };

    static NEXT_SEARCH_DIRECTORY: AtomicU64 = AtomicU64::new(0);

    fn project_with_model(model: Model) -> crate::ProjectContext {
        crate::ProjectContext::new(
            crate::workspace_root(),
            model,
            theseus_model::project_layout().expect("Theseus layout is valid"),
        )
        .expect("Theseus project context is valid")
    }

    fn project() -> crate::ProjectContext {
        project_with_model(theseus_model())
    }

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
        async fn context(&self) -> anyhow::Result<crate::ProjectContext> {
            Ok(project())
        }

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
        async fn context(&self) -> anyhow::Result<crate::ProjectContext> {
            Ok(project())
        }

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
    impl crate::generated::Checkpoint for StubCheckpoint {
        async fn context(&self) -> anyhow::Result<crate::ProjectContext> {
            Ok(project())
        }
    }

    #[derive(Default)]
    struct CheckpointRecording {
        snapshots: usize,
        restores: Vec<String>,
        models: HashMap<String, theseus_modeling::Model>,
    }

    struct RecordingCheckpoint(Arc<Mutex<CheckpointRecording>>);

    #[async_trait::async_trait]
    impl crate::generated::Checkpoint for RecordingCheckpoint {
        async fn context(&self) -> anyhow::Result<crate::ProjectContext> {
            Ok(project())
        }

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
        async fn context(&self) -> anyhow::Result<crate::ProjectContext> {
            Ok(project())
        }

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
        async fn context(&self) -> anyhow::Result<crate::ProjectContext> {
            Ok(project())
        }

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
        let project = project();
        let ctx = crate::Ctx {
            model: &model,
            project: &project,
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
            project(),
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

    #[test]
    fn resumed_state_cannot_change_project_layout() {
        let state = crate::SessionState::new(project());
        let layout = theseus_modeling::RustWorkspaceLayout::new(
            theseus_modeling::ProjectId::new("alternate").unwrap(),
            theseus_modeling::ModelRecord::rust_builder(
                "rust/alternate/src/model.rs",
                "",
                "theseus_model",
            )
            .unwrap(),
        );
        let alternate =
            crate::ProjectContext::new(crate::workspace_root(), theseus_model(), layout).unwrap();
        let result = Session::from_state(
            alternate,
            state,
            &NoopWorkspace,
            &StubCheckpoint,
            &theseus_calculator::Calculator,
            &StubToolchain,
            false,
        );
        assert!(matches!(
            result,
            Err(crate::ProjectBindingError::LayoutMismatch { .. })
        ));
    }

    #[tokio::test]
    async fn the_session_sees_its_own_edit() {
        let mut session = Session::new(
            project(),
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
            project(),
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
            project(),
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
            project(),
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
            project_with_model(later),
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
            project(),
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
            project(),
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
                project(),
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
            project(),
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
            project(),
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
            project(),
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
            project(),
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
            project(),
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

    #[test]
    fn present_invalid_tool_booleans_are_rejected() {
        let patch =
            crate::generated::parse_patch_request_input(&serde_json::json!({ "write": "true" }))
                .expect_err("a string must not silently disable a durable patch");
        assert_eq!(
            patch.to_string(),
            "the `write` field is invalid: expected a boolean"
        );

        let edit = crate::generated::parse_rust_item_request_input(&serde_json::json!({
            "path": "rust/app/src/lib.rs",
            "revision": "revision",
            "item": "fn test() {}",
            "replace": "false"
        }))
        .expect_err("a string must not silently change insertion semantics");
        assert_eq!(
            edit.to_string(),
            "the `replace` field is invalid: expected a boolean"
        );
    }

    #[tokio::test]
    async fn a_drive_is_refused_without_the_gate() {
        let error = Session::new(
            project(),
            &NoopWorkspace,
            &StubCheckpoint,
            &theseus_calculator::Calculator,
            &StubToolchain,
            false,
        )
        .call("drive", &serde_json::json!({ "operation": "verify" }))
        .await
        .expect_err("driving an inbound is an effect the gate refuses");
        assert!(
            error.downcast_ref::<Refused>().is_some(),
            "the refusal should carry the typed gate error: {error}"
        );
    }

    #[tokio::test]
    async fn a_drive_of_an_unknown_operation_names_the_gap() {
        let error = Session::new(
            project(),
            &NoopWorkspace,
            &StubCheckpoint,
            &theseus_calculator::Calculator,
            &StubToolchain,
            true,
        )
        .call("drive", &serde_json::json!({ "operation": "ghost" }))
        .await
        .expect_err("an unknown operation cannot be projected");
        assert!(
            error.to_string().contains("no operation named `ghost`"),
            "{error}"
        );
    }

    #[tokio::test]
    async fn the_read_tool_reads_a_workspace_file() {
        let result = Session::new(
            project(),
            &NoopWorkspace,
            &StubCheckpoint,
            &theseus_calculator::Calculator,
            &StubToolchain,
            false,
        )
        .call("read", &serde_json::json!({ "path": "Cargo.toml" }))
        .await
        .expect("the read tool runs");
        let document: crate::SourceDocument =
            serde_json::from_str(&result).expect("read returns a structured source document");
        assert_eq!(document.path, "Cargo.toml");
        assert!(document.contents.contains("[workspace]"));
        assert!(!document.truncated);
        assert_eq!(
            document.revision,
            theseus_modeling::rust_source_revision(&document.contents)
        );
    }

    #[tokio::test]
    async fn the_read_tool_refuses_an_escape() {
        let error = Session::new(
            project(),
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
            error.downcast_ref::<crate::ProjectPathError>().is_some(),
            "{error}"
        );
    }

    #[tokio::test]
    async fn browse_tools_repair_wrong_path_kinds() {
        let mut session = Session::new(
            project(),
            &NoopWorkspace,
            &StubCheckpoint,
            &theseus_calculator::Calculator,
            &StubToolchain,
            false,
        );

        let read_error = session
            .call("read", &serde_json::json!({ "path": "rust" }))
            .await
            .expect_err("read refuses a directory");
        assert_eq!(
            read_error.to_string(),
            r#"path "rust" is a directory; call `list` with {"path":"rust"}"#
        );

        let list_error = session
            .call("list", &serde_json::json!({ "path": "Cargo.toml" }))
            .await
            .expect_err("list refuses a file");
        assert_eq!(
            list_error.to_string(),
            r#"path "Cargo.toml" is a file; call `read` with {"path":"Cargo.toml"}"#
        );
    }

    #[tokio::test]
    async fn the_rust_item_tool_commits_one_authorized_item() {
        let path = "rust/theseus/src/service.rs";
        let source = std::fs::read_to_string(crate::workspace_root().join(path)).unwrap();
        let recording = Arc::new(Mutex::new(MutationRecording::default()));
        let workspace = RecordingWorkspace(Arc::clone(&recording));
        let mut session = Session::new(
            project(),
            &workspace,
            &StubCheckpoint,
            &theseus_calculator::Calculator,
            &StubToolchain,
            true,
        );

        let response = session
            .call(
                "edit_rust_item",
                &serde_json::json!({
                    "path": path,
                    "revision": theseus_modeling::rust_source_revision(&source),
                    "item": "#[cfg(test)]\nfn governed_item_probe() {}",
                    "replace": false
                }),
            )
            .await
            .expect("the authorized Rust item is edited");
        let result: crate::RustItemResult = serde_json::from_str(&response).unwrap();
        assert!(result.applied, "{}", result.detail);
        assert_eq!(result.item, "fn:governed_item_probe");

        let recording = recording.lock().unwrap();
        let edited = recording
            .applied
            .iter()
            .find(|file| file.path == path)
            .and_then(crate::MutationFile::text_contents)
            .expect("the authorized file is in the atomic write set");
        assert!(edited.starts_with(&source));
        assert!(edited.contains("fn governed_item_probe() {}"));
        assert_eq!(
            result.revision,
            theseus_modeling::rust_source_revision(edited)
        );
        assert_eq!(recording.commits, 1);
        assert_eq!(recording.rollbacks, 0);
    }

    #[tokio::test]
    async fn the_rust_item_tool_rejects_stale_and_generated_sources() {
        let path = "rust/theseus/src/service.rs";
        let recording = Arc::new(Mutex::new(MutationRecording::default()));
        let workspace = RecordingWorkspace(Arc::clone(&recording));
        let mut session = Session::new(
            project(),
            &workspace,
            &StubCheckpoint,
            &theseus_calculator::Calculator,
            &StubToolchain,
            true,
        );

        let stale = session
            .call(
                "edit_rust_item",
                &serde_json::json!({
                    "path": path,
                    "revision": "0000000000000000",
                    "item": "fn never_written() {}",
                    "replace": false
                }),
            )
            .await
            .expect_err("a stale source observation is refused");
        assert!(matches!(
            stale.downcast_ref::<theseus_modeling::RustItemEditError>(),
            Some(theseus_modeling::RustItemEditError::StaleRevision { .. })
        ));

        let generated = session
            .call(
                "edit_rust_item",
                &serde_json::json!({
                    "path": "rust/theseus/src/generated.rs",
                    "revision": "irrelevant",
                    "item": "fn never_written() {}",
                    "replace": false
                }),
            )
            .await
            .expect_err("a generated projection is never authored");
        assert!(matches!(
            generated.downcast_ref::<theseus_modeling::ProjectLayoutError>(),
            Some(theseus_modeling::ProjectLayoutError::AuthoredRustPathNotAuthorized { .. })
        ));

        let recording = recording.lock().unwrap();
        assert!(recording.applied.is_empty());
        assert_eq!(recording.commits, 0);
        assert_eq!(recording.rollbacks, 0);
    }

    #[tokio::test]
    async fn the_rust_item_tool_rolls_back_a_failed_all_target_check() {
        let path = "rust/theseus/src/service.rs";
        let source = std::fs::read_to_string(crate::workspace_root().join(path)).unwrap();
        let recording = Arc::new(Mutex::new(MutationRecording::default()));
        let workspace = RecordingWorkspace(Arc::clone(&recording));
        let mut session = Session::new(
            project(),
            &workspace,
            &StubCheckpoint,
            &theseus_calculator::Calculator,
            &FailingToolchain,
            true,
        );

        let response = session
            .call(
                "edit_rust_item",
                &serde_json::json!({
                    "path": path,
                    "revision": theseus_modeling::rust_source_revision(&source),
                    "item": "fn rejected_item_probe() {}",
                    "replace": false
                }),
            )
            .await
            .expect("compile failure is a structured edit outcome");
        let result: crate::RustItemResult = serde_json::from_str(&response).unwrap();
        assert!(!result.applied);
        assert_eq!(
            result.revision,
            theseus_modeling::rust_source_revision(&source)
        );
        assert!(!result.check.ok);

        let recording = recording.lock().unwrap();
        assert_eq!(recording.commits, 0);
        assert_eq!(recording.rollbacks, 1);
    }

    #[tokio::test]
    async fn the_search_tool_reports_path_and_line() {
        let result = Session::new(
            project(),
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
            project(),
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
            project(),
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
            project(),
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
            project(),
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
            project(),
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
                project(),
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
/// Static skill-guidance catalog embedded in the binary.
///
/// Each topic is a short prose block that version-matches with the running
/// binary's model. The `model` topic omits a hard-coded operation list;
/// instead the handler renders it live from `self.model` so it can never
/// invent operations the binary lacks.
/// The harness diagnostic vocabulary: stable codes for the failure classes an
/// agent meets, each with the rule it names, the next action, and a safety
/// label for what a fix implies. The catalog is a queryable reference; the
/// tools that raise these conditions carry their own messages. Model edit
/// refusals carry a separate `PATCH0xx` family, returned inline by `patch`.
mod explain_catalog {
    /// One diagnostic code's entry.
    pub struct Entry {
        pub code: &'static str,
        pub message: &'static str,
        pub help: &'static str,
        /// What a fix implies: format-only, behavior-preserving,
        /// architecture-changing, or requires-human-review.
        pub safety: &'static str,
    }

    /// Every code, grouped by failure class: SRC (authored source), GATE (write
    /// and compile gates), VFY (conformance), CKP (checkpoints).
    pub const CODES: &[Entry] = &[
        Entry {
            code: "SRC001",
            message: "The revision passed to `edit_rust_item` no longer matches the file on disk.",
            help: "Re-`read` the file for its current revision, then reapply the edit.",
            safety: "behavior-preserving",
        },
        Entry {
            code: "SRC002",
            message: "The path is outside the project root or not owned by the active layout.",
            help: "Target a layout-owned authored file; `list` the root to see what is owned.",
            safety: "requires-human-review",
        },
        Entry {
            code: "GATE001",
            message: "A write was attempted without write permission.",
            help: "Rerun the session with `--allow-writes`; snapshot first if the change is risky.",
            safety: "requires-human-review",
        },
        Entry {
            code: "GATE002",
            message: "A gated write compiled with errors and was rolled back; the tree is unchanged.",
            help: "Read the compile detail in the result, fix the body, and reapply the same tool.",
            safety: "behavior-preserving",
        },
        Entry {
            code: "VFY001",
            message: "The workspace diverges from its model on one of `verify`'s checks.",
            help: "Read the named failing check and gap; `generate` for drift, or author the missing handler.",
            safety: "architecture-changing",
        },
        Entry {
            code: "CKP001",
            message: "The snapshot reference is not a checkpoint pinned in this session.",
            help: "Call `snapshot` in this session first and use the id it returns.",
            safety: "format-only",
        },
    ];

    /// The entry for a code, if the catalog defines it.
    pub fn get(name: &str) -> Option<&'static Entry> {
        CODES.iter().find(|entry| entry.code == name)
    }
}

mod skills_catalog {
    /// Ordered topic names, used for listing and for unknown-topic errors.
    pub const TOPICS: &[&str] = &["workflow", "model", "source", "diagnostics", "project"];

    pub const WORKFLOW: &str = "\
## workflow

The standard session loop:

1. **snapshot** — checkpoint before any risky change (`snapshot` returns an id; pin it).
2. **patch** (write=false) — validate edits against the model dry-run first.
3. **patch** (write=true) / **implement** / **edit_rust_item** — apply; each is gated.
4. **Gate trust** — gated tools already carry a compile verdict in their result.
   - After a successful gated write, do NOT call `check` just to confirm it.
   - Call `test` when behavior changed (new logic, not just scaffolding).
   - Call `verify` when the model changed (new operation, port, or type).
   - Call `check` only when no fresh gated verdict exists (e.g. after a manual edit
     or before `restart` on an otherwise un-gated tree).
5. **drive** — prove a new CLI operation live after `restart`.
6. **restart** — rebuild the binary; the agent loop resumes in the new process.

If the tree wedges and you cannot repair it, `rollback` to your snapshot and say so.
";

    pub const SOURCE: &str = "\
## source

Reading and editing workspace source files:

- **read** — returns file contents plus a `revision` token; always call `read` first.
- **show** — preferred over `read` for a modeled operation handler or adapter method;
  also returns the generated signature when no authored handler exists yet.
- **search** — grep across a subtree; returns `path:line: text`.
- **list** — directory listing; files are refused (use `read`).
- **edit_rust_item** — replace or insert one complete named top-level Rust item
  (fn, struct, impl, mod, const, …) in an existing authored `.rs` file.
  Pass the `revision` from `read`; a stale revision is refused.
  Cannot create files, edit manifests, or touch generated files.
- **implement** — splice a handler body into the service impl or a port adapter.
  Use it after `patch` adds an operation; read the signature with `show` first.

Item kinds accepted by `edit_rust_item`: fn, struct, enum, impl, mod, const, static,
type, trait, use — any named top-level Rust item.
";

    pub const DIAGNOSTICS: &str = "\
## diagnostics

Reading tool results and repair codes:

- **Compile gate rollback** — `implement` and `edit_rust_item` roll back on compile
  failure. The result carries the compiler output. Fix the body and retry; the tool
  replaces the method in place.
- **PATCH0xx** — model patch diagnostics. `PATCH002`: bad handle (re-`query`).
  `PATCH010`: unknown attribute. `PATCH012`: malformed shape.
- **Stale revision** — `edit_rust_item` refuses a stale `revision`; call `read` again
  and pass the fresh token.
- **Path errors** — `read` on a directory or `list` on a file each carry a repair
  hint showing the correct call.
- **verify gaps** — `verify` names each conformance gap. Fix the gap it names, then
  `verify` again. Do not `check` after a successful gated write just to confirm.
- **coverage** — lists operations with no authored handler; implement them in order.
";

    pub const PROJECT: &str = "\
## project

Workspace layout and project context:

- The repository root holds `rust/` (all crates), `docs/`, and the model record.
- Theseus enforces a project root policy: paths outside the root are refused.
- `list` with `{}` (empty path) shows the workspace root entries.
- **drive** — runs a CLI operation through the projected inbound, rebuilding first.
  Use it to prove a grown capability live. Requires write permission.
- **restart** — compile-gates the tree and signals the agent loop to replace the
  process. Call it alone (no other tool in the same turn). After restart, use
  `drive` to prove new operations end-to-end.
- Generated files (`generated.rs`) are read-only; edit the model with `patch` and
  run `generate` to refresh them.
- `scaffold` creates the skeleton of a missing library service crate. Call it after
  a `patch` that adds a new service, then `generate` to populate `generated.rs`.
";

    /// Return the body for the named topic, or `None` when not found.
    pub fn get(name: &str) -> Option<&'static str> {
        match name {
            "workflow" => Some(WORKFLOW),
            "source" => Some(SOURCE),
            "diagnostics" => Some(DIAGNOSTICS),
            "project" => Some(PROJECT),
            // "model" is rendered dynamically in the handler.
            _ => None,
        }
    }
}
