//! An `around_action` wraps every callback registered after it, the
//! before filters included.
//!
//! The dispatcher used to run every before filter and nest every around
//! filter around the bare action. Core's AdminAreaController declares
//! `around_action :with_shop` ahead of the `before_action :authenticate_request`
//! its LoginProtection concern adds, and `with_shop` is what sets `@shop`,
//! so the authentication read `shop` as nil (`undefined method
//! 'identity_accounts' for nil`, every Admin controller id).
//!
//! ActiveSupport's chain is one list in registration order: a before
//! runs where it stands, an around runs its block (the rest of the list)
//! where it stands.

use roundhouse::det::HashMap;
use std::path::PathBuf;

use roundhouse::emit::ruby;
use roundhouse::ingest::ingest_app_from_tree;

const BASE: &str = r#"class BaseController < ActionController::Base
  private
    def early
      @log << :early
    end

    def authenticate
      @log << :authenticate
    end

    def late
      @log << :late
    end

    def with_shop
      @log << :shop_in
      yield
      @log << :shop_out
    end

    def with_locale
      @log << :locale_in
      yield
      @log << :locale_out
    end
end
"#;

const ROOMS: &str = r#"class RoomsController < BaseController
  before_action :early
  around_action :with_shop
  before_action :authenticate
  around_action :with_locale, only: [:show]
  before_action :late

  def index
  end

  def show
  end
end
"#;

fn emitted() -> String {
    let files: Vec<(&str, &str)> = vec![
        ("db/schema.rb", "ActiveRecord::Schema.define do\n  create_table \"rooms\", force: :cascade do |t|\n    t.string \"name\"\n  end\nend\n"),
        ("app/models/room.rb", "class Room < ApplicationRecord\nend\n"),
        ("app/controllers/base_controller.rb", BASE),
        ("app/controllers/rooms_controller.rb", ROOMS),
        ("config/routes.rb", "Rails.application.routes.draw do\n  resources :rooms, only: [:index, :show]\nend\n"),
    ];
    let tree: HashMap<PathBuf, Vec<u8>> =
        files.into_iter().map(|(p, c)| (PathBuf::from(p), c.as_bytes().to_vec())).collect();
    let mut app = ingest_app_from_tree(tree).expect("ingest");
    roundhouse::session::analyze_and_lower(&mut app);
    let files = ruby::emit_lowered_controllers(&app);
    files
        .iter()
        .find(|f| f.path.to_string_lossy().ends_with("rooms_controller.rb"))
        .map(|f| f.content.clone())
        .expect("rooms_controller.rb")
}

#[test]
fn a_before_filter_declared_after_an_around_runs_inside_it() {
    let rooms = emitted();
    let at = |needle: &str| rooms.find(needle).unwrap_or_else(|| panic!("{needle}\n{rooms}"));
    let early = at("early");
    let shop = at("with_shop do");
    let authenticate = at("authenticate");
    let late = at("late");
    // `early` is registered before the around, so it stays outside; the
    // other before filters sit inside `with_shop`'s block.
    assert!(early < shop, "{rooms}");
    assert!(shop < authenticate && authenticate < late, "{rooms}");
}

#[test]
fn a_guarded_around_wraps_the_filters_after_it_under_its_guard() {
    let rooms = emitted();
    let guard = rooms.find("if [:show].include?(action_name)\n").expect("guard");
    let wrap = rooms.find("with_locale do").expect("with_locale");
    let late = rooms[wrap..].find("late").expect("late") + wrap;
    // The filter after the guarded around is inside its block and, with
    // the guard false, still runs in the unwrapped branch.
    assert!(guard < wrap && wrap < late, "{rooms}");
    assert_eq!(rooms.matches("late\n").count(), 2, "{rooms}");
}

/// A concern's `included do before_action … end` registers where the
/// `include` stands, not ahead of the class body: Core's
/// AdminAreaController writes `around_action :with_shop`, then
/// `include LoginProtection` (whose `authenticate_request` is a before
/// callback), so the authentication runs inside `with_shop`.
#[test]
fn a_concern_filter_registers_where_the_include_stands() {
    let concern = "module Guarded\n  extend ActiveSupport::Concern\n\n  included do\n    before_action :authenticate\n  end\nend\n";
    let rooms_src = "class RoomsController < BaseController\n  before_action :early\n  around_action :with_shop\n  include Guarded\n  before_action :late\n\n  def index\n  end\nend\n";
    let files: Vec<(&str, &str)> = vec![
        ("db/schema.rb", "ActiveRecord::Schema.define do\n  create_table \"rooms\", force: :cascade do |t|\n    t.string \"name\"\n  end\nend\n"),
        ("app/models/room.rb", "class Room < ApplicationRecord\nend\n"),
        ("app/controllers/base_controller.rb", BASE),
        ("app/controllers/concerns/guarded.rb", concern),
        ("app/controllers/rooms_controller.rb", rooms_src),
        ("config/routes.rb", "Rails.application.routes.draw do\n  resources :rooms, only: [:index]\nend\n"),
    ];
    let tree: HashMap<PathBuf, Vec<u8>> =
        files.into_iter().map(|(p, c)| (PathBuf::from(p), c.as_bytes().to_vec())).collect();
    let mut app = ingest_app_from_tree(tree).expect("ingest");
    roundhouse::session::analyze_and_lower(&mut app);
    let rooms = ruby::emit_lowered_controllers(&app)
        .iter()
        .find(|f| f.path.to_string_lossy().ends_with("rooms_controller.rb"))
        .map(|f| f.content.clone())
        .expect("rooms_controller.rb");
    let at = |needle: &str| rooms.find(needle).unwrap_or_else(|| panic!("{needle}\n{rooms}"));
    let (early, shop, authenticate, late) =
        (at("early"), at("with_shop do"), at("authenticate"), at("late"));
    assert!(early < shop && shop < authenticate && authenticate < late, "{rooms}");
}
