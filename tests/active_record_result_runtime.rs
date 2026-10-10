// ActiveRecord::Result (runtime/ruby/active_record/connection.rb) against
// the Rails native Core runs (Shopify/rails 5813559f4aed, 8.2.0.alpha;
// tests/support/rails_oracle.rb). tests/active_record_result_runtime.rb
// builds Results through both under a prepended `initialize` shaped like
// Core's ActiveRecordShopScopedResult and requires every observation
// equal. The runtime built `Result.new(rows)` with one argument, so that
// prepend raised `wrong number of arguments (given 1, expected 2..3)` from
// `Connection#execute` (unit-1000's PlaybookActionTest ids).

use std::path::Path;
use std::process::Command;

fn root() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

#[test]
fn result_answers_like_rails() {
    let out = Command::new("ruby")
        .arg(root().join("tests/active_record_result_runtime.rb"))
        .arg("compare")
        .current_dir(root())
        .output()
        .expect("ruby is on PATH");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success() && stdout.contains("Native Rails and emitted ActiveRecord::Result agree"),
        "ActiveRecord::Result diverged from Rails\n=== stdout ===\n{stdout}\n=== stderr ===\n{stderr}"
    );
    for expected in [
        "scoped rows: [[1, 5, \"a\"], [3, nil, \"c\"]]",
        "scoped affected_rows: 3",
        "rows: [[1, 5, \"a\"], [2, 6, \"b\"], [3, nil, \"c\"]]",
        "one argument: \"ArgumentError: \\\"wrong number of arguments (given 1, expected 2..3)\\\"\"",
    ] {
        assert!(stdout.contains(expected), "missing {expected:?}:\n{stdout}");
    }
}

/// `Connection#execute` hands the adapter's name→value rows to Rails'
/// constructor (`from_hashes`): column names, then each row's values in
/// that order, so a prepended Rails-shaped `initialize` sees Arrays.
#[test]
fn execute_builds_the_result_with_rails_constructor() {
    let script = r#"
require_relative "runtime/ruby/active_record/connection"
module Seen
  def initialize(columns, rows, column_types = {}, affected_rows: nil)
    $seen = [columns, rows.map(&:dup)]
    super
  end
end
ActiveRecord::Result.prepend(Seen)
module ActiveRecord
  def self.adapter
    Object.new.tap do |a|
      def a.select_rows(sql) = [{ "id" => 1, "shop_id" => 7 }, { "id" => 2, "shop_id" => 8 }]
    end
  end
end
result = ActiveRecord::Connection.new.execute("SELECT id, shop_id FROM t")
p $seen
p [result.columns, result.rows, result.to_a.last, result.first]
p ActiveRecord::Result.from_hashes([]).then { |r| [r.columns, r.rows, r.to_a] }
"#;
    let out = Command::new("ruby").arg("-e").arg(script).current_dir(root()).output().expect("ruby is on PATH");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "{stdout}\n{}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(
        stdout,
        "[[\"id\", \"shop_id\"], [[1, 7], [2, 8]]]\n\
         [[\"id\", \"shop_id\"], [[1, 7], [2, 8]], {\"id\" => 2, \"shop_id\" => 8}, {\"id\" => 1, \"shop_id\" => 7}]\n\
         [[], [], []]\n"
    );
}

/// The analyzer reads `ActiveRecord::Result`'s surface from connection.rbs
/// (`analyze::registry::ar`); a signature it cannot parse drops the whole
/// surface silently.
#[test]
fn result_signatures_parse() {
    let rbs = std::fs::read_to_string(root().join("runtime/ruby/active_record/connection.rbs")).unwrap();
    let parsed = roundhouse::rbs::parse_app_signatures(&rbs).expect("connection.rbs parses");
    let result = parsed.get(&roundhouse::ident::ClassId(roundhouse::ident::Symbol::from("ActiveRecord::Result"))).expect("Result declared");
    for name in ["columns", "rows", "to_a", "first", "[]", "affected_rows", "from_hashes", "column_types"] {
        assert!(result.contains_key(&roundhouse::ident::Symbol::from(name)), "Result lacks {name}: {:?}", result.keys().collect::<Vec<_>>());
    }
}
