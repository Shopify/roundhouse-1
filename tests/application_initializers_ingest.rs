//! An application class body's `initializer` calls are carried on the
//! `Rails::Application` reopen as written, in source order, when the app
//! runs on PostgreSQL (the railtie runtime that runs them,
//! runtime/rails_railtie.rb, is that tree's), and one that cannot be
//! carried is reported by name rather than dropped. The runtime half,
//! against native Rails, is `application_initializers_pg_runtime`.

use roundhouse::expr::ExprNode;
use roundhouse::ingest::ingest_app_from_tree;
use roundhouse::App;

const SCHEMA: &str = "ActiveRecord::Schema.define(version: 1) do\n  \
    create_table :users do |t|\n    t.string :name\n  end\nend\n";

const APPLICATION: &str = "module Sample\n  class Application < Rails::Application\n    \
    initializer(:one) do |app|\n      app.config.one = 1\n    end\n\n    \
    initializer :two, after: :one do\n      config.two = 2\n    end\n\n    \
    initializer :no_block\n  end\nend\n";

fn app(adapter: &str) -> App {
    let tree = vec![
        ("db/schema.rb", SCHEMA.to_string()),
        ("roundhouse.yml", format!("database_adapter: {adapter}\n")),
        ("config/routes.rb", "Rails.application.routes.draw do\n  resources :users\nend\n".to_string()),
        ("config/application.rb", APPLICATION.to_string()),
        ("app/models/user.rb", "class User < ApplicationRecord\nend\n".to_string()),
    ]
    .into_iter()
    .map(|(p, c)| (std::path::PathBuf::from(p), c.into_bytes()))
    .collect();
    ingest_app_from_tree(tree).expect("ingest tree")
}

fn initializer_name(call: &roundhouse::expr::Expr) -> Option<String> {
    let ExprNode::Send { recv: None, method, args, block: Some(_), .. } = &*call.node else { return None };
    assert_eq!(method.as_str(), "initializer");
    match &*args.first()?.node {
        ExprNode::Lit { value: roundhouse::expr::Literal::Sym { value } } => Some(value.as_str().to_string()),
        other => panic!("{other:?}"),
    }
}

#[test]
fn the_reopen_carries_each_initializer_with_a_block_in_source_order() {
    let app = app("postgresql");
    let reopen = app.rails_application.as_ref().expect("Rails::Application reopen");
    let names: Vec<String> = reopen.unknown_calls.iter().filter_map(initializer_name).collect();
    assert_eq!(names, ["one", "two"]);
    assert_eq!(reopen.unknown_calls.len(), 2);
}

#[test]
fn an_initializer_without_a_block_is_reported() {
    let app = app("postgresql");
    let drops: Vec<_> = app.ingest_drops.iter().filter(|d| d.construct.as_deref() == Some("initializer")).collect();
    assert_eq!(drops.len(), 1, "{:?}", app.ingest_drops);
    assert!(drops[0].message.contains("without a block"), "{}", drops[0].message);
}

#[test]
fn without_the_postgresql_railtie_runtime_nothing_is_carried() {
    let app = app("sqlite3");
    assert!(app.rails_application.as_ref().is_none_or(|lc| lc.unknown_calls.is_empty()));
    assert!(app.ingest_drops.iter().all(|d| d.construct.as_deref() != Some("initializer")));
}
