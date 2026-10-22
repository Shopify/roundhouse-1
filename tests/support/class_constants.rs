//! A class whose body declares a constant of each literal kind, and a
//! class method reading them all (#1223).

use super::emit_and_run::{self, Overlay};

pub const SCALE: &str = r#"class Scale
  WEIGHTS = { "small" => 1, "large" => 9 }.freeze
  NAMES = %w[small large].freeze
  LABEL = "scale"
  LIMIT = 9
  RATIO = 1.5
  FLAGS = [true, false, nil].freeze
  NESTED = { "a" => [1, 2], "b" => { "c" => "d" } }.freeze

  def self.summary
    [WEIGHTS.fetch("large", 0), NAMES.first, LABEL, LIMIT, RATIO, FLAGS.length, NESTED["b"]["c"], NESTED["a"][1]].join(",")
  end
end
"#;

/// What `Scale.summary` answers under CRuby.
#[allow(dead_code)]
pub const SUMMARY: &str = "9,small,scale,9,1.5,3,d,2";

pub fn overlay() -> Overlay {
    emit_and_run::real_blog().write("app/models/scale.rb", SCALE)
}
