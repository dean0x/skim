#!/usr/bin/env node
/**
 * Postinstall script for rskim npm package.
 *
 * This script runs after `npm install` to ensure the binary is available.
 * It downloads the platform-specific binary as a fallback if the optional
 * dependency failed to install (common with npm's optional deps).
 *
 * IMPORTANT: This script only uses Node.js built-in modules (no dependencies).
 */
const { execSync, spawn } = require('child_process');
const fs = require('fs');
const path = require('path');
const https = require('https');
const os = require('os');

/**
 * Detect if running on musl-based Linux
 *
 * ARCHITECTURE NOTE: This function is intentionally duplicated in bin/skim.
 * Postinstall scripts run before node_modules is fully available, so this script
 * must be self-contained with no external dependencies. Do not extract to a shared
 * module - it would break postinstall execution.
 */
function isMusl() {
  if (process.platform !== 'linux') {
    return false;
  }

  try {
    const osRelease = fs.readFileSync('/etc/os-release', 'utf8');
    if (osRelease.includes('Alpine')) {
      return true;
    }
  } catch {
    // Continue checking
  }

  try {
    const lddOutput = execSync('ldd --version 2>&1 || true', { encoding: 'utf8' });
    if (lddOutput.includes('musl')) {
      return true;
    }
  } catch {
    // Continue checking
  }

  try {
    const files = fs.readdirSync('/lib');
    if (files.some(f => f.startsWith('ld-musl'))) {
      return true;
    }
  } catch {
    // Not musl
  }

  return false;
}

/**
 * Get platform-specific package name
 */
function getPlatformPackage() {
  const platform = process.platform;
  const arch = process.arch;
  const musl = isMusl();

  const packages = {
    'darwin-arm64': '@rskim/cli-darwin-arm64',
    'darwin-x64': '@rskim/cli-darwin-x64',
    'linux-arm64': musl ? '@rskim/cli-linux-arm64-musl' : '@rskim/cli-linux-arm64',
    'linux-x64': musl ? '@rskim/cli-linux-x64-musl' : '@rskim/cli-linux-x64',
    'win32-x64': '@rskim/cli-win32-x64',
  };

  return packages[`${platform}-${arch}`];
}

/**
 * Check if platform package is already installed
 */
function isPlatformPackageInstalled(packageName) {
  const binaryName = process.platform === 'win32' ? 'skim.exe' : 'skim';
  const searchPaths = [
    path.join(__dirname, '..', 'node_modules', packageName, binaryName),
    path.join(__dirname, '..', '..', packageName, binaryName),
    path.join(__dirname, '..', '..', '.pnpm', 'node_modules', packageName, binaryName),
  ];

  return searchPaths.some(p => fs.existsSync(p));
}

/**
 * Check if fallback binary already exists
 */
function isFallbackInstalled() {
  const binaryName = process.platform === 'win32' ? 'skim.exe' : 'skim';
  const fallbackPath = path.join(__dirname, '..', 'bin', binaryName);
  return fs.existsSync(fallbackPath);
}

/**
 * Trusted domains for redirect validation.
 * Only follow redirects to these domains to prevent redirect attacks.
 */
const TRUSTED_DOMAINS = [
  'registry.npmjs.org',
  'registry.yarnpkg.com',
  'registry.npmmirror.com',
];

/**
 * Validate that a URL is within trusted domains
 */
function isUrlTrusted(urlString) {
  try {
    const url = new URL(urlString);
    return TRUSTED_DOMAINS.some(domain => url.hostname === domain || url.hostname.endsWith('.' + domain));
  } catch {
    return false;
  }
}

/**
 * Download file with redirect handling
 */
function downloadToFile(url, destPath, maxRedirects = 5) {
  return new Promise((resolve, reject) => {
    if (maxRedirects <= 0) {
      reject(new Error('Too many redirects'));
      return;
    }

    const file = fs.createWriteStream(destPath);

    https.get(url, (response) => {
      if (response.statusCode >= 300 && response.statusCode < 400 && response.headers.location) {
        file.close();
        fs.unlinkSync(destPath);

        // Validate redirect URL stays within trusted domains
        const redirectUrl = response.headers.location;
        if (!isUrlTrusted(redirectUrl)) {
          reject(new Error(`Redirect to untrusted domain blocked: ${redirectUrl}`));
          return;
        }

        downloadToFile(redirectUrl, destPath, maxRedirects - 1)
          .then(resolve)
          .catch(reject);
        return;
      }

      if (response.statusCode !== 200) {
        file.close();
        fs.unlinkSync(destPath);
        reject(new Error(`HTTP ${response.statusCode}: ${url}`));
        return;
      }

      response.pipe(file);
      file.on('finish', () => {
        file.close();
        resolve();
      });
      file.on('error', (err) => {
        file.close();
        fs.unlinkSync(destPath);
        reject(err);
      });
    }).on('error', (err) => {
      file.close();
      try { fs.unlinkSync(destPath); } catch {}
      reject(err);
    });
  });
}

/**
 * Get the npm registry tarball URL for a package
 * Uses the abbreviated metadata endpoint (/{package}/{version}) for efficiency.
 */
async function getPackageTarballUrl(packageName, version) {
  return new Promise((resolve, reject) => {
    // Use abbreviated metadata endpoint - returns only the specific version data
    const url = `https://registry.npmjs.org/${encodeURIComponent(packageName)}/${encodeURIComponent(version)}`;

    https.get(url, (response) => {
      if (response.statusCode !== 200) {
        reject(new Error(`Failed to fetch package info: HTTP ${response.statusCode}`));
        return;
      }

      let data = '';
      response.on('data', chunk => data += chunk);
      response.on('end', () => {
        try {
          const versionData = JSON.parse(data);
          if (!versionData.dist || !versionData.dist.tarball) {
            reject(new Error(`Tarball URL not found for ${packageName}@${version}`));
            return;
          }
          resolve(versionData.dist.tarball);
        } catch (e) {
          reject(new Error(`Failed to parse package info: ${e.message}`));
        }
      });
    }).on('error', reject);
  });
}

/**
 * Extract binary from npm tarball using system tar command
 */
async function extractBinaryFromTarball(tarballUrl, destPath) {
  const binaryName = process.platform === 'win32' ? 'skim.exe' : 'skim';

  // Create a temporary directory for extraction
  const tempDir = fs.mkdtempSync(path.join(os.tmpdir(), 'rskim-'));
  const tarballPath = path.join(tempDir, 'package.tgz');

  try {
    // Download the tarball
    await downloadToFile(tarballUrl, tarballPath);

    // Extract using system tar command
    // The tarball contains: package/<binaryName>
    await new Promise((resolve, reject) => {
      const tarArgs = ['-xzf', tarballPath, '-C', tempDir];
      const tar = spawn('tar', tarArgs, { stdio: 'inherit' });
      tar.on('close', (code) => {
        if (code === 0) {
          resolve();
        } else {
          reject(new Error(`tar exited with code ${code}`));
        }
      });
      tar.on('error', reject);
    });

    // Find the extracted binary (it will be in package/<binaryName>)
    const extractedBinary = path.join(tempDir, 'package', binaryName);
    if (!fs.existsSync(extractedBinary)) {
      throw new Error(`Binary not found in tarball: ${binaryName}`);
    }

    // Ensure destination directory exists
    const destDir = path.dirname(destPath);
    if (!fs.existsSync(destDir)) {
      fs.mkdirSync(destDir, { recursive: true });
    }

    // Copy to destination
    fs.copyFileSync(extractedBinary, destPath);

    // Make executable (Unix only)
    if (process.platform !== 'win32') {
      fs.chmodSync(destPath, 0o755);
    }
  } finally {
    // Cleanup temp directory
    try {
      fs.rmSync(tempDir, { recursive: true, force: true });
    } catch {
      // Ignore cleanup errors
    }
  }
}

/**
 * Main postinstall logic
 */
async function main() {
  const packageName = getPlatformPackage();

  if (!packageName) {
    // Unsupported platform, wrapper will show error at runtime
    return;
  }

  // Check if platform package is already installed
  if (isPlatformPackageInstalled(packageName)) {
    // Already installed via optional dependency
    return;
  }

  // Check if fallback binary already exists
  if (isFallbackInstalled()) {
    return;
  }

  // Read our version from package.json
  const pkgJson = JSON.parse(fs.readFileSync(path.join(__dirname, '..', 'package.json'), 'utf8'));
  const version = pkgJson.version;

  console.log(`rskim: Downloading ${packageName}@${version}...`);

  try {
    const tarballUrl = await getPackageTarballUrl(packageName, version);
    const binaryName = process.platform === 'win32' ? 'skim.exe' : 'skim';
    const destPath = path.join(__dirname, '..', 'bin', binaryName);

    await extractBinaryFromTarball(tarballUrl, destPath);
    console.log('rskim: Binary downloaded successfully');
  } catch (error) {
    // Non-fatal - wrapper will show helpful error at runtime
    console.warn(`rskim: Failed to download binary: ${error.message}`);
    console.warn('rskim: You may need to install via: cargo install rskim');
  }
}

// Run main and handle errors gracefully
main().catch(error => {
  // Non-fatal - wrapper will handle missing binary
  console.warn(`rskim postinstall: ${error.message}`);
});
