# rs-gopls

Go language server prototype in Rust. It follows the GoLand Go plugin's separation between syntax, declaration indexes, and reference resolution.

- `src/workspace.rs` loads the package graph and active source files through `go list`, using the Go SDK and build environment on PATH.
- `src/semantic.rs` parses Go into a Tree-sitter syntax tree, extracts lightweight per-file declaration indexes, and binds local and package names and resolves fields, methods, and call-result selectors. Open files use editor content. Ranged edits update the existing syntax tree and line index.
- `src/main.rs` serves the resolver as `textDocument/definition` over stdio LSP with UTF-16 positions.

## Build and verify

From this repository, with Rust, Go, Python and a C toolchain installed:

```powershell
cargo check -p rs-gopls --locked
cargo test -p rs-gopls --locked
cargo build -p rs-gopls --release --locked
$target = (cargo metadata --format-version 1 --no-deps --locked | ConvertFrom-Json).target_directory
$server = Join-Path $target 'release\rs-gopls.exe'
python -B .\tools\compare_definitions.py $server --iterations 60
```

Launch the resulting executable as a stdio language server with a Go module, a go.work directory, or a GOPATH project as its LSP workspace root. The command above is a local latency check that verifies exact definition locations after unsaved edits. Add `--stdlib` to test a generated caller that resolves into the installed Go SDK's `net/http` package.

## Navigation performance

Use actual GoLand navigation queries as the performance reference. Fix the project revision, Go SDK, search scope, and query positions. Compare complete matching result sets and report startup, new queries on ready indexes, repeated queries, and edits as separate workloads. [Measurement protocol](docs/PLANS.md#导航性能对照) · [GoLand implementation timings](docs/goland-implementation-baseline.md#与-goland-直接对照)

## Definition coverage

Definition lookup covers imports, lexical and package names, and common field and method selections. The resolver follows named and anonymous types, explicit arguments on generic types, aliases, promoted members, container elements, and callable results. It covers range and select bindings, local callbacks, method values and expressions, and package variables initialized by calls. It uses the Go toolchain for active source files and package identities.

The server supports Go Modules, go.work, GOPATH, and test package variants. On clients that support dynamic file-watch registration, it refreshes indexes after changes to Go files and module/workspace files. Open editor buffers take precedence over disk content, including build-neutral new files in an already loaded package. The server refreshes Go toolchain metadata in the background and keeps local bindings and unaffected packages available while the graph loads.

Generic function calls cover common explicit, partial and inferred arguments, result substitution, constraint methods, and slice and map shape constraints. See the GoLand baseline for the validated editor behavior and workspace revisions.

Run the semantic and workspace acceptance checks with the release executable:

~~~powershell
python -B .\tools\check_definitions.py $server --stage 6
python -B .\tools\check_workspace_updates.py $server
~~~

Use the optional workspace argument on the first command to check the matching localchat/server source revision. See the [first-stage definition roadmap](docs/plans/2026-11-25-go-definition.md), [second-stage plan](docs/plans/2026-11-25-go-definition-phase-two.md), and [third-stage navigation plan](docs/plans/2026-11-25-go-navigation-phase-three.md). The [GoLand navigation baseline](docs/goland-definition-baseline.md) records real-project targets.

## Type definition

The same Rust resolver handles `textDocument/typeDefinition`. It follows the type of a name, field, or inferred value to its Go declaration. The GoLand baseline records exact targets for named and anonymous types, aliases, function results, and common generic cases. [Type definition checks and performance comparison](docs/goland-type-definition-baseline.md)

## Find references

The server provides `textDocument/references` for local, package and cross-package Go uses, including test variants and open buffers. It verifies each candidate against the selected declaration, and larger searches run in cancellable batches. [GoLand reference sets and benchmarks](docs/goland-references-baseline.md)

## Go to implementation

Use Zed's **Go to Implementation** on a Go interface type or method to find implementations. From a concrete type or method, the same action finds matching interfaces or interface methods. Searches use the active build and test packages, including open buffers. [GoLand implementation sets and benchmarks](docs/goland-implementation-baseline.md)

## Hover and signature help

The server provides `textDocument/hover` and `textDocument/signatureHelp` (triggered on `(` and `,`). Hover renders Markdown Go signatures with inferred types, struct tags, generic parameters, package docs, and GoDoc comments (filtering compiler directives). Signature help resolves functions, methods, interface methods, generic instantiations, method expressions, function values, and builtins with expanded grouped parameters, variadic clamping, UTF-16 parameter highlight offsets, and unclosed-call editing tolerance. [GoLand hover and signature help baseline and benchmarks](docs/goland-hover-signature-baseline.md)

## Test in Zed on Windows

The extension uses Zed's existing Go language mode and registers `rs-gopls` as an opt-in language server. It accepts the worktree environment, including Go toolchain and GOPATH settings.

The release launcher opens the ready-to-test example using a separate Zed profile:

```powershell
.\tools\test_zed.ps1
```

In that window, trust the example project if Zed shows Restricted Mode. Open `main.go` from the project tree and use **Go to Definition** on `Hello`. The target is `greeting/greeting.go`. The separate Zed profile uses `rs-gopls` for `.go` files and disables language servers for `go.mod` and `go.work`.

To rebuild both artifacts using the configured Cargo output directory:

```powershell
rustup target add wasm32-wasip2
cargo build -p rs-gopls --release --locked
cargo build -p rs-gopls-zed --release --locked --target wasm32-wasip2
.\tools\test_zed.ps1
```

Use `-PrepareOnly` to stage a new server build in the isolated profile without opening a window. The launcher stages each build under a versioned filename so Zed can switch while an older binary is running. To use your own Go project, open its folder in the same test profile. In a different Zed profile with the extension installed, explicitly opt in to `rs-gopls` in that profile's user settings:

```json
{
  "languages": {
    "Go": { "language_servers": ["rs-gopls"] }
  }
}
```

For a legacy GOPATH project, also set `lsp.rs-gopls.binary.env.GO111MODULE` to `"off"` and set `lsp.rs-gopls.binary.env.GOPATH` if needed. Zed can use `lsp.rs-gopls.binary.path` when you want to point the extension at a different local build.
