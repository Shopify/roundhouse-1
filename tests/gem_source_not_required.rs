//! A gem the app carries as source (`gem_sources:` in roundhouse.yml) is
//! compiled with the app, so runtime/gem_facades.rb must not require the
//! installed copy too. (Shopify Core's opentelemetry-shopify 1.22.0 was on
//! the guarded list; carried as a gem source, the native copy loaded
//! first and defined its constants `private_constant`, and the compiled
//! reopen's qualified reads of them raised NameError at boot.) Its own
//! dependencies do load from the gem home, as Bundler would load them.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

use roundhouse::project::BuildTarget;

const LOCK: &str = "PATH\n  remote: components/essentials\n  specs:\n    essentials (0.1.0)\n\nGEM\n  remote: https://rubygems.org/\n  specs:\n    opentelemetry-shopify (1.22.0)\n    rotp (6.3.0)\n\nDEPENDENCIES\n  opentelemetry-shopify\n  rotp\n";

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
    // rotp is on the guarded list (`project::GEM_REQUIRES`); carried as a
    // gem source, it is the compiled copy alone.
    let (dir, _) = app("gem_sources:\n  rotp: vendor/gems/rotp-6.3.0\n")
        .write("vendor/gems/rotp-6.3.0/lib/rotp.rb", "module ROTP\n  def self.on?\n    true\n  end\nend\n")
        .emit(BuildTarget::Ruby);
    let list = guarded_list(&dir);
    assert!(!list.contains("\"rotp\""), "{list}");
    // The rest of the list is untouched.
    assert!(list.contains("\"bcrypt\""), "{list}");
}

#[test]
fn without_gem_sources_the_gem_stays_on_the_list() {
    let (dir, _) = app("").emit(BuildTarget::Ruby);
    assert!(guarded_list(&dir).contains("\"rotp\""));
}

/// A gem source's own dependencies load from the gem home, as its entry
/// file's load-level requires (and those of the compiled files they
/// reach) would load them: opentelemetry-shopify 1.22.0's
/// `require 'opentelemetry/sdk'` and its siblings. A Rails feature is the
/// runtime's port and is named, not required; a file reached only by
/// `autoload`, or a require inside a method, is not load-time; a feature
/// compiled elsewhere (the app's lib/oracle.rb) is not the gem home's.
#[test]
fn a_gem_sources_load_time_dependencies_are_required_from_the_gem_home() {
    let (dir, _) = app("gem_sources:\n  opentelemetry-shopify: vendor/gems/opentelemetry-shopify-1.22.0\n")
        .write(
            "vendor/gems/opentelemetry-shopify-1.22.0/lib/opentelemetry-shopify.rb",
            "require 'opentelemetry/shopify'\nrequire 'opentelemetry/shopify/railtie' if defined?(Rails)\n",
        )
        .write(
            "vendor/gems/opentelemetry-shopify-1.22.0/lib/opentelemetry/shopify.rb",
            "require 'opentelemetry/sdk'\nrequire 'forwardable'\nrequire 'oracle'\nrequire 'essentials/uncarried'\nrequire 'opentelemetry/shopify/version'\n\nmodule OtelShim\n  require 'opentelemetry-common'\n  autoload :Lazy, 'opentelemetry/shopify/lazy'\n\n  def self.on?\n    require 'only/when/called'\n    true\n  end\nend\n",
        )
        .write("vendor/gems/opentelemetry-shopify-1.22.0/lib/opentelemetry/shopify/version.rb", "require_relative '../../deep/dep'\nmodule OtelShim\n  VERSION = '1'\nend\n")
        .write("vendor/gems/opentelemetry-shopify-1.22.0/lib/deep/dep.rb", "require 'json'\nmodule Deep\nend\n")
        .write("vendor/gems/opentelemetry-shopify-1.22.0/lib/opentelemetry/shopify/lazy.rb", "require 'stackprof'\nmodule OtelShim\n  module Lazy\n  end\nend\n")
        .write(
            "vendor/gems/opentelemetry-shopify-1.22.0/lib/opentelemetry/shopify/railtie.rb",
            "require 'active_support/parameter_filter'\nmodule OtelShim\n  class Railtie < Rails::Railtie\n  end\nend\n",
        )
        .emit(BuildTarget::Ruby);
    let facades = std::fs::read_to_string(dir.join("runtime/gem_facades.rb")).expect("gem_facades.rb");
    let block = facades
        .lines()
        .find(|l| l.starts_with("[\"opentelemetry/sdk\""))
        .unwrap_or_else(|| panic!("{facades}"));
    assert_eq!(block, r#"["opentelemetry/sdk", "forwardable", "opentelemetry-common", "json"].each do |feature|"#, "{facades}");
    assert!(facades.contains(r#"Their Rails features are this tree's runtime ports, not required: "active_support/parameter_filter"."#), "{facades}");
    assert!(facades.contains("error[gem_requires_rails]"), "{facades}");
    // Before the Bundler.require list, where Bundler would require the
    // gem source: Core's Gemfile loads panama_client "before anything
    // else that may require grpc", and its grpc_fork_safety raises once
    // GRPC is loaded (the list's opentelemetry-instrumentation-grpc does).
    assert!(facades.find(block).unwrap() < facades.find("require gem_name").unwrap(), "{facades}");
    // A path gem's file the tree does not carry compiled is named, not
    // loaded from the gem home; so is a gem source's (by the gem home's
    // spec that owns it), and a feature whose gem depends on Rails.
    assert!(facades.contains(r#"warn "roundhouse: warning[gem_source_file_not_compiled]: \"essentials/uncarried\""#), "{facades}");
    assert!(facades.contains("__rh_compiled_gems = [\"essentials\", \"opentelemetry-shopify\"].freeze"), "{facades}");
    assert!(facades.contains("Gem::Specification.find_by_path(feature)"), "{facades}");
    let check = emit_and_run::ruby().args(["-c", "runtime/gem_facades.rb"]).current_dir(&*dir).output().expect("ruby -c");
    assert!(check.status.success(), "{}", String::from_utf8_lossy(&check.stderr));
}

/// A gem source's dependencies load from the gem home, so a `GIT` gem in
/// its lock closure is activated as Bundler would (runtime/git_gems.rb):
/// panama_client 3.2.3 requires grpc_fork_safety, which depends on Core's
/// grpc fork, and RubyGems alone never sees that checkout.
#[test]
fn a_git_gem_in_a_gem_sources_closure_is_activated() {
    let lock = "GIT\n  remote: https://github.com/Shopify/grpc.git\n  revision: d5dd2a32be6bfd2e9abd7d10ccfb87d129e868de\n  specs:\n    grpc (1.85.0.dev)\n\nGEM\n  remote: https://rubygems.org/\n  specs:\n    grpc_fork_safety (0.1.0)\n      grpc (>= 1.40)\n    panama_client (3.2.3)\n      grpc_fork_safety\n\nDEPENDENCIES\n  grpc!\n  panama_client\n";
    let (dir, _) = emit_and_run::empty_app()
        .write("roundhouse.yml", "test_paths:\n  - test\ngem_sources:\n  panama_client: vendor/gems/panama_client-3.2.3\n")
        .write("Gemfile.lock", lock)
        .write("vendor/gems/panama_client-3.2.3/lib/panama_client.rb", "require 'grpc_fork_safety'\nmodule PanamaClient\nend\n")
        .write("lib/oracle.rb", "class Oracle\n  def ask\n    PanamaClient\n  end\nend\n")
        .emit(BuildTarget::Ruby);
    let git = std::fs::read_to_string(dir.join("runtime/git_gems.rb")).expect("git_gems.rb");
    assert!(git.contains(r#"["grpc", "grpc-d5dd2a32be6b", "https://github.com/Shopify/grpc.git""#), "{git}");
    let facades = std::fs::read_to_string(dir.join("runtime/gem_facades.rb")).expect("gem_facades.rb");
    assert!(facades.starts_with("require_relative \"git_gems\"\n"), "{facades}");
    assert!(facades.contains(r#"["grpc_fork_safety"].each do |feature|"#), "{facades}");
}

/// Core's Rails is the Shopify fork, a `GIT` source, so runtime/git_gems.rb
/// activates its checkout for the gems that depend on it — but Rails' own
/// files must not load over the runtime ports. hashie (a dependency of a
/// gem source) runs `require 'rails/railtie' if defined?(::Rails)`, rescuing
/// LoadError; with the checkout on $LOAD_PATH it loaded the real railties,
/// and actionpack-xml_parser then the real ActionDispatch, which raised
/// "Session is not a module" against the runtime's port.
#[test]
fn the_rails_git_checkout_activates_but_its_files_do_not_load() {
    let lock = "GIT\n  remote: https://github.com/Shopify/rails.git\n  revision: 5813559f4aedd8f37cb8af46fcb24f64ec3d6e7f\n  specs:\n    activesupport (8.2.0.alpha)\n    railties (8.2.0.alpha)\n      activesupport (= 8.2.0.alpha)\n\nGIT\n  remote: https://github.com/Shopify/grpc.git\n  revision: d5dd2a32be6bfd2e9abd7d10ccfb87d129e868de\n  specs:\n    grpc (1.85.0.dev)\n\nGEM\n  remote: https://rubygems.org/\n  specs:\n    panama_client (3.2.3)\n      grpc\n      railties\n\nDEPENDENCIES\n  activesupport!\n  grpc!\n  panama_client\n  railties!\n";
    let (dir, _) = emit_and_run::empty_app()
        .write("roundhouse.yml", "test_paths:\n  - test\ngem_sources:\n  panama_client: vendor/gems/panama_client-3.2.3\n")
        .write("Gemfile.lock", lock)
        .write("vendor/gems/panama_client-3.2.3/lib/panama_client.rb", "require 'grpc'\nmodule PanamaClient\nend\n")
        .write("lib/oracle.rb", "class Oracle\n  def ask\n    PanamaClient\n  end\nend\n")
        .emit(BuildTarget::Ruby);
    let git = std::fs::read_to_string(dir.join("runtime/git_gems.rb")).expect("git_gems.rb");
    assert!(git.contains(r#"["railties", "rails-5813559f4aed""#), "{git}");
    let facades = std::fs::read_to_string(dir.join("runtime/gem_facades.rb")).expect("gem_facades.rb");
    assert!(facades.contains("|/bundler/gems/rails-\\h+/}"), "the Rails-load check knows the checkout: {facades}");

    // The loader against a gem dir laid out as Bundler installs git gems.
    let gems = dir.join("tmp-gem-home");
    let spec = |name: &str, version: &str, deps: &str| {
        format!("Gem::Specification.new do |s|\n  s.name = {name:?}\n  s.version = {version:?}\n  s.summary = \"x\"\n  s.authors = [\"x\"]\n  s.require_paths = [\"lib\"]\n{deps}end\n")
    };
    let rails = gems.join("bundler/gems/rails-5813559f4aed");
    for (component, deps) in [("activesupport", ""), ("railties", "  s.add_dependency \"activesupport\", \"= 8.2.0.alpha\"\n")] {
        std::fs::create_dir_all(rails.join(component).join("lib")).unwrap();
        std::fs::write(rails.join(component).join(format!("{component}.gemspec")), spec(component, "8.2.0.alpha", deps)).unwrap();
    }
    std::fs::create_dir_all(rails.join("railties/lib/rails")).unwrap();
    std::fs::write(rails.join("railties/lib/rails/railtie.rb"), "module Rails; class Railtie; end; end\n").unwrap();
    std::fs::create_dir_all(rails.join("activesupport/lib")).unwrap();
    std::fs::write(rails.join("activesupport/lib/active_support.rb"), "module ActiveSupport; REAL = true; end\n").unwrap();
    let grpc = gems.join("bundler/gems/grpc-d5dd2a32be6b");
    std::fs::create_dir_all(grpc.join("lib")).unwrap();
    std::fs::write(grpc.join("grpc.gemspec"), spec("grpc", "1.85.0.dev", "")).unwrap();
    std::fs::write(grpc.join("lib/grpc.rb"), "module GRPC; FROM = __FILE__; end\n").unwrap();
    let script = "require_relative 'runtime/git_gems'\n\
        module Rails; end\n\
        begin\n  require 'rails/railtie' if defined?(::Rails)\n  puts 'railtie loaded'\nrescue LoadError\n  puts 'railtie skipped'\nend\n\
        begin\n  require 'active_support'\n  puts 'active_support loaded'\nrescue LoadError\n  puts 'active_support skipped'\nend\n\
        puts Gem.loaded_specs['railties'].version\n\
        require 'grpc'\nputs GRPC::FROM\n";
    let out = std::process::Command::new("ruby")
        .current_dir(&dir)
        .env("GEM_PATH", &gems)
        .env("GEM_HOME", &gems)
        .args(["-e", script])
        .output()
        .expect("ruby");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "{stdout}{}", String::from_utf8_lossy(&out.stderr));
    assert!(stdout.starts_with("railtie skipped\nactive_support skipped\n8.2.0.alpha\n"), "{stdout}");
    assert!(stdout.contains("bundler/gems/grpc-d5dd2a32be6b/lib/grpc.rb"), "{stdout}");
}
