//! A constant read inside nested namespaces binds its first segment in
//! the INNERMOST lexical scope that defines it (Module.nesting order),
//! then the path under it. `Plan::Complete` inside `Outer::Web::Merch`
//! is `Outer::Web::Plan::Complete` even though the outer `Outer::Plan`
//! (a class with no `Complete`) is also an enclosing namespace's
//! constant: the emit must not root it as `::Plan::Complete` (no such
//! top-level constant) because some farther scope's `Plan` lacks the
//! path. Runs emitted on CRuby and natively, which must print the same.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

const FILES: &[(&str, &str)] = &[
    ("app/models/outer/plan.rb", "module Outer\n  class Plan\n  end\nend\n"),
    (
        "app/models/outer/web/plan.rb",
        "module Outer\n  module Web\n    module Plan\n      class Complete\n      end\n      class Missing\n      end\n    end\n  end\nend\n",
    ),
    (
        "app/models/outer/web/merch.rb",
        "module Outer\n  module Web\n    module Merch\n      class Variant\n        def initialize(plan)\n          @plan = plan || Plan::Complete.new\n          @kind = case @plan\n          when Plan::Complete, Plan::Missing\n            :planned\n          else\n            :none\n          end\n        end\n\n        def kind = @kind\n      end\n    end\n  end\nend\n",
    ),
];

const SCRIPT: &str = "p Outer::Web::Merch::Variant.new(nil).kind\np Outer::Web::Merch::Variant.new(Outer::Web::Plan::Missing.new).kind\np Outer::Web::Merch::Variant.new(Object.new).kind\n";

#[test]
fn a_when_constant_binds_in_the_innermost_scope_defining_its_head() {
    let mut app = emit_and_run::empty_app().write("config/routes.rb", "Rails.application.routes.draw do\nend\n");
    for (path, source) in FILES {
        app = app.write(path, source);
    }
    let run = app.run_ruby(SCRIPT);
    run.assert_passes();
    assert_eq!(run.stdout, ":planned\n:planned\n:none\n");
}
