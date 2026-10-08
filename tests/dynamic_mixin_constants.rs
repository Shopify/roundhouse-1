//! Constants written inside a dynamic mixin argument resolve from the
//! lexical scope around the call, as Ruby's do (a block keeps its cref).
//!
//! Core's StoreLocalization::TranslatableContent defines its
//! `<field>_translatable?` methods in an `include(Module.new do ... end)`
//! and reads `Verdict::Flag`, `LocalizableContentTypes` and
//! `NoMethodError` there. Rubydex's indexer skips every non-constant
//! argument of `include` / `extend` / `prepend`, so none of those had a
//! source answer and each was "constant not supported".
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

const KIT: &str = r#"module Kit
  KINDS = ["kit", "last"].freeze

  class Error < StandardError
  end
end
"#;

const FIELDS: &str = r##"module Kit
  module Fields
    def self.included(base)
      base.extend(ClassMethods)
    end

    module ClassMethods
      def field_kinds(*fields)
        include(
          Module.new do
            fields.each do |field|
              define_method("#{field}_kind") do
                value = public_send(field)
                case value
                when Symbol then KINDS.first
                when Integer then Kit::KINDS.last
                else raise Error, "no kind for #{field}"
                end
              rescue NoMethodError
                :missing
              end
            end
          end
        )
      end

      def labelled
        extend(Module.new { def label = "#{name}:#{KINDS.size}" })
      end
    end
  end
end
"##;

const WIDGET: &str = r#"module Kit
  class Widget
    include Kit::Fields

    attr_reader :shape, :count

    def initialize(shape, count)
      @shape = shape
      @count = count
    end
  end
end
"#;

const SCRIPT: &str = r#"Kit::Widget.field_kinds(:shape, :count, :absent)
Kit::Widget.labelled
w = Kit::Widget.new(:round, 3)
p [w.shape_kind, w.count_kind, w.absent_kind, Kit::Widget.label]
begin
  Kit::Widget.new("flat", 1).shape_kind
rescue Kit::Error => e
  p [e.class.name, e.message]
end
"#;

#[test]
fn constants_inside_a_module_new_mixin_resolve_from_the_scope_around_it() {
    let native = emit_and_run::ruby()
        .arg("-e")
        .arg(format!("{KIT}\n{FIELDS}\n{WIDGET}\n{SCRIPT}"))
        .output()
        .expect("native Ruby control");
    assert!(native.status.success(), "{}", String::from_utf8_lossy(&native.stderr));
    let expected = "[\"kit\", \"last\", :missing, \"Kit::Widget:2\"]\n[\"Kit::Error\", \"no kind for shape\"]\n";
    assert_eq!(String::from_utf8_lossy(&native.stdout), expected);
    let run = emit_and_run::empty_app()
        .write("app/models/kit.rb", KIT)
        .write("app/models/kit/fields.rb", FIELDS)
        .write("app/models/kit/widget.rb", WIDGET)
        .run_ruby(SCRIPT);
    run.assert_passes();
    assert_eq!(run.stdout, expected);
}
