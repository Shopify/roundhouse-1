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
    std::fs::create_dir_all(dir.join("action_dispatch")).unwrap();
    std::fs::copy(root.join("runtime/ruby/action_dispatch/headers.rb"), dir.join("action_dispatch/headers.rb")).unwrap();
    let request = dir.join("action_dispatch_request.rb");
    std::fs::copy(root.join("runtime/spinel/scaffold/ruby_overlay/runtime/action_dispatch_request.rb"), &request).unwrap();
    // The tree's core_ext loads first at boot (`create` builds Rails'
    // indifferent cookie hash); its own redefinition warnings are not
    // this file's.
    let core_ext = root.join("runtime/spinel/scaffold/ruby_overlay/runtime/active_support_core_ext.rb");
    let script = format!(
        "$VERBOSE = nil\nrequire {:?}\n$VERBOSE = true\nrequire {:?}\nclass FakeRequest < ActionDispatch::TestRequest; end\n\
         r = FakeRequest.create(\"HTTPS\" => \"on\", host: \"x.test\")\n\
         p [r.class.superclass, r.is_a?(ActionDispatch::Request), r.env[\"HTTPS\"], r.env[\"host\"], r.env[\"HTTP_HOST\"], r.env[\"REQUEST_METHOD\"]]\n\
         p ActionDispatch::TestRequest.create.env.values_at(\"HTTP_HOST\", \"REMOTE_ADDR\", \"HTTP_USER_AGENT\", \"PATH_INFO\")\n",
        core_ext.display().to_string(),
        request.display().to_string()
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
