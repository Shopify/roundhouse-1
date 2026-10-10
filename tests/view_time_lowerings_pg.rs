//! A PostgreSQL tree's views keep Rails' time semantics, as its other
//! bodies do (`lower::time_current::carries_zone_port`): the view
//! pipeline's copies of the `Time.current` and duration groundings
//! (`emit::ruby::library::apply_time_current_lowering`,
//! `apply_duration_lowering`) leave `Time.current` and `<t> - <dur>` to
//! the tree's zone port, where they grounded them to UTC `Time.now.utc`
//! and second arithmetic. A tree without the port still grounds them.

#[path = "support/emit_and_run.rs"]
mod emit_and_run;

use roundhouse::project::BuildTarget;

const VIEW: &str = "<p><%= Time.current.year %></p>\n<p><%= (Time.current - 1.day).day %></p>\n";

fn emitted_view(pg: bool) -> String {
    let mut app = emit_and_run::empty_app()
        .write("app/controllers/application_controller.rb", "class ApplicationController < ActionController::Base\nend\n")
        .write("app/controllers/clocks_controller.rb", "class ClocksController < ApplicationController\n  def show\n  end\nend\n")
        .write("app/views/clocks/show.html.erb", VIEW)
        .write("config/routes.rb", "Rails.application.routes.draw do\n  resource :clock, only: :show\nend\n");
    if pg {
        app = app
            .write("roundhouse.yml", "database_adapter: postgresql\n")
            .write("config/database.yml", "test:\n  adapter: postgresql\n  database: rh_view_time_lowerings\n");
    }
    let (dir, _errors) = app.emit(BuildTarget::Ruby);
    let mut found = Vec::new();
    let mut stack = vec![dir.join("app")];
    while let Some(d) = stack.pop() {
        for entry in std::fs::read_dir(&d).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|e| e == "rb") {
                let text = std::fs::read_to_string(&path).unwrap();
                if text.contains(".year") && text.contains(".day") && path.display().to_string().contains("clocks") {
                    found.push(text);
                }
            }
        }
    }
    assert_eq!(found.len(), 1, "one emitted clocks/show view in {}", dir.display());
    found.pop().unwrap()
}

#[test]
fn postgresql_tree_views_keep_time_current_and_duration_arithmetic() {
    let view = emitted_view(true);
    assert!(view.contains("Time.current.year"), "{view}");
    assert!(!view.contains("Time.now.utc"), "{view}");
    assert!(view.contains("Time.current - "), "{view}");
}

#[test]
fn a_tree_without_the_zone_port_grounds_view_times_to_utc() {
    let view = emitted_view(false);
    assert!(view.contains("Time.now.utc.year"), "{view}");
    assert!(!view.contains("Time.current"), "{view}");
}
