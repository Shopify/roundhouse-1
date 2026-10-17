//! sorbet's `T::Struct.new(hash)`: the props as one positional Hash, built
//! in the caller with `.compact`. Core's CustomDiscountLineBuilder. The
//! struct has required props, so the keyword constructor must receive the
//! Hash splatted. Ruby answers `t:5:nil`.

#[path = "support/emit_and_run.rs"]
mod emit_and_run;

const PACK: &str = r##"class Pack < T::Struct
  const :title, String
  const :value, Numeric
  const :note, T.nilable(String), default: nil
end
"##;

const BUILDER: &str = r##"class Builder
  #: (String title, Integer value, String? note) -> Pack
  def build(title, value, note)
    Pack.new(
      {
        title: title,
        value: value,
        note: note
      }.compact
    )
  end
end
"##;

#[test]
fn a_compacted_hash_is_splatted_into_a_required_prop_struct() {
    emit_and_run::empty_app()
        .write("app/models/pack.rb", PACK)
        .write("app/models/builder.rb", BUILDER)
        .run_ruby(
            r##"
pack = Builder.new.build("t", 5, nil)
text = "#{pack.title}:#{pack.value}:#{pack.note.inspect}"
raise "got #{text.inspect}" unless text == "t:5:nil"
puts "ALL OK"
"##,
        )
        .assert_passes();
}
