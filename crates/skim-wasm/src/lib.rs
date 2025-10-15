//! WASM bindings for Skim
//!
//! This crate provides WebAssembly bindings for the Skim code transformation library,
//! enabling JavaScript/TypeScript applications to transform source code in browsers
//! and Node.js environments.
//!
//! # Usage
//!
//! ```javascript
//! import { Skim, Language, Mode } from '@skim/wasm';
//!
//! // Initialize WASM module
//! await Skim.init();
//!
//! // Transform code
//! const result = await Skim.transform(sourceCode, {
//!   language: Language.TypeScript,
//!   mode: Mode.Structure
//! });
//!
//! console.log(result.content);
//! console.log(`Reduction: ${result.reductionPercentage}%`);
//! ```

use wasm_bindgen::prelude::*;

// When the `wee_alloc` feature is enabled, use `wee_alloc` as the global allocator
#[cfg(feature = "wee_alloc")]
#[global_allocator]
static ALLOC: wee_alloc::WeeAlloc = wee_alloc::WeeAlloc::INIT;

/// Initialize the WASM module
///
/// This should be called once when your application starts.
/// It sets up panic hooks for better error messages in the browser console.
#[wasm_bindgen(start)]
pub fn init() {
    // Set panic hook for better error messages in browser console
    #[cfg(feature = "console_error_panic_hook")]
    console_error_panic_hook::set_once();
}

/// Programming language
#[wasm_bindgen]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Language {
    TypeScript,
    JavaScript,
    Python,
    Rust,
    Go,
    Java,
}

impl From<Language> for skim_core::Language {
    fn from(lang: Language) -> Self {
        match lang {
            Language::TypeScript => skim_core::Language::TypeScript,
            Language::JavaScript => skim_core::Language::JavaScript,
            Language::Python => skim_core::Language::Python,
            Language::Rust => skim_core::Language::Rust,
            Language::Go => skim_core::Language::Go,
            Language::Java => skim_core::Language::Java,
        }
    }
}

/// Transformation mode
#[wasm_bindgen]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Remove function bodies, keep structure (70-80% reduction)
    Structure,
    /// Extract only function/method signatures (85-92% reduction)
    Signatures,
    /// Extract only type definitions (90-95% reduction)
    Types,
    /// No transformation, return original (0% reduction)
    Full,
}

impl From<Mode> for skim_core::Mode {
    fn from(mode: Mode) -> Self {
        match mode {
            Mode::Structure => skim_core::Mode::Structure,
            Mode::Signatures => skim_core::Mode::Signatures,
            Mode::Types => skim_core::Mode::Types,
            Mode::Full => skim_core::Mode::Full,
        }
    }
}

/// Transformation result
#[wasm_bindgen]
pub struct TransformResult {
    content: String,
    original_size: usize,
    transformed_size: usize,
}

#[wasm_bindgen]
impl TransformResult {
    /// Get transformed content
    #[wasm_bindgen(getter)]
    pub fn content(&self) -> String {
        self.content.clone()
    }

    /// Get original size in bytes
    #[wasm_bindgen(getter)]
    pub fn original_size(&self) -> usize {
        self.original_size
    }

    /// Get transformed size in bytes
    #[wasm_bindgen(getter)]
    pub fn transformed_size(&self) -> usize {
        self.transformed_size
    }

    /// Get reduction percentage
    #[wasm_bindgen(getter)]
    pub fn reduction_percentage(&self) -> f64 {
        if self.original_size == 0 {
            return 0.0;
        }
        let reduction = self.original_size.saturating_sub(self.transformed_size) as f64;
        (reduction / self.original_size as f64) * 100.0
    }
}

/// Transform source code
///
/// # Arguments
///
/// * `source` - Source code to transform
/// * `language` - Programming language
/// * `mode` - Transformation mode
///
/// # Returns
///
/// `TransformResult` containing transformed content and statistics
///
/// # Errors
///
/// Returns error string if transformation fails
#[wasm_bindgen]
pub fn transform(source: &str, language: Language, mode: Mode) -> Result<TransformResult, String> {
    let core_language: skim_core::Language = language.into();
    let core_mode: skim_core::Mode = mode.into();

    // Transform using core library
    let transformed = skim_core::transform(source, core_language, core_mode)
        .map_err(|e| format!("Transformation failed: {}", e))?;

    Ok(TransformResult {
        content: transformed.clone(),
        original_size: source.len(),
        transformed_size: transformed.len(),
    })
}

/// Log a message to the browser console (for debugging)
#[wasm_bindgen]
pub fn log(message: &str) {
    web_sys::console::log_1(&message.into());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_language_conversion() {
        let lang = Language::TypeScript;
        let core_lang: skim_core::Language = lang.into();
        assert_eq!(core_lang, skim_core::Language::TypeScript);
    }

    #[test]
    fn test_mode_conversion() {
        let mode = Mode::Structure;
        let core_mode: skim_core::Mode = mode.into();
        assert_eq!(core_mode, skim_core::Mode::Structure);
    }
}
