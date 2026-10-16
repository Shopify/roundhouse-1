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
