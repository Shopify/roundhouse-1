//! The test harness's `ActionResponse` answers the rest of Rails'
//! `ActionDispatch::TestResponse` status surface Core's integration tests
//! read: `code` (the String status), `message`, `response_code` and Rack's
//! status predicates. `response.code` was `undefined method 'code'`.

const HELPER: &str = include_str!("../runtime/spinel/test/test_helper.rb");

/// The class's source, cut out of the helper (which loads a whole test
/// harness) so CRuby can run it alone.
fn class_source() -> String {
    let start = HELPER.find("\nclass ActionResponse\n").expect("class ActionResponse") + 1;
    let end = start + HELPER[start..].find("\nend\n").expect("end of class") + 5;
    HELPER[start..end].to_string()
}

#[test]
fn action_response_answers_code_message_and_status_predicates() {
    let probe = r##"
require "json"
def response(status, location: nil)
  ActionResponse.new(status: status, body: "{}", location: location, flash: nil, cookies: nil)
end
r = response(404)
puts [r.code, r.code.class, r.response_code, r.message, r.status_message].inspect
puts [response(200).code, response(200).message, response(422).message, response(500).message].inspect
puts [r.not_found?, r.missing?, r.client_error?, r.error?, r.successful?, r.server_error?].inspect
puts [response(200).ok?, response(200).successful?, response(201).created?, response(204).no_content?].inspect
puts [response(302, location: "/x").redirection?, response(503).server_error?, response(401).unauthorized?].inspect
puts [response(403).forbidden?, response(400).bad_request?, response(202).accepted?, response(999).message].inspect
"##;
    let dir = std::env::temp_dir().join(format!("rh_action_response_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("probe.rb");
    std::fs::write(&file, format!("{}\n{probe}", class_source())).unwrap();
    let out = std::process::Command::new("ruby").arg(&file).output().expect("ruby");
    std::fs::remove_dir_all(&dir).unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let out = String::from_utf8_lossy(&out.stdout);
    assert_eq!(
        out,
        "[\"404\", String, 404, \"Not Found\", \"Not Found\"]\n\
         [\"200\", \"OK\", \"Unprocessable Content\", \"Internal Server Error\"]\n\
         [true, true, true, true, false, false]\n\
         [true, true, true, true]\n\
         [true, true, true]\n\
         [true, true, true, \"\"]\n"
    );
}
