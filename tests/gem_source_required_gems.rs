//! A constant a gem source (`gem_sources:`, compiled with the app) reads
//! from a gem it requires from the gem home.
//!
//! elasticsearch-shopify 6.0.1 is carried as source (it requires semian,
//! which needs Rails); its lib/elastic_search/shopify.rb does `require
//! "elastic/transport"`, and errors.rb lists
//! `Elastic::Transport::Transport::Errors::*` in a constant at load. The
//! boot requires that feature from the gem home
//! (`project::gem_source_dependency_block`), but elastic-transport is in
//! no Bundler.require closure, so its installed source was never read and
//! every one of those reads was refused as `constant not supported`: the
//! emitted errors.rb raised at load. The compiler now also reads the gems
//! that own the features a gem source requires, among the lock
//! dependencies of the carried gems. A dependency the gem source does not
//! require stays unread, so its constants stay refused.
//!
//! The walk starts at the gem's entry file, which for a hyphenated name
//! with no `lib/<name>.rb` is the one Bundler.require falls back to:
//! elasticsearch-shopify has only lib/elasticsearch/shopify.rb, so its
//! requires (and the boot's gem-home requires for them) were never seen.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

use std::path::{Path, PathBuf};

use roundhouse::project::BuildTarget;

const LOCK: &str = "GEM\n  remote: https://rubygems.org/\n  specs:\n    activesupport (8.0.2)\n    idle (1.0.0)\n    railsy (1.0.0)\n      activesupport\n    transporty (1.0.0)\n    widget-kit (1.0.0)\n      idle\n      railsy\n      transporty\n\nDEPENDENCIES\n  activesupport\n  widget-kit\n";

const WIDGET: &str = "require \"transporty\"\nrequire \"railsy\"\n\nmodule Widget\n  ERRORS = [Transporty::Error, Transporty::Errors::Gone]\nend\n";

fn fake_gem_home() -> PathBuf {
    static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let home = std::env::temp_dir().join(format!(
        "rh_gem_source_requires_{}_{}",
        std::process::id(),
        NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    let spec = |name: &str| format!("Gem::Specification.new do |s|\n  s.name = \"{name}\".freeze\n  s.require_paths = [\"lib\".freeze]\nend\n");
    let files: Vec<(String, String)> = vec![
        ("specifications/transporty-1.0.0.gemspec".into(), spec("transporty")),
        (
            "gems/transporty-1.0.0/lib/transporty.rb".into(),
            "module Transporty\n  class Error < StandardError; end\n  module Errors\n    class Gone < Error; end\n  end\nend\n".into(),
        ),
        ("specifications/idle-1.0.0.gemspec".into(), spec("idle")),
        ("gems/idle-1.0.0/lib/idle.rb".into(), "module Idle\n  class Thing; end\nend\n".into()),
        // Required, but its closure reaches activesupport: the boot
        // refuses the feature, so its constants are not the tree's.
        ("specifications/railsy-1.0.0.gemspec".into(), spec("railsy")),
        ("gems/railsy-1.0.0/lib/railsy.rb".into(), "module Railsy\n  class Hook; end\nend\n".into()),
    ];
    for (path, content) in files {
        let full = home.join(path);
        std::fs::create_dir_all(full.parent().unwrap()).unwrap();
        std::fs::write(full, content).unwrap();
    }
    home
}

fn app(home: &Path, widget: &str) -> emit_and_run::Overlay {
    emit_and_run::empty_app()
        .write(
            "roundhouse.yml",
            &format!("test_paths:\n  - test\ngem_home: {}\ngem_sources:\n  widget-kit: vendor/gems/widget-kit-1.0.0\n", home.display()),
        )
        .write("Gemfile.lock", LOCK)
        .write("test/test_helper.rb", "require \"active_support/test_case\"\n")
        // Hyphenated like elasticsearch-shopify, whose entry is the file
        // Bundler.require falls back to: lib/elasticsearch/shopify.rb.
        .write("vendor/gems/widget-kit-1.0.0/lib/widget/kit.rb", "require \"widget\"\n")
        .write("vendor/gems/widget-kit-1.0.0/lib/widget.rb", widget)
}

fn constant_errors(errors: &[String]) -> Vec<&String> {
    errors.iter().filter(|e| e.contains("constant not supported") || e.contains("no indexed declaration")).collect()
}

#[test]
fn a_required_gems_source_grounds_the_gem_sources_constants() {
    let home = fake_gem_home();
    let (dir, errors) = app(&home, WIDGET).emit(BuildTarget::Ruby);
    let _ = std::fs::remove_dir_all(&home);
    assert!(constant_errors(&errors).is_empty(), "{errors:#?}");
    let widget = std::fs::read_to_string(dir.join("app/models/widget.rb")).expect("widget.rb");
    assert!(widget.contains("ERRORS = [Transporty::Error, Transporty::Errors::Gone]"), "{widget}");
    assert!(!widget.contains("roundhouse:"), "{widget}");
    // The boot requires the feature from the gem home, so the constants
    // exist where the emitted file reads them.
    let block = boot_dependency_block(&dir).expect("a gem-source dependency block in the emitted boot");
    assert!(block.contains("[\"transporty\", \"railsy\"].each do |feature|"), "{block}");
}

/// The emitted file carrying `project::gem_source_dependency_block`.
fn boot_dependency_block(dir: &Path) -> Option<String> {
    let mut stack = vec![dir.to_path_buf()];
    while let Some(next) = stack.pop() {
        for entry in std::fs::read_dir(&next).ok()?.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|e| e == "rb") {
                let text = std::fs::read_to_string(&path).unwrap_or_default();
                if text.contains("__rh_compiled_gems") {
                    return Some(text);
                }
            }
        }
    }
    None
}

#[test]
fn an_unrequired_or_rails_reaching_dependency_stays_refused() {
    for (constant, gem) in [("Idle::Thing", "idle"), ("Railsy::Hook", "railsy")] {
        let home = fake_gem_home();
        let source = WIDGET.replace("Transporty::Errors::Gone", constant);
        let (_, errors) = app(&home, &source).emit(BuildTarget::Ruby);
        let _ = std::fs::remove_dir_all(&home);
        let refused = constant_errors(&errors);
        assert!(refused.iter().any(|e| e.contains(constant)), "{gem}: {errors:#?}");
        assert!(!refused.iter().any(|e| e.contains("Transporty")), "{gem}: {errors:#?}");
    }
}
