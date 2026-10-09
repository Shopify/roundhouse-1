//! A module that nests a module of its own name: analysis writes a bare
//! constant as its full resolved name, and inside the outer module that
//! name's head reads the INNER module first.
//!
//! Shopify Core's `lib/counters.rb`:
//!
//! ```ruby
//! module Counters
//!   module Counters ... end
//!   class LiveCounters ... end
//!   def self.live = LiveCounters.new
//! end
//! ```
//!
//! `LiveCounters` resolves to `Counters::LiveCounters`, and emitted so
//! inside `module Counters` Ruby looks `Counters` up in the class itself
//! first, finds the nested module, and raises
//! `uninitialized constant Counters::Counters::LiveCounters`. The emit
//! roots it (`::Counters::LiveCounters`).

use roundhouse::det::HashMap;
use std::path::PathBuf;
use std::process::Command;

use roundhouse::emit::ruby;
use roundhouse::ingest::ingest_app_from_tree;

const A: &str = "module A\n  module A\n    def self.inner = :inner\n  end\n\n  \
                 class L\n    def tag = :l\n  end\n\n  LIST = [L].freeze\n\n  \
                 def self.f = L.new\n\n  class << self\n    def g\n      L.new\n    end\n  end\nend\n";

const B: &str = "module B\n  class M\n    def tag = :m\n  end\n\n  def self.f = M.new\nend\n";

fn emit() -> Vec<(String, String)> {
    let files: Vec<(&str, &str)> = vec![
        ("db/schema.rb", "ActiveRecord::Schema.define do\nend\n"),
        ("config/routes.rb", "Rails.application.routes.draw do\nend\n"),
        ("app/models/a.rb", A),
        ("app/models/b.rb", B),
    ];
    let tree: HashMap<PathBuf, Vec<u8>> = files
        .into_iter()
        .map(|(p, c)| (PathBuf::from(p), c.as_bytes().to_vec()))
        .collect();
    let mut app = ingest_app_from_tree(tree).expect("ingest");
    let _ = roundhouse::session::analyze_and_lower(&mut app);
    ruby::emit_lowered_models(&app)
        .into_iter()
        .chain(ruby::emit_library(&app))
        .map(|f| (f.path.display().to_string(), f.content))
        .collect()
}

fn file<'a>(files: &'a [(String, String)], path: &str) -> &'a str {
    files
        .iter()
        .find(|(p, _)| p == path)
        .map(|(_, c)| c.as_str())
        .unwrap_or_else(|| panic!("no `{path}` in {:?}", files.iter().map(|(p, _)| p).collect::<Vec<_>>()))
}

#[test]
fn a_resolved_reference_under_a_self_named_module_is_rooted() {
    let files = emit();
    let a = file(&files, "app/models/a.rb");
    assert!(a.contains("::A::L.new"), "`A::L` inside `module A` reads `A::A::L`:\n{a}");
    assert!(!a.contains(" A::L.new"), "{a}");
    let b = file(&files, "app/models/b.rb");
    assert!(!b.contains("::B::M"), "no shadow under B, nothing to root:\n{b}");
}

/// The emitted modules answer what native Ruby does for the source.
#[test]
fn the_rooted_module_runs() {
    let files = emit();
    let dir = std::env::temp_dir().join(format!("rh-self-named-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    for (path, content) in files.iter().filter(|(p, _)| p.starts_with("app/models/") && p.ends_with(".rb")) {
        let target = dir.join(path);
        std::fs::create_dir_all(target.parent().unwrap()).unwrap();
        std::fs::write(&target, content).unwrap();
    }
    // The full tree autoloads; here every emitted model file is required,
    // outermost first.
    let mut paths: Vec<&str> = files
        .iter()
        .map(|(p, _)| p.as_str())
        .filter(|p| p.starts_with("app/models/") && p.ends_with(".rb"))
        .collect();
    paths.sort_by_key(|p| (p.matches('/').count(), p.to_string()));
    let mut script = String::new();
    for path in paths {
        script.push_str(&format!("require {:?}\n", dir.join(path).display().to_string()));
    }
    script.push_str("p A.f.tag, A.g.tag, A::A.inner, A::LIST.first.new.tag, B.f.tag\n");
    let out = Command::new("ruby").arg("-e").arg(&script).output().expect("ruby is on PATH");
    let _ = std::fs::remove_dir_all(&dir);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{stdout}\n{stderr}\n{script}");
    // Native Ruby on A and B as written above.
    assert_eq!(stdout, ":l\n:l\n:inner\n:l\n:m\n", "{stderr}");
}
