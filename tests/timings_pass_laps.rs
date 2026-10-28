//! `ROUNDHOUSE_TIMINGS=1` prints a lap line after each post-analyze pass,
//! so a slow pass is named; nothing is printed without it.

use std::process::Command;

fn check(timings: bool) -> String {
    static RUNS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let run = RUNS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let root = std::env::temp_dir().join(format!("rh_timings_pass_laps_{}_{run}", std::process::id()));
    let files = [
        ("db/schema.rb", "ActiveRecord::Schema.define do\n  create_table :widgets do |t|\n    t.string :title\n  end\nend\n"),
        ("app/models/application_record.rb", "class ApplicationRecord < ActiveRecord::Base\n  self.abstract_class = true\nend\n"),
        ("app/models/widget.rb", "class Widget < ApplicationRecord\nend\n"),
    ];
    for (path, text) in files {
        let file = root.join(path);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(file, text).unwrap();
    }
    let mut command = Command::new(env!("CARGO_BIN_EXE_roundhouse"));
    command.args(["--target", "ruby"]).arg(&root).arg("-o").arg(root.join("out"));
    if timings {
        command.env("ROUNDHOUSE_TIMINGS", "1");
    } else {
        command.env_remove("ROUNDHOUSE_TIMINGS");
    }
    let output = command.output().unwrap();
    std::fs::remove_dir_all(&root).unwrap();
    format!("{}{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr))
}

#[test]
fn each_post_analyze_pass_prints_a_lap_line() {
    let text = check(true);
    let laps = text.lines().filter(|l| l.contains("post-analyze pass:")).count();
    assert!(laps >= 5, "{laps} lap lines in:\n{text}");
    assert!(text.contains("post-analyze pass: unported_rails_subclasses"), "{text}");
}

#[test]
fn no_lap_line_without_the_timings_variable() {
    let text = check(false);
    assert!(!text.contains("post-analyze pass:"), "{text}");
}

#[cfg(feature = "memory-census")]
#[test]
fn each_timing_line_reports_its_own_heap_peak() {
    let text = check(true);
    let lines: Vec<&str> = text.lines().filter(|l| l.starts_with("roundhouse-timing:") && l.contains(", live peak ")).collect();
    assert!(!lines.is_empty(), "{text}");
    for line in lines {
        let number = |label: &str| -> u64 {
            let at = line.find(label).unwrap_or_else(|| panic!("no `{label}` in {line}")) + label.len();
            line[at..].split_whitespace().next().unwrap().parse().unwrap()
        };
        assert!(number("phase peak ") <= number("live peak "), "{line}");
    }
}
