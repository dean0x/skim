/**
 * @skim/wasm - WASM-powered code transformation
 *
 * Main entry point for the package.
 * Re-exports everything from wrapper.js for cleaner imports.
 */

export { init, loadGrammar, transform, Language, Mode } from './wrapper.js';
