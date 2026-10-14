//! A library class file's requires (`emit::ruby::library`): the
//! authored requires its source writes, and the load-time dependencies
//! the emitter computes, which wait on Zeitwerk's answer.
//!
//! - A file the app/models walk ingests has no file program, so its
//!   top-level `require "shopify/x"` of a gem lib file (which nothing
//!   autoloads) was dropped. A reference used only in a method body is
//!   left to app/models.rb, so nothing loaded the file. Core's
//!   essentials model_uuid_columns.rb hit this
//!   (`uninitialized constant Shopify::ModelUuidEnabledAttributes`).
//! - A load-time dependency was a bare `require_relative` above the
//!   class header. Zeitwerk loads nothing for a constant that is already
//!   defined (even while its file is still running its body) and loads a
//!   registered autoload at the reference. A bare require of a file that
//!   is still loading is a circular require, which Core's StrictWarning
//!   raises. Two Core cases: `Pricing::DiscountFunctionApi::APIS` lists
//!   the classes that `extend` it, and payments_api.rb ends in
//!   `run_load_hooks`, whose hook loads a file that requires
//!   payments_api.rb back.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

use roundhouse::project::BuildTarget;

const LAZY: &str = "Rails.application.configure do\n  config.eager_load = false\nend\n";
const EAGER: &str = "Rails.application.configure do\n  config.eager_load = true\nend\n";

/// Core's StrictWarning: $VERBOSE on, and every Ruby warning about a
/// circular require (or an autoload that did not define its constant)
/// raises.
const STRICT: &str = "$VERBOSE = true\n\nmodule Strict\n  def warn(msg, category: nil, **)\n    raise \"strict: #{msg}\" if msg.include?(\"circular require\") || msg.include?(\"to define\")\n\n    super\n  end\nend\nWarning.extend(Strict)\n";

/// Booted as Core's test boot is: deprecation warnings on, every
/// circular-require warning raised.
const RUBYOPT: (&str, &str) = ("RUBYOPT", "-W:deprecated");

fn strict(environment: &str) -> emit_and_run::Overlay {
    emit_and_run::empty_app()
        .write("config/environments/development.rb", environment)
        .write("config/boot.rb", "require_relative \"../lib/strict\"\n")
        .write("config/application.rb", "require_relative \"boot\"\n")
        .write("lib/strict.rb", STRICT)
}

fn read(run: &emit_and_run::Run, path: &str) -> String {
    std::fs::read_to_string(run.emitted.join(path)).unwrap_or_else(|e| panic!("{path}: {e}"))
}

// (1) An authored require of a gem lib file whose constant only a method
// body reads.

const ALLOWLIST: &str = "module Shopify\n  module Allowlist\n    BY_MODEL = { \"Shop\" => true }.freeze\n  end\nend\n";

const UUID_COLUMNS: &str = "require \"shopify/allowlist\"\n\nmodule UuidColumns\n  def self.allowed?(name)\n    ::Shopify::Allowlist::BY_MODEL.fetch(name, false)\n  end\nend\n";

#[test]
fn an_authored_require_of_a_gem_lib_file_read_only_in_a_method_is_kept() {
    let run = strict(LAZY)
        .write("Gemfile.lock", "PATH\n  remote: gems/allow\n  specs:\n    allow (1.0)\n\nDEPENDENCIES\n  allow!\n")
        .write("gems/allow/lib/shopify/allowlist.rb", ALLOWLIST)
        .write("app/models/concerns/uuid_columns.rb", UUID_COLUMNS)
        .run_ruby_env("p UuidColumns.allowed?(\"Shop\")\np UuidColumns.allowed?(\"Order\")\n", &[RUBYOPT]);
    let file = read(&run, "app/models/uuid_columns.rb");
    assert!(file.contains("require_relative \"shopify/allowlist\"\n"), "{file}");
    // Where the source has it: ahead of the module.
    assert!(file.find("require_relative \"shopify/allowlist\"") < file.find("module UuidColumns"), "{file}");
    assert!(run.success, "{file}\n{}\n{}", run.stdout, run.stderr);
    assert_eq!(run.stdout, "true\nfalse\n", "{}", run.stderr);
}

// (2) A module whose constant lists classes that each `extend` it.

const API: &str = "module Pricing\n  module FunctionApi\n    APIS = [Pricing::Discounts::OrderApi, Pricing::Discounts::ProductApi].freeze\n\n    def self.types\n      APIS.map(&:type)\n    end\n\n    def label\n      \"api #{type}\"\n    end\n  end\nend\n";
const ORDER: &str = "module Pricing\n  module Discounts\n    class OrderApi\n      extend Pricing::FunctionApi\n\n      def self.type\n        :order\n      end\n    end\n  end\nend\n";
const PRODUCT: &str = "module Pricing\n  module Discounts\n    class ProductApi\n      extend Pricing::FunctionApi\n\n      def self.type\n        :product\n      end\n    end\n  end\nend\n";

fn mutual(environment: &str) -> emit_and_run::Overlay {
    strict(environment)
        .write("app/models/pricing/function_api.rb", API)
        .write("app/models/pricing/discounts/order_api.rb", ORDER)
        .write("app/models/pricing/discounts/product_api.rb", PRODUCT)
}

fn assert_mutual_boots(run: &emit_and_run::Run, stdout: &str) {
    assert!(run.success, "{}\n{}", run.stdout, run.stderr);
    assert_eq!(run.stdout, stdout, "{}", run.stderr);
    assert!(!run.stderr.contains("circular require"), "{}", run.stderr);
}

#[test]
fn a_module_listing_classes_that_extend_it_loads_first_without_a_cycle() {
    let run = mutual(LAZY).run_ruby_env("p Pricing::FunctionApi.types\np Pricing::Discounts::OrderApi.label\n", &[RUBYOPT]);
    assert_mutual_boots(&run, "[:order, :product]\n\"api order\"\n");
    // Each load-time dependency waits on its constant, as Zeitwerk does.
    let api = read(&run, "app/models/pricing/function_api.rb");
    assert!(
        api.contains("require_relative \"discounts/order_api\" unless defined?(::Pricing::Discounts::OrderApi)\n"),
        "{api}"
    );
    let order = read(&run, "app/models/pricing/discounts/order_api.rb");
    assert!(order.contains("require_relative \"../function_api\" unless defined?(::Pricing::FunctionApi)\n"), "{order}");
}

#[test]
fn a_class_extending_the_module_that_lists_it_loads_first_without_a_cycle() {
    let run = mutual(LAZY).run_ruby_env("p Pricing::Discounts::OrderApi.label\np Pricing::FunctionApi.types\n", &[RUBYOPT]);
    assert_mutual_boots(&run, "\"api order\"\n[:order, :product]\n");
}

#[test]
fn the_mutual_pair_eager_loads_without_a_cycle() {
    let run = mutual(EAGER).run_ruby_env("p Pricing::FunctionApi.types\np Pricing::Discounts::ProductApi.label\n", &[RUBYOPT]);
    assert_mutual_boots(&run, "[:order, :product]\n\"api product\"\n");
}

// (3) A file ending in `ActiveSupport.run_load_hooks(:x, …)`, whose hook
// loads a file that requires it back.

const PAYMENTS: &str = "module Payments\n  module CallbackInterface\n    def callback? = true\n  end\n\n  def self.hooks\n    @hooks ||= []\n  end\nend\n\nActiveSupport.run_load_hooks(:payments, Payments)\n";
const FINISHED: &str = "module Checkouts\n  module Finished\n    extend Payments::CallbackInterface\n  end\nend\n";
const FINISHED_HOOK: &str = "ActiveSupport.on_load(:payments) do\n  hooks << Checkouts::Finished\nend\n";
const DATABASE_YML: &str = "default: &default\n  adapter: postgresql\n\ndevelopment:\n  <<: *default\n  database: hook_probe\n\ntest:\n  <<: *default\n  database: hook_probe\n";

/// A PostgreSQL tree: its boot runs the initializers' `on_load` hooks
/// and the class files' `run_load_hooks`.
fn load_hook_cycle() -> emit_and_run::Overlay {
    load_hook_cycle_in(LAZY)
}

fn load_hook_cycle_in(environment: &str) -> emit_and_run::Overlay {
    strict(environment)
        .write("roundhouse.yml", "database_adapter: postgresql\n")
        .write("config/database.yml", DATABASE_YML)
        .write("app/models/payments.rb", PAYMENTS)
        .write("app/models/checkouts/finished.rb", FINISHED)
        .write("config/initializers/finished.rb", FINISHED_HOOK)
}

#[test]
fn a_file_its_own_load_hook_reaches_back_waits_on_the_defined_constant() {
    let (out, _errors) = load_hook_cycle().emit(BuildTarget::Ruby);
    let payments = std::fs::read_to_string(out.join("app/models/payments.rb")).expect("payments.rb");
    assert!(payments.ends_with("ActiveSupport.run_load_hooks(:payments, Payments)\n"), "{payments}");
    let finished = std::fs::read_to_string(out.join("app/models/checkouts/finished.rb")).expect("finished.rb");
    assert!(
        finished.contains(
            "require_relative \"../payments/callback_interface\" unless Object.autoload?(:Payments, false) || defined?(::Payments::CallbackInterface)\n"
        ),
        "{finished}"
    );
    // No bare require of the namespace's file, which is still loading
    // when its hook runs.
    assert!(!finished.contains("require_relative \"../payments\"\n"), "{finished}");
}

fn assert_hook_boots(run: &emit_and_run::Run, stdout: &str) {
    assert!(run.success, "{}\n{}", run.stdout, run.stderr);
    assert_eq!(run.stdout, stdout, "{}", run.stderr);
    assert!(!run.stderr.contains("circular require"), "{}", run.stderr);
    assert!(!run.stderr.contains("initializer_statement_not_run"), "{}", run.stderr);
}

/// The boot: `Payments` loads, its body's `run_load_hooks` runs the
/// initializer's hook, and that loads `Checkouts::Finished`, whose
/// `extend Payments::CallbackInterface` finds `Payments` defined. Boot
/// loads the `pg` gem (it never connects): run with `--ignored` and a
/// `GEM_PATH` that has it (plus `LD_PRELOAD` when its extension needs one).
#[test]
#[ignore = "requires ruby with the pg gem"]
fn a_load_hook_loading_a_file_that_requires_its_file_back_boots_without_a_cycle() {
    let run = load_hook_cycle().run_ruby_env("p Payments.hooks\np Checkouts::Finished.callback?\n", &[RUBYOPT]);
    assert_hook_boots(&run, "[Checkouts::Finished]\ntrue\n");
}

/// The other order: `Checkouts::Finished` loads first. Natively its
/// `extend Payments::CallbackInterface` loads `Payments` from inside the
/// module body, so the hook finds `Checkouts::Finished` already defined.
/// The emitted file leaves `Payments` to that reference while its
/// autoload is pending. Loading it at the top of the file ran the hook
/// before the module existed (NameError in the hook).
#[test]
#[ignore = "requires ruby with the pg gem"]
fn a_file_that_extends_a_hook_namespace_loads_it_from_its_body() {
    let run = load_hook_cycle().run_ruby_env("p Checkouts::Finished.callback?\np Payments.hooks\n", &[RUBYOPT]);
    assert_hook_boots(&run, "true\n[Checkouts::Finished]\n");
}


/// Eager loading requires every file in path order: checkouts/ before
/// payments.rb, as in the order above.
#[test]
#[ignore = "requires ruby with the pg gem"]
fn the_hook_cycle_eager_loads_without_a_cycle() {
    let run = load_hook_cycle_in(EAGER).run_ruby_env("p Payments.hooks\np Checkouts::Finished.callback?\n", &[RUBYOPT]);
    assert_hook_boots(&run, "[Checkouts::Finished]\ntrue\n");
}

// (4) A class whose constant reads an object built by another file
// (`Allocator = AllocatorImpl.new`), where that file's class includes a
// module nested in the first class. Core's
// `Checkouts::One::OversellProtection::ReservationsOversellManager`.

const MANAGER: &str = "module Oversell\n  class Manager\n    class Proxy\n      def initialize(allocator)\n        @allocator = allocator\n      end\n\n      def allocate\n        @allocator.allocate\n      end\n    end\n\n    DEFAULT_PROXY = Proxy.new(Oversell::Allocator)\n\n    def proxy\n      DEFAULT_PROXY\n    end\n  end\nend\n";
const MANAGER_ALLOCATOR_API: &str = "module Oversell\n  class Manager\n    module AllocatorApi\n      def kind\n        :reservations\n      end\n    end\n  end\nend\n";
const ALLOCATOR: &str = "module Oversell\n  Allocator = AllocatorImpl.new\nend\n";
const ALLOCATOR_IMPL: &str = "module Oversell\n  class AllocatorImpl\n    include Oversell::Manager::AllocatorApi\n\n    def allocate\n      kind\n    end\n  end\nend\n";

fn manager_allocator(environment: &str) -> emit_and_run::Overlay {
    strict(environment)
        .write("app/models/oversell/manager.rb", MANAGER)
        .write("app/models/oversell/manager/allocator_api.rb", MANAGER_ALLOCATOR_API)
        .write("app/models/oversell/allocator.rb", ALLOCATOR)
        .write("app/models/oversell/allocator_impl.rb", ALLOCATOR_IMPL)
}

#[test]
fn a_class_whose_constant_reads_an_object_built_from_a_class_including_its_module_loads_without_a_cycle() {
    let run = manager_allocator(LAZY).run_ruby_env("p Oversell::Manager.new.proxy.allocate\n", &[RUBYOPT]);
    let manager = read(&run, "app/models/oversell/manager.rb");
    assert_mutual_boots(&run, ":reservations\n");
    // The allocator waits until the class exists, so the impl it loads
    // finds the manager defined.
    assert!(
        !manager.lines().take_while(|l| !l.starts_with("module Oversell")).any(|l| l.contains("allocator\"")),
        "{manager}"
    );
}

#[test]
fn the_manager_allocator_pair_loads_from_the_impl_without_a_cycle() {
    // Natively the allocator cannot load first (its impl loads the
    // manager, whose body reads the allocator mid-load): the impl does.
    let run = manager_allocator(LAZY).run_ruby_env("p Oversell::AllocatorImpl.new.allocate\np Oversell::Manager.new.proxy.allocate\n", &[RUBYOPT]);
    assert_mutual_boots(&run, ":reservations\n:reservations\n");
}
