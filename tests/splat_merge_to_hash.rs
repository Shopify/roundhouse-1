//! `Klass.new(**schema, k: v)` where `schema` is an object that answers
//! `to_hash` (Core's Component::Schema inputs): Ruby converts the value
//! with `to_hash` for the `**`. The ingest desugar wrote it as
//! `schema.merge({ k: v })`, a `merge` the object does not have
//! (`undefined method 'merge' for an instance of
//! Sales::OrderEditing::Express::Inputs::DecrementFulfilledLineItem`).
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

const SOURCE: &str = r#"
class Amounts
  attr_reader :values

  def initialize(**values)
    @values = values
  end

  def to_hash
    @values.dup
  end
end

class Total
  attr_reader :fields

  def initialize(base: 0, extra: 0, other: 0)
    @fields = { base: base, extra: extra, other: other }
  end
end

class Totals
  #: (Amounts amounts) -> Total
  def self.with_extra(amounts)
    Total.new(**amounts, extra: 5)
  end

  #: (Amounts amounts) -> Hash[Symbol, Integer]
  def self.literal_after(amounts)
    { **amounts, extra: 7 }
  end

  #: (Hash[Symbol, Integer] hash) -> Hash[Symbol, Integer]
  def self.hash_base(hash)
    { **hash, extra: 9 }
  end
end
"#;

#[test]
fn a_splatted_object_converts_through_to_hash() {
    emit_and_run::real_blog()
        .write("app/models/totals.rb", SOURCE)
        .run_ruby(
            r#"
amounts = Amounts.new(base: 1, other: 2)
raise "kwargs splat: #{Totals.with_extra(amounts).fields}" unless Totals.with_extra(amounts).fields == { base: 1, extra: 5, other: 2 }
raise "literal splat: #{Totals.literal_after(amounts)}" unless Totals.literal_after(amounts) == { base: 1, other: 2, extra: 7 }
raise "hash base" unless Totals.hash_base({ base: 3 }) == { base: 3, extra: 9 }
puts "ok"
"#,
        )
        .assert_passes();
}

const UNSIGNED: &str = r#"
class Inputs
  attr_reader :values

  def initialize(**values)
    @values = values
  end

  def to_hash
    @values.dup
  end
end

class Decrement
  def self.call(schemas:)
    new.call(schemas: schemas)
  end

  # No signature: `schemas` and the block's `schema` are untyped, so the
  # analysis cannot name the splatted value.
  def call(schemas:)
    schemas.map do |schema|
      Inputs.new(**schema, line_item_id: 7)
    end
  end
end
"#;

/// `schema` has no type the analysis can name; Ruby's `**` still asks it
/// for `to_hash` (Core's `Express::DecrementFulfilledLineItem#call`).
#[test]
fn an_untyped_splatted_object_converts_through_to_hash() {
    emit_and_run::real_blog()
        .write("app/models/decrement.rb", UNSIGNED)
        .run_ruby(
            r#"
inputs = Inputs.new(fulfillment_line_item_id: 3)
out = Decrement.call(schemas: [inputs, { fulfillment_line_item_id: 4 }])
raise out.map(&:values).inspect unless out.map(&:values) == [{ fulfillment_line_item_id: 3, line_item_id: 7 }, { fulfillment_line_item_id: 4, line_item_id: 7 }]
puts "ok"
"#,
        )
        .assert_passes();
}

const REASSIGNED: &str = r#"
class Inputs
  attr_reader :values

  def initialize(**values)
    @values = values
  end

  def to_hash
    @values.dup
  end
end

class Wider
  attr_reader :values

  def initialize(**values)
    @values = values
  end

  def to_hash
    @values.dup
  end
end

class Decrement
  #: (schemas: Array[Inputs], lookup: Hash[Integer, Integer]) -> Array[Wider]
  def call(schemas:, lookup:)
    schemas.map do |schema|
      found = lookup[schema.values[:id]]
      return [nil] if found.nil?

      if schema.values[:line_item_id].present?
        return [nil] unless found == schema.values[:line_item_id]
      else
        schema = Wider.new(
          **schema,
          line_item_id: found
        )
      end

      Wider.new(**schema)
    end
  end
end
"#;

/// `schema` is a typed block parameter reassigned from the splat in the
/// same block, so the splatted read carries the union of both writes
/// (Core's `Express::DecrementFulfilledLineItem#call` with its signature).
#[test]
fn a_reassigned_typed_splatted_object_converts_through_to_hash() {
    emit_and_run::real_blog()
        .write("app/models/decrement.rb", REASSIGNED)
        .run_ruby(
            r#"
out = Decrement.new.call(schemas: [Inputs.new(id: 3)], lookup: { 3 => 7 })
raise out.map(&:values).inspect unless out.map(&:values) == [{ id: 3, line_item_id: 7 }]
puts "ok"
"#,
        )
        .assert_passes();
}

const UNION: &str = r#"
class Inputs
  attr_reader :values

  def initialize(**values)
    @values = values
  end

  def to_hash
    @values.dup
  end
end

class Other < Inputs
end

class Combine
  #: ((Inputs | Other) schema) -> Inputs
  def self.union_param(schema)
    Inputs.new(**schema, line_item_id: 7)
  end

  #: (Array[Inputs] schemas, Integer found) -> Array[Inputs]
  def self.each_reassigned(schemas, found)
    schemas.map do |schema|
      schema = Other.new(**schema, line_item_id: found) if schema.values[:id] == 3
      Inputs.new(**schema, extra: 1)
    end
  end

  #: (Inputs? schema) -> Inputs
  def self.nilable(schema)
    Inputs.new(**schema.not_nil!, line_item_id: 7)
  end
end
"#;

#[test]
fn a_union_typed_splatted_object_converts_through_to_hash() {
    emit_and_run::real_blog()
        .write("app/models/combine.rb", UNION)
        .run_ruby(
            r#"
a = Inputs.new(id: 3)
raise Combine.union_param(a).values.inspect unless Combine.union_param(a).values == { id: 3, line_item_id: 7 }
raise Combine.each_reassigned([a], 9).map(&:values).inspect unless Combine.each_reassigned([a], 9).map(&:values) == [{ id: 3, line_item_id: 9, extra: 1 }]
raise "nilable" unless Combine.nilable(a).values == { id: 3, line_item_id: 7 }
puts "ok"
"#,
        )
        .assert_passes();
}
