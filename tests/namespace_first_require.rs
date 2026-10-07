//! A load-time reference into a nested file loads the namespace's own
//! file first, as Zeitwerk does.
//!
//! Core's verdict gem: `Verdict::Flags::MonorailEventProcessor`'s
//! `PRODUCER_MAPPING` reads `SubjectType::PiiLevel::NonPii` at load.
//! Natively Zeitwerk autoloads `SubjectType` (subject_type.rb) before
//! `PiiLevel`; subject_type.rb's own `DEFAULT_SUBJECT_CLASS_MAPPING`
//! autoloads `ApiClient`, whose class body autoloads `PiiLevel`. The
//! emitted file required `subject_type/pii_level` directly instead. That
//! file's `module SubjectType` fired SubjectType's autoload, whose file
//! requires api_client up front, which requires pii_level back while it
//! is still loading. That is Ruby's circular-require warning, which
//! Core's StrictWarning raises at boot.
//!
//! The control is native Ruby running the real Zeitwerk on the same
//! source.

#[path = "support/emit_and_run.rs"]
mod emit_and_run;

const ENTRY: &str = r#"require "zeitwerk"

module Vd
  ZeitwerkLoader = Zeitwerk::Loader.for_gem #: Zeitwerk::Loader
  private_constant(:ZeitwerkLoader)
  ZeitwerkLoader.setup

  def self.loader
    ZeitwerkLoader
  end
end
"#;

const SUBJECT_TYPE: &str = r#"module Vd
  module SubjectType
    MAPPING = {
      "api_client" => ApiClient,
      "shop" => Shop,
    }.freeze

    def self.for(key)
      MAPPING.fetch(key)
    end
  end
end
"#;

const BASE: &str = r#"module Vd
  module SubjectType
    class Base
      def self.pii_level=(level)
        @pii_level = level
      end

      def self.pii_level
        @pii_level
      end
    end
  end
end
"#;

const API_CLIENT: &str = r#"module Vd
  module SubjectType
    class ApiClient < Base
      self.pii_level = PiiLevel::NonPii
    end
  end
end
"#;

const SHOP: &str = r#"module Vd
  module SubjectType
    class Shop < Base
      self.pii_level = PiiLevel::Owned
    end
  end
end
"#;

const PII_LEVEL: &str = r#"module Vd
  module SubjectType
    class PiiLevel
      def initialize(label)
        @label = label
      end

      def to_s
        @label
      end

      NonPii = new("non-pii")
      Owned = new("owned-pii")
    end
  end
end
"#;

const PROCESSOR: &str = r#"module Vd
  module Flags
    class Processor
      MAPPING = {
        SubjectType::PiiLevel::NonPii => "non-pii producer",
        SubjectType::PiiLevel::Owned => "owned producer",
      }.freeze

      def self.producer(level)
        MAPPING[level]
      end
    end
  end
end
"#;

/// Core's StrictWarning: $VERBOSE on, and a warning about a circular
/// require (or about an autoload that did not define its constant)
/// raises.
const STRICT: &str = "$VERBOSE = true\n\nmodule Strict\n  def warn(msg, category: nil, **)\n    raise \"strict: #{msg}\" if msg.include?(\"circular require\") || msg.include?(\"to define\")\n\n    super\n  end\nend\nWarning.extend(Strict)\n";

const REPORT: &str = r#"puts Vd::Flags::Processor.producer(Vd::SubjectType::PiiLevel::NonPii)
puts Vd::SubjectType::MAPPING.keys.inspect
puts Vd::SubjectType.for("api_client").pii_level
puts Vd::SubjectType::Shop.pii_level
"#;

const FILES: [(&str, &str); 7] = [
    ("gems/vd/lib/vd.rb", ENTRY),
    ("gems/vd/lib/vd/subject_type.rb", SUBJECT_TYPE),
    ("gems/vd/lib/vd/subject_type/base.rb", BASE),
    ("gems/vd/lib/vd/subject_type/api_client.rb", API_CLIENT),
    ("gems/vd/lib/vd/subject_type/shop.rb", SHOP),
    ("gems/vd/lib/vd/subject_type/pii_level.rb", PII_LEVEL),
    ("gems/vd/lib/vd/flags/processor.rb", PROCESSOR),
];

#[test]
fn a_load_time_reference_into_a_nested_file_loads_its_namespace_file_first() {
    namespace_first(false);
}

/// Eager loading requires every file in path order, so the processor's
/// file (flags/) loads before subject_type.rb has been referenced.
#[test]
fn eager_loading_loads_the_namespace_file_first_too() {
    namespace_first(true);
}

fn namespace_first(eager: bool) {
    let (expected, run) = native_and_emitted(eager, &FILES, REPORT);
    assert_eq!(expected, "non-pii producer\n[\"api_client\", \"shop\"]\nnon-pii\nowned-pii\n", "native control");
    assert_eq!(run.stdout, expected, "{}", run.stderr);
    let processor =
        std::fs::read_to_string(run.emitted.join("app/models/vd/flags/processor.rb")).expect("processor.rb");
    let namespace = processor.find("require_relative \"../subject_type\"\n");
    let child = processor.find("require_relative \"../subject_type/pii_level\"\n");
    assert!(namespace.is_some() && namespace < child, "the namespace file is not required first:\n{processor}");
}

/// The namespace's file reads a constant of the requiring file at load
/// (`"registry" => Registry`), so it requires that file up front. A
/// require of the namespace file from there would close a cycle, so none
/// is added: the namespace is the file loading it.
const SUBJECT_TYPE_WITH_REGISTRY: &str = r#"module Vd
  module SubjectType
    MAPPING = {
      "api_client" => ApiClient,
      "registry" => Registry,
    }.freeze
  end
end
"#;

const REGISTRY: &str = r#"module Vd
  class Registry
    LEVELS = [SubjectType::PiiLevel::NonPii, SubjectType::PiiLevel::Owned].freeze
  end
end
"#;

#[test]
fn a_namespace_file_that_requires_the_requirer_is_not_required_back() {
    let files = [
        ("gems/vd/lib/vd.rb", ENTRY),
        ("gems/vd/lib/vd/subject_type.rb", SUBJECT_TYPE_WITH_REGISTRY),
        ("gems/vd/lib/vd/subject_type/base.rb", BASE),
        ("gems/vd/lib/vd/subject_type/api_client.rb", API_CLIENT),
        ("gems/vd/lib/vd/subject_type/pii_level.rb", PII_LEVEL),
        ("gems/vd/lib/vd/registry.rb", REGISTRY),
    ];
    let report = "puts Vd::SubjectType::MAPPING.keys.inspect\nputs Vd::Registry::LEVELS.map(&:to_s).inspect\n";
    let (expected, run) = native_and_emitted(false, &files, report);
    assert_eq!(expected, "[\"api_client\", \"registry\"]\n[\"non-pii\", \"owned-pii\"]\n", "native control");
    assert_eq!(run.stdout, expected, "{}", run.stderr);
    let registry = std::fs::read_to_string(run.emitted.join("app/models/vd/registry.rb")).expect("registry.rb");
    assert!(registry.contains("require_relative \"subject_type/pii_level\"\n"), "{registry}");
    assert!(!registry.contains("require_relative \"subject_type\"\n"), "{registry}");
}

/// The native control's stdout (real Zeitwerk, StrictWarning on) and the
/// emitted tree's run of the same gem, which must boot without a
/// circular require.
fn native_and_emitted(eager: bool, files: &[(&str, &str)], report: &str) -> (String, emit_and_run::Run) {
    let native_root = std::env::temp_dir().join(format!(
        "roundhouse-namespace-first-native-{}-{eager}-{}",
        std::process::id(),
        files.len()
    ));
    let _ = std::fs::remove_dir_all(&native_root);
    for (path, text) in files.iter().chain([("strict.rb", STRICT)].iter()) {
        let file = native_root.join(path);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(file, text).unwrap();
    }
    let native = emit_and_run::ruby()
        .arg("-I")
        .arg(native_root.join("gems/vd/lib"))
        .arg("-r")
        .arg(native_root.join("strict.rb"))
        .arg("-e")
        .arg(format!("require \"vd\"\n{report}"))
        .output()
        .expect("spawn ruby");
    let _ = std::fs::remove_dir_all(&native_root);
    assert!(native.status.success(), "native control failed:\n{}", String::from_utf8_lossy(&native.stderr));
    let expected = String::from_utf8_lossy(&native.stdout).into_owned();

    let mut overlay = emit_and_run::empty_app()
        .write("Gemfile.lock", "PATH\n  remote: gems/vd\n  specs:\n    vd (0.1.0)\n\nGEM\n  remote: https://rubygems.org/\n  specs:\n")
        .write(
            "config/environments/development.rb",
            &format!("Rails.application.configure do\n  config.eager_load = {eager}\nend\n"),
        )
        .write("config/boot.rb", "require_relative \"../lib/strict\"\n")
        .write("config/application.rb", "require_relative \"boot\"\nrequire \"vd\"\n")
        .write("lib/strict.rb", STRICT);
    for (path, text) in files {
        overlay = overlay.write(path, text);
    }
    let run = overlay.run_ruby(report);
    assert!(run.errors.is_empty(), "{}", run.errors.join("\n"));
    assert!(run.success, "emitted program failed in {}:\n{}", run.emitted.display(), run.stderr);
    assert!(!run.stderr.contains("circular require"), "{}", run.stderr);
    (expected, run)
}

/// Core's checkouts/one/oversell_protection: the namespace module reads
/// its manager at load (`@manager = ReservationsOversellManager.new`),
/// the manager's class body reads `InventoryItemAllocator`, and that
/// constant is written in a file of its own,
/// oversell_protection/inventory_item_allocator.rb, which reopens the
/// namespace for it. Natively Zeitwerk loads the constant from that file
/// while the namespace's file is still loading. Ingest merges the reopen
/// into the namespace, so the constant was emitted in the namespace's
/// file, and the manager's require of it was a require of the file
/// already loading.
const OVERSELL_PROTECTION: &str = r#"module Vd
  module OversellProtection
    @manager = ReservationsOversellManager.new

    def self.manager
      @manager
    end
  end
end
"#;

const RESERVATIONS_OVERSELL_MANAGER: &str = r#"module Vd
  module OversellProtection
    class ReservationsOversellManager
      class ReservationsBackendProxyImpl
        def initialize(allocator)
          @allocator = allocator
        end

        def allocator
          @allocator
        end
      end

      ReservationsBackendProxy = ReservationsBackendProxyImpl.new(InventoryItemAllocator)

      def backend
        ReservationsBackendProxy
      end
    end
  end
end
"#;

const INVENTORY_ITEM_ALLOCATOR_IMPL: &str = r#"module Vd
  module OversellProtection
    class InventoryItemAllocatorImpl
      class ReservationRepositoryWrapper
        def label
          "wrapped repository"
        end
      end

      def initialize(reservation_repository:)
        @reservation_repository = reservation_repository
      end

      def repository_label
        @reservation_repository.label
      end
    end
  end
end
"#;

const INVENTORY_ITEM_ALLOCATOR: &str = r#"module Vd
  module OversellProtection
    InventoryItemAllocator = InventoryItemAllocatorImpl.new(reservation_repository: InventoryItemAllocatorImpl::ReservationRepositoryWrapper.new)
  end
end
"#;

const OVERSELL_FILES: [(&str, &str); 5] = [
    ("gems/vd/lib/vd.rb", ENTRY),
    ("gems/vd/lib/vd/oversell_protection.rb", OVERSELL_PROTECTION),
    ("gems/vd/lib/vd/oversell_protection/reservations_oversell_manager.rb", RESERVATIONS_OVERSELL_MANAGER),
    ("gems/vd/lib/vd/oversell_protection/inventory_item_allocator_impl.rb", INVENTORY_ITEM_ALLOCATOR_IMPL),
    ("gems/vd/lib/vd/oversell_protection/inventory_item_allocator.rb", INVENTORY_ITEM_ALLOCATOR),
];

const OVERSELL_REPORT: &str = r#"allocator = Vd::OversellProtection.manager.backend.allocator
puts allocator.repository_label
puts allocator.equal?(Vd::OversellProtection::InventoryItemAllocator)
"#;

#[test]
fn a_namespace_constant_written_in_its_own_file_loads_from_that_file() {
    constant_in_its_own_file(false);
}

#[test]
fn eager_loading_loads_a_namespace_constant_from_its_own_file() {
    constant_in_its_own_file(true);
}

fn constant_in_its_own_file(eager: bool) {
    let (expected, run) = native_and_emitted(eager, &OVERSELL_FILES, OVERSELL_REPORT);
    assert_eq!(expected, "wrapped repository\ntrue\n", "native control");
    assert_eq!(run.stdout, expected, "{}", run.stderr);
    let read = |path: &str| std::fs::read_to_string(run.emitted.join(path)).unwrap_or_else(|e| panic!("{path}: {e}"));
    let namespace = read("app/models/vd/oversell_protection.rb");
    assert!(!namespace.contains("InventoryItemAllocator ="), "{namespace}");
    let constant = read("app/models/vd/oversell_protection/inventory_item_allocator.rb");
    assert!(constant.contains("InventoryItemAllocator ="), "{constant}");
    let manager = read("app/models/vd/oversell_protection/reservations_oversell_manager.rb");
    assert!(manager.contains("require_relative \"inventory_item_allocator\"\n"), "{manager}");
    assert!(!manager.contains("require_relative \"../oversell_protection\"\n"), "{manager}");
    let models = read("app/models.rb");
    assert!(
        models.contains("[:InventoryItemAllocator, \"models/vd/oversell_protection/inventory_item_allocator\", false]"),
        "no autoload entry for the constant's file:\n{models}"
    );
}

/// A gem that loads its files by `require`, not Zeitwerk: the namespace
/// constant written in vd/setup.rb still has a file of its own in the
/// tree, and the require of vd/setup.rb loads it right there, as Ruby
/// does, not on first reference.
#[test]
fn a_required_file_that_writes_a_namespace_constant_loads_it_when_required() {
    let files = [
        (
            "gems/vd/lib/vd.rb",
            "require \"vd/setup\"\n\nmodule Vd\n  def self.banner\n    \"vd #{Setup}\"\n  end\nend\n",
        ),
        ("gems/vd/lib/vd/setup.rb", "module Vd\n  Setup = \"ready\".tap { |state| puts \"setup #{state}\" }\nend\n"),
    ];
    let report = "puts \"booted\"\nputs Vd.banner\n";
    let (expected, run) = native_and_emitted(false, &files, report);
    assert_eq!(expected, "setup ready\nbooted\nvd ready\n", "native control");
    assert_eq!(run.stdout, expected, "{}", run.stderr);
}
