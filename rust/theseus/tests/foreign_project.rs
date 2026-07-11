use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
    sync::atomic::{AtomicU64, Ordering},
};

use theseus::{
    CargoToolchain, FsWorkspace, GitCheckpoint, ProjectContext, QueryRequest, ReadRequest,
    RustItemResult, Session, SourceDocument, StatefulSession, TheseusService as _,
};
use theseus_modeling::{Model, ModelRecord, ProjectId, RustWorkspaceLayout};

static NEXT_PROJECT: AtomicU64 = AtomicU64::new(0);

struct ForeignRepository {
    root: PathBuf,
}

impl ForeignRepository {
    fn journal_copy() -> Self {
        let repository_root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .ancestors()
            .nth(2)
            .expect("theseus lives at <root>/rust/theseus");
        let sequence = NEXT_PROJECT.fetch_add(1, Ordering::Relaxed);
        let root = repository_root.join("adopters").join(format!(
            ".journal-session-{}-{sequence}",
            std::process::id()
        ));
        copy_tree(&repository_root.join("adopters/journal"), &root);
        git(&root, &["init", "--quiet"]);
        git(&root, &["config", "user.name", "Theseus Test"]);
        git(&root, &["config", "user.email", "theseus@example.invalid"]);
        git(&root, &["add", "--", "."]);
        git(&root, &["commit", "--quiet", "-m", "initial"]);
        Self { root }
    }
}

impl Drop for ForeignRepository {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

#[tokio::test]
async fn a_session_develops_and_restores_a_foreign_project() {
    let repository_root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .unwrap();
    let source_record = repository_root.join("adopters/journal/model.json");
    let source_before = fs::read(&source_record).unwrap();
    let repository = ForeignRepository::journal_copy();

    let initial_source = fs::read_to_string(repository.root.join("model.json")).unwrap();
    let initial_model: Model = serde_json::from_str(&initial_source).unwrap();
    let layout = RustWorkspaceLayout::new(
        ProjectId::new("journal").unwrap(),
        ModelRecord::json("model.json").unwrap(),
    );
    let project = ProjectContext::new(&repository.root, initial_model, layout).unwrap();

    let restored_paths = [
        "Cargo.lock",
        "model.json",
        "rust/journal/src/generated.rs",
        "rust/journal/src/service.rs",
        "rust/cli/src/generated.rs",
    ];
    let before: Vec<_> = restored_paths
        .iter()
        .map(|path| fs::read(repository.root.join(path)).unwrap())
        .collect();
    fs::write(repository.root.join("unrelated.txt"), "keep me\n").unwrap();

    let workspace = FsWorkspace::for_project(&project);
    let checkpoint = GitCheckpoint::for_project(project.clone());
    let calculator = theseus_calculator::Calculator;
    let toolchain = CargoToolchain::for_project(&project);
    let mut session = Session::new(
        project.clone(),
        &workspace,
        &checkpoint,
        &calculator,
        &toolchain,
        true,
    );

    let service_path = repository.root.join("rust/journal/src/service.rs");
    let valid_service = fs::read(&service_path).unwrap();
    fs::write(&service_path, "this is not Rust\n").unwrap();
    let rooted_check: serde_json::Value =
        serde_json::from_str(&session.call("check", &serde_json::json!({})).await.unwrap())
            .unwrap();
    assert_eq!(rooted_check["ok"], false, "{rooted_check:#}");
    fs::write(&service_path, &valid_service).unwrap();

    let verified: serde_json::Value = serde_json::from_str(
        &session
            .call("verify", &serde_json::json!({}))
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(verified["conformant"], true, "{verified:#}");

    let snapshot = session
        .call(
            "snapshot",
            &serde_json::json!({ "label": "before journal count" }),
        )
        .await
        .unwrap();

    let patched: serde_json::Value = serde_json::from_str(
        &session
            .call(
                "patch",
                &serde_json::json!({
                    "edit": [{
                        "verb": "add",
                        "parent": "service:journal:Journal",
                        "kind": "operation",
                        "name": "count",
                        "attrs": {
                            "summary": "Count the journal entries.",
                            "request": "Empty",
                            "response": "String",
                            "uses": "store"
                        }
                    }],
                    "write": true
                }),
            )
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(patched["ok"], true, "{patched:#}");

    let coverage = session
        .call("coverage", &serde_json::json!({}))
        .await
        .unwrap();
    assert!(coverage.contains("count"), "{coverage}");

    let implemented: serde_json::Value = serde_json::from_str(
        &session
            .call(
                "implement",
                &serde_json::json!({
                    "method": "count",
                    "body": "let entries = self.store.read_all().await?; Ok(entries.lines().count().to_string())"
                }),
            )
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(implemented["applied"], true, "{implemented:#}");

    let authored: SourceDocument = serde_json::from_str(
        &session
            .call(
                "read",
                &serde_json::json!({ "path": "rust/journal/src/service.rs" }),
            )
            .await
            .unwrap(),
    )
    .unwrap();
    assert!(!authored.truncated);
    let rejected: RustItemResult = serde_json::from_str(
        &session
            .call(
                "edit_rust_item",
                &serde_json::json!({
                    "path": authored.path,
                    "revision": authored.revision,
                    "item": "#[cfg(test)]\nmod rejected_governed_test { #[test] fn does_not_compile() { missing_symbol(); } }",
                    "replace": false
                }),
            )
            .await
            .unwrap(),
    )
    .unwrap();
    assert!(!rejected.applied, "{}", rejected.detail);
    assert!(!rejected.check.ok);
    assert_eq!(
        fs::read_to_string(&service_path).unwrap(),
        authored.contents
    );

    let accepted: RustItemResult = serde_json::from_str(
        &session
            .call(
                "edit_rust_item",
                &serde_json::json!({
                    "path": authored.path,
                    "revision": rejected.revision,
                    "item": "#[cfg(test)]\nmod governed_test { #[test] fn journal_count_contract_is_executable() { assert_eq!(2 + 2, 4); } }",
                    "replace": false
                }),
            )
            .await
            .unwrap(),
    )
    .unwrap();
    assert!(accepted.applied, "{}", accepted.detail);
    assert_eq!(accepted.item, "mod:governed_test");

    for operation in ["check", "test"] {
        let report: serde_json::Value = serde_json::from_str(
            &session
                .call(operation, &serde_json::json!({}))
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(report["ok"], true, "{operation}: {report:#}");
    }
    let verified: serde_json::Value = serde_json::from_str(
        &session
            .call("verify", &serde_json::json!({}))
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(verified["conformant"], true, "{verified:#}");

    session
        .call("rollback", &serde_json::json!({ "reference": snapshot }))
        .await
        .unwrap();
    let error = session
        .call("query", &serde_json::json!({ "node": "op:journal:count" }))
        .await
        .expect_err("rollback restores the model without count");
    assert!(error.to_string().contains("no node with handle"), "{error}");

    for (path, expected) in restored_paths.iter().zip(before) {
        assert_eq!(fs::read(repository.root.join(path)).unwrap(), expected);
    }
    assert_eq!(
        fs::read_to_string(repository.root.join("unrelated.txt")).unwrap(),
        "keep me\n"
    );
    assert_eq!(fs::read(&source_record).unwrap(), source_before);

    let cold_model =
        serde_json::from_str(&fs::read_to_string(repository.root.join("model.json")).unwrap())
            .unwrap();
    let cold_project =
        ProjectContext::new(&repository.root, cold_model, project.layout().clone()).unwrap();
    let stateful = StatefulSession::new(
        cold_project.clone(),
        FsWorkspace::for_project(&cold_project),
        GitCheckpoint::for_project(cold_project.clone()),
        theseus_calculator::Calculator,
        CargoToolchain::for_project(&cold_project),
        false,
    );
    let model_record = stateful
        .read(ReadRequest {
            path: "model.json".to_string(),
        })
        .await
        .unwrap();
    assert!(model_record.contents.contains("Journal"));
    let operations = stateful
        .query(QueryRequest {
            find: Some("count".to_string()),
            node: None,
            kind: Some("operation".to_string()),
        })
        .await
        .unwrap();
    assert!(operations.handles.is_empty());
}

fn copy_tree(source: &Path, destination: &Path) {
    fs::create_dir_all(destination).unwrap();
    for entry in fs::read_dir(source).unwrap() {
        let entry = entry.unwrap();
        let name = entry.file_name();
        if matches!(name.to_str(), Some("target" | ".git" | ".theseus")) {
            continue;
        }
        let source_path = entry.path();
        let destination_path = destination.join(name);
        let file_type = entry.file_type().unwrap();
        if file_type.is_dir() {
            copy_tree(&source_path, &destination_path);
        } else if file_type.is_file() {
            fs::copy(&source_path, &destination_path).unwrap();
            fs::set_permissions(
                &destination_path,
                fs::metadata(&source_path).unwrap().permissions(),
            )
            .unwrap();
        } else {
            panic!(
                "unexpected non-file adopter entry: {}",
                source_path.display()
            );
        }
    }
}

fn git(root: &Path, args: &[&str]) {
    let output = Command::new("git")
        .args(args)
        .current_dir(root)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}
