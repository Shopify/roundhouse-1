//! A namespaced model's `include ::GraphApi::Admin::IdSerializer` keeps the
//! constant it names when a sibling `Merch::GraphApi::Admin` shadows the
//! bare first segment.
//!
//! Core's `Merchandising::PricingEngine::PriceList` includes the top-level
//! `::GraphApi::Admin::IdSerializer` beside `Merchandising::GraphApi::Admin::
//! PriceList`. Written bare, the include raised `uninitialized constant
//! Merch::GraphApi::Admin::IdSerializer` mid class body, so the class kept no
//! `table_name` and every query on it died at `table_name must be overridden`.

#[path = "support/emit_and_run.rs"]
mod emit_and_run;

const SERIALIZER: &str = r#"
module GraphApi
  module Admin
    module IdSerializer
      extend T::Helpers

      requires_ancestor { Object }

      def gid
        "gid://#{id}"
      end
    end
  end
end
"#;

const SERIALIZER_ERROR: &str = r#"
module GraphApi
  module Admin
    module IdSerializer
      Error = Class.new(StandardError)
    end
  end
end
"#;

const SIBLING: &str = r#"
module Merch
  module GraphApi
    module Admin
      class Plist
      end
    end
  end
end
"#;

const MODEL: &str = r#"
module Merch
  module Engine
    class Plist < ApplicationRecord
      include ::GraphApi::Admin::IdSerializer
    end
  end
end
"#;

const SCRIPT: &str = r#"
p Merch::Engine::Plist.table_name
p Merch::Engine::Plist.include?(::GraphApi::Admin::IdSerializer)
p Merch::Engine::Plist.new(id: 7).gid
"#;

const EXPECTED: &str = "\"plists\"\ntrue\n\"gid://7\"\n";

fn overlay() -> emit_and_run::Overlay {
    emit_and_run::empty_app()
        .write("app/models/application_record.rb", "class ApplicationRecord < ActiveRecord::Base\n  self.abstract_class = true\nend\n")
        .write("db/schema.rb", "ActiveRecord::Schema.define do\n  create_table \"plists\" do |t|\n    t.string \"name\"\n  end\nend\n")
        .write("app/models/graph_api/admin/id_serializer.rb", SERIALIZER)
        .write("app/models/graph_api/admin/id_serializer/error.rb", SERIALIZER_ERROR)
        .write("app/models/merch/graph_api/admin/plist.rb", SIBLING)
        .write("app/models/merch/engine/plist.rb", MODEL)
}

#[test]
fn a_rooted_model_include_survives_a_shadowing_sibling_namespace() {
    let run = overlay().run_ruby(SCRIPT);
    run.assert_passes();
    assert_eq!(run.stdout, EXPECTED);
}
