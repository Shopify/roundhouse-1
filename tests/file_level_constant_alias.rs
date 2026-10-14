//! A file whose whole program is `A::B = C` (a constant alias at file
//! level) is carried, not dropped.
//!
//! Core's payments_partners/inputs/*.rb are twelve of them, e.g.
//! `PaymentsPartners::Inputs::ShippingLineItem = PaymentsPartners::
//! ShippingLineItem`. The ingest carried `A::B = call(...)` and
//! `A::B = Class.new(...)` but not a constant read, so no file was
//! emitted and each read of the alias was `uninitialized constant
//! PaymentsPartners::Inputs::ShippingLineItem` (3 ids in unit-1000-v2).

#[path = "support/emit_and_run.rs"]
mod emit_and_run;

const ADDRESS: &str = r#"module PaymentsPartners
  class Address
    attr_reader :city

    def initialize(city:)
      @city = city
    end
  end
end
"#;

const ALIAS: &str = r#"# typed: strict
# frozen_string_literal: true

PaymentsPartners::Inputs::Address = PaymentsPartners::Address
"#;

const INPUTS: &str = r#"module PaymentsPartners
  module Inputs
  end
end
"#;

const BUILDER: &str = r#"module PaymentsPartners
  class Builder
    def self.build
      PaymentsPartners::Inputs::Address.new(city: "Ottawa")
    end
  end
end
"#;

#[test]
fn a_file_level_constant_alias_is_emitted_and_resolves() {
    let run = emit_and_run::empty_app()
        .write("app/models/payments_partners/address.rb", ADDRESS)
        .write("app/models/payments_partners/inputs.rb", INPUTS)
        .write("app/models/payments_partners/inputs/address.rb", ALIAS)
        .write("app/models/payments_partners/builder.rb", BUILDER)
        .run_ruby(
            "a = PaymentsPartners::Builder.build\n\
             puts a.city\n\
             puts a.class\n\
             puts PaymentsPartners::Inputs::Address.equal?(PaymentsPartners::Address)\n",
        );
    run.assert_passes();
    assert_eq!(run.stdout.trim(), "Ottawa\nPaymentsPartners::Address\ntrue", "{}", run.stdout);
}
