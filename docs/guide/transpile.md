# `roundhouse --target` — transpile an app

```sh
roundhouse --target rust -o out/rust /path/to/your/rails/app
cd out/rust
```

That writes a standalone Rust project: models, controllers, views and
routes translated from your app, a small hand-written runtime library
for the parts that don't belong in generated code (the database
connection, the HTTP server, Action Cable), tests, and a `README.md`
that says how to build and run it. There is no Rails in the output and
no Ruby at runtime. The same command with a different `--target`
writes the same app as a Go module, a TypeScript package, a Crystal
shard, a Mix project, a Gradle build, a Swift package, a Python
project, a .NET solution, or a Ruby tree that runs on stock CRuby or
JRuby — or the Ruby shape that [Spinel](spinel.md) compiles to a
native binary.

## The command

```
roundhouse --target LANG [-o OUT] [INPUT] [--survey] [--allow-unsupported]
```

- `LANG` — one of `crystal`, `csharp`, `elixir`, `go`, `kotlin`,
  `python`, `rust`, `swift`, `typescript`, `typescript-worker`, `ruby`,
  `jruby`, `spinel`. [`targets.md`](targets.md) describes each.
- `INPUT` — the Rails app's root; default is the working directory.
- `-o OUT` — where to write; default is `./out/<lang>/`. The directory
  is created if needed; files are written over an existing one, and
  files a previous run left behind are not removed, so regenerate into
  a clean directory when the app has lost files.

## Test folders

Roundhouse reads every `.rb` file recursively from these folders by default:

- `test/models`
- `test/controllers`
- `test/helpers`
- `test/channels`
- `test/lib`

The app root can contain a `roundhouse.yml` file that adds folders:

```yaml
test_paths:
  - test/unit
  - quality/specs
```

`test_paths` adds folders. It does not replace the default folders. Each
value must name a literal directory relative to the app root. Roundhouse
does not expand globs, environment variables, or `~`. A configuration
error stops ingestion.
Roundhouse rejects a configured path with a symbolic link. It does not
follow symbolic links below selected folders.

Roundhouse reads `test/test_helper.rb`, `test/test_helpers/`, and
`test/fixtures/` outside this folder list.

### Database configurations

Without further configuration, Roundhouse loads `db/schema.rb`, or
`db/structure.sql` when there is no `db/schema.rb`. An app with several
databases, a schema elsewhere, or a configuration resolved through ERB
states Rails' own resolution as data instead:

```yaml
database_configurations: config/roundhouse/databases.json
```

Write the manifest inside the app, under the environment it describes:

```sh
RAILS_ENV=test bin/rails runner /path/to/roundhouse/scripts/rails-database-manifest.rb -- \
  --out config/roundhouse/databases.json
```

The manifest lists each configuration of the test environment (name,
adapter, primary or replica, schema format, schema source,
`schema_search_path`, non-secret options, migrations paths) and pins, by
sha256, the configuration file Rails loaded, `Gemfile.lock`, every
schema file it selects, and the extractor itself. It never records
credentials, hosts, database names or code. An input whose name or
value looks like a credential is recorded by name only. When the authored
setup loads a schema that Rails would not dump (for example
`schema_dump: false` with a versioned dump), name it with
`--schema-source NAME=PATH:FORMAT:NOTE`. Roundhouse does not infer these
filenames.

The primary's schema is chosen by its configured format and source, not
by which file exists. A manifest that is missing, from another version,
environment or extractor, or whose pins no longer match the tree is an
error. Roundhouse never falls back to the default order. The recorded Rails and Ruby
versions are checked against `Gemfile.lock` where it locks them. The
hashes do not prove that the manifest is fresh. ERB in a
configuration file can read environment variables, other files, and app
constants that the manifest does not hash, and the source revision is
recorded but not verified. Roundhouse reports these as a warning.
Regenerate the manifest whenever any input changes.

Which database a model uses comes from source: a literal
`connects_to database: { writing: :name }` in an abstract class, which
its subclasses inherit. A database the emitted runtime does not open is
an error at that `connects_to`. Today that is any database other than
the primary, and any postgresql, postgis, mysql2 or trilogy database.
Other routing is also an error at its call: a computed or conditional
`connects_to`, `shards:`, `establish_connection`, `connected_to`, or a
connection method the app defines itself.

Transpilation is the analysis from [`check.md`](check.md) followed by
lowering and emit. It requires zero analysis errors and a runtime for
the modeled constructs on the selected target. A clean target-independent
`check` does not imply availability on every target (see Date below).
On a covered app the command prints nothing and exits 0. Otherwise it
stops, and the two flags decide how.

### Date-only values

`t.date` is modeled as `Date` (nullable as `Date?`), not `Time`.
The `ruby` target hydrates a native Date from YYYY-MM-DD storage text,
normalizes Date or ISO date text writes, and preserves SQL NULL as nil.
`Date#>>` uses Ruby's calendar arithmetic, including end-of-month clamping;
Date JSON is an ISO date without a clock or zone. DateTime/time columns
retain their timestamp type and runtime.

Date-only emission is currently supported and executed only on `ruby`.
`jruby`, `spinel`, `roda` and the non-Ruby targets (including
`typescript-worker`) reject Date at the project boundary before emitting
files, even with `--allow-unsupported`. This is an observable support
boundary, replacing silent timestamp treatment, not evidence that these
languages cannot represent dates. JRuby's date-only adapter path remains
unverified. The `blog` source archive is not a transpilation target.

This is a bounded Date surface, not all of ActiveSupport's Date extensions:
unmodeled methods still diagnose, and nonliteral strict-local defaults
remain an existing ingestion gap.

### Forwarding a computed Proc

Ruby emission preserves `items.map(&@callback)` and
`items.map(&factory())`, including evaluating `factory()` once before
the call. Arbitrary `&expr` forwarding is not implemented for `rust`,
`crystal`, `go`, `python`, `kotlin`, `swift` or `elixir`: these targets
reject it at the project boundary, even with `--allow-unsupported`,
rather than dropping the callback or evaluating its producer per item.
Existing literal blocks, local block variables and bound-method
references retain their target-specific support and limitations.

Roundhouse currently refuses generated enum `*_before_type_cast` readers.
Rails returns the original input after an unsaved assignment and stored data
after persistence; the compiler has only the stored enum slot. Negative enum
scopes are supported. Source-defined readers keep their ordinary behavior.

## Apps that aren't fully covered yet

By default, ingest is strict — the first construct roundhouse does not
recognize aborts the run — and analysis errors fail the emit. That is
the right contract for an app you intend to ship from the output: a
transpile that silently dropped something is worse than one that
refused.

For a first look at an app you know is outside the current coverage:

```sh
roundhouse --target ruby --survey --allow-unsupported ~/src/mastodon -o out/mastodon
```

`--survey` keeps ingest going past unrecognized constructs, recording
each as a gap (with a nil placeholder in its place) and printing the
deduplicated punch list at the end — the same report `check --continue`
prints. `--allow-unsupported` lets emit proceed past the diagnostics
those gaps and the analyzer's own limits produce: a stub is emitted at
each unsupported site, the diagnostics are downgraded to warnings, and
the output is written anyway. Together they answer *how much of this
app comes through today, and what exactly doesn't* — every stub is
marked in the output, and the punch list is the inventory. What they
produce is a survey, not a deployable; a stubbed action renders
nothing useful.

Before either flag, the `wont_lower` question is worth asking: which
constructs in this app have no lowering for this target at all. The
MCP server's [`wont_lower` tool](mcp.md) answers it per target without
running the emit.

## What comes out

Every emitted project has the same shape at the top: the translated
app under the target's conventional source layout, the runtime
library beside it, `db/seed.sql` (schema plus the app's seeds, self-
contained), prebuilt static assets when the app ships them, a test
suite, and for the server targets an `e2e/` Playwright suite. The
`README.md` in each one is short and specific — prerequisites, build,
setup, run, test — and it is executed verbatim by the project's CI
against the blog fixture, so the commands in it are known to work.

What does NOT come out is sorbet-runtime. An app that annotates with
it transpiles to a tree that does not need it at run time: a `sig` is
read (see [`check.md`](check.md)) and then dropped, along with
`extend T::Sig`, `abstract!` and the rest of the annotations;
`T.type_alias` constants go with the signatures that were their only
reader, except where a check below reads them. The assertions split by
what they do natively:

- `T.must`, `T.must_because` and the `#: as !nil` comment keep their
  nil check on the Ruby trees: `TypeError` with sorbet-runtime's
  message, raised where the assertion stood.
- The `#: as Type` comment becomes the value it wraps, unchanged: it is
  inert natively, so `nil #: as Integer` is nil here as it is there,
  never `0`.
- `T.let`, `T.cast`, `T.bind` and `T.assert_type!` keep sorbet-runtime's
  class check on the CRuby and JRuby trees. Each lowers to
  `SorbetRuntimeCheck.cast(value, type, "T.let", path, line)` (the
  overlay's `runtime/sorbet_runtime_check.rb`), which returns the value
  or raises sorbet's `TypeError`, message and `Caller:` line included.
  The type is checked as sorbet 0.6.13502 does: the outer class
  (`is_a?`), `T.nilable` as nil-or, `T.any` as any-of, `T::Boolean` as
  true/false, `T.class_of`, and the stdlib generics (`T::Array[X]`, ...)
  and user generics by their outer class only. A check that cannot fail
  (`T.untyped`, `checked: false`, `T.let(nil, T.nilable(X))`,
  `T.let([], T::Array[X])`) is the value alone. A type the check cannot
  rebuild faithfully (`T.proc`, `T.self_type`, `T.attached_class`, type
  parameters, `T.all`, tuple and hash shapes, sorbet's own constants
  such as `T::Struct`) is refused at the site by name. A type alias
  (`X = T.type_alias { T }` in a library class, model, test or module
  body) that a check reads (directly or through another alias) is
  carried on the Ruby trees as
  `X = SorbetRuntimeCheck.type_alias { <T> }`, resolved at its first
  check as sorbet's `aliased_type` is and unwrapped wherever a type is
  read (a union of aliases names their flattened members); the analysis
  reads the declared type through it. Still dropped, so their sites keep
  the refusal: aliases whose type is a form above (shapes, procs), aliases
  in controller bodies or `class << self`, and the blockless
  `T.type_alias(T)`. A file-level alias outside any class is written at
  the top level of its file, like every file-level constant (see below), but
  the analysis expands it only inside the file's first class. The other
  targets (spinel, roda and the strict ones) refuse the check by name
  (`sorbet type check not supported`) rather than drop it.
  `tests/author_casts_run.rs` compares every result and message with
  the native run on Core's sorbet-runtime.
- The type a cast declares still types the analysis, and
  `roundhouse check` trusts it. Lowerings do not fold a value-dependent
  predicate on it: `n = list.first #: as Integer; n.present?` keeps the
  runtime predicate (`ActiveSupport.present?(n)`, false for nil), where
  folding on the declared `Integer` would answer `true`. A class-typed
  cast keeps its grounding plus a nil check. A declared type that
  arrives through a method return or an RBS signature is trusted like
  any other declared type.
- The strict targets keep the conversion each emitter writes for a `#:`
  cast or a check that cannot fail
  (Go and Swift assert the type and fail loudly; C# `Convert.*` and
  Kotlin's parse convert), because a typed slot needs a representation.
  An author cast with no backing type is a known per-target divergence
  there, not something the Ruby trees share.

The two constructs that are class GENERATORS rather than
annotations are lowered into the plain Ruby they stand for — a
`T::Struct` into readers plus the keyword constructor it generates
(and `==`, where it included `ActsAsComparable`), a `T::Enum` into its
members plus `serialize` / `values` / `deserialize` /
`try_deserialize` / `from_serialized` / `has_serialized?`. `T.absurd`
is the one that keeps behavior rather than losing it: it RAISES where
it stood, message and value included, because dropping it would turn
"this cannot happen" into "this returns nil".

What remains is a `const` or `prop` on a base class from a gem: nothing
in the tree says that DSL is a typed prop rather than some other one,
and guessing from the method name would rewrite calls that were never
props.

A constant a file writes at file level, outside every class and
module, is Object's in Ruby, and a bare read from any class in any
file finds it. The Ruby trees write it at the top level of the
emitted file of that source file's first class, ahead of the class
where the source had it ahead of the first declaration and after it
otherwise; that file then loads at boot. A file with no class or
module declaration has no such carrier unless it declares a factory
class (`X = Struct.new(...) do … end`): its other constants, such as
a class alias `RemoteDomain = DomainSubscription` or a blockless
`Pair = Struct.new(:a, :b)`, are not carried, and a read of one is
refused as `constant not supported`. The strict targets keep the
constant on the first class. `tests/file_level_constants_run.rs`
compares the CRuby tree with the native run.

The server targets all serve the app on `http://localhost:3000` (`PORT`
overrides), with Action Cable at `/cable`, against a SQLite file at
the Rails-traditional `storage/development.sqlite3`. Setup is one
line:

```sh
sqlite3 storage/development.sqlite3 < db/seed.sql
```

then the target's build and run commands from its README — for Rust,
`cargo build --release` and `./target/release/app`; for Go,
`go mod tidy && go build -o server . && ./server`; and so on.

Emitted code is meant to be read. Method names, file layout and the
order of things follow the Ruby they came from; the runtime library is
a few files of ordinary code in the target language, not a framework.
The emitted tests come from the default test folders and any additional
folders in `roundhouse.yml`.

## Regenerating

The output is not meant to be edited and kept. Change the Rails app,
run the command again. Each README ends with the exact `roundhouse
--target` line that produced the tree, so a project can be regenerated
by someone who has only the output. If you find yourself patching the
emitted code, that is a bug report: either a construct roundhouse
should lower and doesn't, or a translation it gets wrong — and the
[compare oracle](verifying.md) is how to show the second kind.

## Which target

For a service you intend to run, the answer depends on what you and
your team already operate; every server target passes the same
conformance gate against the blog fixture, and [`targets.md`](targets.md)
says what each is additionally tested against. For the closest
behavior to Rails itself — the widest slice of the framework and
the fewest [deliberate divergences](rails-coverage.md) — the Ruby
shape compiled by Spinel is ahead of every other target and will stay
ahead for the foreseeable future, because its runtime *is* the
framework runtime the other targets get a translation of.
[`spinel.md`](spinel.md) is that door.
