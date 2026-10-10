// ActionDispatch::TestRequest's writers, ActionController::TestRequest
// with its TestSession, and ActionController::HttpAuthentication::Basic
// (the CRuby overlay's runtime/action_dispatch_request.rb and
// runtime/spinel/http_authentication.rb) against the Rails native Core
// runs (Shopify/rails 5813559f4aed, 8.2.0.alpha; tests/support/
// rails_oracle.rb). tests/action_controller_test_request_runtime.rb runs
// the same calls through the pinned actionpack and through the ports and
// requires every observation equal.
//
// Core reads both at load or setup: TaxesApi's `API` constant carries
// `Basic.encode_credentials(...)`, and the checkout PolicyContextBuilder
// builds `ActionController::TestRequest.create(GraphQLController)`. Both
// were "constant not supported".

use std::path::Path;
use std::process::Command;

fn root() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

#[test]
fn test_request_and_basic_auth_answer_like_actionpack() {
    let out = Command::new("ruby")
        .arg(root().join("tests/action_controller_test_request_runtime.rb"))
        .arg("compare")
        .current_dir(root())
        .output()
        .expect("ruby is on PATH");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success() && stdout.contains("Native Rails and emitted TestRequest/HttpAuthentication::Basic agree"),
        "ports diverged from actionpack\n=== stdout ===\n{stdout}\n=== stderr ===\n{stderr}"
    );
    for expected in [
        "ad host=: \"shop.example.com\"",
        "ad path=: \"/en/collections\"",
        "ac controller_class: WidgetsController",
        "ac session class: \"ActionController::TestSession\"",
        "encode: \"Basic dGF4ZXM6czNjcjpldA==\"",
        "user_name_and_password: [\"taxes\", \"s3cr:et\"]",
    ] {
        assert!(stdout.contains(expected), "missing {expected:?}:\n{stdout}");
    }
}

const FILES: &[(&str, &str)] = &[
    ("roundhouse.yml", "test_paths:\n  - test\n"),
    ("db/schema.rb", "ActiveRecord::Schema[8.0].define(version: 1) do\n  create_table \"cards\", force: :cascade do |t|\n    t.string \"code\"\n  end\nend\n"),
    ("app/controllers/application_controller.rb", "class ApplicationController < ActionController::Base\nend\n"),
    ("app/controllers/widgets_controller.rb", "class WidgetsController < ApplicationController\nend\n"),
    // TaxesApi's shape: a constant computed from encode_credentials at
    // load, read again inside a method.
    (
        "app/models/auth_probe.rb",
        "module AuthProbe\n  HEADER = ActionController::HttpAuthentication::Basic.encode_credentials(\"user\", \"pa:ss\")\n\n  \
def self.build\n    request = ActionController::TestRequest.create(WidgetsController)\n    request.host = \"shop.example\"\n    \
request.env[\"HTTP_AUTHORIZATION\"] = HEADER\n    [request, ActionController::HttpAuthentication::Basic.user_name_and_password(request)]\n  end\nend\n",
    ),
];

#[test]
fn test_request_and_basic_auth_constants_resolve() {
    let root = std::env::temp_dir().join(format!("rh_action_controller_test_request_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    for (path, text) in FILES {
        let file = root.join("app").join(path);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(file, text).unwrap();
    }
    let output = Command::new(env!("CARGO_BIN_EXE_roundhouse"))
        .args(["--target", "ruby"])
        .arg(root.join("app"))
        .arg("-o")
        .arg(root.join("out"))
        .arg("--allow-unsupported")
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stderr}");
    assert!(!stderr.contains("not supported"), "{stderr}");
    let model = std::fs::read_to_string(root.join("out/app/models/auth_probe.rb")).unwrap();
    assert!(!model.contains("not supported"), "{model}");
    for expected in [
        "HEADER = ActionController::HttpAuthentication::Basic.encode_credentials(\"user\", \"pa:ss\")",
        "request = ActionController::TestRequest.create(WidgetsController)",
        "request.env[\"HTTP_AUTHORIZATION\"] = AuthProbe::HEADER",
    ] {
        assert!(model.contains(expected), "missing {expected:?}:\n{model}");
    }
    let request = std::fs::read_to_string(root.join("out/runtime/action_dispatch_request.rb")).unwrap();
    assert!(request.contains("class TestRequest < ActionDispatch::TestRequest"), "{request}");
    let _ = std::fs::remove_dir_all(&root);
}
