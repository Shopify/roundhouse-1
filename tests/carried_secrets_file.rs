//! The one secrets-named file a tree carries: `config/secrets.test.json`,
//! when it is git-tracked in the source app (its work tree, or for a
//! slice the Core checkout its manifest names, with equal bytes). Core's
//! essentials `Secrets.from_env` reads it behind `File.exist?` at test
//! boot, so without it the boot silently runs on empty secrets. Every
//! other secrets-named file stays refused. Fixtures hold dummy values.
use std::path::{Path, PathBuf};
use std::process::Command;

use roundhouse::ingest::ingest_app;
use roundhouse::project::{target_files, BuildTarget};

const DUMMY: &str = "{\"dummy\": \"not-a-secret\"}\n";

fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("roundhouse-carried-secrets-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("mkdir");
    dir
}

fn write(root: &Path, rel: &str, content: &str) {
    let path = root.join(rel);
    std::fs::create_dir_all(path.parent().unwrap()).expect("mkdir");
    std::fs::write(path, content).expect("write");
}

fn git(dir: &Path, args: &[&str]) {
    let status = Command::new("git")
        .arg("-C").arg(dir)
        .args(["-c", "core.fsmonitor=false"])
        .args(args)
        .stdout(std::process::Stdio::null())
        .status()
        .expect("spawn git");
    assert!(status.success(), "git {args:?}");
}

fn app_files(root: &Path) {
    write(root, "lib/thing.rb", "class Thing\nend\n");
    write(root, "config/secrets.test.json", DUMMY);
    write(root, "config/secrets.development.json", DUMMY);
    write(root, "config/secrets.json", DUMMY);
}

fn carried(source: &Path) -> Vec<String> {
    let mut app = ingest_app(source).expect("ingest");
    roundhouse::session::analyze_and_lower(&mut app);
    let files = target_files(&app, source, BuildTarget::Ruby).expect("target files");
    let mut secrets: Vec<String> = files.into_iter().map(|(p, _)| p).filter(|p| p.contains("secrets")).collect();
    secrets.sort();
    secrets
}

#[test]
fn the_tracked_test_secrets_file_is_carried_and_no_other() {
    let root = scratch("tracked");
    app_files(&root);
    git(&root, &["init", "-q"]);
    git(&root, &["add", "lib/thing.rb", "config/secrets.test.json", "config/secrets.development.json"]);
    assert_eq!(carried(&root), vec!["config/secrets.test.json".to_string()]);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn an_untracked_test_secrets_file_is_refused() {
    let root = scratch("untracked");
    app_files(&root);
    git(&root, &["init", "-q"]);
    git(&root, &["add", "lib/thing.rb"]);
    assert!(carried(&root).is_empty());
    let _ = std::fs::remove_dir_all(&root);
}

/// A slice root is not a work tree; its manifest names the tracked Core
/// file, and the copy must be byte-equal to it.
#[test]
fn a_slice_carries_it_when_core_tracks_the_same_bytes() {
    let base = scratch("slice");
    let core = base.join("core");
    app_files(&core);
    git(&core, &["init", "-q"]);
    git(&core, &["add", "config/secrets.test.json"]);
    let root = base.join("slice/root");
    app_files(&root);
    let manifest = format!(
        "{{\"core_root\": {:?}, \"files\": [{{\"core_path\": \"config/secrets.test.json\", \"slice_path\": \"config/secrets.test.json\"}}]}}",
        core.to_string_lossy()
    );
    write(&base, "slice/slice-manifest.json", &manifest);
    assert_eq!(carried(&root), vec!["config/secrets.test.json".to_string()]);

    // An edited copy is not the tracked file.
    write(&root, "config/secrets.test.json", "{\"dummy\": \"edited\"}\n");
    assert!(carried(&root).is_empty());
    let _ = std::fs::remove_dir_all(&base);
}
