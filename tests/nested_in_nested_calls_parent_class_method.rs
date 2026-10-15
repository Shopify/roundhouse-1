//! A class nested two levels below an owner whose body calls the owner's
//! `class << self` helper as it loads (Core's
//! `GraphApi::Admin::Analytics::InsightOpinion`, whose
//! `InsightOpinionDataInputs` nests enums built by
//! `InsightOpinion.generate_values_from_proto`). The inner class loads
//! inside its enclosing class's block, so that block has to follow the
//! owner's methods, as a directly nested one does.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

#[test]
fn a_class_nested_in_a_nested_class_loads_after_the_owners_class_methods() {
    let run = emit_and_run::empty_app()
        .write(
            "app/models/opinion.rb",
            r#"class Opinion
  class << self
    def values_for(names)
      names.map(&:to_s).sort
    end
  end

  class Inputs
    class Grain
      VALUES = Opinion.values_for([:week, :day])
    end
  end

  class Status
    VALUES = Opinion.values_for([:open, :closed])
  end
end
"#,
        )
        .run_ruby("p [Opinion::Inputs::Grain::VALUES, Opinion::Status::VALUES]\n");
    assert!(run.success, "{}\n{}", run.stdout, run.stderr);
    assert_eq!(run.stdout, "[[\"day\", \"week\"], [\"closed\", \"open\"]]\n", "{}", run.stderr);
}
