//! A gem source's require of a feature a runtime port answers.
//!
//! Core's gem sources reach `require "globalid"` and `require "global_id"`
//! (the component gem's `trusted_id/global_identification.rb`). globalid
//! depends on activesupport, so the boot's gem-source dependency block
//! (`project::gem_source_dependency_block`) refused the feature at every
//! boot as `error[gem_requires_rails]: "global_id", which a gem source
//! requires, is not loaded: globalid depends on activesupport`. Every
//! CRuby tree already carries globalid's port (`OVERLAY_GEM_PORTS`,
//! runtime/global_id.rb, required by the overlay boot), so the feature is
//! the port's and the block has nothing to load for it. A feature of a
//! Rails-dependent gem no port answers (`tracking_number`) is still named.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

use std::path::{Path, PathBuf};

use roundhouse::project::BuildTarget;

const LOCK: &str = "GEM\n  remote: https://rubygems.org/\n  specs:\n    activesupport (8.0.2)\n    globalid (1.4.0)\n      activesupport\n    tracky (1.0.0)\n      activesupport\n    widget-kit (1.0.0)\n      globalid\n      tracky\n\nDEPENDENCIES\n  activesupport\n  widget-kit\n";

const KIT: &str = "require \"globalid\"\nrequire \"global_id\"\nrequire \"global_id/identification\"\nrequire \"tracky\"\n\nmodule WidgetKit\n  Locator = :kit\nend\n";

fn fake_gem_home() -> PathBuf {
    static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let home = std::env::temp_dir().join(format!(
        "rh_gem_source_overlay_port_{}_{}",
        std::process::id(),
        NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    let spec = |name: &str| format!("Gem::Specification.new do |s|\n  s.name = \"{name}\".freeze\n  s.require_paths = [\"lib\".freeze]\nend\n");
    for (path, content) in [
        ("specifications/globalid-1.4.0.gemspec", spec("globalid")),
        ("gems/globalid-1.4.0/lib/globalid.rb", "require \"global_id\"\n".to_string()),
        ("gems/globalid-1.4.0/lib/global_id.rb", "class GlobalID; end\n".to_string()),
        ("specifications/tracky-1.0.0.gemspec", spec("tracky")),
        ("gems/tracky-1.0.0/lib/tracky.rb", "module Tracky; end\n".to_string()),
    ] {
        let full = home.join(path);
        std::fs::create_dir_all(full.parent().unwrap()).unwrap();
        std::fs::write(full, content).unwrap();
    }
    home
}

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
fn a_gem_sources_require_of_a_ported_gems_feature_is_the_ports() {
    let home = fake_gem_home();
    let (dir, errors) = emit_and_run::empty_app()
        .write(
            "roundhouse.yml",
            &format!("test_paths:\n  - test\ngem_home: {}\ngem_sources:\n  widget-kit: vendor/gems/widget-kit-1.0.0\n", home.display()),
        )
        .write("Gemfile.lock", LOCK)
        .write("test/test_helper.rb", "require \"active_support/test_case\"\n")
        .write("vendor/gems/widget-kit-1.0.0/lib/widget-kit.rb", KIT)
        .emit(BuildTarget::Ruby);
    let _ = std::fs::remove_dir_all(&home);
    let block = boot_dependency_block(&dir).unwrap_or_else(|| panic!("a gem-source dependency block in the emitted boot: {errors:#?}"));
    // The one Rails-dependent feature no port answers is still loaded
    // through the block (which refuses it by name at boot); the port's
    // features are not in it.
    assert!(block.contains("[\"tracky\"].each do |feature|"), "{block}");
    for feature in ["globalid", "global_id"] {
        assert!(!block.contains(&format!("\"{feature}")), "{feature} is globalid's port, not the gem home's: {block}");
    }
}
