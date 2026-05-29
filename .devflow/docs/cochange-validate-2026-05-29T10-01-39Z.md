# Co-change Validation Report

## Summary

- **Best threshold (macro F1):** 0.10 — F1 0.2819, P 0.2145, R 0.4111
- **Repos evaluated:** 6/7 passed quality gates
- **Run timestamp:** 2026-05-29T10:01:39Z

## Threshold Sweep (Aggregate)

| Threshold | Macro P | Macro R | Macro F1 | Micro P | Micro R | Micro F1 | Commits | Queries |
|-----------|---------|---------|----------|---------|---------|----------|---------|--------|
| 0.01 | 0.1093 | 0.7944 | 0.1922 | 0.1732 | 0.6259 | 0.2714 | 1587 | 16417 |
| 0.05 | 0.1581 | 0.5618 | 0.2467 | 0.2409 | 0.4279 | 0.3083 | 1587 | 16417 |
| 0.10 | 0.2145 | 0.4111 | 0.2819 | 0.2886 | 0.3030 | 0.2956 | 1587 | 16417 |
| 0.20 | 0.2477 | 0.2885 | 0.2665 | 0.3894 | 0.2257 | 0.2858 | 1587 | 16417 |
| 0.30 | 0.2006 | 0.1944 | 0.1974 | 0.3853 | 0.1516 | 0.2176 | 1587 | 16417 |
| 0.50 | 0.1093 | 0.0936 | 0.1008 | 0.3776 | 0.1032 | 0.1621 | 1587 | 16417 |

## Per-Repo Results

### ripgrep

- Commits: 1711 train / 428 test (168 multi-file, 260 single-file)
- Matrix: 342 files, 5130 pairs
- Commits skipped (too large): 3
- Unmapped files in test: 163

| Threshold | Macro P | Macro R | Macro F1 | Micro P | Micro R | Micro F1 |
|-----------|---------|---------|----------|---------|---------|----------|
| 0.01 | 0.0557 | 0.6109 | 0.1020 | 0.0874 | 0.5691 | 0.1516 |
| 0.05 | 0.0792 | 0.4138 | 0.1329 | 0.1137 | 0.4676 | 0.1830 |
| 0.10 | 0.1468 | 0.3608 | 0.2086 | 0.1277 | 0.4016 | 0.1938 |
| 0.20 | 0.0993 | 0.2333 | 0.1393 | 0.1547 | 0.2461 | 0.1900 |
| 0.30 | 0.0790 | 0.0933 | 0.0855 | 0.1696 | 0.1170 | 0.1385 |
| 0.50 | 0.0228 | 0.0166 | 0.0192 | 0.1396 | 0.0298 | 0.0492 |

### fd

- Commits: 1096 train / 275 test (65 multi-file, 210 single-file)
- Matrix: 84 files, 650 pairs
- Unmapped files in test: 24

| Threshold | Macro P | Macro R | Macro F1 | Micro P | Micro R | Micro F1 |
|-----------|---------|---------|----------|---------|---------|----------|
| 0.01 | 0.0891 | 0.8295 | 0.1610 | 0.1318 | 0.7990 | 0.2263 |
| 0.05 | 0.1385 | 0.4210 | 0.2084 | 0.1941 | 0.4271 | 0.2669 |
| 0.10 | 0.1098 | 0.1081 | 0.1090 | 0.2046 | 0.1332 | 0.1613 |
| 0.20 | 0.0643 | 0.0488 | 0.0555 | 0.3121 | 0.0553 | 0.0939 |
| 0.30 | 0.0000 | 0.0000 | 0.0000 | 0.0000 | 0.0000 | 0.0000 |
| 0.50 | 0.0000 | 0.0000 | 0.0000 | 0.0000 | 0.0000 | 0.0000 |

### flask

**Error:** clone failed: git clone failed for https://github.com/pallets/flask

### pydantic

- Commits: 3180 train / 795 test (474 multi-file, 321 single-file)
- Matrix: 1509 files, 26570 pairs
- Commits skipped (too large): 18
- Unmapped files in test: 1347

| Threshold | Macro P | Macro R | Macro F1 | Micro P | Micro R | Micro F1 |
|-----------|---------|---------|----------|---------|---------|----------|
| 0.01 | 0.0568 | 0.7671 | 0.1058 | 0.0886 | 0.2603 | 0.1322 |
| 0.05 | 0.1059 | 0.4571 | 0.1720 | 0.1776 | 0.0918 | 0.1210 |
| 0.10 | 0.1800 | 0.2781 | 0.2186 | 0.2881 | 0.0435 | 0.0756 |
| 0.20 | 0.2335 | 0.1929 | 0.2113 | 0.4370 | 0.0278 | 0.0523 |
| 0.30 | 0.1118 | 0.1011 | 0.1062 | 0.5181 | 0.0215 | 0.0413 |
| 0.50 | 0.0138 | 0.0138 | 0.0138 | 0.6498 | 0.0179 | 0.0348 |

### gin

- Commits: 1025 train / 257 test (107 multi-file, 150 single-file)
- Matrix: 202 files, 3055 pairs
- Commits skipped (too large): 4
- Unmapped files in test: 36

| Threshold | Macro P | Macro R | Macro F1 | Micro P | Micro R | Micro F1 |
|-----------|---------|---------|----------|---------|---------|----------|
| 0.01 | 0.0484 | 0.8471 | 0.0916 | 0.0689 | 0.6799 | 0.1251 |
| 0.05 | 0.0962 | 0.6370 | 0.1672 | 0.1137 | 0.3745 | 0.1745 |
| 0.10 | 0.2403 | 0.4980 | 0.3242 | 0.1584 | 0.1614 | 0.1599 |
| 0.20 | 0.3168 | 0.2572 | 0.2839 | 0.2729 | 0.0590 | 0.0971 |
| 0.30 | 0.2347 | 0.1902 | 0.2101 | 0.3776 | 0.0341 | 0.0626 |
| 0.50 | 0.0058 | 0.0012 | 0.0020 | 0.1905 | 0.0037 | 0.0072 |

### fiber

- Commits: 1923 train / 481 test (241 multi-file, 240 single-file)
- Matrix: 854 files, 18939 pairs
- Commits skipped (too large): 23
- Unmapped files in test: 191

| Threshold | Macro P | Macro R | Macro F1 | Micro P | Micro R | Micro F1 |
|-----------|---------|---------|----------|---------|---------|----------|
| 0.01 | 0.0835 | 0.7592 | 0.1504 | 0.1024 | 0.4691 | 0.1681 |
| 0.05 | 0.1046 | 0.5586 | 0.1762 | 0.1372 | 0.2379 | 0.1740 |
| 0.10 | 0.1588 | 0.4527 | 0.2352 | 0.1952 | 0.1219 | 0.1501 |
| 0.20 | 0.2784 | 0.3120 | 0.2943 | 0.2753 | 0.0509 | 0.0859 |
| 0.30 | 0.2638 | 0.2007 | 0.2280 | 0.3294 | 0.0264 | 0.0488 |
| 0.50 | 0.0841 | 0.0399 | 0.0541 | 0.3519 | 0.0076 | 0.0148 |

### nest

- Commits: 6344 train / 1586 test (693 multi-file, 893 single-file)
- Matrix: 2431 files, 46472 pairs
- Commits skipped (too large): 62
- Unmapped files in test: 218

| Threshold | Macro P | Macro R | Macro F1 | Micro P | Micro R | Micro F1 |
|-----------|---------|---------|----------|---------|---------|----------|
| 0.01 | 0.3222 | 0.9524 | 0.4815 | 0.5603 | 0.9780 | 0.7124 |
| 0.05 | 0.4239 | 0.8831 | 0.5729 | 0.7090 | 0.9686 | 0.8187 |
| 0.10 | 0.4510 | 0.7689 | 0.5686 | 0.7573 | 0.9565 | 0.8453 |
| 0.20 | 0.4940 | 0.6864 | 0.5745 | 0.8845 | 0.9149 | 0.8995 |
| 0.30 | 0.5143 | 0.5810 | 0.5456 | 0.9168 | 0.7107 | 0.8007 |
| 0.50 | 0.5292 | 0.4902 | 0.5089 | 0.9337 | 0.5602 | 0.7003 |

## Methodology

- **Train/test split:** 80/20 (chronological, oldest commits train, newest test)
- **Quality gates:** ≥50 multi-file commits, ≥6 months history span
- **Precision:** |predicted ∩ actual| / |predicted| per query
- **Recall:** |predicted ∩ actual| / |actual| per query (unmapped files excluded)
- **Macro average:** per-commit, then averaged across commits
- **Micro average:** accumulated over all individual queries
- **Deny-list patterns applied:**
  - `*.generated.go`
  - `*.min.css`
  - `*.min.js`
  - `*.pb.go`
  - `.git/`
  - `.tox/`
  - `Cargo.lock`
  - `Gemfile.lock`
  - `Pipfile.lock`
  - `__pycache__/`
  - `build/`
  - `composer.lock`
  - `dist/`
  - `flake.lock`
  - `go.sum`
  - `node_modules/`
  - `package-lock.json`
  - `pnpm-lock.yaml`
  - `poetry.lock`
  - `target/`
  - `vendor/`
  - `yarn.lock`

## Reproducibility Manifest

- **Corpus config:** `/Users/dean/Sandbox/skim-search/crates/rskim-research/cochange-corpus.toml`

| Repo | HEAD SHA | Train Cutoff | Train Commits | Test Commits |
|------|----------|--------------|---------------|--------------|
| ripgrep | `4857d6fa` | 1662758622 | 1711 | 428 |
| fd | `42b2ab8a` | 1689660961 | 1096 | 275 |
| pydantic | `a20c0ee2` | 1727897538 | 3180 | 795 |
| gin | `5f4f9643` | 1710222563 | 1025 | 257 |
| fiber | `f0752ced` | 1756994924 | 1923 | 481 |
| nest | `8859e895` | 1737705305 | 6344 | 1586 |

