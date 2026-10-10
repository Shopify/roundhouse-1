//! `_transaction_record_key` (runtime/spinel/cruby/active_record_transactions.rb)
//! keys a record's transaction callback candidate as Rails' `hash`/`eql?`
//! do (activerecord core.rb): class and id once the id is present, the
//! record itself before that. Rails never compares the id with 0 or "", so
//! an app id type whose `==` refuses an Integer (Core's Shopify::Uuid)
//! must not be asked.

use std::path::Path;
use std::process::Command;

const PROBE: &str = r#"
module ActiveRecord
  class ActiveRecordError < StandardError; end
end
load ARGV.fetch(0)

class StrictId
  attr_reader :value
  def initialize(value) = @value = value
  def ==(other)
    raise ArgumentError, "compared with #{other.class}" unless other.is_a?(StrictId)
    value == other.value
  end
  alias eql? ==
  def hash = value.hash
end

class Rec
  include ActiveRecord::Transactions
  attr_accessor :id
  def initialize(id) = @id = id
  def new_record? = false
end

a = Rec.new(StrictId.new("u1"))
b = Rec.new(StrictId.new("u1"))
puts "uuid key: #{a._transaction_record_key == b._transaction_record_key}"
puts "zero id key: #{Rec.new(0)._transaction_record_key.inspect}"
n = Rec.new(nil)
puts "nil id key is self: #{n._transaction_record_key.equal?(n)}"
c = Rec.new([1, nil])
puts "partial composite key is self: #{c._transaction_record_key.equal?(c)}"
"#;

#[test]
fn transaction_record_key_matches_rails_record_identity() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let out = Command::new("ruby")
        .arg("-e")
        .arg(PROBE)
        .arg(root.join("runtime/spinel/cruby/active_record_transactions.rb"))
        .output()
        .expect("ruby is on PATH");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "probe failed\n{stdout}\n{stderr}");
    for expected in [
        "uuid key: true",
        "zero id key: [Rec, 0]",
        "nil id key is self: true",
        "partial composite key is self: true",
    ] {
        assert!(stdout.contains(expected), "missing {expected:?}:\n{stdout}\n{stderr}");
    }
}
