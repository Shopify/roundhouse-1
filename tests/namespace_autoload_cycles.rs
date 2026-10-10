//! A file that opens a namespace (`module Plat; module Artifact; module
//! EmptyCheckable`) or reads a qualified constant
//! (`::Caps::Abstract`) loads that namespace's own file first, as Ruby's
//! autoload does: `module Artifact` and the lookup of `Caps` each fire
//! the namespace constant's autoload. Those are load-time require edges
//! no `require_relative` line shows (`emit::ruby::library`,
//! `load_cycle_components`), and the cycles they close must break the
//! way Zeitwerk's load order does, or Ruby warns about a circular
//! require, which Core's StrictWarning raises.
//!
//! Core had both, and every r7 unit-1000 test failed at boot:
//! - checkout_platform/artifact.rb's `DEPRECATED_CONSTANTS` reads
//!   `Checkouts::One::Web::Artifact::MerchandiseTerms`, which includes
//!   `CheckoutPlatform::Artifact::EmptyCheckable`, whose file reopens
//!   `module Artifact` (artifact.rb:5).
//! - metafields/capabilities.rb's `CAPABILITY_MAP` reads
//!   `Merchandising::Metafields::SmartCollectionConditionCapability`,
//!   which extends `::Metafields::Capabilities::AbstractCapability`
//!   (capabilities.rb:4).
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

const LAZY: &str = "Rails.application.configure do\n  config.eager_load = false\nend\n";

/// Core's StrictWarning: $VERBOSE on, and a circular-require warning
/// raises.
const STRICT: &str = "$VERBOSE = true\n\nmodule Strict\n  def warn(msg, category: nil, **)\n    raise \"strict: #{msg}\" if msg.include?(\"circular require\") || msg.include?(\"to define\")\n\n    super\n  end\nend\nWarning.extend(Strict)\n";

const RUBYOPT: (&str, &str) = ("RUBYOPT", "-W:deprecated");

fn base() -> emit_and_run::Overlay {
    emit_and_run::empty_app()
        .write("config/environments/development.rb", LAZY)
        .write("config/boot.rb", "require_relative \"../lib/strict\"\n")
        .write("config/application.rb", "require_relative \"boot\"\n")
        .write("lib/strict.rb", STRICT)
}

/// The artifact shape: the namespace's constant reads a class whose
/// include lives in a file nested under that namespace.
fn artifact_app() -> emit_and_run::Overlay {
    base()
        .write(
            "app/models/plat/artifact.rb",
            "module Plat\n  module Artifact\n    DEPRECATED = { merch: Terms::Merch }.freeze\n\n    def self.deprecated\n      DEPRECATED\n    end\n  end\nend\n",
        )
        .write(
            "app/models/plat/artifact/empty_checkable.rb",
            "module Plat\n  module Artifact\n    module EmptyCheckable\n      def empty?\n        true\n      end\n    end\n  end\nend\n",
        )
        .write(
            "app/models/terms/merch.rb",
            "module Terms\n  class Merch\n    include Plat::Artifact::EmptyCheckable\n\n    def self.label\n      \"merch\"\n    end\n  end\nend\n",
        )
}

/// The capabilities shape: the namespace's constant lists classes that
/// each extend a module nested under that namespace.
fn capabilities_app() -> emit_and_run::Overlay {
    base()
        .write(
            "app/models/caps.rb",
            "module Caps\n  MAP = { admin: ::Caps::Admin, smart: ::Shelf::Smart }.freeze\n\n  def self.map\n    MAP\n  end\nend\n",
        )
        .write(
            "app/models/caps/abstract.rb",
            "module Caps\n  module Abstract\n    def kind\n      name.downcase\n    end\n  end\nend\n",
        )
        .write("app/models/caps/admin.rb", "module Caps\n  class Admin\n    extend ::Caps::Abstract\n  end\nend\n")
        .write("app/models/shelf/smart.rb", "module Shelf\n  class Smart\n    extend ::Caps::Abstract\n  end\nend\n")
}

fn assert_quiet(run: &emit_and_run::Run, stdout: &str) {
    assert!(run.success, "{}\n{}", run.stdout, run.stderr);
    assert!(!run.stderr.contains("circular require"), "{}", run.stderr);
    assert_eq!(run.stdout, stdout, "{}", run.stderr);
}

#[test]
fn a_class_including_a_module_of_the_namespace_that_reads_it_loads_first() {
    let run = artifact_app().run_ruby_env("p Terms::Merch.new.empty?\np Plat::Artifact.deprecated.keys\n", &[RUBYOPT]);
    assert_quiet(&run, "true\n[:merch]\n");
}

#[test]
fn the_namespace_that_reads_the_class_loads_first() {
    let run = artifact_app().run_ruby_env("p Plat::Artifact.deprecated[:merch].label\np Terms::Merch.new.empty?\n", &[RUBYOPT]);
    assert_quiet(&run, "\"merch\"\ntrue\n");
}

#[test]
fn the_nested_module_loads_first() {
    let run = artifact_app().run_ruby_env("p Plat::Artifact::EmptyCheckable.instance_methods\np Plat::Artifact.deprecated.keys\n", &[RUBYOPT]);
    assert_quiet(&run, "[:empty?]\n[:merch]\n");
}

#[test]
fn a_class_extending_a_module_of_the_namespace_that_reads_it_loads_first() {
    let run = capabilities_app().run_ruby_env("p Shelf::Smart.kind\np Caps.map.keys\np Caps::Admin.kind\n", &[RUBYOPT]);
    assert_quiet(&run, "\"shelf::smart\"\n[:admin, :smart]\n\"caps::admin\"\n");
}

#[test]
fn the_capabilities_namespace_loads_first() {
    let run = capabilities_app().run_ruby_env("p Caps.map.values.map(&:kind)\n", &[RUBYOPT]);
    assert_quiet(&run, "[\"caps::admin\", \"shelf::smart\"]\n");
}

/// The compile-time cycle and declaration-require checks
/// (`roundhouse::require_graph`) over an
/// emitted tree already on disk: `ROUNDHOUSE_TREE=<out> cargo test
/// --test namespace_autoload_cycles -- --ignored --nocapture`.
#[test]
#[ignore]
fn require_cycles_of_an_emitted_tree() {
    let root = std::path::PathBuf::from(std::env::var("ROUNDHOUSE_TREE").expect("ROUNDHOUSE_TREE"));
    let mut files: Vec<(String, String)> = Vec::new();
    let mut stack = vec![root.join("app")];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).unwrap().flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|e| e == "rb") {
                let rel = path.strip_prefix(&root).unwrap().to_string_lossy().into_owned();
                files.push((rel, std::fs::read_to_string(&path).unwrap_or_default()));
            }
        }
    }
    let cycles = roundhouse::require_graph::cycles(&files);
    for cycle in &cycles {
        println!("require_cycle: {}", cycle.describe());
    }
    let direct = roundhouse::require_graph::declaration_requires(&files);
    for found in &direct {
        println!("declaration_require: {}", found.describe());
    }
    println!("{} files, {} cycles, {} declaration requires", files.len(), cycles.len(), direct.len());
}

/// Core's hedwig gem: hedwig.rb requires hedwig/logging and includes
/// `Hedwig::Logging`; logging.rb runs a statement (a require), so its
/// program loads the declarations and `Hedwig::Logging`'s autoload names
/// that program. logging.rb's `module Hedwig` reopens the namespace,
/// which closes a load cycle, so the require moves into hedwig.rb's body.
/// It still loads the program: required directly, the declarations
/// file's `module Logging` fired the autoload into the program, whose
/// require of that file was in progress, and the r7 boot died with
/// "constant Hedwig::Logging not defined" (and ActiveResource::Callbacks
/// the same way, from `class Base`).
fn hedwig_app() -> emit_and_run::Overlay {
    base()
        .write(
            "lib/hedwig.rb",
            "require \"logger\"\n\nmodule Hedwig\n  include Hedwig::Logging\n\n  extend self\nend\n",
        )
        .write(
            "lib/hedwig/logging.rb",
            "require \"logger\"\n\nmodule Hedwig\n  module Logging\n    FORMATTER = proc { |severity, msg| \"#{severity}: #{msg}\" }\n\n    def format_line(msg)\n      FORMATTER.call(\"INFO\", msg)\n    end\n  end\nend\n",
        )
}

#[test]
fn a_namespace_requiring_its_nested_programs_module_in_its_body_loads_the_program() {
    let run = hedwig_app().run_ruby_env("p Hedwig.format_line(\"up\")\np Hedwig::Logging::FORMATTER.call(\"WARN\", \"x\")\n", &[RUBYOPT]);
    assert_quiet(&run, "\"INFO: up\"\n\"WARN: x\"\n");
    let hedwig = std::fs::read_to_string(run.emitted.join("app/models/hedwig.rb")).expect("hedwig.rb");
    assert!(!hedwig.contains("require_relative \"hedwig/logging\""), "{hedwig}");
}

#[test]
fn the_nested_programs_module_loads_first() {
    let run = hedwig_app().run_ruby_env("p Hedwig::Logging.instance_methods\np Hedwig.format_line(\"up\")\n", &[RUBYOPT]);
    assert_quiet(&run, "[:format_line]\n\"INFO: up\"\n");
}
