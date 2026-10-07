//! A gem the app carries as source (`gem_sources:` in roundhouse.yml) is
//! compiled with the app, so runtime/gem_facades.rb must not require the
//! installed copy too. Shopify Core's opentelemetry-shopify 1.22.0 is on
//! the guarded list for the trees that load it natively; carried as a gem
//! source, the native copy loaded first and defined its constants
//! `private_constant`, and the compiled reopen's qualified reads of them
//! raised NameError at boot.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

use roundhouse::project::BuildTarget;

const LOCK: &str = "GEM\n  remote: https://rubygems.org/\n  specs:\n    opentelemetry-shopify (1.22.0)\n    rotp (6.3.0)\n\nDEPENDENCIES\n  opentelemetry-shopify\n  rotp\n";

fn app(gem_sources: &str) -> emit_and_run::Overlay {
    emit_and_run::empty_app()
        .write("roundhouse.yml", &format!("test_paths:\n  - test\n{gem_sources}"))
        .write("Gemfile.lock", LOCK)
        .write(
            "vendor/gems/opentelemetry-shopify-1.22.0/lib/opentelemetry-shopify.rb",
            "module OtelShim\n  def self.on?\n    true\n  end\nend\n",
        )
        .write("lib/oracle.rb", "class Oracle\n  def ask\n    OtelShim.on?\n  end\nend\n")
}

fn guarded_list(dir: &std::path::Path) -> String {
    let facades = std::fs::read_to_string(dir.join("runtime/gem_facades.rb")).expect("gem_facades.rb");
    facades.lines().find(|l| l.ends_with("].each do |gem_name|")).unwrap_or_else(|| panic!("{facades}")).to_string()
}

#[test]
fn a_gem_source_is_not_also_required_from_the_gem_home() {
    let (dir, _) = app("gem_sources:\n  opentelemetry-shopify: vendor/gems/opentelemetry-shopify-1.22.0\n")
        .emit(BuildTarget::Ruby);
    let list = guarded_list(&dir);
    assert!(!list.contains("\"opentelemetry-shopify\""), "{list}");
    // The rest of the list is untouched.
    assert!(list.contains("\"rotp\""), "{list}");
}

#[test]
fn without_gem_sources_the_gem_stays_on_the_list() {
    let (dir, _) = app("").emit(BuildTarget::Ruby);
    assert!(guarded_list(&dir).contains("\"opentelemetry-shopify\""));
}
