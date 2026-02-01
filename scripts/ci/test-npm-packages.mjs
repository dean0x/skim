#!/usr/bin/env node
/**
 * Test suite for npm package structure
 *
 * This tests the npm package structure, wrapper script, and postinstall logic.
 * Run with: node scripts/ci/test-npm-packages.mjs
 */
import fs from 'fs';
import path from 'path';
import { fileURLToPath } from 'url';
import assert from 'assert';

const __filename = fileURLToPath(import.meta.url);
const __dirname = path.dirname(__filename);
const ROOT = path.join(__dirname, '..', '..');

// Colors for output
const GREEN = '\x1b[32m';
const RED = '\x1b[31m';
const RESET = '\x1b[0m';

let testsPassed = 0;
let testsFailed = 0;

function test(name, fn) {
  try {
    fn();
    testsPassed++;
    console.log(`${GREEN}✓${RESET} ${name}`);
  } catch (error) {
    testsFailed++;
    console.log(`${RED}✗${RESET} ${name}`);
    console.log(`  ${error.message}`);
  }
}

console.log('\n🧪 npm Package Structure Test Suite\n');

// Test 1: Main package.json exists and is valid
test('Main package.json exists and has correct structure', () => {
  const pkgPath = path.join(ROOT, 'npm', 'rskim', 'package.json');
  assert.ok(fs.existsSync(pkgPath), 'Main package.json should exist');

  const pkg = JSON.parse(fs.readFileSync(pkgPath, 'utf8'));
  assert.strictEqual(pkg.name, 'rskim', 'Package name should be rskim');
  assert.ok(pkg.bin.skim, 'Should have skim binary');
  assert.ok(pkg.bin.rskim, 'Should have rskim binary alias');
  assert.ok(pkg.optionalDependencies, 'Should have optionalDependencies');
  assert.ok(pkg.scripts.postinstall, 'Should have postinstall script');
});

// Test 2: Wrapper script exists
test('Wrapper script exists and is valid Node.js', () => {
  const wrapperPath = path.join(ROOT, 'npm', 'rskim', 'bin', 'skim');
  assert.ok(fs.existsSync(wrapperPath), 'Wrapper script should exist');

  const content = fs.readFileSync(wrapperPath, 'utf8');
  assert.ok(content.startsWith('#!/usr/bin/env node'), 'Should have node shebang');
  assert.ok(content.includes('isMusl'), 'Should have musl detection');
  assert.ok(content.includes('getPlatformPackage'), 'Should have platform package resolution');
  assert.ok(content.includes('spawnSync'), 'Should use spawnSync for execution');
});

// Test 3: Postinstall script exists
test('Postinstall script exists', () => {
  const postinstallPath = path.join(ROOT, 'npm', 'rskim', 'scripts', 'postinstall.js');
  assert.ok(fs.existsSync(postinstallPath), 'Postinstall script should exist');

  const content = fs.readFileSync(postinstallPath, 'utf8');
  assert.ok(content.includes('isMusl'), 'Should have musl detection');
  assert.ok(content.includes('downloadToFile'), 'Should have download fallback');
});

// Test 4: All platform packages exist
const PLATFORM_PACKAGES = [
  'cli-darwin-arm64',
  'cli-darwin-x64',
  'cli-linux-arm64',
  'cli-linux-x64',
  'cli-linux-arm64-musl',
  'cli-linux-x64-musl',
  'cli-win32-x64',
];

for (const pkg of PLATFORM_PACKAGES) {
  test(`Platform package ${pkg} exists with valid package.json`, () => {
    const pkgPath = path.join(ROOT, 'npm', pkg, 'package.json');
    assert.ok(fs.existsSync(pkgPath), `${pkg}/package.json should exist`);

    const pkgJson = JSON.parse(fs.readFileSync(pkgPath, 'utf8'));
    assert.strictEqual(pkgJson.name, `@rskim/${pkg}`, `Package name should be @rskim/${pkg}`);
    assert.ok(pkgJson.os, 'Should specify os');
    assert.ok(pkgJson.cpu, 'Should specify cpu');
    assert.ok(pkgJson.files, 'Should specify files');

    // musl packages should have libc field
    if (pkg.includes('musl')) {
      assert.ok(pkgJson.libc, 'musl packages should specify libc');
      assert.ok(pkgJson.libc.includes('musl'), 'musl packages should specify musl libc');
    }
  });
}

// Test 5: Optional dependencies in main package match platform packages
test('Main package optional dependencies match platform packages', () => {
  const mainPkg = JSON.parse(fs.readFileSync(path.join(ROOT, 'npm', 'rskim', 'package.json'), 'utf8'));
  const optDeps = Object.keys(mainPkg.optionalDependencies);

  for (const pkg of PLATFORM_PACKAGES) {
    const fullName = `@rskim/${pkg}`;
    assert.ok(optDeps.includes(fullName), `Should have ${fullName} as optional dependency`);
  }

  assert.strictEqual(optDeps.length, PLATFORM_PACKAGES.length, 'Should have exactly the right number of optional deps');
});

// Test 6: Generate script exists
test('Generate npm packages script exists', () => {
  const scriptPath = path.join(ROOT, 'scripts', 'generate-npm-packages.mjs');
  assert.ok(fs.existsSync(scriptPath), 'Generate script should exist');

  const content = fs.readFileSync(scriptPath, 'utf8');
  assert.ok(content.includes('PLATFORM_PACKAGES'), 'Should define PLATFORM_PACKAGES');
  assert.ok(content.includes('updatePackageVersion'), 'Should have version update function');
  assert.ok(content.includes('copyBinary'), 'Should have binary copy function');
});

// Test 7: Musl detection logic validation
test('Musl detection checks multiple sources', () => {
  const wrapperContent = fs.readFileSync(path.join(ROOT, 'npm', 'rskim', 'bin', 'skim'), 'utf8');

  // Check for all three detection methods
  assert.ok(wrapperContent.includes('/etc/os-release'), 'Should check /etc/os-release');
  assert.ok(wrapperContent.includes('ldd --version'), 'Should check ldd output');
  assert.ok(wrapperContent.includes('ld-musl'), 'Should check for musl linker');
});

// Test 8: Error messages are user-friendly
test('Wrapper has helpful error messages', () => {
  const wrapperContent = fs.readFileSync(path.join(ROOT, 'npm', 'rskim', 'bin', 'skim'), 'utf8');

  assert.ok(wrapperContent.includes('cargo install rskim'), 'Should suggest cargo install');
  assert.ok(wrapperContent.includes('https://github.com/dean0x/skim/issues'), 'Should link to issues');
  assert.ok(wrapperContent.includes('Supported platforms'), 'Should list supported platforms');
});

// Summary
console.log('\n' + '='.repeat(50));
console.log(`${GREEN}Passed: ${testsPassed}${RESET}`);
if (testsFailed > 0) {
  console.log(`${RED}Failed: ${testsFailed}${RESET}`);
  process.exit(1);
} else {
  console.log(`${GREEN}All tests passed!${RESET}`);
  process.exit(0);
}
