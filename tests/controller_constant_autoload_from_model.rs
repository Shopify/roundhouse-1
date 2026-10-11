//! A controller named from a model or test before any route dispatches to
//! it. `app/controllers` is a Zeitwerk autoload path (Rails' autoloaders),
//! so `Reports::GraphQLController` resolves on first reference from any
//! code. The emitted tree required controller files only from the route
//! dispatch arms, so such a reference raised NameError.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

const APP_CONTROLLER: &str = "class ApplicationController < ActionController::Base\nend\n";
const ROUTES: &str = "Rails.application.routes.draw do\nend\n";
const INFLECTIONS: &str = "ActiveSupport::Inflector.inflections(:en) do |inflect|\n  inflect.acronym \"GraphQL\"\nend\n";
const CONTROLLER: &str = "module Reports\n  class GraphQLController < ApplicationController\n    LIMIT = 3\n\n    def index\n      head :ok\n    end\n  end\nend\n";
const BUILDER: &str = "module Reports\n  class RequestBuilder\n    def self.limit\n      GraphQLController::LIMIT\n    end\n\n    def self.controller\n      Reports::GraphQLController\n    end\n  end\nend\n";
const SCRIPT: &str = "p Reports::RequestBuilder.limit\np Reports::RequestBuilder.controller\np Reports::RequestBuilder.controller.superclass\n";

#[test]
fn a_model_names_a_controller_no_route_has_required() {
    let run = emit_and_run::empty_app()
        .write("app/controllers/application_controller.rb", APP_CONTROLLER)
        .write("app/controllers/reports/graphql_controller.rb", CONTROLLER)
        .write("app/models/reports/request_builder.rb", BUILDER)
        .write("config/routes.rb", ROUTES)
        .write("config/initializers/inflections.rb", INFLECTIONS)
        .run_ruby(SCRIPT);
    run.assert_passes();
    assert_eq!(run.stdout, "3\nReports::GraphQLController\nApplicationController\n");
}
