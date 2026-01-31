#!/usr/bin/env node
/**
 * Generate npm packages for release.
 *
 * This script:
 * 1. Updates all package.json versions
 * 2. Copies binaries from artifacts to platform packages
 * 3. Sets executable permissions
 *
 * Usage: node scripts/generate-npm-packages.mjs <version> <artifacts-dir>
 */
import fs from 'fs';
import path from 'path';
import { fileURLToPath } from 'url';

const __filename = fileURLToPath(import.meta.url);
const __dirname = path.dirname(__filename);
const ROOT = path.join(__dirname, '..');

/**
 * Platform package configuration
 */
const PLATFORM_PACKAGES = {
  'cli-darwin-arm64': {
    artifact: 'skim-aarch64-apple-darwin',
    binary: 'skim',
  },
  'cli-darwin-x64': {
    artifact: 'skim-x86_64-apple-darwin',
    binary: 'skim',
  },
  'cli-linux-arm64': {
    artifact: 'skim-aarch64-unknown-linux-gnu',
    binary: 'skim',
  },
  'cli-linux-x64': {
    artifact: 'skim-x86_64-unknown-linux-gnu',
    binary: 'skim',
  },
  'cli-linux-arm64-musl': {
    artifact: 'skim-aarch64-unknown-linux-musl',
    binary: 'skim',
  },
  'cli-linux-x64-musl': {
    artifact: 'skim-x86_64-unknown-linux-musl',
    binary: 'skim',
  },
  'cli-win32-x64': {
    artifact: 'skim-x86_64-pc-windows-msvc',
    binary: 'skim.exe',
  },
};

/**
 * Validate semantic version format
 */
function isValidVersion(version) {
  return /^\d+\.\d+\.\d+(-[a-zA-Z0-9.-]+)?$/.test(version);
}

/**
 * Update version in package.json
 */
function updatePackageVersion(packagePath, version) {
  const pkgPath = path.join(packagePath, 'package.json');
  const pkg = JSON.parse(fs.readFileSync(pkgPath, 'utf8'));

  pkg.version = version;

  // Update optional dependencies versions in main package
  if (pkg.optionalDependencies) {
    for (const dep of Object.keys(pkg.optionalDependencies)) {
      pkg.optionalDependencies[dep] = version;
    }
  }

  fs.writeFileSync(pkgPath, JSON.stringify(pkg, null, 2) + '\n');
  console.log(`Updated ${pkgPath} to version ${version}`);
}

/**
 * Copy binary from artifacts to package
 */
function copyBinary(artifactsDir, packageDir, config) {
  const srcPath = path.join(artifactsDir, config.artifact, config.binary);
  const destPath = path.join(packageDir, config.binary);

  if (!fs.existsSync(srcPath)) {
    throw new Error(`Binary not found: ${srcPath}`);
  }

  fs.copyFileSync(srcPath, destPath);

  // Make executable on Unix
  if (config.binary !== 'skim.exe') {
    fs.chmodSync(destPath, 0o755);
  }

  console.log(`Copied ${srcPath} -> ${destPath}`);
}

/**
 * Main entry point
 */
function main() {
  const args = process.argv.slice(2);

  if (args.length !== 2) {
    console.error('Usage: node generate-npm-packages.mjs <version> <artifacts-dir>');
    console.error('');
    console.error('Example:');
    console.error('  node generate-npm-packages.mjs 1.0.0 ./artifacts');
    process.exit(1);
  }

  const [version, artifactsDir] = args;

  // Validate version
  if (!isValidVersion(version)) {
    console.error(`ERROR: Invalid version format: ${version}`);
    console.error('Version must be semantic: X.Y.Z or X.Y.Z-prerelease');
    process.exit(1);
  }

  // Validate artifacts directory
  if (!fs.existsSync(artifactsDir)) {
    console.error(`ERROR: Artifacts directory not found: ${artifactsDir}`);
    process.exit(1);
  }

  const npmDir = path.join(ROOT, 'npm');

  // Update main package version
  console.log('\n📦 Updating package versions...');
  updatePackageVersion(path.join(npmDir, 'rskim'), version);

  // Copy README to main package
  const readmeSrc = path.join(ROOT, 'crates', 'rskim', 'README.md');
  const readmeDest = path.join(npmDir, 'rskim', 'README.md');
  if (fs.existsSync(readmeSrc)) {
    fs.copyFileSync(readmeSrc, readmeDest);
    console.log(`Copied README.md to main package`);
  }

  // Process each platform package
  console.log('\n📦 Processing platform packages...');
  for (const [pkgName, config] of Object.entries(PLATFORM_PACKAGES)) {
    const packageDir = path.join(npmDir, pkgName);

    // Update version
    updatePackageVersion(packageDir, version);

    // Copy binary
    try {
      copyBinary(artifactsDir, packageDir, config);
    } catch (error) {
      console.error(`ERROR: Failed to process ${pkgName}: ${error.message}`);
      process.exit(1);
    }
  }

  console.log('\n✅ All npm packages prepared successfully');
  console.log(`   Version: ${version}`);
  console.log(`   Packages: ${Object.keys(PLATFORM_PACKAGES).length + 1} (main + platform)`);
}

main();
