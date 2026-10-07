//! Constants a boot initializer assigns at its top level
//! (`ingest::initializer_constants`), on the CRuby tree, against the
//! same files run natively.
//!
//! Shopify Core's essentials engine writes `ShopifyTracer =
//! OpenTelemetry.tracer_provider.tracer("Shopify") #:
//! OpenTelemetry::Trace::Tracer` in `config/initializers/00_tracing.rb`
//! and reads it across the app. Rails `load`s every initializer at boot
//! in sorted order (railties engine.rb `load_config_initializers`), so
//! the constant exists before any app code runs, whether or not anything
//! reads it, and one initializer can read what an earlier one assigned.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

use roundhouse::project::BuildTarget;
use std::process::Command;

/// Files sort by full path string: `00_log` runs first.
const LOG: &str = "BOOT_ORDER = []\n";
/// Reads `BOOT_ORDER`, which only an earlier initializer defines, and
/// records that it ran.
const ALPHA: &str = "# frozen_string_literal: true\n\nALPHA = BOOT_ORDER.push(\"alpha\").length\n";
/// Two constants in one file, one with an RBS-inline ascription.
const BETA: &str = "BETA = BOOT_ORDER.push(\"beta\").length\nGREETING = [\"hi\"].first #: String\n";

const READER: &str = r##"class Reader
  def self.read
    [ALPHA + BETA, GREETING.upcase, BOOT_ORDER.length]
  end
end
"##;

const INITIALIZERS: [(&str, &str); 3] = [
    ("config/initializers/00_log.rb", LOG),
    ("config/initializers/10_alpha.rb", ALPHA),
    ("config/initializers/20_beta.rb", BETA),
];

/// `BETA` is read before `ALPHA`: had either been defined on first read,
/// `BOOT_ORDER` would list them in read order.
const SCRIPT: &str = r##"def seen(label)
  value = begin
    yield
  rescue StandardError => e
    [e.class.name, e.message]
  end
  puts "#{label}: #{value.inspect}"
end
seen("order") { [BETA, ALPHA, BOOT_ORDER] }
seen("reader") { Reader.read }
seen("greeting") { [GREETING, GREETING.length] }
"##;

const EXPECTED: [&str; 3] = [
    "order: [2, 1, [\"alpha\", \"beta\"]]",
    "reader: [3, \"HI\", 2]",
    "greeting: [\"hi\", 2]",
];

fn app() -> emit_and_run::Overlay {
    let mut app = emit_and_run::empty_app()
        .write("roundhouse.yml", "test_paths:\n  - test\n")
        .write("test/test_helper.rb", "require \"active_support/test_case\"\n")
        .write("app/models/reader.rb", READER);
    for (path, source) in INITIALIZERS {
        app = app.write(path, source);
    }
    app
}

/// The initializers on plain Ruby, loaded in Rails' order, then the app.
fn native() -> String {
    static SEQ: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("rh_initializer_constants_native_{}_{seq}", std::process::id()));
    std::fs::create_dir_all(dir.join("config/initializers")).unwrap();
    std::fs::create_dir_all(dir.join("app/models")).unwrap();
    let mut program = String::new();
    for (path, source) in INITIALIZERS {
        std::fs::write(dir.join(path), source).unwrap();
        program.push_str(&format!("load {:?}\n", path));
    }
    std::fs::write(dir.join("app/models/reader.rb"), READER).unwrap();
    program.push_str("require_relative \"app/models/reader\"\n");
    program.push_str(SCRIPT);
    std::fs::write(dir.join("native.rb"), program).unwrap();
    let out = Command::new("ruby").arg("native.rb").current_dir(&dir).output().expect("ruby is on PATH");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(out.status.success(), "native run failed:\n{stdout}\n{}", String::from_utf8_lossy(&out.stderr));
    std::fs::remove_dir_all(&dir).unwrap();
    stdout
}

#[test]
fn initializer_constants_are_defined_at_boot_in_initializer_order() {
    let native = native();
    assert_eq!(native.lines().collect::<Vec<_>>(), EXPECTED, "native:\n{native}");
    let emitted = app().run_ruby(SCRIPT);
    emitted.assert_passes();
    assert_eq!(emitted.stdout.lines().collect::<Vec<_>>(), EXPECTED, "emitted:\n{}", emitted.stdout);
}

/// boot.rb requires the holders in initializer order, before the app.
#[test]
fn boot_requires_each_holder_before_the_app() {
    let (dir, errors) = app().emit(BuildTarget::Ruby);
    assert!(errors.is_empty(), "{}", errors.join("\n"));
    let boot = std::fs::read_to_string(dir.join("boot.rb")).expect("boot.rb");
    let at = |needle: &str| boot.find(needle).unwrap_or_else(|| panic!("{needle} missing:\n{boot}"));
    let log = at("require_relative \"app/models/boot_order\"\n");
    let alpha = at("require_relative \"app/models/alpha\"\n");
    let beta = at("require_relative \"app/models/beta\"\n");
    let models = at("require_relative \"app/models\"\n");
    assert!(log < alpha && alpha < beta && beta < models, "{boot}");
    assert!(boot.contains("# BETA, GREETING: config/initializers/20_beta.rb\n"), "{boot}");
}

/// The analyzer types each constant from its value (the ascription
/// included), where app code reads it.
#[test]
fn reads_are_typed_from_the_value() {
    let tree = INITIALIZERS
        .iter()
        .copied()
        .chain([("app/models/reader.rb", READER)])
        .map(|(p, c)| (std::path::PathBuf::from(p), c.as_bytes().to_vec()))
        .collect();
    let mut app = roundhouse::ingest::ingest_app_from_tree(tree).expect("ingest tree");
    roundhouse::analyze::Analyzer::new(&app).analyze(&mut app);
    let reader = app.library_classes.iter().find(|c| c.name.0.as_str() == "Reader").expect("Reader");
    let read = reader.methods.iter().find(|m| m.name.as_str() == "read").expect("Reader.read");
    let mut reads = Vec::new();
    const_reads(&read.body, &mut reads);
    reads.sort_by(|a, b| a.0.cmp(&b.0));
    let names: Vec<&str> = reads.iter().map(|r| r.0.as_str()).collect();
    assert_eq!(names, ["ALPHA", "BETA", "BOOT_ORDER", "GREETING"], "{reads:?}");
    use roundhouse::ty::Ty;
    assert_eq!(reads[0].1, Some(Ty::Int), "{reads:?}");
    assert_eq!(reads[1].1, Some(Ty::Int), "{reads:?}");
    assert!(matches!(reads[2].1, Some(Ty::Array { .. })), "{reads:?}");
    assert_eq!(reads[3].1, Some(Ty::Str), "{reads:?}");
}

fn const_reads(expr: &roundhouse::expr::Expr, out: &mut Vec<(String, Option<roundhouse::ty::Ty>)>) {
    if let roundhouse::expr::ExprNode::Const { path } = &*expr.node {
        out.push((path.iter().map(|s| s.as_str()).collect::<Vec<_>>().join("::"), expr.ty.clone()));
    }
    expr.node.for_each_child(&mut |c| const_reads(c, out));
}

/// A value that reads a local of its file cannot run apart from it: a
/// named gap at the statement, and the constant stays undefined.
#[test]
fn a_value_reading_a_file_local_is_refused_at_its_statement() {
    let (dir, errors) = app()
        .write("config/initializers/30_local.rb", "base = 2\nDOUBLED = base * 2\n")
        .emit(BuildTarget::Ruby);
    assert!(
        errors.iter().any(|e| e.contains(
            "`DOUBLED` is not defined at boot: its value reads `base`, a local of the initializer"
        )),
        "{}",
        errors.join("\n")
    );
    let boot = std::fs::read_to_string(dir.join("boot.rb")).expect("boot.rb");
    assert!(!boot.contains("app/models/doubled"), "{boot}");
}

/// Two initializers assigning one name: the second is a named gap.
#[test]
fn a_reassigned_name_is_refused() {
    let (_dir, errors) = app()
        .write("config/initializers/30_again.rb", "ALPHA = 7\n")
        .emit(BuildTarget::Ruby);
    assert!(
        errors.iter().any(|e| e.contains(
            "`ALPHA` is reassigned (not modeled); first assigned by config/initializers/10_alpha.rb"
        )),
        "{}",
        errors.join("\n")
    );
}
