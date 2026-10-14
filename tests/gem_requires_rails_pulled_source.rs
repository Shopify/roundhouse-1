//! A `rails_dependent` gem whose source is compiled (`gem_sources:`) is not
//! refused by name: the constants its source declares are the compiled
//! classes, so `IdentityClient::SessionStore.new(session)` in a Core test
//! helper (authentication_helper.rb#login_without_db_writes) runs.
//!
//! Core lists identity_client as `rails_dependent`
//! (extensions/core/gems.yml). bin/gem-sources.rb compiles it, with the
//! openid_connect chain it requires, unless the emit sets
//! `GEM_SOURCES_PULL_RAILS=` (empty). The unit-1000-v2 emits did, and 14
//! ids failed in setup or in the test with
//! `roundhouse: gem_requires_rails not supported`: the helpers'
//! `IdentityClient::SessionStore.new`, `IdentityClient::Ping.any_instance`
//! and `IdentityClient.client_identifiers` sites emitted as a raise.
//! `a_refused_gem_constant_is_not_defined` (gem_requires_rails_defined.rs) is
//! the gem-absent half of this pair.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

use roundhouse::project::BuildTarget;

const LOCK: &str = "GEM\n  remote: https://rubygems.org/\n  specs:\n    identity_client (59.3.0)\n\nDEPENDENCIES\n  identity_client\n";

/// As Core's extension lists the gem.
const GEMS: &str = "rails_dependent:\n  - {gem: identity_client, why: \"requiring it loads 199 Rails framework files\"}\n";

const RBI: &str = "# typed: true\nmodule IdentityClient\n  class SessionStore\n    def initialize(session, expiry_leeway = nil, grace_period: nil); end\n    def login(scope, attrs); end\n  end\nend\n";

const LIB: &str = "vendor/gems/identity_client-59.3.0/lib";

const ENTRY: &str = "module IdentityClient\n  def self.client_identifiers = @client_identifiers ||= []\nend\n\nrequire \"identity_client/session_store\"\n";

const SESSION_STORE: &str = "module IdentityClient\n  class SessionStore\n    attr_reader :logins\n\n    def initialize(session, expiry_leeway = nil, grace_period: nil)\n      @session = session\n      @expiry_leeway = expiry_leeway\n      @grace_period = grace_period\n      @logins = []\n    end\n\n    def login(scope, attrs)\n      @session[scope] = attrs\n      @logins << scope\n      self\n    end\n  end\nend\n";

const HELPER: &str = "module AuthenticationHelper\n  def self.login_without_db_writes(session)\n    store = IdentityClient::SessionStore.new(session)\n    store.login(:account, access_token: \"t\")\n    IdentityClient.client_identifiers << :app\n    [session[:account], IdentityClient.client_identifiers]\n  end\nend\n";

fn app() -> emit_and_run::Overlay {
    emit_and_run::empty_app()
        .write(
            "roundhouse.yml",
            "extensions:\n  - ext\ntest_paths:\n  - test\ngem_sources:\n  identity_client: vendor/gems/identity_client-59.3.0\n",
        )
        .write("ext/gems.yml", GEMS)
        .write("Gemfile", "source \"https://rubygems.org\"\ngem \"identity_client\", \"59.3.0\"\n")
        .write("Gemfile.lock", LOCK)
        .write("config/application.rb", "Bundler.require(*Rails.groups)\n")
        .write("sorbet/rbi/gems/identity_client@59.3.0.rbi", RBI)
        .write(&format!("{LIB}/identity_client.rb"), ENTRY)
        .write(&format!("{LIB}/identity_client/session_store.rb"), SESSION_STORE)
        .write("app/models/authentication_helper.rb", HELPER)
}

#[test]
fn a_rails_dependent_gem_compiled_from_source_is_not_refused() {
    let (dir, errors) = app().emit(BuildTarget::Ruby);
    assert!(!errors.iter().any(|e| e.contains("gem_requires_rails")), "{errors:#?}");
    let helper = std::fs::read_to_string(dir.join("app/models/authentication_helper.rb")).expect("helper");
    assert!(helper.contains("IdentityClient::SessionStore.new(session)"), "{helper}");
    assert!(!helper.contains("gem_requires_rails"), "{helper}");
}

#[test]
fn a_rails_dependent_gem_compiled_from_source_runs() {
    let run = app().run_ruby("p AuthenticationHelper.login_without_db_writes({})\n");
    assert!(!run.errors.iter().any(|e| e.contains("gem_requires_rails")), "{:#?}", run.errors);
    assert!(run.success, "{}\n{}", run.stdout, run.stderr);
    assert_eq!(run.stdout, "[{access_token: \"t\"}, [:app]]\n", "{}", run.stderr);
}
