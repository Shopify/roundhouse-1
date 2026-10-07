//! `include` of a module only a gem's runtime port declares.
//!
//! elasticsearch-shopify 6.0.1's `ElasticSearch::Shopify::BaseError`
//! (compiled as app code; it requires semian, which needs Rails) does
//! `include ::Semian::AdapterError`. semian runs as its runtime port
//! (runtime/spinel/semian.rb); without Core's sorbet RBIs nothing else
//! names the module, so analysis refused the include as unresolved and
//! the class body raised `roundhouse: include not supported` at load.
//! The port declares it, so the include resolves; `project` refuses it
//! per tree where the finished tree lacks the port file.
//!
//! Expected output is native semian 0.28.3 (Core's pinned gem home,
//! jemalloc preloaded, its extension loaded) running the same class and
//! script:
//! `["[es] boom", "[es] boom", true, "x", "[es] again"]`.

use std::path::{Path, PathBuf};
use std::process::Command;

use roundhouse::project::BuildTarget;

const MANIFEST_DIR: &str = env!("CARGO_MANIFEST_DIR");

const BASE_ERROR: &str = "module SemianProbe\n  class BaseError < StandardError\n    include ::Semian::AdapterError\n  end\nend\n";

struct TempDir(PathBuf);

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn temp_dir(tag: &str) -> TempDir {
    let dir = std::env::temp_dir().join(format!(
        "rh-gem-port-include-{tag}-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    TempDir(dir)
}

fn copy_tree(from: &Path, to: &Path) {
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let dest = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            std::fs::create_dir_all(&dest).unwrap();
            copy_tree(&entry.path(), &dest);
        } else {
            std::fs::copy(entry.path(), &dest).unwrap();
        }
    }
}

/// tiny-blog plus the include, compiled for PostgreSQL or not.
fn app(postgres: bool) -> TempDir {
    let app = temp_dir(if postgres { "pg" } else { "plain" });
    copy_tree(&Path::new(MANIFEST_DIR).join("fixtures/tiny-blog"), &app.0);
    let write = |rel: &str, text: &str| {
        let path = app.0.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    };
    write("app/models/semian_probe/base_error.rb", BASE_ERROR);
    if postgres {
        write("app/models/application_record.rb", "class ApplicationRecord < ActiveRecord::Base\n  self.abstract_class = true\nend\n");
        write("roundhouse.yml", "database_adapter: postgresql\n");
        write("config/database.yml", "test:\n  adapter: postgresql\n  database: rh_e2e_gem_port_include\n");
    }
    app
}

/// The Ruby tree's files and the emit diagnostics' messages.
fn emit(app: &TempDir) -> (Vec<(String, String)>, Vec<String>) {
    let mut ingested = roundhouse::ingest::ingest_app(&app.0).expect("ingest fixture");
    let lowered = roundhouse::session::analyze_and_lower(&mut ingested);
    let (files, diags) = roundhouse::emit::diagnostics::scope(|| {
        roundhouse::project::target_files(&ingested, &Path::new(MANIFEST_DIR).join("fixtures/tiny-blog"), BuildTarget::Ruby)
    });
    let messages = roundhouse::analyze::diagnose(&ingested).into_iter().chain(lowered).chain(diags)
        .map(|d| d.message).collect();
    (files.expect("package the Ruby project"), messages)
}

fn base_error(files: &[(String, String)]) -> &str {
    files.iter().find(|(p, _)| p == "app/models/semian_probe/base_error.rb")
        .map(|(_, text)| text.as_str()).expect("emitted base_error.rb")
}

#[test]
fn a_port_declared_module_is_included_on_the_tree_that_carries_the_port() {
    let app = app(true);
    let (files, messages) = emit(&app);
    let semian: Vec<_> = messages.iter().filter(|m| m.contains("Semian")).collect();
    assert!(semian.is_empty(), "the include was refused: {semian:#?}");
    let text = base_error(&files);
    assert!(text.contains("include Semian::AdapterError") && !text.contains("roundhouse:"), "{text}");

    let out = temp_dir("pg-out");
    for (name, content) in &files {
        let path = out.0.join(name);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    }
    let run = Command::new("ruby")
        .arg("-e")
        .arg(concat!(
            "require \"./runtime/semian\"\n",
            "require \"./app/models/semian_probe/base_error\"\n",
            "e = SemianProbe::BaseError.new(\"boom\"); e.semian_identifier = :es\n",
            "f = SemianProbe::BaseError.new(\"[es] again\"); f.semian_identifier = :es\n",
            "p [e.to_s, e.message, SemianProbe::BaseError.ancestors.include?(Semian::AdapterError), SemianProbe::BaseError.new(\"x\").message, f.message]\n",
        ))
        .current_dir(&out.0)
        // The port's bulkheads need the gem's compiled extension; the
        // error module does not, and semian documents this switch.
        .env("SEMIAN_SEMAPHORES_DISABLED", "1")
        .output()
        .expect("ruby is on PATH");
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(run.status.success(), "{stdout}\n{}", String::from_utf8_lossy(&run.stderr));
    assert_eq!(stdout.trim(), r#"["[es] boom", "[es] boom", true, "x", "[es] again"]"#);
}

#[test]
fn a_tree_without_the_port_names_the_port_file() {
    let app = app(false);
    let (_, messages) = emit(&app);
    assert!(
        messages.iter().any(|m| m.contains("SemianProbe::BaseError includes Semian::AdapterError")
            && m.contains("runtime/semian.rb")
            && m.contains("only the PostgreSQL CRuby tree carries")),
        "{messages:#?}"
    );
    assert!(!messages.iter().any(|m| m.contains("includes unresolved")), "{messages:#?}");
}
