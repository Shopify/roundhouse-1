//! The schema of every database the app configures, for column lookups.
//!
//! `App::schema` is the primary database's. Rails' multiple-database model
//! gives each configuration its own dump, and a model's table lives in the
//! database its class (or the nearest abstract ancestor with a
//! `connects_to`) names: `App::model_databases`
//! (`ingest::database_ownership`).
//!
//! Where a configuration's dump is, in Rails' own order
//! (activerecord `database_configurations/hash_config.rb`, `#schema_dump`;
//! `tasks/database_tasks.rb`, `#schema_dump_path`):
//! 1. its `schema_dump:` key, a file name under `db/` (`false` writes none);
//! 2. else `schema.rb` / `structure.sql` for the primary and
//!    `<name>_schema.rb` / `<name>_structure.sql` for any other.
//!
//! `config/database.yml` is ERB and is resolved at boot, which a compile
//! cannot do, so its text is read structurally: the `test` section's
//! configurations, `<<: *anchor` merges and `schema_dump:` keys, with ERB
//! lines skipped. A configuration whose dump this cannot name is not
//! guessed at; a database whose dump is absent or unreadable is reported
//! by name and its models stay Rails'.

use std::collections::HashMap;

use crate::App;
use crate::database::ModelDatabase;
use crate::ident::{ClassId, Symbol};
use crate::ingest::{ingest_schema, ingest_structure_sql};
use crate::schema::Schema;

/// The environment the app's own suite runs in.
const ENV: &str = "test";
const DATABASE_YML: &str = "config/database.yml";

struct Dump {
    path: String,
    /// `None` when the file is present but did not ingest; see `note`.
    schema: Option<Schema>,
    note: Option<String>,
}

#[derive(Default)]
pub struct DatabaseSchemas {
    dumps: Vec<Dump>,
    /// Configuration name → index into `dumps`.
    by_name: HashMap<String, usize>,
}

/// A column lookup that could not be answered, with the reason the site
/// stays Rails'.
pub type Lookup = Result<bool, String>;

impl DatabaseSchemas {
    /// The dumps of the app's non-primary databases, read from `files`
    /// (the app's tree) rather than the file system.
    pub fn load(app: &App, files: &[(String, String)]) -> Self {
        let file = |path: &str| files.iter().find(|(p, _)| p == path).map(|(_, c)| c.as_str());
        let mut names: Vec<(String, Option<String>)> = Vec::new();
        let mut extra_dumps: Vec<String> = Vec::new();
        if let Some(yml) = file(DATABASE_YML) {
            for config in configurations(yml) {
                if config.primary {
                    continue;
                }
                match config.schema_dump {
                    DumpDecl::Off => {}
                    DumpDecl::File(f) => {
                        let path = format!("db/{f}");
                        if !extra_dumps.contains(&path) {
                            extra_dumps.push(path.clone());
                        }
                        names.push((config.name, Some(path)));
                    }
                    DumpDecl::Default => names.push((config.name, None)),
                }
            }
        }
        // Rails' default dump of any other database sits in `db/` under its
        // name, whether or not `database.yml` could be read for it.
        for (path, _) in files {
            let Some(stem) = path.strip_prefix("db/") else { continue };
            if let Some(name) = stem.strip_suffix("_structure.sql").or_else(|| stem.strip_suffix("_schema.rb")) {
                if !name.is_empty() && !name.contains('/') && !extra_dumps.contains(path) {
                    extra_dumps.push(path.clone());
                }
            }
        }
        // A database a `connects_to` names that `database.yml` did not
        // show (its ERB generated the configuration, or there is none).
        for ownership in app.model_databases.values() {
            if let ModelDatabase::Named { writing, reading, .. } = ownership {
                for n in std::iter::once(writing).chain(reading.iter()) {
                    if !names.iter().any(|(m, _)| m == n) {
                        names.push((n.clone(), None));
                    }
                }
            }
        }
        for db in &app.databases {
            if !db.primary {
                if let Some(source) = &db.schema_source {
                    names.push((db.name.clone(), Some(source.path.clone())));
                }
            }
        }

        // A dump may hold statements the ingest does not model (`PREPARE`,
        // `ALTER TABLE`): Rails loads the file itself, so they are gaps, not
        // refusals. Survey mode records them and keeps the tables it models.
        let own_survey = !crate::ingest::survey::is_active();
        if own_survey {
            crate::ingest::survey::activate();
        }
        let mut out = DatabaseSchemas::default();
        let mut by_path: HashMap<String, usize> = HashMap::new();
        let mut read = |path: &str, out: &mut DatabaseSchemas| -> usize {
            if let Some(i) = by_path.get(path) {
                return *i;
            }
            let (schema, note) = match file(path) {
                None => (None, Some(format!("`{path}` is not in the tree"))),
                Some(text) => {
                    let parsed = if path.ends_with(".rb") {
                        ingest_schema(text.as_bytes(), path)
                    } else {
                        ingest_structure_sql(text.as_bytes(), path)
                    };
                    match parsed {
                        Ok(s) => (Some(s), None),
                        Err(e) => (None, Some(format!("`{path}` did not ingest: {e}"))),
                    }
                }
            };
            out.dumps.push(Dump { path: path.to_string(), schema, note });
            by_path.insert(path.to_string(), out.dumps.len() - 1);
            out.dumps.len() - 1
        };
        for path in &extra_dumps {
            read(path, &mut out);
        }
        for (name, declared) in names {
            let candidates = match declared {
                Some(path) => vec![path],
                None => vec![format!("db/{name}_structure.sql"), format!("db/{name}_schema.rb")],
            };
            // A name with no dump in the tree gets no entry; lookups say so by name.
            let Some(path) = candidates.iter().find(|p| file(p).is_some()) else { continue };
            let i = read(path, &mut out);
            out.by_name.entry(name).or_insert(i);
        }
        if own_survey {
            crate::ingest::survey::drain();
        }
        out
    }

    /// Whether `table` has `column`, in the database `model` runs on.
    ///
    /// A model under a `connects_to` is looked up in that database's dump
    /// alone. A model with no `connects_to` runs on the primary; when its
    /// table is not in the primary's dump the app routes it elsewhere at
    /// run time (`establish_connection`, `connected_to`, a shard switch),
    /// which a compile cannot see, so the column counts only when every
    /// other dump that holds the table has it.
    pub fn has_column(&self, app: &App, model: &ClassId, table: &Symbol, column: &Symbol) -> Lookup {
        let has = |schema: &Schema| schema.tables.get(table).map(|t| t.columns.iter().any(|c| &c.name == column));
        if let Some(ModelDatabase::Named { writing, .. }) = app.model_databases.get(model) {
            let Some(&i) = self.by_name.get(writing) else {
                return Err(format!("database `{writing}` has no schema dump in the tree"));
            };
            let dump = &self.dumps[i];
            let Some(schema) = &dump.schema else {
                return Err(dump.note.clone().unwrap_or_else(|| format!("`{}` was not read", dump.path)));
            };
            return has(schema).ok_or_else(|| format!("table `{}` is not in `{}`", table.as_str(), dump.path));
        }
        if let Some(found) = has(&app.schema) {
            return Ok(found);
        }
        let mut holders = self.dumps.iter().filter_map(|d| d.schema.as_ref().and_then(|s| has(s)));
        match holders.next() {
            None => Err(format!("table `{}` is in no database schema", table.as_str())),
            Some(first) => Ok(first && holders.all(|h| h)),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
enum DumpDecl {
    Off,
    File(String),
    Default,
}

struct Config {
    name: String,
    primary: bool,
    schema_dump: DumpDecl,
}

#[derive(Default)]
struct Node {
    key: String,
    value: String,
    anchor: Option<String>,
    merges: Vec<String>,
    schema_dump: Option<DumpDecl>,
    children: Vec<Node>,
}

/// `database.yml`'s `test` configurations, with their dump declarations.
fn configurations(text: &str) -> Vec<Config> {
    let roots = parse(text);
    // An anchor defined twice (in the branches of an ERB conditional) is
    // ambiguous and resolves to nothing.
    let mut anchors: HashMap<&str, Option<&Node>> = HashMap::new();
    fn index<'n>(nodes: &'n [Node], anchors: &mut HashMap<&'n str, Option<&'n Node>>) {
        for n in nodes {
            if let Some(a) = &n.anchor {
                anchors.entry(a.as_str()).and_modify(|e| *e = None).or_insert(Some(n));
            }
            index(&n.children, anchors);
        }
    }
    index(&roots, &mut anchors);

    // `<<: *a` merges: a key set on the node wins; among merges the first.
    fn dump(node: &Node, anchors: &HashMap<&str, Option<&Node>>, depth: usize) -> Option<DumpDecl> {
        if let Some(d) = &node.schema_dump {
            return Some(d.clone());
        }
        if depth > 16 {
            return None;
        }
        node.merges.iter().find_map(|a| anchors.get(a.as_str()).copied().flatten().and_then(|n| dump(n, anchors, depth + 1)))
    }
    fn selects_a_database(node: &Node) -> bool {
        node.schema_dump.is_some() || !node.merges.is_empty() || node.children.iter().any(|c| c.key == "adapter")
    }
    let Some(env) = roots.iter().find(|n| n.key == ENV) else { return Vec::new() };
    let mut configs = Vec::new();
    // Two-tier files put the one configuration straight under the environment.
    if env.children.iter().any(|c| c.key == "adapter") || env.schema_dump.is_some() {
        configs.push(Config {
            name: "primary".into(),
            primary: true,
            schema_dump: dump(env, &anchors, 0).unwrap_or(DumpDecl::Default),
        });
    }
    for child in &env.children {
        if child.value.is_empty() && selects_a_database(child) {
            configs.push(Config {
                name: child.key.clone(),
                primary: child.key == "primary" || child.key.starts_with("primary_"),
                schema_dump: dump(child, &anchors, 0).unwrap_or(DumpDecl::Default),
            });
        }
    }
    configs
}

/// An indentation tree of the file's `key: value` lines. ERB lines (a line
/// that opens with `<%`) and comments are skipped; ERB inside a value is
/// left as written.
fn parse(text: &str) -> Vec<Node> {
    let mut roots: Vec<Node> = Vec::new();
    // (indent, path of child indexes) for the open nodes.
    let mut open: Vec<(usize, Vec<usize>)> = Vec::new();
    // Open ERB conditionals and loops. What a branch declares may not hold,
    // so declarations inside one are not read.
    let mut erb_depth = 0usize;
    for line in text.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("<%") && !trimmed.starts_with("<%#") && !trimmed.starts_with("<%=") {
            let tag = trimmed.trim_start_matches("<%").trim_start_matches('-').trim_start();
            let body = tag.split("%>").next().unwrap_or("").trim_end();
            if ["if ", "unless ", "case "].iter().any(|k| tag.starts_with(k)) || body.ends_with(" do") || body.ends_with('|') {
                erb_depth += 1;
            } else if body == "end" {
                erb_depth = erb_depth.saturating_sub(1);
            }
        }
        if trimmed.is_empty() || trimmed.starts_with('#') || trimmed.starts_with("<%") {
            continue;
        }
        let indent = line.len() - trimmed.len();
        let Some((key, rest)) = trimmed.split_once(':') else { continue };
        if key.contains(char::is_whitespace) && !key.starts_with("<<") {
            continue;
        }
        let rest = rest.trim();
        while open.last().is_some_and(|(i, _)| *i >= indent) {
            open.pop();
        }
        let parent_path = open.last().map(|(_, p)| p.clone());
        let siblings: &mut Vec<Node> = match &parent_path {
            None => &mut roots,
            Some(path) => {
                let mut node = &mut roots[path[0]];
                for i in &path[1..] {
                    node = &mut node.children[*i];
                }
                &mut node.children
            }
        };
        let key = key.trim().trim_matches(|c| c == '"' || c == '\'').to_string();
        if key == "<<" {
            // `<<: *anchor` belongs to the parent node.
            // An anchor named by ERB is chosen at boot; it is not guessed.
            if let Some(a) = rest.strip_prefix('*').filter(|a| !a.contains("<%") && erb_depth == 0) {
                let a = a.trim().to_string();
                let path = parent_path.clone();
                drop(siblings);
                attach(&mut roots, &path, |n| n.merges.push(a));
            }
            continue;
        }
        let (anchor, value) = match rest.strip_prefix('&') {
            Some(after) => match after.split_once(char::is_whitespace) {
                Some((a, v)) => (Some(a.to_string()), v.trim().to_string()),
                None => (Some(after.to_string()), String::new()),
            },
            None => (None, rest.to_string()),
        };
        if key == "schema_dump" {
            if erb_depth > 0 {
                continue;
            }
            let v = value.trim_matches(|c| c == '"' || c == '\'');
            let d = match v {
                "false" => DumpDecl::Off,
                "" | "nil" | "null" | "~" => DumpDecl::Default,
                v if v.contains("<%") => continue,
                v => DumpDecl::File(v.to_string()),
            };
            let path = parent_path.clone();
            drop(siblings);
            attach(&mut roots, &path, |n| n.schema_dump = Some(d));
            continue;
        }
        siblings.push(Node { key, value, anchor, ..Node::default() });
        let mut path = parent_path.unwrap_or_default();
        path.push(siblings.len() - 1);
        open.push((indent, path));
    }
    roots
}

fn attach(roots: &mut [Node], path: &Option<Vec<usize>>, f: impl FnOnce(&mut Node)) {
    let Some(path) = path else { return };
    let mut node = &mut roots[path[0]];
    for i in &path[1..] {
        node = &mut node.children[*i];
    }
    f(node);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dump_declarations_follow_merges_and_skip_erb() {
        let yml = "\
base: &base
  schema_dump: \"events_structure.sql\"
other: &other
  <<: *base
  <% if x %>
  pool: 1
  <% end %>
test:
  primary:
    adapter: postgresql
  events:
    <<: *other
  audit:
    adapter: postgresql
    schema_dump: false
  reporting:
    adapter: postgresql
";
        let configs = configurations(yml);
        let got: Vec<(&str, bool, &DumpDecl)> = configs.iter().map(|c| (c.name.as_str(), c.primary, &c.schema_dump)).collect();
        assert_eq!(
            got,
            vec![
                ("primary", true, &DumpDecl::Default),
                ("events", false, &DumpDecl::File("events_structure.sql".into())),
                ("audit", false, &DumpDecl::Off),
                ("reporting", false, &DumpDecl::Default),
            ]
        );
    }

    #[test]
    fn merges_through_erb_or_ambiguous_anchors_are_not_guessed() {
        let yml = "\
sql_dump: &sql_dump
  schema_dump: \"a_structure.sql\"
variant: &variant
  schema_dump: \"b_structure.sql\"
variant: &variant
  schema_dump: false
test:
  chosen_at_boot:
    adapter: postgresql
    <<: *<%= \"sql_dump\" %>
  twice_defined:
    <<: *variant
  branched:
    adapter: postgresql
    <% if ENV[\"X\"] %>
    schema_dump: \"x_structure.sql\"
    <% end %>
  plain:
    <<: *sql_dump
";
        let configs = configurations(yml);
        let got: Vec<(&str, &DumpDecl)> = configs.iter().map(|c| (c.name.as_str(), &c.schema_dump)).collect();
        assert_eq!(
            got,
            vec![
                ("chosen_at_boot", &DumpDecl::Default),
                ("twice_defined", &DumpDecl::Default),
                ("branched", &DumpDecl::Default),
                ("plain", &DumpDecl::File("a_structure.sql".into())),
            ]
        );
    }
}
