//! A nested file that names its own constants fully qualified
//! (secure_headers' clear_site_data.rb: `module SecureHeaders; class
//! ClearSiteData; ALL_TYPES = [SecureHeaders::ClearSiteData::CACHE, ...]`)
//! needs only itself at load. Resolving that reference by its first
//! segment required the namespace's file (secure_headers.rb), whose body
//! (`ALL_HEADER_CLASSES = [SecureHeaders::ClearSiteData, ...]`) requires
//! the nested files up front: a require cycle native load order does not
//! have (the namespace's file requires the nested ones, which require
//! nothing back), and Ruby's circular-require warning, which Core's
//! StrictWarning raises at boot.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

const LAZY: &str = "Rails.application.configure do\n  config.eager_load = false\nend\n";

/// Core's StrictWarning: $VERBOSE on, every Ruby warning about a
/// circular require (or an autoload that did not define its constant)
/// raises.
const STRICT: &str = "$VERBOSE = true\n\nmodule Strict\n  def warn(msg, category: nil, **)\n    raise \"strict: #{msg}\" if msg.include?(\"circular require\") || msg.include?(\"to define\")\n\n    super\n  end\nend\nWarning.extend(Strict)\n";

const CLEAR: &str = "module Guarding\n  class Clear\n    CACHE = \"cache\"\n    TYPES = [Guarding::Clear::CACHE].freeze\n  end\nend\n";

#[test]
fn a_nested_files_qualified_reference_to_its_own_constant_does_not_require_its_namespace() {
    let run = emit_and_run::empty_app()
        .write("config/environments/development.rb", LAZY)
        .write("config/boot.rb", "require_relative \"../lib/strict\"\n")
        .write("config/application.rb", "require_relative \"boot\"\n")
        .write("lib/strict.rb", STRICT)
        .write("lib/guarding.rb", "module Guarding\n  ALL = [Guarding::Clear].freeze\nend\n")
        .write("lib/guarding/clear.rb", CLEAR)
        .run_ruby("puts \"booted\"\np Guarding::ALL\np Guarding::Clear::TYPES\n");
    assert!(run.success, "{}\n{}", run.stdout, run.stderr);
    assert_eq!(run.stdout, "booted\n[Guarding::Clear]\n[\"cache\"]\n", "{}", run.stderr);
    assert!(!run.stderr.contains("circular require"), "{}", run.stderr);
    let clear = std::fs::read_to_string(run.emitted.join("app/models/guarding/clear.rb")).expect("clear.rb");
    assert!(!clear.contains("require_relative \"../guarding\""), "{clear}");
}

/// secure_headers.rb itself: a program (Bundler.require's entry, here a
/// boot require) that requires the nested files, then defines the
/// namespace's body that lists them.
#[test]
fn a_program_requiring_nested_files_before_its_namespace_body_boots_without_a_cycle() {
    let run = emit_and_run::empty_app()
        .write("config/environments/development.rb", LAZY)
        .write("config/boot.rb", "require_relative \"../lib/strict\"\n")
        .write("config/application.rb", "require_relative \"boot\"\nrequire \"guarding\"\n")
        .write("lib/strict.rb", STRICT)
        .write(
            "lib/guarding.rb",
            "require \"guarding/clear\"\n\nmodule Guarding\n  ALL = [Guarding::Clear].freeze\nend\n\nputs \"guarding entry\"\n",
        )
        .write("lib/guarding/clear.rb", CLEAR)
        .run_ruby("puts \"booted\"\np Guarding::ALL\np Guarding::Clear::TYPES\n");
    assert!(run.success, "{}\n{}", run.stdout, run.stderr);
    assert_eq!(run.stdout, "guarding entry\nbooted\n[Guarding::Clear]\n[\"cache\"]\n", "{}", run.stderr);
    assert!(!run.stderr.contains("circular require"), "{}", run.stderr);
}

/// secure_headers' content_security_policy_config.rb reads
/// `SecureHeaders::PolicyManagement::ALL_DIRECTIVES` at load: the file it
/// needs is PolicyManagement's, not the namespace's (which requires this
/// file back).
#[test]
fn a_qualified_reference_into_a_sibling_requires_the_siblings_file_not_the_namespace() {
    let run = emit_and_run::empty_app()
        .write("config/environments/development.rb", LAZY)
        .write("config/boot.rb", "require_relative \"../lib/strict\"\n")
        .write("config/application.rb", "require_relative \"boot\"\n")
        .write("lib/strict.rb", STRICT)
        .write("lib/guarding.rb", "module Guarding\n  ALL = [Guarding::Config].freeze\nend\n")
        .write("lib/guarding/policy.rb", "module Guarding\n  module Policy\n    DIRECTIVES = [:a, :b].freeze\n  end\nend\n")
        .write("lib/guarding/config.rb", "module Guarding\n  class Config\n    ATTRS = Guarding::Policy::DIRECTIVES + [:c]\n  end\nend\n")
        .run_ruby("puts \"booted\"\np Guarding::ALL\np Guarding::Config::ATTRS\n");
    assert!(run.success, "{}\n{}", run.stdout, run.stderr);
    assert_eq!(run.stdout, "booted\n[Guarding::Config]\n[:a, :b, :c]\n", "{}", run.stderr);
    assert!(!run.stderr.contains("circular require"), "{}", run.stderr);
    let config = std::fs::read_to_string(run.emitted.join("app/models/guarding/config.rb")).expect("config.rb");
    assert!(config.contains("require_relative \"policy\""), "{config}");
    assert!(!config.contains("require_relative \"../guarding\""), "{config}");
}

/// Core's checkouts/one ProcessingService: a nested class with nested
/// classes of its own (`InventoryError`, whose `FailedToReserveInventory`
/// constant builds one of them) is rendered in its own file and then
/// spliced into its owner's. Its requires were rebased onto the owner's
/// file, including the one naming that file itself (the constant's), so
/// the file required itself: a circular require. Spliced, the constant
/// also ran ahead of the nested class it builds, which the source
/// defines first.
#[test]
fn a_spliced_nested_class_does_not_require_the_file_that_holds_it() {
    let svc = "class Svc\n  class Error < StandardError\n  end\n\n  class InventoryError < Error\n    class Impl < InventoryError; end\n    Reserved = Impl.new\n\n    class Claim < InventoryError\n      def initialize(reverted)\n        super()\n        @reverted = reverted\n      end\n\n      attr_reader :reverted\n    end\n  end\nend\n";
    let run = emit_and_run::empty_app()
        .write("config/environments/development.rb", LAZY)
        .write("config/boot.rb", "require_relative \"../lib/strict\"\n")
        .write("config/application.rb", "require_relative \"boot\"\n")
        .write("lib/strict.rb", STRICT)
        .write("lib/svc.rb", svc)
        .run_ruby("p Svc::InventoryError::Reserved.class\np Svc::InventoryError::Claim.new(true).reverted\np Svc::InventoryError.superclass\n");
    assert!(run.success, "{}\n{}", run.stdout, run.stderr);
    assert_eq!(run.stdout, "Svc::InventoryError::Impl\ntrue\nSvc::Error\n", "{}", run.stderr);
    let file = std::fs::read_to_string(run.emitted.join("app/models/svc.rb")).expect("svc.rb");
    assert!(!file.contains("require_relative \"svc\""), "{file}");
}
