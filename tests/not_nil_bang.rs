//! Non-nil assertion names need an app implementation or a shared nil-check lowerer.
//!
//! The CRuby and JRuby trees carry type_toolkit's `not_nil!`
//! (`tests/not_nil_type_toolkit.rs` runs it), so the call is refused per
//! target at emit, on every tree that does not define it.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

use roundhouse::project::BuildTarget;

fn refusals(action: &str, target: BuildTarget) -> Vec<String> {
    let controller = format!(
        "class UsersController < ApplicationController\n  def index\n{action}\n  end\nend\n"
    );
    let (_, errors) = emit_and_run::empty_app()
        .write("db/schema.rb", "ActiveRecord::Schema.define do\n  create_table \"users\", force: :cascade do |t|\n    t.string \"name\"\n  end\nend\n")
        .write("app/models/application_record.rb", "class ApplicationRecord < ActiveRecord::Base\nend\n")
        .write("app/models/user.rb", "class User < ApplicationRecord\nend\n")
        .write("app/controllers/application_controller.rb", "class ApplicationController < ActionController::Base\nend\n")
        .write("app/controllers/users_controller.rb", &controller)
        .write("config/routes.rb", "Rails.application.routes.draw do\n  get \"/users\", to: \"users#index\"\nend\n")
        .emit(target);
    errors
}

#[test]
fn universal_not_nil_bang_is_refused() {
    for target in [BuildTarget::Spinel, BuildTarget::Rust, BuildTarget::Typescript] {
        let diags = refusals("    name = User.first.name\n    name.not_nil!.upcase\n    [name].compact.first.not_nil!.strip", target);
        assert!(diags.iter().any(|d| d.contains("not_nil!")), "{target:?}: {diags:?}");
    }
}
