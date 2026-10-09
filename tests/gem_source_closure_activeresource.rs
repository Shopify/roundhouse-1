//! activeresource compiled as a gem source (the harness's
//! bin/gem-sources.rb closure, e2e round 6). Core's
//! components/platform/testing/test_runtime.rb requires
//! `active_resource/http_mock`; activeresource is git-sourced
//! (rails/activeresource b2ad3bd7a95f), so the emitted boot's native
//! require found no gem home spec for it and raised LoadError, which ended
//! test_runtime.rb's begin block before faker, the support requires and
//! the test helpers' setup calls. This compiles the real gem's files (and
//! activemodel-serializers-xml 1.0.3, which it requires) from the gem home
//! the native Core run pins, and boots a resource over HttpMock. Skipped,
//! with a note, where that gem home is absent.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

use std::path::{Path, PathBuf};

const GEM_HOME: &str = "/nix/store/kr4swvh5mw01fs3kixfa9afy57zsyihg-ruby-gems-dev/lib/ruby/gems/4.0.0";

const LOCK: &str = "GIT\n  remote: https://github.com/rails/activeresource.git\n  revision: b2ad3bd7a95f2510fbf221ba37318ec2e449101a\n  specs:\n    activeresource (6.2.0)\n      activemodel (>= 7.0)\n      activemodel-serializers-xml (~> 1.0)\n      activesupport (>= 7.0)\n\nGEM\n  remote: https://rubygems.org/\n  specs:\n    activemodel (8.0.2)\n      activesupport\n    activemodel-serializers-xml (1.0.3)\n      activemodel (>= 5.0.0.a)\n      activesupport (>= 5.0.0.a)\n    activesupport (8.0.2)\n\nDEPENDENCIES\n  activeresource!\n  activesupport\n";

const APPLICATION: &str = "require_relative \"boot\"\n\nrequire \"rails\"\nrequire \"active_record/railtie\"\n\nBundler.require(*Rails.groups)\n\nmodule ResourceProbeApp\n  class Application < Rails::Application\n  end\nend\n";

const CARRIED: &str = "test_paths:\n  - test\ngem_sources:\n  activemodel-serializers-xml: vendor/gems/activemodel-serializers-xml-1.0.3\n  activeresource: vendor/gems/activeresource-6.2.0\n";

const HTTP_MOCK_TEST: &str = r#"require "test_helper"
require "active_resource/http_mock"

class Person < ActiveResource::Base
  self.site = "http://people.example"
end

class HttpMockTest < ActiveSupport::TestCase
  test "a resource finds and creates over HttpMock" do
    ActiveResource::HttpMock.respond_to do |mock|
      mock.get "/people/1.json", {}, { id: 1, name: "Matz" }.to_json
      mock.post "/people.json", {}, { id: 2, name: "Koichi" }.to_json, 201, "Location" => "/people/2.json"
    end
    person = Person.find(1)
    assert_equal "Matz", person.name
    created = Person.create(name: "Koichi")
    assert_equal 2, created.id
    assert_equal 2, ActiveResource::HttpMock.requests.size
  end
end
"#;

/// Every file under `dir/lib`, as (path relative to `dir`, text).
fn lib_files(dir: &Path) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut stack: Vec<PathBuf> = vec![dir.join("lib")];
    while let Some(at) = stack.pop() {
        for entry in std::fs::read_dir(&at).expect("read gem lib") {
            let path = entry.expect("dir entry").path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|e| e == "rb") {
                let rel = path.strip_prefix(dir).unwrap().to_string_lossy().into_owned();
                out.push((rel, std::fs::read_to_string(&path).expect("read gem file")));
            }
        }
    }
    out.sort();
    out
}

fn app() -> Option<emit_and_run::Overlay> {
    let resource = Path::new(GEM_HOME).join("bundler/gems/activeresource-b2ad3bd7a95f");
    let xml = Path::new(GEM_HOME).join("gems/activemodel-serializers-xml-1.0.3");
    if !resource.is_dir() || !xml.is_dir() {
        eprintln!("skipped: {GEM_HOME} has no activeresource b2ad3bd7a95f checkout or activemodel-serializers-xml 1.0.3");
        return None;
    }
    let mut app = emit_and_run::empty_app()
        .write("roundhouse.yml", CARRIED)
        .write(
            "Gemfile",
            "source \"https://rubygems.org\"\n\ngem \"activesupport\"\ngem \"activeresource\", github: \"rails/activeresource\"\n",
        )
        .write("Gemfile.lock", LOCK)
        .write("config/boot.rb", "require \"bundler/setup\"\n")
        .write("config/application.rb", APPLICATION)
        .write("config/routes.rb", "Rails.application.routes.draw do\nend\n")
        .write("test/test_helper.rb", "require \"active_support/test_case\"\n")
        .write("config/environment.rb", "require_relative \"application\"\n\nRails.application.initialize!\n")
        .write("test/models/http_mock_test.rb", HTTP_MOCK_TEST);
    for (dir, prefix) in [(&resource, "vendor/gems/activeresource-6.2.0"), (&xml, "vendor/gems/activemodel-serializers-xml-1.0.3")] {
        for (rel, text) in lib_files(dir) {
            app = app.write(&format!("{prefix}/{rel}"), &text);
        }
    }
    Some(app)
}

/// Survey mode, as Core's emit runs (`--allow-unsupported --survey`):
/// activemodel-serializers-xml's `included do extend ActiveModel::Naming
/// end` on a plain includer is a recorded gap, and ActiveResource::Base,
/// its includer here, extends ActiveModel::Naming itself (base.rb:1860).
///
/// Not yet (e2e round 6). Here the boot raises at active_resource.rb:4
/// (`URI::RFC2396_PARSER`, "constant not supported"; Core's tree carries it)
/// and the check reports ActiveSupport::XmlMini, Builder::XmlMarkup,
/// `to_param` and `acts_like?` (Object extensions), `const_get` /
/// `const_defined?` on an unproven receiver, the association builders'
/// constants and ActiveModel::ForbiddenAttributesProtection. In a capped
/// Core emit it loads, and the boot raises NoMethodError (`subclasses` for
/// module ActiveResource::Validations): the emitted Validations module,
/// a Concern in the gem, runs `include ActiveModel::Validations` on itself,
/// and the runtime port's append_features (runtime/active_model_validations.rb)
/// defines callbacks on the module instead of deferring to the including
/// class. bin/gem-sources.rb keeps shopify_api held (GEM_SOURCES_PULL_GIT
/// off) until this passes.
#[test]
#[ignore = "compiled activeresource does not boot yet: URI::RFC2396_PARSER here; in Core, ActiveResource::Validations' include of ActiveModel::Validations (runtime port append_features)"]
fn activeresource_compiled_as_a_gem_source_answers_http_mock() {
    use roundhouse::ingest::survey;
    let Some(app) = app() else { return };
    survey::activate();
    let run = app.run_test("test/models/http_mock_test.rb");
    let _ = survey::drain();
    run.assert_passes();
}
