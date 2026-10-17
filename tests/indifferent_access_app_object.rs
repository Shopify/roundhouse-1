//! `value.with_indifferent_access` where `value` is an app object that
//! defines it (the country_db gem's `Zone`, delegating to its `to_h`) but
//! the analysis read as a Hash (inferred from its callers). The call lowers
//! to `ActiveSupport.indifferent(value)`, which
//! built an empty indifferent Hash defaulting to the object, so every
//! province Core's `Country#province_from_default_params` built had no
//! name or code.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

const ZONE: &str = r##"class Zone
  def initialize(code, name)
    @to_h = { "code" => code, "name" => name }
  end

  def with_indifferent_access(...)
    @to_h.with_indifferent_access(...)
  end
end
"##;

const BUILDER: &str = r##"class Builder
  #: (Hash[String, untyped] params) -> String
  def code_of(params)
    params.with_indifferent_access.slice(:code, :name)[:code]
  end
end
"##;

#[test]
fn an_app_objects_own_with_indifferent_access_answers() {
    emit_and_run::empty_app()
        .write("app/models/zone.rb", ZONE)
        .write("app/models/builder.rb", BUILDER)
        .run_ruby(
            r##"
builder = Builder.new
raise "zone" unless builder.code_of(Zone.new("AL", "Alabama")) == "AL"
raise "hash" unless builder.code_of({ code: "TX", name: "Texas" }) == "TX"
puts "ALL OK"
"##,
        )
        .assert_passes();
}
