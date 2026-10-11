//! A constant read scoped by a value, not a constant path.
//!
//! job-iteration 1.11 iteration.rb `enum.is_a?(JobIteration.enumerator_builder::Wrapper)`:
//! the scope of `::Wrapper` is a method call. Ingest read only the constant
//! segments of the path and dropped the receiver, so the read was the
//! unresolved top-level `Wrapper` ("constant not supported"). Ruby
//! evaluates the receiver and looks the name up in the class or module it
//! answers (`rb_public_const_get_from`), which is its `const_get`.
//!
//! Expected output is native Ruby running the same module.

use std::path::{Path, PathBuf};
use std::process::Command;

use roundhouse::project::BuildTarget;

const MANIFEST_DIR: &str = env!("CARGO_MANIFEST_DIR");

const SOURCE: &str = r#"module DynScope
  def self.builder
    DynScope::Builder
  end

  def self.wrapper?(value)
    value.is_a?(builder::Wrapper)
  end

  def self.limit
    builder::Inner::LIMIT
  end

  def self.missing
    builder::Nope
  end
end
"#;

const BUILDER: &str = r#"module DynScope
  class Builder
    class Wrapper; end

    module Inner
      LIMIT = 3
    end
  end
end
"#;

struct TempDir(PathBuf);

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
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

fn emit() -> (TempDir, String, Vec<String>) {
    let dir = std::env::temp_dir().join(format!("rh-dynamic-constant-scope-{}-{:?}", std::process::id(), std::thread::current().id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    copy_tree(&Path::new(MANIFEST_DIR).join("fixtures/tiny-blog"), &dir);
    let path = dir.join("app/models/dyn_scope.rb");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, SOURCE).unwrap();
    std::fs::create_dir_all(dir.join("app/models/dyn_scope")).unwrap();
    std::fs::write(dir.join("app/models/dyn_scope/builder.rb"), BUILDER).unwrap();
    let mut ingested = roundhouse::ingest::ingest_app(&dir).expect("ingest fixture");
    let lowered = roundhouse::session::analyze_and_lower(&mut ingested);
    let (files, diags) = roundhouse::emit::diagnostics::scope(|| {
        roundhouse::project::target_files(&ingested, &Path::new(MANIFEST_DIR).join("fixtures/tiny-blog"), BuildTarget::Ruby)
    });
    let messages = roundhouse::analyze::diagnose(&ingested).into_iter().chain(lowered).chain(diags)
        .map(|d| d.message).collect();
    let files = files.expect("package the Ruby project");
    let out = dir.join("out");
    for (name, content) in &files {
        let path = out.join(name);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    }
    let text = files.into_iter().find(|(p, _)| p == "app/models/dyn_scope.rb").map(|(_, t)| t).expect("emitted dyn_scope.rb");
    (TempDir(dir), text, messages)
}

#[test]
fn a_constant_scoped_by_a_call_reads_through_the_value() {
    let (dir, text, messages) = emit();
    let refused: Vec<_> = messages.iter().filter(|m| m.contains("DynScope")).collect();
    assert!(refused.is_empty(), "the read was refused: {refused:#?}\n{messages:#?}\n{text}");
    assert!(!text.contains("roundhouse:"), "{text}");

    let run = Command::new("ruby")
        .arg("-e")
        .arg(concat!(
            "require \"./app/models/dyn_scope/builder\"\nrequire \"./app/models/dyn_scope\"\n",
            "w = DynScope::Builder::Wrapper.new\n",
            "m = begin; DynScope.missing; rescue NameError => e; e.class; end\n",
            "p [DynScope.wrapper?(w), DynScope.wrapper?(Object.new), DynScope.limit, m]\n",
        ))
        .current_dir(dir.0.join("out"))
        .output()
        .expect("ruby is on PATH");
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(run.status.success(), "{stdout}\n{}\n{text}", String::from_utf8_lossy(&run.stderr));
    assert_eq!(stdout.trim(), "[true, false, 3, NameError]");
}
