# Building Skim WASM

This guide explains how to build the WASM bindings for Skim.

## Prerequisites

1. **Rust toolchain** (1.70+)
   ```bash
   curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
   ```

2. **wasm-pack** (WASM build tool)
   ```bash
   curl https://rustwasm.github.io/wasm-pack/installer/init.sh -sSf | sh
   ```

3. **Node.js** (18+) for testing
   ```bash
   # On macOS
   brew install node

   # On Linux
   curl -fsSL https://deb.nodesource.com/setup_18.x | sudo -E bash -
   sudo apt-get install -y nodejs
   ```

## Build for Web (Browser)

```bash
cd crates/skim-wasm

# Build for web target
wasm-pack build --target web --release

# Output: ./pkg/
#   skim_wasm.js         # JavaScript bindings
#   skim_wasm_bg.wasm    # WASM binary
#   skim_wasm.d.ts       # TypeScript definitions
#   package.json         # npm package metadata
```

### Test in Browser

```bash
# Serve the example
python3 -m http.server 8000

# Open http://localhost:8000/examples/basic.html
```

## Build for Node.js

```bash
cd crates/skim-wasm

# Build for Node.js target
wasm-pack build --target nodejs --release

# Test the example
node examples/node-example.js
```

## Build for Bundlers (Webpack, Rollup, etc.)

```bash
cd crates/skim-wasm

# Build for bundler target
wasm-pack build --target bundler --release
```

## Development Build (Faster Compilation)

```bash
# Debug build (faster, larger binary)
wasm-pack build --target web --dev

# With source maps for debugging
wasm-pack build --target web --dev -- --features console_error_panic_hook
```

## Optimize Build Size

The release build is already optimized for size (opt-level = "s"), but you can further optimize:

```bash
# Install wasm-opt
cargo install wasm-opt

# Build
wasm-pack build --target web --release

# Optimize WASM binary
wasm-opt -Oz -o pkg/skim_wasm_bg_opt.wasm pkg/skim_wasm_bg.wasm

# Replace original
mv pkg/skim_wasm_bg_opt.wasm pkg/skim_wasm_bg.wasm
```

## Publish to npm

```bash
cd crates/skim-wasm

# Build for all targets
wasm-pack build --target web --release

# Login to npm (first time only)
npm login

# Publish to npm
wasm-pack publish --access public
```

**Note:** Package will be published as `skim-wasm` (or `@skim/wasm` if scoped).

## Common Issues

### Error: `wasm-pack` not found
```bash
# Install wasm-pack
curl https://rustwasm.github.io/wasm-pack/installer/init.sh -sSf | sh
```

### Error: `wasm32-unknown-unknown` target not installed
```bash
# Add WASM target to Rust
rustup target add wasm32-unknown-unknown
```

### Error: Build fails with tree-sitter linking errors
This is expected in WASM builds because tree-sitter grammars need to be loaded differently in WASM.

**Current Status (v0.2.0):**
- ✅ WASM bindings implemented
- ⏳ Grammar loading via `web-tree-sitter` (Phase 2)
- ⏳ Integration with `@vscode/tree-sitter-wasm` (Phase 2)

**Workaround:** For now, WASM build compiles but won't work until Phase 2 (grammar integration) is complete.

## Build Output

Successful build produces:

```
pkg/
├── skim_wasm.js          # JavaScript bindings
├── skim_wasm_bg.wasm     # WASM binary (~500KB optimized)
├── skim_wasm.d.ts        # TypeScript definitions
├── package.json          # npm metadata
└── README.md             # Auto-generated from crate README
```

## Next Steps

1. **Phase 2:** Integrate `web-tree-sitter` runtime
2. **Phase 2:** Add lazy grammar loading from `@vscode/tree-sitter-wasm`
3. **Phase 3:** Create npm package with all targets
4. **Phase 3:** Add browser compatibility tests

See [WASM_ROADMAP.md](../../.docs/WASM_ROADMAP.md) for full implementation plan.
