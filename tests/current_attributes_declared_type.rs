//! `attribute :buyer_context #: BuyerContext::CustomerAccountImpl` — a
//! CurrentAttributes attribute whose type rides a trailing RBS comment.
//!
//! Core's `Customers::CustomerAccount::Current` is written so. Ingest
//! reads the comment as a cast around the `attribute` call, and the
//! lowering took only a bare `attribute` send: the declaration stayed
//! unconsumed, the class got no accessor, and `Current.buyer_context`
//! was `no known method` (and absent from the emitted class).

use roundhouse::det::HashMap;
use std::path::PathBuf;

use roundhouse::analyze::diagnose;
use roundhouse::emit::ruby;
use roundhouse::ingest::ingest_app_from_tree;

const CURRENT: &str = r#"
module Cust
  module Ca
    class Current < ActiveSupport::CurrentAttributes
      attribute :buyer_context #: Bc::Impl
    end
  end
end
"#;

const IMPL: &str = r#"
module Cust
  module Ca
    module Bc
      class Impl
        #: -> Integer
        def n = 1
      end
    end
  end
end
"#;

const CONTROLLER: &str = r#"
module Cust
  module Ca
    class AppController < ApplicationController
      def show
        make unless Current.buyer_context
        render plain: Current.buyer_context&.n.zzz
      end

      private

      def make
        Current.buyer_context = Bc::Impl.new
      end
    end
  end
end
"#;

fn app() -> roundhouse::App {
    let tree: HashMap<PathBuf, Vec<u8>> = [
        ("db/schema.rb", "ActiveRecord::Schema.define(version: 1) do\nend\n"),
        ("config/routes.rb", "Rails.application.routes.draw do\nend\n"),
        ("app/models/cust/ca/current.rb", CURRENT),
        ("app/models/cust/ca/bc/impl.rb", IMPL),
        ("app/controllers/application_controller.rb", "class ApplicationController < ActionController::Base\nend\n"),
        ("app/controllers/cust/ca/app_controller.rb", CONTROLLER),
    ]
    .into_iter()
    .map(|(p, c)| (PathBuf::from(p), c.as_bytes().to_vec()))
    .collect();
    let mut app = ingest_app_from_tree(tree).expect("ingest tree");
    roundhouse::session::analyze_and_lower(&mut app);
    app
}

fn emitted(app: &roundhouse::App, stem: &str) -> String {
    let mut files = ruby::emit_spinel(app);
    files.extend(ruby::emit_library(app));
    files
        .iter()
        .find(|f| f.path.to_string_lossy().ends_with(stem))
        .map(|f| f.content.clone())
        .unwrap_or_else(|| panic!("no emitted file ends with {stem}"))
}

/// The accessor exists, and its read answers the written type, found
/// in the class's lexical scope (`Bc::Impl` is `Cust::Ca::Bc::Impl`).
#[test]
fn an_annotated_attribute_reads_its_declared_type() {
    let errors: Vec<String> = diagnose(&app())
        .into_iter()
        .map(|d| d.to_string())
        .filter(|d| d.starts_with("error"))
        .collect();
    assert!(!errors.iter().any(|e| e.contains("`buyer_context`")), "{errors:?}");
    assert!(errors.iter().any(|e| e.contains("no known method `zzz` on Integer")), "{errors:?}");
}

/// The attribute is nil until the request sets it, so the declared
/// type reads nilable and `unless Current.buyer_context` is still asked.
#[test]
fn the_declared_type_stays_nilable_so_presence_is_asked() {
    let app = app();
    let sig = emitted(&app, "cust/ca/current.rbs");
    assert!(sig.contains("def buyer_context: () -> Bc::Impl?"), "{sig}");
    let current = emitted(&app, "app/models/cust/ca/current.rb");
    assert!(current.contains("def buyer_context=(value)"), "{current}");
    let controller = emitted(&app, "app/controllers/cust/ca/app_controller.rb");
    assert!(controller.contains("if Cust::Ca::Current.buyer_context"), "{controller}");
}
