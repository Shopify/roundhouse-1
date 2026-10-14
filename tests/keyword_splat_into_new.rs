//! `new(**attributes)` into `initialize(**attributes)`, the hash built in
//! the caller: Core's MoneyBag::Variadic.zero. Ruby answers `a_ctx,b_ctx`.

#[path = "support/emit_and_run.rs"]
mod emit_and_run;

const BAG: &str = r##"class Bag
  def self.zero(**currencies)
    attributes = currencies.each_with_object({}) do |(name, value), memo|
      memo["#{name}_ctx".to_sym] = value
    end
    new(**attributes)
  end

  def initialize(**attributes)
    @attributes = attributes
  end

  def names
    @attributes.keys.join(",")
  end
end
"##;

#[test]
fn a_built_hash_splats_into_keyword_new() {
    emit_and_run::empty_app()
        .write("app/models/bag.rb", BAG)
        .run_ruby(
            r##"
text = Bag.zero(a: 1, b: 2).names
raise "got #{text.inspect}" unless text == "a_ctx,b_ctx"
puts "ALL OK"
"##,
        )
        .assert_passes();
}
