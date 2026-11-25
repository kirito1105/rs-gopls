# rs-gopls

Go language server prototype in Rust. It follows the GoLand Go plugin's separation between syntax, declaration indexes, and reference resolution.

- `src/workspace.rs` loads the package graph and active source files through `go list`, using the Go SDK and build environment on PATH.
- `src/semantic.rs` parses Go into a Tree-sitter syntax tree, extracts lightweight per-file declaration indexes, and resolves package-qualified references using import aliases, package names and local scopes. Open files use editor content. Ranged edits update the existing syntax tree and line index.
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

Launch the resulting executable as a stdio language server with a Go module as its LSP workspace root. The benchmark's optional `--baseline` uses `gopls` from PATH. It checks exact definition locations, including after unsaved edits to the caller and dependency, before reporting latency. Add --stdlib to test unsaved caller edits that resolve into the installed Go SDK net/http package. Both workloads use a small generated caller. The harness asks gopls to run diagnostics on save to reduce unrelated work during measured edits. Treat startup as a single sample with shared system and Go caches.

## Prototype scope

Definition lookup covers exported package-level functions, variables, constants and types in a single Go module and its loaded dependencies. The package graph comes from disk on the first applicable request. Restart the server after adding or removing files or changing build configuration or module dependencies. Project-wide file watching, test variants, type-based member resolution, completion and diagnostics are future work.

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

Use `-PrepareOnly` to stage the extension, native server and example without opening a window. To use your own Go project, open its folder in the same test profile. In a different Zed profile with the extension installed, explicitly opt in to `rs-gopls` in that profile's user settings:

```json
{
  "languages": {
    "Go": { "language_servers": ["rs-gopls"] }
  }
}
```

For a legacy GOPATH project, also set `lsp.rs-gopls.binary.env.GO111MODULE` to `"off"` and set `lsp.rs-gopls.binary.env.GOPATH` if needed. Zed can use `lsp.rs-gopls.binary.path` when you want to point the extension at a different local build.

