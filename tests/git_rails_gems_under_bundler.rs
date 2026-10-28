//! runtime/git_gems.rb takes Rails' own components off $LOAD_PATH after
//! activating them from their Git checkout: Rails' code cannot run over
//! the tree's runtime ports, which answer for Rails. Under `bundle exec`
//! (the README's test command) Bundler has activated them already and put
//! their lib directories on $LOAD_PATH itself, and the loader skipped an
//! activated gem before taking its paths off, so a native gem's `require
//! "active_support/duration"` loaded activesupport's file over the
//! runtime's ActiveSupport::Duration (NoMethodError `zero?` for nil, from
//! the port's `Duration.second`). An activated Rails component now comes
//! off $LOAD_PATH too.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

use roundhouse::project::BuildTarget;

const REVISION: &str = "0123456789abcdef0123456789abcdef01234567";

fn lock() -> String {
    format!(
        "GIT\n  remote: https://github.com/example/rails.git\n  revision: {REVISION}\n  specs:\n    activesupport (8.2.0.alpha)\n\nGEM\n  remote: https://rubygems.org/\n  specs:\n    sourcegem (1.0.0)\n      activesupport\n\nDEPENDENCIES\n  activesupport!\n  sourcegem\n"
    )
}

#[test]
fn a_rails_component_bundler_activated_comes_off_the_load_path() {
    let (dir, _) = emit_and_run::empty_app()
        .write("roundhouse.yml", "test_paths:\n  - test\ngem_sources:\n  sourcegem: vendor/gems/sourcegem-1.0.0\n")
        .write("Gemfile.lock", &lock())
        .write("vendor/gems/sourcegem-1.0.0/lib/sourcegem.rb", "module Sourcegem\nend\n")
        .write("app/models/oracle.rb", "class Oracle\n  def ask\n    Sourcegem\n  end\nend\n")
        .emit(BuildTarget::Ruby);
    let git_gems = std::fs::read_to_string(dir.join("runtime/git_gems.rb")).expect("git_gems.rb");
    assert!(git_gems.contains("[\"activesupport\", \"rails-0123456789ab\""), "{git_gems}");

    // A gem dir laid out as Bundler installs a git gem.
    let home = dir.join("tmp-gem-home");
    let component = home.join("bundler/gems/rails-0123456789ab/activesupport");
    std::fs::create_dir_all(component.join("lib/active_support")).unwrap();
    std::fs::write(
        component.join("activesupport.gemspec"),
        "Gem::Specification.new do |s|\n  s.name = \"activesupport\"\n  s.version = \"8.2.0.alpha\"\n  s.summary = \"x\"\n  s.authors = [\"x\"]\n  s.files = [\"lib/active_support/duration.rb\"]\n  s.require_paths = [\"lib\"]\nend\n",
    )
    .unwrap();
    std::fs::write(component.join("lib/active_support/duration.rb"), "raise \"the gem's own duration.rb\"\n").unwrap();
    let out = emit_and_run::ruby()
        .current_dir(&*dir)
        .env("GEM_PATH", &home)
        .env("GEM_HOME", &home)
        .args([
            "-e",
            // What `bundle exec` leaves: the spec activated, its lib on $LOAD_PATH.
            "spec = Gem::Specification.load(Dir[File.join(ENV['GEM_HOME'], 'bundler/gems/rails-0123456789ab/activesupport/activesupport.gemspec')].first)\n\
             spec.full_gem_path = File.dirname(spec.loaded_from)\n\
             spec.activate\n\
             puts !$LOAD_PATH.resolve_feature_path('active_support/duration').nil?\n\
             require_relative 'runtime/git_gems'\n\
             puts $LOAD_PATH.resolve_feature_path('active_support/duration').inspect\n\
             puts Gem.loaded_specs['activesupport'].version\n",
        ])
        .output()
        .expect("ruby");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "{stdout}{}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(stdout, "true\nnil\n8.2.0.alpha\n");
}
