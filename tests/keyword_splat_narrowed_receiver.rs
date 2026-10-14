//! `key.merge(**{...}.compact)` on a receiver narrowed by `is_a?`, into a
//! keyword-only `merge`: Core's Pagination::Query composition_helper.
//! Ruby answers `name:int`.

#[path = "support/emit_and_run.rs"]
mod emit_and_run;

const DEFINITION: &str = r##"class Definition
  attr_reader :key, :type

  def initialize(key, type)
    @key = key
    @type = type
  end

  def merge(type: nil, owner: nil)
    Definition.new(@key, type || @type)
  end
end
"##;

const HELPER: &str = r##"module Helper
  def self.define(key, type: nil)
    param = if key.is_a?(Definition)
      key.merge(**{
        type: type,
        owner: self,
      }.compact)
    end
    param ||= Definition.new(key, type)
    "#{param.key}:#{param.type}"
  end
end
"##;

#[test]
fn a_compacted_hash_splats_into_keyword_merge() {
    emit_and_run::empty_app()
        .write("app/models/definition.rb", DEFINITION)
        .write("app/models/helper.rb", HELPER)
        .run_ruby(
            r##"
text = Helper.define(Definition.new(:name, :str), type: :int)
raise "got #{text.inspect}" unless text == "name:int"
puts "ALL OK"
"##,
        )
        .assert_passes();
}

fn run(helper: &str, script: &str) {
    emit_and_run::empty_app()
        .write("app/models/definition.rb", DEFINITION)
        .write("app/models/helper.rb", helper)
        .run_ruby(script)
        .assert_passes();
}

const CALL: &str = r##"
text = Helper.define(Definition.new(:name, :str), type: :int)
raise "got #{text.inspect}" unless text == "name:int"
puts "ALL OK"
"##;

/// A receiver typed to the class (no narrowing) is refused by name, as
/// before: `2634cfae1` ledgers a `**` into flattened optional keywords
/// rather than guess which positional slots the keys fill.
#[test]
fn an_unnarrowed_typed_receiver_is_refused_by_name() {
    let (_emitted, errors) = emit_and_run::empty_app()
        .write("app/models/definition.rb", DEFINITION)
        .write(
            "app/models/helper.rb",
            r##"module Helper
  def self.define(key, type: nil)
    param = Definition.new(key, :old).merge(**{ type: type, owner: self }.compact)
    "#{param.key}:#{param.type}"
  end
end
"##,
        )
        .emit(roundhouse::project::BuildTarget::Ruby);
    assert!(
        errors.iter().any(|e| e.contains("flattened keyword arguments not supported")),
        "expected the named refusal, got {errors:?}"
    );
}

#[test]
fn a_compacted_local_splats_into_keyword_merge() {
    run(
        r##"module Helper
  def self.define(key, type: nil)
    opts = { type: type, owner: self }
    param = if key.is_a?(Definition)
      key.merge(**opts.compact)
    end
    "#{param.key}:#{param.type}"
  end
end
"##,
        CALL,
    );
}

#[test]
fn a_literal_hash_splats_into_keyword_merge() {
    run(
        r##"module Helper
  def self.define(key, type: nil)
    param = if key.is_a?(Definition)
      key.merge(**{ type: type })
    end
    "#{param.key}:#{param.type}"
  end
end
"##,
        CALL,
    );
}

#[test]
fn hash_merge_keeps_its_receiver_and_splat() {
    run(
        r##"module Helper
  def self.define(key, type: nil)
    base = { a: 1 }
    extra = { type: type }.compact
    merged = base.merge(**extra)
    param = key.merge(**merged.slice(:type))
    "#{param.key}:#{param.type}:#{merged[:a]}"
  end
end
"##,
        r##"
text = Helper.define(Definition.new(:name, :str), type: :int)
raise "got #{text.inspect}" unless text == "name:int:1"
puts "ALL OK"
"##,
    );
}
