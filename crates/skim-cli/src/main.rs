//! Skim CLI - Command-line interface for skim-core
//!
//! ARCHITECTURE: Thin I/O layer over skim-core library.
//! This binary handles:
//! - File I/O (reading from disk/stdin)
//! - CLI argument parsing (clap)
//! - Output formatting (stdout/stderr)
//! - Process exit codes

use clap::Parser;
use std::fs;
use std::io::{self, BufWriter, Read, Write};
use std::path::PathBuf;

use skim_core::{transform, transform_auto, Language, Mode};

/// Skim - Smart code reader for AI agents
///
/// Transform source code by stripping implementation details while
/// preserving structure, signatures, and types.
#[derive(Parser, Debug)]
#[command(name = "skim")]
#[command(author, version, about, long_about = None)]
#[command(after_help = "EXAMPLES:\n  \
    skim file.ts                       Read TypeScript with structure mode\n  \
    skim file.py --mode signatures     Extract Python signatures\n  \
    skim file.rs | bat -l rust         Skim Rust and highlight\n  \
    cat code.ts | skim - --lang=ts     Read from stdin with explicit language\n  \
    skim - -l python < script.py       Short form language flag\n\n\
For more info: https://github.com/youruser/skim")]
struct Args {
    /// File to read (use '-' for stdin)
    #[arg(value_name = "FILE")]
    file: PathBuf,

    /// Transformation mode
    #[arg(short, long, value_enum, default_value = "structure")]
    #[arg(help = "Transformation mode: structure, signatures, types, or full")]
    mode: ModeArg,

    /// Explicit language (required when reading from stdin)
    #[arg(short, long, value_enum)]
    #[arg(help = "Programming language: typescript, python, rust, go, java")]
    language: Option<LanguageArg>,

    /// Force parsing even if language unsupported
    #[arg(long)]
    force: bool,
}

/// Mode argument (clap value_enum wrapper)
#[derive(Debug, Clone, Copy, clap::ValueEnum)]
enum ModeArg {
    Structure,
    Signatures,
    Types,
    Full,
}

impl From<ModeArg> for Mode {
    fn from(arg: ModeArg) -> Self {
        match arg {
            ModeArg::Structure => Mode::Structure,
            ModeArg::Signatures => Mode::Signatures,
            ModeArg::Types => Mode::Types,
            ModeArg::Full => Mode::Full,
        }
    }
}

/// Language argument (clap value_enum wrapper)
#[derive(Debug, Clone, Copy, clap::ValueEnum)]
enum LanguageArg {
    #[value(alias = "ts")]
    TypeScript,
    #[value(alias = "js")]
    JavaScript,
    #[value(alias = "py")]
    Python,
    #[value(alias = "rs")]
    Rust,
    Go,
    Java,
}

impl From<LanguageArg> for Language {
    fn from(arg: LanguageArg) -> Self {
        match arg {
            LanguageArg::TypeScript => Language::TypeScript,
            LanguageArg::JavaScript => Language::JavaScript,
            LanguageArg::Python => Language::Python,
            LanguageArg::Rust => Language::Rust,
            LanguageArg::Go => Language::Go,
            LanguageArg::Java => Language::Java,
        }
    }
}

fn main() -> anyhow::Result<()> {
    let args = Args::parse();

    // Read source (from file or stdin)
    let is_stdin = args.file.to_str() == Some("-");
    let source = if is_stdin {
        let mut buffer = String::new();
        io::stdin().read_to_string(&mut buffer)?;
        buffer
    } else {
        fs::read_to_string(&args.file)?
    };

    // Transform using core library
    let mode = Mode::from(args.mode);

    let result = match args.language {
        // Explicit language provided (required for stdin)
        Some(lang_arg) => {
            let language = Language::from(lang_arg);
            transform(&source, language, mode)?
        }
        // Auto-detect from file path
        None => {
            if is_stdin {
                anyhow::bail!(
                    "Language detection failed: reading from stdin requires --language flag\n\
                     Example: cat file.ts | skim - --language=typescript"
                );
            }
            transform_auto(&source, &args.file, mode)?
        }
    };

    // Write to stdout (buffered)
    let stdout = io::stdout();
    let mut writer = BufWriter::new(stdout.lock());
    write!(writer, "{}", result)?;
    writer.flush()?;

    Ok(())
}

#[cfg(test)]
mod tests {
    // CLI tests with assert_cmd (Week 4)
}
