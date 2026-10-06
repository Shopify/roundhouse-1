//! The transpiled `active_record/errors.rb` on the targets whose
//! compilers check a superclass constructor call against the parent's
//! declared constructors (Kotlin, Swift, C#). A class whose initializer
//! hands `message` to its parent needs that parent to take one, or the
//! emitted runtime does not compile. Those toolchains are `#[ignore]`d
//! (`kotlin_toolchain.rs`, `swift_toolchain.rs`, `csharp_toolchain.rs`)
//! and absent from a default run, so this pins the contract on the
//! emitted text in the default suite.

use roundhouse::analyze::Analyzer;
use roundhouse::emit::{csharp, kotlin, swift};
use roundhouse::ingest::ingest_app;

/// `(class, parent)` pairs declared in `source`, for each class that
/// passes `message` up to its parent.
fn message_passing_parents(source: &str, target: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let lines: Vec<&str> = source.lines().collect();
    for (i, line) in lines.iter().enumerate() {
        let Some((class, parent)) = class_and_parent(line, target) else { continue };
        let body: String = lines[i + 1..]
            .iter()
            .take_while(|l| !l.starts_with('}'))
            .copied()
            .collect::<Vec<_>>()
            .join("\n");
        let passes = match target {
            "kotlin" => line.contains(&format!(": {parent}(message)")),
            "swift" => body.contains("super.init(message)"),
            _ => body.contains(": base(message)"),
        };
        if passes {
            out.push((class, parent));
        }
    }
    out
}

fn class_and_parent(line: &str, target: &str) -> Option<(String, String)> {
    let rest = match target {
        "kotlin" => line.strip_prefix("open class ")?,
        "swift" => line.strip_prefix("class ")?,
        _ => line.strip_prefix("public class ")?,
    };
    let name: String = rest.chars().take_while(|c| c.is_alphanumeric() || *c == '_').collect();
    let after = &rest[rest.find(" : ").or_else(|| rest.find(": "))?..];
    let parent: String = after
        .trim_start_matches(|c: char| c == ':' || c == ' ')
        .chars()
        .take_while(|c| c.is_alphanumeric() || *c == '_')
        .collect();
    Some((name, parent))
}

/// Whether `class`, declared in `source`, has a constructor taking a
/// `message`.
fn takes_message(source: &str, class: &str, target: &str) -> bool {
    match target {
        "kotlin" => source.contains(&format!("open class {class}(message: String")),
        "swift" => {
            let Some(start) = source.find(&format!("class {class}:")) else { return false };
            let body = &source[start..];
            let body = &body[..body.find("\n}").unwrap_or(body.len())];
            body.contains("init(_ message: String")
        }
        _ => source.contains(&format!("public {class}(string message")),
    }
}

#[test]
fn every_error_parent_handed_a_message_takes_one() {
    let mut app = ingest_app(roundhouse::fixtures::real_blog()).expect("ingest");
    Analyzer::new(&app).analyze(&mut app);
    let emitted = [
        ("kotlin", kotlin::emit(&app), "src/main/kotlin/Errors.kt"),
        ("swift", swift::emit(&app), "Sources/App/Errors.swift"),
        ("csharp", csharp::emit(&app), "app/runtime/Errors.cs"),
    ];
    for (target, files, path) in emitted {
        let file = files
            .iter()
            .find(|f| f.path.to_string_lossy() == path)
            .unwrap_or_else(|| panic!("{target}: no {path}"));
        let source = &file.content;
        let pairs = message_passing_parents(source, target);
        assert!(
            pairs.iter().any(|(c, p)| c == "ValueTooLong" && p == "StatementInvalid"),
            "{target}: expected ValueTooLong to hand its message to StatementInvalid, saw {pairs:?}\n{source}"
        );
        for (class, parent) in &pairs {
            if !source.contains(&format!("class {parent}")) {
                continue; // a platform base (RuntimeException, Error, Exception)
            }
            assert!(
                takes_message(source, parent, target),
                "{target}: {class} passes `message` to {parent}, which takes none:\n{source}"
            );
        }
    }
}
