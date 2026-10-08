//! A support file's file program loads when Ruby loads its source: at
//! boot only where native boot requires it, otherwise when its constant
//! is first referenced, as Zeitwerk autoloads the file.
//!
//! Every program nothing in the emitted tree required used to load at
//! boot (app/models.rb's program list), so Core's lazy test boot ran
//! 2,686 files native never loads at boot (gems/proto-shopify's
//! generated messages, components' services and graph clients), and
//! died in one of them (organizations_api.rb's `require
//! "shopify/config"`). Native boot loads only what its own boot files
//! require: config/boot.rb (Core's `require_relative
//! ".../strict_warnings"`), config/application.rb's requires and
//! Bundler.require's gem entries, the environment file's requires.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

const LAZY: &str = "Rails.application.configure do\n  config.eager_load = false\nend\n";

#[test]
fn a_program_under_an_autoload_root_runs_on_its_constants_first_reference_once_not_at_boot() {
    let run = emit_and_run::empty_app()
        .write("config/environments/development.rb", LAZY)
        .write(
            "app/services/greeter.rb",
            "puts \"loading greeter\"\n\nclass Greeter\n  def self.hi\n    \"hi\"\n  end\nend\n\nputs \"loaded greeter #{Greeter.hi}\"\n",
        )
        .write(
            "app/services/billing/invoice.rb",
            "puts \"loading invoice\"\n\nmodule Billing\n  class Invoice\n    TOTAL = 3\n  end\nend\n",
        )
        .write("app/services/billing/refund.rb", "module Billing\n  class Refund\n  end\nend\n")
        // Declares nothing and nothing requires it: native lazy boot
        // never loads it.
        .write("lib/stray.rb", "puts \"stray ran\"\n")
        .run_ruby(
            "puts \"booted\"\n\
             p Greeter.hi\n\
             p Greeter.hi\n\
             p defined?(Billing::Refund)\n\
             p Billing::Invoice::TOTAL\n\
             p Billing::Invoice::TOTAL\n",
        );
    assert!(run.success, "{}\n{}", run.stdout, run.stderr);
    assert_eq!(
        run.stdout,
        "booted\nloading greeter\nloaded greeter hi\n\"hi\"\n\"hi\"\n\"constant\"\nloading invoice\n3\n3\n",
        "{}",
        run.stderr
    );
    let models = std::fs::read_to_string(run.emitted.join("app/models.rb")).expect("app/models.rb");
    // The constants autoload the programs, which load the declarations.
    assert!(models.contains("[:Greeter, \"models/roundhouse_files/app/services/greeter\", false]"), "{models}");
    assert!(models.contains("[:Invoice, \"models/roundhouse_files/app/services/billing/invoice\", false]"), "{models}");
    assert!(!models.contains("].each { |file, envs|"), "no program loads at boot: {models}");
    assert!(models.contains("  UNREACHED_PROGRAMS = [\n    \"models/roundhouse_files/lib/stray\",\n  ].freeze\n"), "{models}");
}

#[test]
fn eager_load_runs_every_program_once() {
    let run = emit_and_run::empty_app()
        .write("app/services/greeter.rb", "puts \"loading greeter\"\n\nclass Greeter\nend\n")
        .write("lib/stray.rb", "puts \"stray ran\"\n")
        .run_ruby("puts \"booted\"\np Greeter.name\n");
    assert!(run.success, "{}\n{}", run.stdout, run.stderr);
    let lines: Vec<&str> = run.stdout.lines().collect();
    assert_eq!(lines.iter().filter(|l| **l == "loading greeter").count(), 1, "{}", run.stdout);
    assert_eq!(lines.iter().filter(|l| **l == "stray ran").count(), 1, "{}", run.stdout);
    assert_eq!(lines.last().copied(), Some("\"Greeter\""), "{}", run.stdout);
    let booted = lines.iter().position(|l| *l == "booted").expect("booted");
    assert!(lines.iter().position(|l| *l == "loading greeter").unwrap() < booted, "{}", run.stdout);
}

#[test]
fn a_program_the_native_boot_files_require_loads_at_boot_in_their_order() {
    let run = emit_and_run::empty_app()
        .write("config/environments/development.rb", &format!("require \"dev_only\"\n{LAZY}"))
        .write("config/environments/production.rb", "require \"prod_only\"\n")
        .write(
            "config/boot.rb",
            "require_relative \"../lib/boot_flag\"\nrequire_relative \"../lib/maybe\" if ENV[\"RH_NEVER_SET\"]\nrequire \"json\"\n",
        )
        .write("config/application.rb", "require_relative \"boot\"\nrequire \"app_setup\"\n")
        .write("lib/boot_flag.rb", "puts \"boot flag\"\n\nmodule BootFlag\n  ON = true\nend\n")
        .write("lib/app_setup.rb", "puts \"app setup\"\n")
        .write("lib/dev_only.rb", "puts \"dev only\"\n")
        .write("lib/prod_only.rb", "puts \"prod only\"\n")
        .write("lib/maybe.rb", "puts \"maybe\"\n\nmodule Maybe\nend\n")
        .run_ruby("puts \"booted\"\np BootFlag::ON\np defined?(Maybe)\nMaybe\n");
    assert!(run.success, "{}\n{}", run.stdout, run.stderr);
    assert_eq!(
        run.stdout,
        "boot flag\napp setup\ndev only\nbooted\ntrue\n\"constant\"\nmaybe\n",
        "{}",
        run.stderr
    );
    let models = std::fs::read_to_string(run.emitted.join("app/models.rb")).expect("app/models.rb");
    assert!(models.contains("  [\"models/roundhouse_files/lib/boot_flag\", nil], # config/boot.rb:1\n"), "{models}");
    assert!(models.contains("  [\"models/roundhouse_files/lib/prod_only\", [\"production\"]], # config/environments/production.rb:1\n"), "{models}");
    assert!(
        models.contains("    \"models/roundhouse_files/lib/maybe\" => \"config/boot.rb:2\",\n"),
        "a conditional boot require is named: {models}"
    );
}

#[test]
fn bundler_require_loads_the_entry_program_of_each_carried_gem_its_groups_and_require_select() {
    let lock = "PATH\n  remote: gems/widget\n  specs:\n    widget (0.1)\n\nPATH\n  remote: gems/prodgem\n  specs:\n    prodgem (0.1)\n\nPATH\n  remote: gems/quiet\n  specs:\n    quiet (0.1)\n\nPATH\n  remote: gems/named-gem\n  specs:\n    named-gem (0.1)\n\nGEM\n  remote: https://rubygems.org/\n  specs:\n\nPLATFORMS\n  ruby\n\nDEPENDENCIES\n  named-gem!\n  prodgem!\n  quiet!\n  widget!\n";
    let run = emit_and_run::empty_app()
        .write("config/environments/development.rb", LAZY)
        .write(
            "Gemfile",
            "source \"https://rubygems.org\"\n\ndef local_gem(name, require: name)\n  gem name, path: \"gems/#{name}\", require: require\nend\n\ngem \"widget\", path: \"gems/widget\"\ngroup :production do\n  gem \"prodgem\", path: \"gems/prodgem\"\nend\ngem(\n  \"quiet\",\n  path: \"gems/quiet\",\n  require: false,\n)\nlocal_gem \"named-gem\", require: \"named\"\n",
        )
        .write("Gemfile.lock", lock)
        .write("config/application.rb", "Bundler.require(*Rails.groups)\n")
        .write("gems/widget/lib/widget.rb", "puts \"widget entry\"\n\nmodule Widget\nend\n")
        .write("gems/prodgem/lib/prodgem.rb", "puts \"prodgem entry\"\n\nmodule Prodgem\nend\n")
        .write("gems/quiet/lib/quiet.rb", "puts \"quiet entry\"\n\nmodule Quiet\nend\n")
        .write("gems/named-gem/lib/named.rb", "puts \"named entry\"\n\nmodule Named\nend\n")
        .run_ruby("puts \"booted\"\np defined?(Quiet)\n");
    assert!(run.success, "{}\n{}", run.stdout, run.stderr);
    // `require: false` keeps quiet's entry unloaded, and nothing
    // autoloads a plain gem's `lib/`: natively `Quiet` stays undefined
    // until the app requires "quiet".
    assert_eq!(run.stdout, "widget entry\nnamed entry\nbooted\nnil\n", "{}", run.stderr);
    let models = std::fs::read_to_string(run.emitted.join("app/models.rb")).expect("app/models.rb");
    assert!(
        models.contains("  [\"models/roundhouse_files/gems/prodgem/lib/prodgem\", [\"production\"]], # config/application.rb:1 Bundler.require (Gemfile:9)\n"),
        "{models}"
    );
}

/// essentials' high_throughput_local_cache.rb: a file named for its own
/// constant that also reopens a runtime namespace (ActiveSupport's
/// LocalCache) to define a module in it. Zeitwerk autoloads only the
/// constant the path names; the reopened namespace's module is defined
/// when that file loads, so the program is no autoload of it.
#[test]
fn a_program_that_also_reopens_a_runtime_namespace_is_autoloaded_only_by_the_constant_its_path_names() {
    let run = emit_and_run::empty_app()
        .write("config/environments/development.rb", LAZY)
        .write(
            "lib/local_cache_patch.rb",
            "require \"securerandom\"\n\nmodule LocalCachePatch\n  def self.on\n    Comparable::RhStrategy.name\n  end\nend\n\nmodule Comparable\n  module RhStrategy\n    def rh_strategy\n      :local\n    end\n  end\nend\n\nputs \"patched\"\n",
        )
        .run_ruby("puts \"booted\"\np LocalCachePatch.on\n");
    assert!(run.success, "{}\n{}", run.stdout, run.stderr);
    assert!(run.stdout.ends_with("\"Comparable::RhStrategy\"\n"), "{}\n{}", run.stdout, run.stderr);
    let models = std::fs::read_to_string(run.emitted.join("app/models.rb")).expect("app/models.rb");
    assert!(models.contains("[:LocalCachePatch, \"models/roundhouse_files/lib/local_cache_patch\", true]"), "{models}");
    assert!(!models.contains("[:RhStrategy, \"models/roundhouse_files/"), "{models}");
}

/// Core's verdict/experiment.rb includes Verdict::BucketHash, whose
/// source (bucket_hash.rb) also runs a statement, so a program loads its
/// declarations. The load-time dependency loads that program, not the
/// declarations file directly: loaded directly, its `module Helping`
/// fired Helping's autoload (the program) mid-file, the program's
/// require of the file was a circular no-op, and Helping was undefined.
#[test]
fn a_load_time_dependency_on_a_programs_constant_loads_the_program() {
    let run = emit_and_run::empty_app()
        .write("config/environments/development.rb", LAZY)
        .write("app/services/helping.rb", "puts \"loading helping\"\n\nmodule Helping\n  def helped\n    :helped\n  end\nend\n")
        .write("app/services/consumer.rb", "class Consumer\n  include Helping\nend\n")
        .run_ruby("puts \"booted\"\np Consumer.new.helped\n");
    assert!(run.success, "{}\n{}", run.stdout, run.stderr);
    assert_eq!(run.stdout, "booted\nloading helping\n:helped\n", "{}", run.stderr);
    let user = std::fs::read_to_string(run.emitted.join("app/models/consumer.rb")).expect("consumer.rb");
    assert!(user.contains("RoundhouseAutoload.require_through(\"models/roundhouse_files/app/services/helping\", \"models/helping\")"), "{user}");
}

/// essentials' lib/podding/patches/active_record.rb reopens the runtime's
/// ActiveRecord: its path names Podding::Patches::ActiveRecord, so it is
/// no autoload of ActiveRecord (which, already defined, would have loaded
/// the patch at registration, before boot).
#[test]
fn a_patch_file_named_for_the_constant_it_reopens_is_not_that_constants_file() {
    let run = emit_and_run::empty_app()
        .write("config/environments/development.rb", LAZY)
        .write(
            "lib/podding/patches/comparable.rb",
            "puts \"patched\"\n\nmodule Comparable\n  def rh_patched\n    :patched\n  end\nend\n",
        )
        .run_ruby("puts \"booted\"\n");
    assert!(run.success, "{}\n{}", run.stdout, run.stderr);
    assert_eq!(run.stdout, "booted\n", "{}", run.stderr);
    let models = std::fs::read_to_string(run.emitted.join("app/models.rb")).expect("app/models.rb");
    assert!(!models.contains("[:Comparable, \"models/roundhouse_files/"), "{models}");
}

/// observe-instrument defines Observe before app/models.rb; essentials'
/// lib/observe.rb reopens it and runs requires. Native boot loads that
/// file where its boot files require it, after Bundler.require's gem
/// entries, so registration (which loads an already-defined constant's
/// file) waits for the boot list instead of running it first.
#[test]
fn an_already_defined_constants_program_loads_after_native_boots_requires() {
    let run = emit_and_run::empty_app()
        .write("config/environments/development.rb", LAZY)
        .write("config/boot.rb", "require_relative \"../lib/boot_first\"\n")
        .write("lib/boot_first.rb", "puts \"boot first\"\n")
        .write(
            "lib/comparable.rb",
            "puts \"comparable reopened\"\n\nmodule Comparable\n  def rh_reopened\n    :reopened\n  end\nend\n",
        )
        .run_ruby("puts \"booted\"\np 1.rh_reopened\n");
    assert!(run.success, "{}\n{}", run.stdout, run.stderr);
    assert_eq!(run.stdout, "boot first\ncomparable reopened\nbooted\n:reopened\n", "{}", run.stderr);
}

/// panama_client.rb requires panama_client/railtie, a program (it runs
/// `require "rails/railtie"`) and PanamaClient::Railtie's autoload. Loaded
/// by that plain require, its declarations' `class Railtie` resolved
/// through the autoload, re-required the in-flight program and warned
/// "Expected ... to define PanamaClient::Railtie but it didn't", which
/// Core's StrictWarning ($VERBOSE, Warning#warn raising) turns fatal.
/// Also: a Roundhouse diagnostic is no Ruby warning for StrictWarning.
#[test]
fn a_program_required_before_its_constant_is_referenced_defines_it_without_warnings() {
    let run = emit_and_run::empty_app()
        .write("config/environments/development.rb", LAZY)
        .write("config/boot.rb", "require_relative \"../lib/strict\"\n")
        .write("config/application.rb", "require_relative \"boot\"\nrequire \"hooked\"\nrequire \"metadata\"\n")
        .write(
            "lib/strict.rb",
            "$VERBOSE = true\n\nmodule Strict\n  def warn(msg, category: nil, **)\n    raise \"strict: #{msg}\" if msg.include?(\"circular require\") || msg.include?(\"to define\") || msg.include?(\"roundhouse:\")\n\n    super\n  end\nend\nWarning.extend(Strict)\n",
        )
        // A statement that fails is named on stderr, not through
        // Warning.warn, which StrictWarning raises from.
        .write("lib/hooked.rb", "require \"hooked/hook\"\nrequire \"rh_absent_feature\"\n\nmodule Hooked\nend\n")
        .write("lib/hooked/hook.rb", "require \"json\"\n\nmodule Hooked\n  class Hook\n    ON = :hooked\n  end\nend\n")
        // panama_metadata.rb: the namespace is declared (registering
        // Schema's autoload) before the file requires schema.rb.
        .write("lib/metadata.rb", "module Metadata\nend\n\nrequire \"metadata/schema\"\n")
        .write("lib/metadata/schema.rb", "require \"json\"\n\nmodule Metadata\n  class Schema\n    ON = :schema\n  end\nend\n")
        .run_ruby("puts \"booted\"\np Hooked::Hook::ON\np Metadata::Schema::ON\n");
    assert!(run.success, "{}\n{}", run.stdout, run.stderr);
    assert_eq!(run.stdout, "booted\n:hooked\n:schema\n", "{}", run.stderr);
    assert!(!run.stderr.contains("circular require") && !run.stderr.contains("to define"), "{}", run.stderr);
    assert!(run.stderr.contains("error[file_statement_not_run]: the file-level statement at lib/hooked.rb:2"), "{}", run.stderr);
}

/// shopify-money's lib/money.rb (Bundler.require's entry) requires
/// money/money.rb, which declares Money but is no path match for it, so
/// Money autoloads its declarations file; that file first requires
/// money/null_currency.rb (a class-body dependency), whose `class Money`
/// resolved the autoload into the in-flight declarations file: a
/// circular require, fatal under Core's StrictWarning.
#[test]
fn a_declarations_file_required_while_its_constant_autoloads_it_does_not_re_enter_itself() {
    let run = emit_and_run::empty_app()
        .write("config/environments/development.rb", LAZY)
        .write("config/boot.rb", "require_relative \"../lib/strict\"\n")
        .write("config/application.rb", "require_relative \"boot\"\nrequire \"purse\"\n")
        .write(
            "lib/strict.rb",
            "$VERBOSE = true\n\nmodule Strict\n  def warn(msg, category: nil, **)\n    raise \"strict: #{msg}\" if msg.include?(\"circular require\") || msg.include?(\"to define\")\n\n    super\n  end\nend\nWarning.extend(Strict)\n",
        )
        .write("lib/purse.rb", "require \"purse/purse\"\nrequire \"purse/null\"\nputs \"purse entry\"\n")
        .write("lib/purse/purse.rb", "class Purse\n  NULL = Purse::Null.new\nend\n")
        .write("lib/purse/null.rb", "class Purse\n  class Null\n    def kind\n      :null\n    end\n  end\nend\n")
        .run_ruby("puts \"booted\"\np Purse::NULL.kind\n");
    assert!(run.success, "{}\n{}", run.stdout, run.stderr);
    assert_eq!(run.stdout, "purse entry\nbooted\n:null\n", "{}", run.stderr);
    assert!(!run.stderr.contains("circular require") && !run.stderr.contains("to define"), "{}", run.stderr);
}

/// secure_headers.rb (a program, SecureHeaders' autoload) requires
/// headers/content_security_policy, whose policy_management depends on
/// SecureHeaders: that dependency loads through the program, which is
/// the file loading now. Natively nothing requires secure_headers.rb
/// back; requiring it again was a circular require (StrictWarning).
#[test]
fn a_dependency_on_a_program_loading_now_loads_its_declarations_not_the_program_again() {
    let run = emit_and_run::empty_app()
        .write("config/environments/development.rb", LAZY)
        .write("config/boot.rb", "require_relative \"../lib/strict\"\n")
        .write("config/application.rb", "require_relative \"boot\"\nrequire \"shield\"\n")
        .write(
            "lib/strict.rb",
            "$VERBOSE = true\n\nmodule Strict\n  def warn(msg, category: nil, **)\n    raise \"strict: #{msg}\" if msg.include?(\"circular require\") || msg.include?(\"to define\")\n\n    super\n  end\nend\nWarning.extend(Strict)\n",
        )
        .write("lib/shield.rb", "module Shield\n  VERSION = \"1.0\"\nend\n\nrequire \"shield/policy\"\nputs \"shield entry\"\n")
        .write("lib/shield/policy.rb", "module Shield\n  module Policy\n    DEFAULT = \"v#{Shield::VERSION}\"\n  end\nend\n")
        .run_ruby("puts \"booted\"\np Shield::Policy::DEFAULT\n");
    assert!(run.success, "{}\n{}", run.stdout, run.stderr);
    assert_eq!(run.stdout, "shield entry\nbooted\n\"v1.0\"\n", "{}", run.stderr);
    assert!(!run.stderr.contains("circular require"), "{}", run.stderr);
}
