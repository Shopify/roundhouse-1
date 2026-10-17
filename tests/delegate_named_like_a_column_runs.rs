//! A `delegate` whose name is also a column overrides the generated reader.
//!
//! Core's `Order` delegates `total_received` (and two more) to its
//! financial calculations service while `orders.total_received` is a
//! column. Rails defines the column readers in the generated attribute
//! methods module, so the class's delegate wins. The Ruby emit kept the
//! column reader and dropped the forwarder, so the reader answered the
//! stored `BigDecimal` where callers expect the service's money.

#[path = "support/emit_and_run.rs"]
mod emit_and_run;

const DECORATOR: &str = "class Decorator
  def initialize(record)
    @record = record
  end

  def title(suffix = \"\")
    \"decorated#{suffix}\"
  end
end
";

const MODEL: &str = "class Headline < ApplicationRecord
  self.table_name = \"articles\"

  delegate :title, to: :decorator

  def decorator
    Decorator.new(self)
  end

  def loud
    title(\"!\")
  end
end
";

#[test]
fn a_delegate_named_like_a_column_overrides_the_column_reader() {
    emit_and_run::real_blog()
        .write("app/models/decorator.rb", DECORATOR)
        .write("app/models/headline.rb", MODEL)
        .run_ruby(
            r##"
h = Headline.new(title: "raw")
raise h.title.inspect unless h.title == "decorated"
raise h.loud.inspect unless h.loud == "decorated!"
puts "delegate over column ok"
"##,
        )
        .assert_passes();
}
