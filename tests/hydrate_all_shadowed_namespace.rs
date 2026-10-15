//! A model whose namespace holds a module named like the namespace's own
//! root (`Tenants::Repos::Tenants`, the directory
//! `tenants/repos/tenants/` the autoloader turns into a module). The
//! synthesized `_hydrate_all` names its owner by the qualified name as one
//! segment; written bare in the class's body, Ruby reads `Tenants` as the
//! nearer `Tenants::Repos::Tenants` ("uninitialized constant
//! Tenants::Repos::Tenants::Entry"). Core: `AppVersions::Systems::
//! Repositories::PersistenceRecords::Release`, beside
//! `Repositories::AppVersions`.

#[path = "support/emit_and_run.rs"]
mod emit_and_run;

const MARKER: &str = "module Tenants\n  module Repos\n    module Tenants\n      class Marker\n      end\n    end\n  end\nend\n";

const ENTRY: &str = r#"module Tenants
  module Repos
    class Entry < ApplicationRecord
      self.table_name = "articles"
    end
  end
end
"#;

#[test]
fn hydrating_a_namespaced_model_reads_it_through_the_root_when_a_sibling_shadows_it() {
    let run = emit_and_run::real_blog()
        .write("app/models/tenants/repos/tenants/marker.rb", MARKER)
        .write("app/models/tenants/repos/entry.rb", ENTRY)
        .run_ruby(
            "Article.create!(title: 'A title long enough', body: 'A body long enough to pass validation')\nputs Tenants::Repos::Entry.take.class\nputs Tenants::Repos::Entry.all.to_a.size\n",
        );
    run.assert_passes();
    assert!(run.stdout.lines().any(|l| l == "Tenants::Repos::Entry"), "{}", run.stdout);
    assert!(run.stdout.lines().any(|l| l == "1"), "{}", run.stdout);
}
