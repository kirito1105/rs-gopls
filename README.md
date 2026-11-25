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
python .\tools\compare_definitions.py $server --baseline --iterations 60
```

Launch the resulting executable as a stdio language server with a Go module, a go.work directory, or a GOPATH project as its LSP workspace root. The benchmark's optional `--baseline` uses `gopls` from PATH. It checks exact definition locations, including after unsaved edits to the caller and dependency, before reporting latency. Add --stdlib to test unsaved caller edits that resolve into the installed Go SDK net/http package. Both workloads use a small generated caller. The harness asks gopls to run diagnostics on save to reduce unrelated work during measured edits. Treat startup as a single sample with shared system and Go caches.

## Definition coverage

Definition lookup covers imports, lexical and package names, and common field and method selections. The resolver follows named types, explicit generic arguments, aliases, promoted members, and function and method results. It uses the Go toolchain for active source files and package identities.

The server supports Go Modules, go.work, GOPATH, and test package variants. On clients that support dynamic file-watch registration, it refreshes indexes after changes to Go files and module/workspace files. Open editor buffers take precedence over disk content. Go package metadata follows the on-disk workspace.

Type analysis covers the tested syntax-driven cases. Implicit generic type inference, structural and function-valued types, and all GoLand-specific navigation behavior remain outside the verified coverage.

Run the semantic and workspace acceptance checks with the release executable:

~~~powershell
python -B .\tools\check_definitions.py $server --stage 3
python -B .\tools\check_workspace_updates.py $server
~~~

Use the optional workspace argument on the first command to check the matching localchat/server source revision. See the [first-stage definition roadmap](docs/definition-plan.md) for existing coverage and the [next-stage plan](docs/definition-next-phase.md) for upcoming milestones. See the [GoLand navigation baseline](docs/goland-definition-baseline.md) for real-project targets.

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
