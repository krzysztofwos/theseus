use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
    sync::atomic::{AtomicU64, Ordering},
};

use theseus::{
    CargoToolchain, FsWorkspace, GitCheckpoint, ProjectContext, RustItemResult, Session,
    SourceDocument, initialize_project,
};
use theseus_modeling::ProjectId;

static NEXT_PROJECT: AtomicU64 = AtomicU64::new(0);

struct EmptyRepository {
    root: PathBuf,
}

impl EmptyRepository {
    fn new() -> Self {
        let sequence = NEXT_PROJECT.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "theseus-empty-project-{}-{sequence}",
            std::process::id()
        ));
        fs::create_dir(&root).unwrap();
        git(&root, &["init", "--quiet"]);
        Self { root }
    }
}

impl Drop for EmptyRepository {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

#[tokio::test]
async fn an_empty_project_becomes_a_working_and_recoverable_service() {
    let repository = EmptyRepository::new();
    let modeling = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("theseus and modeling are workspace siblings")
        .join("modeling");
    let project = initialize_project(
        &repository.root,
        ProjectId::new("cold-app").unwrap(),
        modeling,
    )
    .await
    .unwrap();
    assert!(!git_succeeds(
        &repository.root,
        &["rev-parse", "--verify", "HEAD"]
    ));

    let restored_paths = project.owned_paths(project.initial_model()).unwrap();
    let before: Vec<_> = restored_paths
        .iter()
        .map(|path| read_optional(&repository.root.join(path)))
        .collect();

    let workspace = FsWorkspace::for_project(&project);
    let checkpoint = GitCheckpoint::for_project(project.clone());
    let calculator = theseus_calculator::Calculator;
    let toolchain = CargoToolchain::for_project(&project);
    let mut session = Session::new(
        project,
        &workspace,
        &checkpoint,
        &calculator,
        &toolchain,
        true,
    );
    let snapshot = session
        .call(
            "snapshot",
            &serde_json::json!({ "label": "initialized seed" }),
        )
        .await
        .unwrap();
    assert!(!git_succeeds(
        &repository.root,
        &["rev-parse", "--verify", "HEAD"]
    ));
    let snapshot_commit = git_output(&repository.root, &["cat-file", "commit", &snapshot]);
    let (headers, _) = snapshot_commit
        .split_once("\n\n")
        .expect("the snapshot commit has a manifest body");
    assert!(
        !headers.lines().any(|line| line.starts_with("parent ")),
        "an unborn repository must produce a root snapshot commit: {headers}"
    );

    let patched: serde_json::Value = serde_json::from_str(
        &session
            .call(
                "patch",
                &serde_json::json!({
                    "edit": [{
                        "verb": "add",
                        "parent": "service:cold-app:App",
                        "kind": "operation",
                        "name": "hello",
                        "attrs": {
                            "summary": "Return the initialized application greeting.",
                            "request": "Empty",
                            "response": "String"
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

    let implemented: serde_json::Value = serde_json::from_str(
        &session
            .call(
                "implement",
                &serde_json::json!({
                    "method": "hello",
                    "body": "Ok(\"hello from initialized project\".to_string())"
                }),
            )
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(implemented["applied"], true, "{implemented:#}");
    assert_eq!(implemented["check"]["ok"], true, "{implemented:#}");
    assert!(
        !implemented["check"]["detail"]
            .as_str()
            .is_some_and(|detail| detail.contains("with warnings")),
        "the argument-free generated CLI must be warning-free: {implemented:#}"
    );

    let service: SourceDocument = serde_json::from_str(
        &session
            .call(
                "read",
                &serde_json::json!({ "path": "rust/app/src/service.rs" }),
            )
            .await
            .unwrap(),
    )
    .unwrap();
    assert!(
        service
            .contents
            .contains("\n        Ok(\"hello from initialized project\".to_string())\n"),
        "the governed handler body is not indented:\n{}",
        service.contents
    );
    let tested_handler: RustItemResult = serde_json::from_str(
        &session
            .call(
                "edit_rust_item",
                &serde_json::json!({
                    "path": service.path,
                    "revision": service.revision,
                    "replace": false,
                    "item": r#"#[cfg(test)]
mod tests {
    use crate::{AppService, MemoryStore, Standalone};

    #[tokio::test(flavor = "current_thread")]
    async fn hello_returns_the_initialized_greeting() {
        let app = Standalone {
            model: crate::load_model().expect("the initialized model loads"),
            store: MemoryStore,
        };

        assert_eq!(
            app.hello().await.expect("the hello operation succeeds"),
            "hello from initialized project"
        );
    }
}"#
                }),
            )
            .await
            .unwrap(),
    )
    .unwrap();
    assert!(tested_handler.applied, "{}", tested_handler.detail);
    assert_eq!(tested_handler.item, "mod:tests");

    let main: SourceDocument = serde_json::from_str(
        &session
            .call(
                "read",
                &serde_json::json!({ "path": "rust/app-cli/src/main.rs" }),
            )
            .await
            .unwrap(),
    )
    .unwrap();
    assert!(!main.truncated);
    let edited: RustItemResult = serde_json::from_str(
        &session
            .call(
                "edit_rust_item",
                &serde_json::json!({
                    "path": main.path,
                    "revision": main.revision,
                    "replace": true,
                    "item": r#"#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow::Result<()> {
    let model = app::load_model()?;
    let app = app::Standalone {
        model,
        store: app::MemoryStore,
    };
    let matches = generated::command().get_matches();
    generated::dispatch(&app, generated::Invocation::from_matches(&matches)?).await
}"#
                }),
            )
            .await
            .unwrap(),
    )
    .unwrap();
    assert!(edited.applied, "{}", edited.detail);
    assert_eq!(edited.item, "fn:main");

    let tested: serde_json::Value =
        serde_json::from_str(&session.call("test", &serde_json::json!({})).await.unwrap()).unwrap();
    assert_eq!(tested["ok"], true, "{tested:#}");
    let verified: serde_json::Value = serde_json::from_str(
        &session
            .call("verify", &serde_json::json!({}))
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(verified["conformant"], true, "{verified:#}");
    let output = Command::new("cargo")
        .args(["run", "--quiet", "-p", "app-cli", "--", "hello"])
        .current_dir(&repository.root)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "initialized CLI failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8(output.stdout).unwrap().trim(),
        "hello from initialized project"
    );

    fs::write(repository.root.join("Cargo.toml"), "not a Cargo manifest\n").unwrap();
    fs::write(repository.root.join("theseus.json"), "{}\n").unwrap();

    session
        .call("rollback", &serde_json::json!({ "reference": snapshot }))
        .await
        .unwrap();
    for (path, expected) in restored_paths.iter().zip(before) {
        assert_eq!(
            read_optional(&repository.root.join(path)),
            expected,
            "rollback did not restore owned path {path}"
        );
    }
    let cold = ProjectContext::open(&repository.root).unwrap();
    assert!(cold.initial_model().operation("hello").is_none());
    assert!(!git_succeeds(
        &repository.root,
        &["rev-parse", "--verify", "HEAD"]
    ));
}

fn read_optional(path: &Path) -> Option<Vec<u8>> {
    match fs::symlink_metadata(path) {
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => None,
        Err(source) => panic!("inspecting {}: {source}", path.display()),
        Ok(_) => Some(
            fs::read(path).unwrap_or_else(|source| panic!("reading {}: {source}", path.display())),
        ),
    }
}

fn git(root: &Path, args: &[&str]) {
    let output = git_command(root, args);
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn git_output(root: &Path, args: &[&str]) -> String {
    let output = git_command(root, args);
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).expect("Git output is UTF-8")
}

fn git_succeeds(root: &Path, args: &[&str]) -> bool {
    git_command(root, args).status.success()
}

fn git_command(root: &Path, args: &[&str]) -> std::process::Output {
    Command::new("git")
        .args(args)
        .current_dir(root)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .output()
        .unwrap()
}
