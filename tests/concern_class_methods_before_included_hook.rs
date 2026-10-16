//! A module with its own `self.included(base)` hook runs the hook as the
//! `include` does, so it sees the `ClassMethods` of the concerns included
//! above it already (Concern#append_features extended them at their own
//! line). Core's `ColorsInput` includes `HasDefaultFullInputAccessConcern`
//! and then `ColorRolesGroupInputFields`, whose hook calls `base.field`;
//! the `ClassMethods` were written after the includes, so `validators`
//! read an object the hook had frozen: FrozenError on every schema build.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

fn app() -> emit_and_run::Overlay {
    emit_and_run::empty_app()
        .write(
            "app/models/fielded.rb",
            "module Fielded\n  extend ActiveSupport::Concern\n\n  module ClassMethods\n    def fields\n      @fields ||= []\n    end\n\n    def field(name)\n      fields << name\n    end\n  end\nend\n",
        )
        .write(
            "app/models/hooky.rb",
            "module Hooky\n  class << self\n    def included(base)\n      base.field(:hooked)\n    end\n  end\nend\n",
        )
        .write("app/models/thing.rb", "class Thing\n  include Fielded\n  include Hooky\n  field :own\nend\n")
}

#[test]
fn an_included_hook_sees_the_class_methods_of_the_includes_above_it() {
    let run = app().run_ruby("p Thing.fields\n");
    assert!(run.success, "{}\n{}", run.stdout, run.stderr);
    assert_eq!(run.stdout, "[:hooked, :own]\n", "{}", run.stderr);
}
