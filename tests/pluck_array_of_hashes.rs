//! `pluck(:key)` on an Array of Hashes (`T::Array[{ line_item: .. }]`):
//! ActiveSupport's `Enumerable#pluck` reads `row[:key]`. Core's
//! `CalculatedLineItemsBuilderBase.build_line_items_with_changes` plucks
//! `:line_item` off its changeset containers; the projection was written
//! as a reader call, `undefined method 'line_item' for an instance of Hash`.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

const BUILDER: &str = r##"class Builder
  #: (Array[{ name: String, size: Integer }] rows) -> Array[String]
  def names(rows)
    rows.pluck(:name)
  end
end
"##;

#[test]
fn pluck_on_an_array_of_hashes_reads_the_key() {
    emit_and_run::empty_app()
        .write("app/models/builder.rb", BUILDER)
        .run_ruby(
            r##"
got = Builder.new.names([{ name: "a", size: 1 }, { name: "b", size: 2 }])
raise got.inspect unless got == ["a", "b"]
puts "ALL OK"
"##,
        )
        .assert_passes();
}
