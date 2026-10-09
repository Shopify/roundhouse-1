//! The library emit writes each class's body in the order native load
//! order runs it.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

const LAZY: &str = "Rails.application.configure do\n  config.eager_load = false\nend\n";

/// Core's StrictWarning: $VERBOSE on, every Ruby warning about a
/// circular require (or an autoload that did not define its constant)
/// raises.
const STRICT: &str = "$VERBOSE = true\n\nmodule Strict\n  def warn(msg, category: nil, **)\n    raise \"strict: #{msg}\" if msg.include?(\"circular require\") || msg.include?(\"to define\")\n\n    super\n  end\nend\nWarning.extend(Strict)\n";

const BASE: &str = "class Base\n  def self.field(name, type)\n    (@fields ||= []) << [name, type]\n  end\n\n  def self.fields\n    @fields\n  end\nend\n";

/// GraphApiTestSchemaHelper::UpdateProduct::Input: a class nested two
/// levels below its file's owner, written above the statement of its
/// enclosing class that reads it (`field :variants, [VariantInput]`).
/// Emitted as a block of its own after the enclosing class's, the read
/// raised NameError as the file loaded.
#[test]
fn a_class_nested_in_a_nested_class_is_defined_before_its_enclosing_body_reads_it() {
    let run = emit_and_run::empty_app()
        .write("config/environments/development.rb", LAZY)
        .write("lib/base.rb", BASE)
        .write(
            "lib/helper.rb",
            "module Helper\n  class Update < Base\n    class Input < Base\n      class VariantInput < Base\n        field :options, :string\n      end\n\n      field :name, :string\n      field :variants, [VariantInput]\n    end\n\n    field :input, Input\n  end\nend\n",
        )
        .run_ruby("p Helper::Update.fields\np Helper::Update::Input.fields\np Helper::Update::Input::VariantInput.fields\n");
    assert!(run.success, "{}\n{}", run.stdout, run.stderr);
    assert_eq!(
        run.stdout,
        "[[:input, Helper::Update::Input]]\n[[:name, :string], [:variants, [Helper::Update::Input::VariantInput]]]\n[[:options, :string]]\n",
        "{}",
        run.stderr
    );
}

/// Core's price_list_prices/service.rb and rate_repository/pricing.rb:
/// a case/in arm whose constant the tree does not carry
/// (`in ::GraphApi::MoneyInput`, `in Partner[Partner::RateType::Flow]`).
/// The refusal was written bare, `in (raise "…")`, which does not parse.
/// Pinned it parses: the arms before it match, and testing it raises the
/// named refusal where Ruby would raise NameError.
#[test]
fn an_uncarried_constant_in_a_pattern_is_a_parseable_named_refusal() {
    let run = emit_and_run::empty_app()
        .write("config/environments/development.rb", LAZY)
        .write(
            "lib/conv.rb",
            "module Conv\n  def self.of(input)\n    case input\n    in Integer then input\n    in ::Missing::MoneyInput then :money\n    in Missing::Partner[Missing::Partner::Flow] then :flow\n    in { amount: } then amount\n    end\n  end\nend\n",
        )
        .run_ruby("p Conv.of(1)\nbegin\n  Conv.of({ amount: 2 })\nrescue RuntimeError => e\n  puts e.message\nend\n");
    assert!(run.success, "{}\n{}", run.stdout, run.stderr);
    assert_eq!(run.stdout, "1\nroundhouse: constant not supported (all targets)\n", "{}", run.stderr);
    let conv = std::fs::read_to_string(run.emitted.join("app/models/conv.rb")).expect("conv.rb");
    assert!(!conv.contains("in (raise"), "{conv}");
}

const LEGACY: &str = "module Vault\n  module Legacy\n    def self.for(key) = MAPPING.fetch(key)\n\n    def kind = INVERTED.fetch(self.class)\n\n    MAPPING = {\n      \"a\" => Vault::A,\n      \"b\" => Vault::B,\n    }.freeze\n\n    INVERTED = MAPPING.invert\n  end\nend\n";

fn vault() -> emit_and_run::Overlay {
    emit_and_run::empty_app()
        .write("config/environments/development.rb", LAZY)
        .write("config/boot.rb", "require_relative \"../lib/strict\"\n")
        .write("config/application.rb", "require_relative \"boot\"\n")
        .write("lib/strict.rb", STRICT)
        .write("db/schema.rb", "ActiveRecord::Schema.define do\n  create_table :alphas do |t|\n    t.string :name\n  end\n  create_table :betas do |t|\n    t.string :name\n  end\nend\n")
        .write("app/models/application_record.rb", "class ApplicationRecord < ActiveRecord::Base\n  primary_abstract_class\nend\n")
        .write("app/models/vault/legacy.rb", LEGACY)
        .write("app/models/vault/a.rb", "module Vault\n  class A < ApplicationRecord\n    self.table_name = \"alphas\"\n    include ::Vault::Legacy\n  end\nend\n")
        .write("app/models/vault/b.rb", "module Vault\n  class B < ApplicationRecord\n    self.table_name = \"betas\"\n    include ::Vault::Legacy\n  end\nend\n")
}

/// Core's payments/vault: LegacyPaymentInstrument's `CLASS_MAPPING` reads
/// the models that include it. Natively each model's class is open when
/// its `include` autoloads the module, and the module is open when its
/// constant autoloads the models, so no file is required while it loads.
/// Required at the tops of the files, before either header ran, each
/// file required the other back: Ruby's circular-require warning, which
/// Core's StrictWarning raises. Whichever file loads first.
#[test]
fn a_module_whose_constant_reads_its_including_models_loads_without_a_cycle() {
    for script in [
        "p Vault::A.new.kind\np Vault::Legacy.for(\"b\")\n",
        "p Vault::Legacy.for(\"b\")\np Vault::A.new.kind\n",
        "p Vault::B.new.kind\np Vault::Legacy.for(\"b\")\n",
    ] {
        let run = vault().run_ruby(script);
        assert!(run.success, "{script}\n{}\n{}", run.stdout, run.stderr);
        assert!(!run.stderr.contains("circular require"), "{}", run.stderr);
        let expected = if script.starts_with("p Vault::B") { "\"b\"\nVault::B\n" } else if script.starts_with("p Vault::A") { "\"a\"\nVault::B\n" } else { "Vault::B\n\"a\"\n" };
        assert_eq!(run.stdout, expected, "{script}\n{}", run.stderr);
    }
}
