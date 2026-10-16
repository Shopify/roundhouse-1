//! A Concern's `included do` that includes ActiveModel::AttributeMethods and
//! registers `attribute_method_suffix '='`, Core's KeyValueModel gem
//! (`KeyValueModel::AttributeMethods`, under `CartNext`):
//!
//! ```ruby
//! included do
//!   include ActiveModel::AttributeMethods
//!   attribute_method_suffix '='
//! end
//! ```
//!
//! The emit dropped the suffix registration (`attribute_method_suffix in
//! included do ... is not modeled on a plain`), so no `attribute :x` had a
//! writer: `undefined method 'shop_id=' for an instance of CartNext`.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

const CONCERN: &str = r##"# typed: true
module Kv
  module AttributeMethods
    extend ActiveSupport::Concern

    included do
      include ActiveModel::AttributeMethods
      attribute_method_suffix "="
    end

    module ClassMethods
      def columns
        @columns ||= []
      end

      def attribute(name, type, options = {})
        columns << name.to_s
        define_attribute_method(name)
      end
    end

    def attributes
      @attributes.dup
    end

    private

    def attribute(name)
      @attributes[name.to_s]
    end

    def attribute=(name, value)
      @attributes[name.to_s] = value
    end
  end
end
"##;

const BASE: &str = r##"# typed: true
module Kv
  class Base
    include Kv::AttributeMethods

    def initialize
      @attributes = {}
    end
  end
end
"##;

const CART: &str = r##"# typed: true
class Cart < Kv::Base
  attribute :shop_id, Integer, null: false
  attribute :name, String

  def initialize(shop_id:)
    super()
    self.shop_id = shop_id
    self.name = "cart-#{shop_id}"
  end
end
"##;

/// The PostgreSQL CRuby tree carries the ActiveModel::AttributeMethods port
/// (`active_model_ports_carried`); `database_adapter: postgresql` is what
/// makes this probe's emit that tree.
fn overlay() -> emit_and_run::Overlay {
    emit_and_run::empty_app()
        .write("roundhouse.yml", "database_adapter: postgresql\n")
        .write("app/models/concerns/kv/attribute_methods.rb", CONCERN)
        .write("app/models/kv/base.rb", BASE)
        .write("app/models/cart.rb", CART)
}

#[test]
fn an_included_block_attribute_method_suffix_is_written_after_the_port_include() {
    let (emitted, errors) = overlay().emit(roundhouse::project::BuildTarget::Ruby);
    let errors: Vec<&String> = errors.iter().filter(|e| !e.contains("database_adapter not supported")).collect();
    assert!(errors.is_empty(), "{errors:?}");
    let base = std::fs::read_to_string(emitted.join("app/models/kv/base.rb")).unwrap();
    let include = base.find("include ActiveModel::AttributeMethods").unwrap_or_else(|| panic!("no port include in:\n{base}"));
    let suffix = base.find("attribute_method_suffix \"=\"").unwrap_or_else(|| panic!("no suffix registration in:\n{base}"));
    assert!(include < suffix, "the suffix is registered before the port is included in:\n{base}");
}
