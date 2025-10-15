// Example: Using Skim WASM in Node.js
//
// Usage:
//   npm install (first time only)
//   node examples/node-example.js

import { init, transform, Language, Mode } from '../src/wrapper.js';

const typeScriptCode = `
function fibonacci(n: number): number {
  if (n <= 1) {
    return n;
  }
  return fibonacci(n - 1) + fibonacci(n - 2);
}

class MathUtils {
  static factorial(n: number): number {
    if (n <= 1) return 1;
    return n * MathUtils.factorial(n - 1);
  }

  static isPrime(n: number): boolean {
    if (n <= 1) return false;
    for (let i = 2; i * i <= n; i++) {
      if (n % i === 0) return false;
    }
    return true;
  }
}
`;

async function main() {
  console.log('🔍 Skim WASM - Node.js Example\n');

  // Initialize
  console.log('⏳ Initializing...');
  await init();
  console.log('✅ Initialized\n');

  console.log('Original Code:');
  console.log('─'.repeat(50));
  console.log(typeScriptCode);
  console.log('─'.repeat(50));
  console.log(`Size: ${typeScriptCode.length} bytes\n`);

  // Transform with different modes
  const modes = [
    { mode: Mode.Structure, name: 'Structure' },
    { mode: Mode.Signatures, name: 'Signatures' },
    { mode: Mode.Types, name: 'Types' },
  ];

  for (const { mode, name } of modes) {
    try {
      const result = await transform(typeScriptCode, Language.TypeScript, mode);

      console.log(`\n${name} Mode (${result.reductionPercentage.toFixed(1)}% reduction):`);
      console.log('─'.repeat(50));
      console.log(result.content);
      console.log('─'.repeat(50));
      console.log(`Size: ${result.transformedSize} bytes (was ${result.originalSize} bytes)`);
    } catch (error) {
      console.error(`❌ ${name} mode failed:`, error.message);
    }
  }

  console.log('\n✅ Transformation complete');
}

main().catch(console.error);
