//! An integer literal wider than 64 bits (Shopify Core's
//! `MetaobjectsInternal::Codec::DoubleLayout::SIGN_BIT =
//! 0x8000000000000000`) is an Integer like any other in Ruby. The
//! Ruby family carries it at full width; a fixed-width target refuses it
//! by name. CRuby is the oracle for the emitted program.

#[path = "support/emit_and_run.rs"]
mod emit_and_run;

use std::process::Command;

use roundhouse::project::BuildTarget;

const LAYOUT: &str = r##"
module Layout
  SIGN_BIT = 0x8000000000000000
  ALL_BITS = 0xFFFF_FFFF_FFFF_FFFF
  BELOW = -0x1_0000_0000_0000_0001
  DECIMAL = 123456789012345678901234567890

  def self.encode(value)
    bits = [value].pack("G").unpack1("Q>")
    [bits.anybits?(SIGN_BIT) ? bits ^ ALL_BITS : bits | SIGN_BIT].pack("Q>")
  end

  def self.decode(bytes)
    ordered = bytes.unpack1("Q>")
    bits = ordered.anybits?(SIGN_BIT) ? ordered ^ SIGN_BIT : ordered ^ ALL_BITS
    [bits].pack("Q>").unpack1("G")
  end
end
"##;

const SCRIPT: &str = r##"
p Layout.encode(-1.5).unpack1("H*"), Layout.encode(2.0).unpack1("H*"), Layout.decode(Layout.encode(-1.5))
p Layout.encode(-1.5) < Layout.encode(2.0), Layout::SIGN_BIT, Layout::ALL_BITS == 2**64 - 1
p Layout::BELOW, Layout::DECIMAL, Layout::DECIMAL.class
"##;

#[test]
fn wide_integer_literals_run_at_full_width_in_emitted_ruby() {
    let expected = "\"4007ffffffffffff\"\n\"c000000000000000\"\n-1.5\ntrue\n9223372036854775808\ntrue\n\
                    -18446744073709551617\n123456789012345678901234567890\nInteger\n";
    let native = Command::new("ruby")
        .arg("-e")
        .arg(format!("{LAYOUT}\n{SCRIPT}"))
        .output()
        .expect("native Ruby control");
    assert!(native.status.success(), "{}", String::from_utf8_lossy(&native.stderr));
    assert_eq!(String::from_utf8_lossy(&native.stdout), expected);

    let run = emit_and_run::real_blog().write("app/lib/layout.rb", LAYOUT).run_ruby(SCRIPT);
    run.assert_passes();
    assert_eq!(run.stdout, expected);
    let emitted = std::fs::read_to_string(run.emitted.join("app/models/layout.rb")).expect("emitted layout.rb");
    assert!(emitted.contains("SIGN_BIT = 9223372036854775808"), "{emitted}");
    assert!(emitted.contains("BELOW = -18446744073709551617"), "{emitted}");
}

#[test]
fn fixed_width_targets_refuse_a_wide_integer_literal() {
    let tree: roundhouse::det::HashMap<std::path::PathBuf, Vec<u8>> =
        [(std::path::PathBuf::from("app/models/layout.rb"), LAYOUT.as_bytes().to_vec())].into_iter().collect();
    let app = roundhouse::ingest::ingest_app_from_tree(tree).expect("ingest");
    for target in [BuildTarget::Spinel, BuildTarget::Typescript, BuildTarget::Go] {
        let (result, diagnostics) = roundhouse::emit::diagnostics::scope(|| {
            roundhouse::project::target_files(&app, std::path::Path::new("not-a-fixture"), target)
        });
        let error = result.err().unwrap_or_else(|| panic!("{} emitted a wide literal", target.as_str()));
        assert!(error.contains("an integer literal wider than 64 bits is not supported"), "{error}");
        let named: Vec<_> = diagnostics.iter().map(|d| d.message.clone()).collect();
        assert!(
            named.iter().any(|m| m.contains("9223372036854775808 does not fit in a 64-bit integer")),
            "{named:#?}"
        );
    }
}
