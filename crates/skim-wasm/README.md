# @skim/wasm

WASM-powered code transformation for JavaScript and TypeScript.

Transform source code by intelligently removing implementation details while preserving structure, signatures, and types - perfect for optimizing code for LLM context windows.

## Features

- 🌐 **Universal** - Works in browsers and Node.js
- 🚀 **Fast** - WASM-compiled Rust with tree-sitter parsing
- 📦 **Small** - ~2.3MB total with all 6 language grammars
- 🔒 **Safe** - Built-in DoS protections and memory limits
- 📝 **6 Languages** - TypeScript, JavaScript, Python, Rust, Go, Java
- 🎯 **4 Modes** - Structure, Signatures, Types, Full

## Installation

```bash
npm install @skim/wasm
```

## Usage

### Basic Example

```javascript
import { transform, Language, Mode } from '@skim/wasm';

const sourceCode = `
function add(a: number, b: number): number {
  return a + b;
}
`;

const result = transform(sourceCode, Language.TypeScript, Mode.Structure);

console.log(result.content);
// Output: function add(a: number, b: number): number { /* ... */ }

console.log(`Reduction: ${result.reductionPercentage}%`);
// Output: Reduction: 75.2%
```

### Languages

```javascript
Language.TypeScript
Language.JavaScript
Language.Python
Language.Rust
Language.Go
Language.Java
```

### Modes

```javascript
Mode.Structure   // Remove function bodies (70-80% reduction)
Mode.Signatures  // Extract only signatures (85-92% reduction)
Mode.Types       // Extract only type definitions (90-95% reduction)
Mode.Full        // No transformation (0% reduction)
```

### Advanced Example

```javascript
import { transform, Language, Mode } from '@skim/wasm';

// Read file content
const code = await fetch('/api/code').then(r => r.text());

// Transform
const result = transform(code, Language.Python, Mode.Signatures);

// Use in LLM context
const prompt = `
Analyze this Python API:

${result.content}

What endpoints are exposed?
`;
```

## API Reference

### `transform(source, language, mode)`

Transform source code.

**Parameters:**
- `source` (string) - Source code to transform
- `language` (Language) - Programming language
- `mode` (Mode) - Transformation mode

**Returns:**
- `TransformResult` object with:
  - `content` (string) - Transformed code
  - `originalSize` (number) - Original size in bytes
  - `transformedSize` (number) - Transformed size in bytes
  - `reductionPercentage` (number) - Reduction percentage

**Throws:**
- Error string if transformation fails

### `log(message)`

Log a message to browser console (for debugging).

## Use Cases

### 1. Code Editor Extension

```javascript
import { transform, Language, Mode } from '@skim/wasm';

function skimCurrentFile(editor) {
  const text = editor.document.getText();
  const lang = detectLanguage(editor.document.languageId);

  const result = transform(text, lang, Mode.Structure);

  // Show in new tab
  editor.openTextDocument({ content: result.content });
}
```

### 2. Documentation Generator

```javascript
import { transform, Language, Mode } from '@skim/wasm';

async function extractAPI(files) {
  const apis = [];

  for (const file of files) {
    const content = await readFile(file);
    const result = transform(content, Language.TypeScript, Mode.Signatures);
    apis.push({ file, signatures: result.content });
  }

  return apis;
}
```

### 3. LLM Context Optimization

```javascript
import { transform, Language, Mode } from '@skim/wasm';

function optimizeForLLM(codebase) {
  return codebase.map(file => {
    const result = transform(file.content, file.language, Mode.Structure);
    return {
      ...file,
      optimized: result.content,
      savingsPercent: result.reductionPercentage
    };
  });
}
```

## Performance

- **Parse + Transform:** <50ms for 1000-line files
- **Token Reduction:** 60-95% depending on mode
- **Bundle Size:** ~2.3MB with all grammars (lazy-loadable in future)

## Security

Built-in protections against:
- Stack overflow attacks (max depth: 500)
- Memory exhaustion (max input: 50MB, max nodes: 100k)
- UTF-8 boundary violations
- Path traversal attacks

## Browser Compatibility

- ✅ Chrome 57+
- ✅ Firefox 52+
- ✅ Safari 11+
- ✅ Edge 16+

## License

MIT

## Links

- [GitHub Repository](https://github.com/dean0x/skim)
- [Documentation](https://github.com/dean0x/skim/tree/main/.docs)
- [Native CLI](https://crates.io/crates/skim)
