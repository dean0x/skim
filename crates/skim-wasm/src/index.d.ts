/**
 * TypeScript type definitions for @skim/wasm
 */

/**
 * Programming language
 */
export enum Language {
  TypeScript = 'typescript',
  JavaScript = 'javascript',
  Python = 'python',
  Rust = 'rust',
  Go = 'go',
  Java = 'java',
}

/**
 * Transformation mode
 */
export enum Mode {
  /** Remove function bodies, keep structure (70-80% reduction) */
  Structure = 'structure',
  /** Extract only function/method signatures (85-92% reduction) */
  Signatures = 'signatures',
  /** Extract only type definitions (90-95% reduction) */
  Types = 'types',
  /** No transformation, return original (0% reduction) */
  Full = 'full',
}

/**
 * Transformation result
 */
export interface TransformResult {
  /** Transformed code content */
  content: string;
  /** Original size in bytes */
  originalSize: number;
  /** Transformed size in bytes */
  transformedSize: number;
  /** Reduction percentage */
  reductionPercentage: number;
}

/**
 * Initialization options
 */
export interface InitOptions {
  /** Custom path to tree-sitter.wasm runtime */
  treeSitterWasmPath?: string;
}

/**
 * Initialize the WASM module
 *
 * Must be called once before using transform() or loadGrammar().
 *
 * @param options - Initialization options
 * @example
 * ```typescript
 * import { init } from '@skim/wasm';
 * await init();
 * ```
 */
export function init(options?: InitOptions): Promise<void>;

/**
 * Load a grammar for a specific language
 *
 * Grammars are lazy-loaded on first use, but you can preload them
 * with this function to avoid latency on first transform.
 *
 * @param language - Language to load
 * @param grammarPath - Optional custom path to grammar WASM file
 * @example
 * ```typescript
 * import { loadGrammar, Language } from '@skim/wasm';
 * await loadGrammar(Language.TypeScript);
 * ```
 */
export function loadGrammar(
  language: Language | string,
  grammarPath?: string
): Promise<unknown>;

/**
 * Transform source code
 *
 * @param source - Source code to transform
 * @param language - Programming language
 * @param mode - Transformation mode
 * @returns Promise with transformation result
 * @throws Error if transformation fails
 * @example
 * ```typescript
 * import { transform, Language, Mode } from '@skim/wasm';
 *
 * const result = await transform(
 *   'function add(a: number, b: number): number { return a + b; }',
 *   Language.TypeScript,
 *   Mode.Structure
 * );
 *
 * console.log(result.content);
 * // Output: function add(a: number, b: number): number { /* ... */ }
 *
 * console.log(`Reduction: ${result.reductionPercentage.toFixed(1)}%`);
 * // Output: Reduction: 75.0%
 * ```
 */
export function transform(
  source: string,
  language: Language | string,
  mode: Mode | string
): Promise<TransformResult>;
