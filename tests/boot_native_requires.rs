// config/boot.rb's requires of gem-home features run first on the
// PostgreSQL CRuby tree, as native boot makes them before `require
// "rails"` and Bundler.require (App::boot_native_requires,
// project.rs::boot_native_requires_text).
//
// Core's boot died in gems/ci/lib/ci.rb:5 and
// components/platform/essentials/lib/ci_environment.rb:4 (`require
// "shopify/types/setup"`, file_statement_not_run: "Set the default
// checked level earlier"): the tree never ran config/boot.rb:85's
// require, so a gem that evaluated a sig as it loaded read sorbet's
// default level before Shopify::Types::Configuration.configure! set it.
//
// The probe: config/boot.rb requires shopify/types/setup (shopify-types
// 0.10.11 in Core's pinned gem home), and two gems load in Gemfile
// order: the gem source `sigkit`, whose native dependency `sigged_native`
// evaluates a sig at load, then the path gem `cikit`, gems/ci's shape.
// tests/boot_native_requires.rb runs it natively and on the tree.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

use roundhouse::project::BuildTarget;
use std::path::Path;

fn root() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

const LOCK: &str = "PATH\n  remote: gems/cikit\n  specs:\n    cikit (0.1.0)\n\nGEM\n  remote: https://rubygems.org/\n  specs:\n    sigkit (0.1.0)\n\n\
DEPENDENCIES\n  cikit!\n  sigkit\n";

const SIGGED_NATIVE: &str = r#"require "sorbet-runtime"

module SiggedNative
  extend T::Sig

  sig { params(x: Integer).returns(Integer) }
  def self.double(x) = x * 2

  DOUBLED = double(21)
end
"#;

const SIGKIT: &str = "require \"sigged_native\"\n\nmodule Sigkit\nend\n";

const CIKIT: &str = r#"# Configure Sorbet before any signatures are evaluated.
require "shopify/types/setup"

module Cikit
  def self.loaded = true
end
"#;

const CIKIT_GEMSPEC: &str = "Gem::Specification.new do |s|\n  s.name = \"cikit\"\n  s.version = \"0.1.0\"\n  s.summary = \"fixture\"\n  s.authors = [\"x\"]\n  s.files = [\"lib/cikit.rb\"]\nend\n";

const BOOT: &str = "ENV[\"BUNDLE_GEMFILE\"] ||= File.expand_path(\"../Gemfile\", __dir__)\n\
require \"bundler/setup\"\n\
require \"bootsnap/setup\" if ENV[\"BOOTSNAP\"]\n\
require \"shopify/types/setup\"\n";

fn run(mode: &str, dir: &Path, rest: &[&Path]) -> String {
    let mut cmd = emit_and_run::ruby();
    cmd.arg(root().join("tests/boot_native_requires.rb")).arg("run").arg(mode).arg(dir).args(rest);
    let out = cmd.output().expect("ruby");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(out.status.success(), "{mode} failed\n=== stdout ===\n{stdout}\n=== stderr ===\n{}", String::from_utf8_lossy(&out.stderr));
    stdout
}

#[test]
fn config_boots_gem_requires_run_before_any_gem_evaluates_a_sig() {
    let (tree, errors) = emit_and_run::empty_app()
        .write("roundhouse.yml", "test_paths:\n  - test\ngem_sources:\n  sigkit: vendor/gems/sigkit-0.1.0\ndatabase_adapter: postgresql\n")
        .write("Gemfile", "source \"https://rubygems.org\"\ngem \"rails\"\ngem \"sigkit\"\ngem \"cikit\", path: \"gems/cikit\"\n")
        .write("Gemfile.lock", LOCK)
        .write("config.ru", "")
        .write("config/boot.rb", BOOT)
        .write("config/application.rb", "require_relative \"boot\"\nrequire \"rails\"\nBundler.require(*Rails.groups)\n")
        .write("config/environments/test.rb", "Rails.application.configure do\n  config.eager_load = false\nend\n")
        .write("config/database.yml", "test:\n  adapter: postgresql\n  database: rh_boot_native_requires_unused\n")
        .write("db/schema.rb", "ActiveRecord::Schema.define do\n  create_table \"probes\" do |t|\n    t.string \"title\"\n  end\nend\n")
        .write("app/models/application_record.rb", "class ApplicationRecord < ActiveRecord::Base\n  self.abstract_class = true\nend\n")
        .write("app/models/probe.rb", "class Probe < ApplicationRecord\nend\n")
        .write("vendor/gems/sigkit-0.1.0/lib/sigkit.rb", SIGKIT)
        .write("gems/cikit/lib/cikit.rb", CIKIT)
        .write("gems/cikit/cikit.gemspec", CIKIT_GEMSPEC)
        .emit(BuildTarget::Ruby);
    let scratch = tree.parent().expect("scratch").to_path_buf();
    let native_lib = scratch.join("native_lib");
    std::fs::create_dir_all(&native_lib).unwrap();
    std::fs::write(native_lib.join("sigged_native.rb"), SIGGED_NATIVE).unwrap();
    let gems = scratch.join("gems");
    for (gem, text) in [("sigkit", SIGKIT), ("cikit", CIKIT)] {
        let lib = gems.join(gem).join("lib");
        std::fs::create_dir_all(&lib).unwrap();
        std::fs::write(lib.join(format!("{gem}.rb")), text).unwrap();
    }
    let (sigkit, cikit) = (gems.join("sigkit/lib"), gems.join("cikit/lib"));
    let native = run("native", &scratch, &[&native_lib, &sigkit, &cikit]);
    assert_eq!(native, "level: :tests\ndoubled: 42\ncikit: true\nchecked: TypeError\n", "native control");
    let emitted = run("emitted", &tree, &[&native_lib]);
    assert_eq!(emitted, native, "the tree configured sorbet differently from native boot\nerrors: {errors:#?}");

    // First in gem_facades, before any gem source's dependency; never
    // Bundler's or Bootsnap's process machinery.
    let facades = std::fs::read_to_string(tree.join("runtime/gem_facades.rb")).unwrap();
    let setup = facades.find("require \"shopify/types/setup\" # config/boot.rb:4").unwrap_or_else(|| panic!("{facades}"));
    let dependency = facades.find("\"sigged_native\"").unwrap_or_else(|| panic!("{facades}"));
    assert!(setup < dependency, "{facades}");
    assert!(!facades.contains("bundler/setup") && !facades.contains("bootsnap/setup"), "{facades}");
}
