//! An autoloaded file whose constant a gem already defined when the
//! autoloaders are set up, against native Rails' Zeitwerk (Shopify/rails
//! 5813559f4aed; tests/shadowed_autoload_file.rb).
//!
//! Core's in-repo proto-shopify engine carries
//! app/gen/proto/shopify/common/country_code.rb, and the native proto gem
//! has its own copy, which shopify_spanner requires at boot. Zeitwerk sets
//! up the engine's autoloads after that, finds `CountryCode` defined and
//! ignores the engine's file as shadowed. The emitted `register` loaded
//! every already-defined constant's file. Here that ran the engine's copy
//! too: "already initialized constant
//! Proto::Shopify::Common::CountryCode::UNSPECIFIED", which Core's
//! StrictWarning turns fatal. A file no autoload root maps to its constant
//! still loads, as native runs it: an initializer's `module PublicSuffix`
//! body that reopens the public_suffix gem's namespace.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

use roundhouse::project::BuildTarget;
use std::path::Path;

fn root() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

/// The gem's file, outside the app: it defines the constants first.
const GEM_COUNTRY: &str = "module Codes\n  module Country\n    UNSPECIFIED = 0\n\n    def self.origin\n      \"gem\"\n    end\n  end\nend\n\nmodule Listing\nend\n";

/// The app's copy under an autoload root, which Zeitwerk ignores.
const APP_COUNTRY: &str = "module Codes\n  module Country\n    UNSPECIFIED = 0\n    US = 1\n\n    def self.origin\n      \"app\"\n    end\n  end\nend\n";

/// An initializer reopening the gem's namespace (Core's
/// config/initializers/public_suffix.rb): native runs it.
const INITIALIZER: &str = "module Listing\n  LIST = \"initializer\"\nend\n";

/// A constant nothing defined first: autoloads from its file. It reads
/// the initializer's constant, as Core's app code reads
/// `PublicSuffix::LIST_PATH`.
const WIDGET: &str = "class Widget\n  def self.kind\n    \"app\"\n  end\n\n  def self.list\n    Listing::LIST\n  end\nend\n";

fn run(mode: &str, dir: &Path, gem: &Path, eager: bool) -> String {
    let mut cmd = emit_and_run::ruby();
    cmd.arg(root().join("tests/shadowed_autoload_file.rb")).arg("run").arg(mode).arg(dir).arg(gem);
    cmd.env("SHADOW_EAGER", if eager { "1" } else { "0" });
    let out = cmd.output().expect("ruby");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(out.status.success(), "{mode} failed\n=== stdout ===\n{stdout}\n=== stderr ===\n{}", String::from_utf8_lossy(&out.stderr));
    stdout
}

/// What Rails answers (checked natively), so two equal wrong answers
/// still fail.
const EXPECTED: &str = "country: gem [:UNSPECIFIED]\nlist: initializer\nwidget: app\nredefined: 0\n";

/// Lazy boot: `register` sets up the autoloads and loads the files of
/// constants already defined, except a shadowed one.
#[test]
fn a_file_whose_constant_a_gem_defined_first_is_shadowed_as_zeitwerk_ignores_it() {
    shadowed(false);
}

/// Eager boot: `eager_load!` skips the shadowed file, as Zeitwerk's
/// eager load does.
#[test]
fn eager_loading_skips_the_shadowed_file() {
    shadowed(true);
}

fn shadowed(eager: bool) {
    let (tree, errors) = emit_and_run::empty_app()
        .write("roundhouse.yml", "test_paths:\n  - test\n")
        .write("config/environments/test.rb", &format!("Rails.application.configure do\n  config.eager_load = {eager}\nend\n"))
        .write("test/test_helper.rb", "require \"active_support/test_case\"\n")
        .write("app/models/codes/country.rb", APP_COUNTRY)
        .write("app/models/widget.rb", WIDGET)
        .write("config/initializers/listing.rb", INITIALIZER)
        .emit(BuildTarget::Ruby);
    assert!(errors.is_empty(), "{errors:#?}");
    let models = std::fs::read_to_string(tree.join("app/models.rb")).unwrap();
    // The initializer's module body is no file Zeitwerk manages; the
    // app/models files are.
    assert!(models.contains("  UNNAMED_FILES = {\n    \"models/listing\" => true,\n  }.freeze\n"), "{models}");
    let scratch = tree.parent().expect("scratch").to_path_buf();
    let gem = scratch.join("codes-1.0.0/lib/codes/country.rb");
    std::fs::create_dir_all(gem.parent().unwrap()).unwrap();
    std::fs::write(&gem, GEM_COUNTRY).unwrap();
    let native = run("native", &scratch.join("app"), &gem, eager);
    assert_eq!(native, EXPECTED, "native");
    let emitted = run("emitted", &tree, &gem, eager);
    assert_eq!(emitted, native, "emitted");
}
