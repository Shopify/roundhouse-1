//! `ActionController::Renderers` (actionpack's metal/renderers.rb): the
//! registry behind `render json:`/`xml:`/`js:`, which a gem `include`s
//! and overrides. active_model_serializers 0.9.13's
//! `ActionController::Serialization` does `include
//! ActionController::Renderers` in its module body and redefines
//! `_render_with_renderer_json`, calling `super`; with no Renderers in the
//! runtime every controller that reached it (Core's ApiAreaController)
//! raised NameError at class load (e2e v2-r29, 27 ids).
//!
//! The probe's AMS-shaped module is included in a plain module, as the
//! emitted gem body does, and that module in a controller.

#[path = "support/emit_and_run.rs"]
mod emit_and_run;

const CONTROLLER: &str = "class WidgetsController < ApplicationController\n  def index\n    head :ok\n  end\nend\n";

const ROUTES: &str = "Rails.application.routes.draw do\n  resources :widgets, only: [:index]\nend\n";

const SCHEMA: &str = "ActiveRecord::Schema[8.1].define(version: 2026_01_01_000000) do\n  create_table \"widgets\", force: :cascade do |t|\n    t.string \"name\"\n  end\nend\n";

const SCRIPT: &str = r#"require "json"
module Ser
  include ActionController::Renderers
  [:_render_option_json, :_render_with_renderer_json].each do |m|
    define_method m do |resource, options|
      super(resource.is_a?(Hash) ? "WRAPPED:" + resource.to_json : resource, options)
    end
  end
end
class SerController < ActionController::Base
  include Ser
end

out = []
out << ActionController::Renderers::RENDERERS.to_a
out << SerController._renderers.equal?(ActionController::Renderers::RENDERERS)

c = SerController.new
out << c.render_to_body(json: { a: 1 })
out << c.content_type

d = SerController.new
out << d.render_to_body(json: { a: 1 }, status: :created, callback: "cb")
out << [d.status, d.content_type]

e = SerController.new
out << e.render_to_body(json: "[1]")
out << e.render_to_body(foo: 1)

ActionController::Renderers.add(:csv) { |rows, _opts| rows.join(",") }
out << SerController.new.render_to_body(csv: [1, 2])
ActionController::Renderers.remove(:csv)
out << SerController.new.render_to_body(csv: [1, 2])

class SubController < SerController; use_renderers :extra; end
out << [SubController._renderers.to_a.last, SerController._renderers.include?(:extra)]

want = [
  [:json, :js, :xml], true,
  "WRAPPED:{\"a\":1}", "application/json",
  "/**/cb(WRAPPED:{\"a\":1})", [201, "text/javascript"],
  "[1]", nil,
  "1,2", nil,
  [:extra, false],
]
raise "renderers answered #{out.inspect}, want #{want.inspect}" unless out == want
"#;

#[test]
fn renderers_register_dispatch_and_chain_to_a_gems_override() {
    emit_and_run::empty_app()
        .write("app/controllers/widgets_controller.rb", CONTROLLER)
        .write("config/routes.rb", ROUTES)
        .write("db/schema.rb", SCHEMA)
        .run_ruby(SCRIPT)
        .assert_passes();
}
