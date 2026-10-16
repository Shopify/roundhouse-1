//! The CRuby overlay's `ActionDispatch::TestRequest` is a `Request`
//! subclass, as Rails' is: Core's `class FakeRequest <
//! ActionDispatch::TestRequest` raised "superclass must be an instance
//! of Class (given Module)" at load. `create` takes Rails' default test
//! env under the given keys.

use std::path::Path;

#[test]
fn test_request_is_a_request_subclass_with_rails_default_env() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let dir = std::env::temp_dir().join(format!("roundhouse-test-request-{}", std::process::id()));
    // The overlay request reads the controller base's session and cookies
    // and the exceptions file at load: the files the tree boots with
    // (tests/action_controller_test_request_runtime.rb's set), laid out
    // as the tree lays them out.
    let tree = [
        ("runtime/spinel/scaffold/ruby_overlay/runtime/active_support_core_ext.rb", "active_support_core_ext.rb"),
        ("runtime/ruby/action_controller/parameter_missing.rb", "action_controller/parameter_missing.rb"),
        ("runtime/spinel/action_controller_exceptions.rb", "action_controller_exceptions.rb"),
        ("runtime/ruby/action_dispatch/headers.rb", "action_dispatch/headers.rb"),
        ("runtime/spinel/http_authentication.rb", "http_authentication.rb"),
        ("runtime/ruby/action_controller/cookies.rb", "action_controller/cookies.rb"),
        ("runtime/spinel/scaffold/ruby_overlay/runtime/action_dispatch_request.rb", "action_dispatch_request.rb"),
    ];
    for (from, to) in tree {
        let target = dir.join(to);
        std::fs::create_dir_all(target.parent().unwrap()).unwrap();
        std::fs::copy(root.join(from), target).unwrap();
    }
    // The tree's core_ext loads first at boot (`create` builds Rails'
    // indifferent cookie hash); its own redefinition warnings are not
    // this file's.
    let load = |file: &str| format!("require {:?}\n", dir.join(file).display().to_string());
    let script = format!(
        "$VERBOSE = nil\nrequire {:?}\n{}{}{}{}$VERBOSE = true\n{}class FakeRequest < ActionDispatch::TestRequest; end\n\
         r = FakeRequest.create(\"HTTPS\" => \"on\", host: \"x.test\")\n\
         p [r.class.superclass, r.is_a?(ActionDispatch::Request), r.env[\"HTTPS\"], r.env[\"host\"], r.env[\"HTTP_HOST\"], r.env[\"REQUEST_METHOD\"]]\n\
         p ActionDispatch::TestRequest.create.env.values_at(\"HTTP_HOST\", \"REMOTE_ADDR\", \"HTTP_USER_AGENT\", \"PATH_INFO\")\n",
        root.join("runtime/ruby/action_controller/base").display().to_string(),
        load("active_support_core_ext.rb"),
        load("action_controller_exceptions.rb"),
        load("http_authentication.rb"),
        load("action_controller/cookies.rb"),
        load("action_dispatch_request.rb"),
    );
    let out = std::process::Command::new("ruby").args(["-w", "-e", &script]).output().expect("ruby");
    let _ = std::fs::remove_dir_all(&dir);
    assert!(out.status.success() && out.stderr.is_empty(), "{}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "[ActionDispatch::TestRequest, true, \"on\", \"x.test\", \"test.host\", \"GET\"]\n\
         [\"test.host\", \"0.0.0.0\", \"Rails Testing\", \"/\"]\n"
    );
}
