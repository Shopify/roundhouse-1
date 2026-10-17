//! `Klass.new(**schema, k: v)` where `schema` is an object of an app class
//! that defines `to_hash` (Core's `Component::Schema` inputs, in
//! `Sales::OrderEditing::Express::DecrementFulfilledLineItem#call`). The
//! ingest desugar merges the pair into the receiver; the receiver must be
//! read through `to_hash` first, as for a splat inside a Hash literal.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

const BASE: &str = r##"# frozen_string_literal: true

class SchemaBase
  class << self
    alias_method :super_new, :new
    private :super_new

    # A schema takes its attributes as one positional Hash.
    def new(args)
      super_new(args)
    end
  end

  def initialize(args)
    @attrs = args.to_h
  end

  def [](key)
    @attrs[key]
  end

  def to_h
    @attrs.dup
  end
  alias_method :to_hash, :to_h
end

"##;

const CALLER: &str = r##"# frozen_string_literal: true

module Sales
  module Express
    module Inputs
      class DecrementFulfilledLineItem < SchemaBase
        #: -> Integer?
        def line_item_id
          @attrs[:line_item_id]
        end
      end
    end

    class DecrementFulfilledLineItem
      # @param schemas [Inputs::DecrementFulfilledLineItem]
      # @return [Array<Inputs::DecrementFulfilledLineItem>]
      #: (
      #|   schemas: Array[Inputs::DecrementFulfilledLineItem],
      #|   order_id: Integer
      #| ) -> Array[Inputs::DecrementFulfilledLineItem]
      def call(schemas:, order_id:)
        schemas.map do |schema|
          if schema.line_item_id.nil?
            schema = Inputs::DecrementFulfilledLineItem.new(
              **schema,
              line_item_id: order_id
            )
          end
          schema
        end
      end
    end
  end
end
"##;

#[test]
fn a_splatted_app_object_in_new_kwargs_is_read_through_to_hash() {
    emit_and_run::empty_app()
        .write("app/models/schema_base.rb", BASE)
        .write("app/models/decrementer.rb", CALLER)
        .run_ruby(
            r#"
out = Sales::Express::DecrementFulfilledLineItem.new.call(schemas: [Sales::Express::Inputs::DecrementFulfilledLineItem.new(delta: 1)], order_id: 7)
raise out.inspect unless out.first.to_h == { delta: 1, line_item_id: 7 }
puts "passed"
"#,
        )
        .assert_passes();
}
