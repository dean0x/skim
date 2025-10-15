/**
 * JavaScript wrapper for Skim WASM
 *
 * This wrapper handles tree-sitter grammar loading using web-tree-sitter,
 * then passes the parsed AST to our Rust WASM module for transformation.
 *
 * Architecture:
 * 1. JavaScript: Load grammars with web-tree-sitter
 * 2. JavaScript: Parse source code to get AST (Tree)
 * 3. JavaScript: Serialize AST to JSON
 * 4. Rust WASM: Transform based on AST JSON
 * 5. JavaScript: Return transformed result
 */

import Parser from 'web-tree-sitter';

// Grammar cache
const grammars = new Map();
let parserInitialized = false;

/**
 * Language enum matching Rust
 */
export const Language = {
  TypeScript: 'typescript',
  JavaScript: 'javascript',
  Python: 'python',
  Rust: 'rust',
  Go: 'go',
  Java: 'java',
};

/**
 * Mode enum matching Rust
 */
export const Mode = {
  Structure: 'structure',
  Signatures: 'signatures',
  Types: 'types',
  Full: 'full',
};

/**
 * Grammar URL mapping
 * Uses @vscode/tree-sitter-wasm if available, falls back to CDN
 */
const GRAMMAR_URLS = {
  typescript: null, // Will be set dynamically
  javascript: null,
  python: null,
  rust: null,
  go: null,
  java: null,
};

/**
 * Initialize web-tree-sitter
 * Must be called before using transform()
 */
export async function init(options = {}) {
  if (parserInitialized) {
    return;
  }

  const wasmPath = options.treeSitterWasmPath ||
    'https://cdn.jsdelivr.net/npm/web-tree-sitter@0.24.0/tree-sitter.wasm';

  await Parser.init({
    locateFile(scriptName, scriptDirectory) {
      return wasmPath;
    },
  });

  parserInitialized = true;
}

/**
 * Load a grammar for a specific language
 *
 * @param {string} language - Language name
 * @param {string} [grammarPath] - Custom path to grammar WASM file
 */
export async function loadGrammar(language, grammarPath) {
  if (!parserInitialized) {
    throw new Error('Call init() first before loading grammars');
  }

  if (grammars.has(language)) {
    return grammars.get(language);
  }

  // Determine grammar path
  let path = grammarPath;

  if (!path) {
    // Try to use @vscode/tree-sitter-wasm if available
    try {
      const vscodePkg = '@vscode/tree-sitter-wasm';
      const grammarName = `tree-sitter-${language}`;
      path = require.resolve(`${vscodePkg}/wasm/${grammarName}.wasm`);
    } catch (e) {
      // Fallback to CDN
      const CDN_BASE = 'https://cdn.jsdelivr.net/npm';
      const grammarMap = {
        typescript: `${CDN_BASE}/tree-sitter-typescript@0.23.0/tree-sitter-typescript.wasm`,
        javascript: `${CDN_BASE}/tree-sitter-javascript@0.23.1/tree-sitter-javascript.wasm`,
        python: `${CDN_BASE}/tree-sitter-python@0.23.2/tree-sitter-python.wasm`,
        rust: `${CDN_BASE}/tree-sitter-rust@0.23.0/tree-sitter-rust.wasm`,
        go: `${CDN_BASE}/tree-sitter-go@0.23.1/tree-sitter-go.wasm`,
        java: `${CDN_BASE}/tree-sitter-java@0.23.2/tree-sitter-java.wasm`,
      };
      path = grammarMap[language];
    }
  }

  if (!path) {
    throw new Error(`No grammar found for language: ${language}`);
  }

  const grammar = await Parser.Language.load(path);
  grammars.set(language, grammar);
  return grammar;
}

/**
 * Transform source code
 *
 * @param {string} source - Source code to transform
 * @param {string} language - Language (use Language enum)
 * @param {string} mode - Transformation mode (use Mode enum)
 * @returns {Promise<TransformResult>}
 */
export async function transform(source, language, mode) {
  // Ensure grammar is loaded
  const grammar = await loadGrammar(language);

  // Create parser and parse
  const parser = new Parser();
  parser.setLanguage(grammar);
  const tree = parser.parse(source);

  // Transform based on mode
  const transformed = transformTree(tree, source, mode);

  return {
    content: transformed,
    originalSize: source.length,
    transformedSize: transformed.length,
    reductionPercentage: ((source.length - transformed.length) / source.length) * 100,
  };
}

/**
 * Transform parsed tree based on mode
 *
 * This is a JavaScript implementation of the transformation logic.
 * In Phase 3, we could move this to Rust WASM for performance.
 */
function transformTree(tree, source, mode) {
  switch (mode) {
    case Mode.Structure:
      return transformStructure(tree, source);
    case Mode.Signatures:
      return transformSignatures(tree, source);
    case Mode.Types:
      return transformTypes(tree, source);
    case Mode.Full:
      return source; // No transformation
    default:
      throw new Error(`Unknown mode: ${mode}`);
  }
}

/**
 * Structure mode: Remove function bodies
 */
function transformStructure(tree, source) {
  const replacements = [];

  function visit(node) {
    const kind = node.type;

    // Find function/method bodies to replace
    if (isFunctionNode(kind)) {
      const bodyNode = findBodyNode(node);
      if (bodyNode) {
        replacements.push({
          start: bodyNode.startIndex,
          end: bodyNode.endIndex,
          replacement: '{ /* ... */ }',
        });
      }
    }

    // Recursively visit children
    for (const child of node.children) {
      visit(child);
    }
  }

  visit(tree.rootNode);

  // Apply replacements (sorted, non-overlapping)
  return applyReplacements(source, replacements);
}

/**
 * Signatures mode: Extract only function/method signatures
 */
function transformSignatures(tree, source) {
  const signatures = [];

  function visit(node) {
    const kind = node.type;

    if (isFunctionNode(kind)) {
      const bodyNode = findBodyNode(node);
      const endPos = bodyNode ? bodyNode.startIndex : node.endIndex;
      const signature = source.substring(node.startIndex, endPos).trim();
      if (signature) {
        signatures.push(signature);
      }
    }

    for (const child of node.children) {
      visit(child);
    }
  }

  visit(tree.rootNode);
  return signatures.join('\n');
}

/**
 * Types mode: Extract only type definitions
 */
function transformTypes(tree, source) {
  const types = [];

  function visit(node) {
    const kind = node.type;

    if (isTypeNode(kind)) {
      const text = source.substring(node.startIndex, node.endIndex).trim();
      if (text) {
        types.push(text);
      }
    }

    for (const child of node.children) {
      visit(child);
    }
  }

  visit(tree.rootNode);
  return types.join('\n\n');
}

/**
 * Check if node is a function/method
 */
function isFunctionNode(kind) {
  return [
    'function_declaration',
    'function_definition',
    'function_item',
    'method_declaration',
    'method_definition',
    'arrow_function',
    'function_expression',
  ].includes(kind);
}

/**
 * Check if node is a type definition
 */
function isTypeNode(kind) {
  return [
    'interface_declaration',
    'type_alias_declaration',
    'type_alias_statement',
    'enum_declaration',
    'struct_item',
    'trait_item',
    'type_item',
    'class_declaration',
  ].includes(kind);
}

/**
 * Find body node for a function/method
 */
function findBodyNode(node) {
  for (const child of node.children) {
    if (['statement_block', 'block', 'compound_statement', 'body'].includes(child.type)) {
      return child;
    }
  }
  return null;
}

/**
 * Apply replacements to source (non-overlapping, sorted)
 */
function applyReplacements(source, replacements) {
  // Sort by start position
  replacements.sort((a, b) => a.start - b.start);

  let result = '';
  let lastPos = 0;

  for (const { start, end, replacement } of replacements) {
    // Skip overlapping replacements
    if (start < lastPos) {
      continue;
    }

    result += source.substring(lastPos, start);
    result += replacement;
    lastPos = end;
  }

  result += source.substring(lastPos);
  return result;
}
