// ActiveSupport's signed and encrypted messages
// (runtime/spinel/active_support_messages.rb: SecurityUtils, KeyGenerator,
// CachingKeyGenerator, Messages::{Codec,Metadata,Rotator,
// SerializerWithFallback}, MessageVerifier, MessageEncryptor,
// SecureCompareRotator; with runtime/spinel/active_support_json.rb)
// against activesupport 8.2.0.alpha (Core's pinned Shopify/rails 5813559f,
// from CORE_GEM_HOME): tests/messages_runtime.rb runs the same
// observations natively and on the port, has each side read the
// messages the other made, and requires every observation equal.

use std::path::Path;
use std::process::Command;

#[test]
fn messages_answer_like_activesupport() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let out = Command::new("ruby")
        .arg(root.join("tests/messages_runtime.rb"))
        .arg("compare")
        .current_dir(root)
        .output()
        .expect("ruby is on PATH");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success() && stdout.contains("Native ActiveSupport and the emitted ActiveSupport messages agree"),
        "ActiveSupport messages diverged from activesupport\n=== stdout ===\n{stdout}\n=== stderr ===\n{stderr}"
    );
    // Not a vacuous agreement: both sides observed the whole script,
    // including the messages the other side made.
    assert!(stdout.lines().count() > 90, "too few observations:\n{stdout}");
    assert!(stdout.contains("other encryptor: "), "no cross-read observations:\n{stdout}");
    // The 8.2.0.alpha surface: a frozen CachingKeyGenerator still caches,
    // and use_authenticated_message_encryption is MessageEncryptor's alone.
    for seen in ["frozen caching: [true, true, true, false]", "authenticated subclass: [nil, \"aes-256-cbc\", 2]"] {
        assert!(stdout.contains(seen), "missing observation {seen:?}");
    }
}
