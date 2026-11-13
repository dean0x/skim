---
feature: symbol-read
name: Symbol read (skim FILE --symbol NAME) and the rskim-core declaration rule
description: "Use when changing skim FILE --symbol, the rskim-core declaration rule, parse_bounded, new_for_extension, the symbol cache key, or reusing declarations in search. Keywords: --symbol, Declaration, DeclScan."
category: domain-knowledge
directories: [crates/rskim-core/src/, crates/rskim/src/]
created: 2026-11-12
updated: 2026-11-12
---

# Symbol read and the declaration rule

## Rules

- **KB-INV-1** The cache key of a symbol read holds `symbol:<byte length>:<name>|` between the schema version and the path. A whole-file read keeps the exact hash input it had before, so no warm entry is orphaned.
- **KB-INV-2** A successful symbol read prints its stderr notice on every run, warm or cold, because the reader sees part of the file. The notice is stored in the cache entry, and the read records no analytics row.
- **KB-INV-3** A variable declared in a function body (TypeScript, JavaScript, Go) is a local and is not a declaration. A variable at the top level is a declaration.
- **KB-INV-4** The search engine reuses `Declaration.name_bytes`, `enclosing_declaration`, `DeclScan.truncated` and `Parser::parse_bounded`. Keep their signatures and meaning.
- **KB-AP-1** Never treat a name that is missing from `DeclScan.decls` as absent from the file when `DeclScan.truncated` is true. The walk may not have reached it.
- **KB-AP-2** Do not expect a declaration for a C or C++ function. Its name sits under the `declarator` field, the generic rule needs a `name` field, so the scan finds nothing.

## Overview

`skim FILE --symbol NAME` prints the raw source lines of one named declaration. The work has two halves.

- `rskim-core` owns the declaration rule (`decl.rs`): one walk over a tree-sitter tree gives a list of `Declaration` values. It reads no file and keeps no state. `Parser` in `types.rs` gives two helpers for it: `new_for_extension` and `parse_bounded`.
- `rskim` owns the reader. `symbol.rs` holds the pure parts (select, render, notice, errors). `process.rs` holds the pipeline (read, parse, scan, cache). `main.rs` holds the argument checks. `cache.rs` holds the cache identity.

No mode transform runs. The output is the lines `git show` would show for that range. The read uses `Mode::Full` only as a cache-key value.

## The declaration rule (`decl.rs`)

### API

- `declarations(&Tree, &str, Language) -> DeclScan`. It returns declarations in source order. An outer declaration comes before the declarations nested in it. A declaration whose name range lies outside `source` is dropped. A wrong source gives fewer results, not a wrong name.
- `Declaration` fields: `name`, `kind` (the tree-sitter node kind), `name_bytes` (range of the name), `first_row`, `last_row` (0-based; a node that ends at column 0 of the next row does not count that row), `container` (name of the holding impl type, trait, module, struct, enum, class or interface; `None` at the top level and in a function body), `byte_range` (range of the whole declaration).
- `DeclScan { decls, truncated }`. `truncated` is true when the depth cap or the node cap cut the walk. The walk uses `AstWalkConfig::default()` (`DEFAULT_MAX_DEPTH`, `DEFAULT_MAX_NODES`). It walks one node past the node cap, so a tree of exactly the cap size is complete. Tests `declarations_above_the_node_cap_are_truncated`, `declarations_below_the_depth_cap_are_not_truncated` and the matching pair pin the edges.
- `enclosing_declaration(&[Declaration], byte) -> Option<&Declaration>`. It returns the innermost function or container that holds the byte. `decls` must be in `declarations` order, because the function uses `partition_point` on `byte_range.start`. `byte_range` is half-open. The function skips `LEAF_KINDS` (constant, static, type alias, field, enum variant, const and var specs): the function or container around them holds the byte.

### Kinds by language

| Language | Declaring kinds | Container / scope |
|---|---|---|
| Rust | `function_item`, `function_signature_item`, `struct_item`, `enum_item`, `union_item`, `trait_item`, `type_item`, `const_item`, `static_item`, `mod_item`, `macro_definition`, `enum_variant`, `field_declaration` | Containers: `impl_item` (the container name is the impl type, with references, generics and path prefixes peeled), trait, mod, struct, enum, union, enum variant. Scope: `block`. |
| TypeScript, TSX, JavaScript | `function_declaration`, `generator_function_declaration`, `function_signature`, `class_declaration`, `abstract_class_declaration`, `interface_declaration`, `type_alias_declaration`, `enum_declaration`, `method_definition`, `variable_declarator` | Containers: class, abstract class, interface, enum. Scope: `statement_block`. |
| Python | `function_definition`, `class_definition` | Container: class. Scope: function. |
| Go | `function_declaration`, `method_declaration`, `type_spec`, `const_spec`, `var_spec` | No container node. A method takes its receiver type as its container. Scope: `block`. |
| Every other tree-sitter language | A node of structural priority 4 or 5 (functions, type definitions) or 2 (containers) that has a `name` field | Containers: priority 2 or 5. Scope: priority 4. |

### The local-variable rule

The scan finds the nearest ancestor that is a container or a scope. A container gives `Placement::Member(name)`. A scope gives `Placement::Nested`. A nested declaration of kind `variable_declarator`, `var_spec` or `const_spec` is a local, so the scan drops it. Other nested declarations stay, with `container: None` (for example a Rust function in a function, a Python function in a function). A destructuring pattern (`const { a, b } = x`) has no single name, so it gives nothing.

Top-level `const f = () => {}` in TypeScript is a declaration. A Go `var` or `const` outside a function body is a declaration.

## `Parser` helpers (`types.rs`)

### `Parser::new_for_extension(language, ext)`

For `Language::TypeScript` and the extension `tsx` (any case) it loads the TSX grammar. Every other pair behaves like `Parser::new`. The plain TypeScript grammar reads JSX as a type assertion and loses every declaration after it. `Language::from_extension` and the transform modes do not change: they keep the TypeScript grammar for `.tsx`. The symbol read passes the file extension of the path, so `--language typescript` on a `.tsx` file also gets TSX.

### `Parser::parse_bounded(source, limit) -> Result<Option<Tree>>`

- The progress callback returns `true` to stop the parse. It compares the time since the call with `limit`.
- A stop gives `Ok(None)`. The caller has no tree and must treat the file as unlabelled.
- A stop calls `reset()` on the parser. Without it, the next parse would resume at the stop point, whatever source it gets. Test `parser_after_a_stopped_parse_gives_the_tree_a_new_parser_gives` pins this.
- If the tree is `None` and the callback did not stop it, the result is `SkimError::ParseError`.
- The callback runs only about once per 100 parse steps. A source short enough to finish before the first call always parses, even with `Duration::ZERO`. A test that wants a stop needs a large source (`many_rust_functions(5000)` in `tests/decl.rs`).
- The symbol read does not call it: `process_symbol_file` uses plain `parse`. Only tests call it today.

## The reader

### Order of checks (`main.rs`, then `process.rs`)

1. clap parse. `mode_given` is true only when `--mode` was typed, because the default mode `structure` would look like a conflict on every symbol read.
2. `validate_args`, then `--clear-cache` returns.
3. No FILE: bail with `FILE argument is required`. This is exit 1 (see Gotchas).
4. `validate_symbol_args` (all `SymbolError::Usage`, exit 2): empty name; not exactly one file, or the file is `-`; `--mode` typed and not `full`; `--tokens` given.
5. One argument goes to `process_single_arg`. `SKIM_PASSTHROUGH=1` is checked first and prints raw bytes, so `--symbol` is ignored.
6. `run_symbol_read`: a directory, or a glob that is not an existing file, is `Usage` (exit 2). An existing file with glob characters in its name (`pages/[id].tsx`) is read as a file.
7. `process_symbol_file`: if the language is known from `--language` or the path, `require_supported` runs before the cache is read, so a refused file never meets a stored entry. Then the cache read. Then `read_and_validate`. Then the language from the shebang; no language gives `Unsupported` (exit 3). Then `Parser::new_for_extension(..).parse`, `declarations`, `select`. An empty selection is `not_found` (exit 1). Then `render`, `bound_and_number`, token counts when `--show-stats`, cache write.
8. `main` prints a `SymbolError` as `skim: <message>`, with no `Error:` prefix, and exits with `exit_code()`. Any other error takes the generic path (`Error: ...`; a `ParseError` gives exit 2, `UnsupportedLanguage` gives exit 3, the rest exit 1).

### `SymbolError` and exit codes

| Variant | Exit | Cause |
|---|---|---|
| `Usage` | 2 | The command line cannot mean a symbol read. |
| `Unsupported` | 3 | Markdown, JSON, YAML, TOML, or no detected language (the message names `--language` as the remedy). |
| `NotFound` | 1 | The file parsed and no declaration has that name. |

`SymbolRead` is not a `ProcessResult`. `ProcessResult` feeds the analytics row, and that row would count the selection as a token saving. `run_symbol_read` records no row.

### Selection and rendering (`symbol.rs`)

- `select`: an exact name match wins first, so a declaration whose whole name holds a dot (`sales.orders`) still matches. If no exact match exists, the query splits at the last `::`, or else the last `.`, into container and name. The match is every declaration with that name and that container. There is one qualifier level only: `a::b::c` matches nothing. Matching is exact and case-sensitive.
- One match prints its lines only. Two or more print one block each, in source order, with a `==> path:A-B name (k of n) <==` header and an empty line between blocks. A source line is printed once: `render` keeps a `next_unseen` cursor (the test `notice_for` uses), and a block prints `lines[first.max(next_unseen)..=last]`. When an earlier block printed some or all of the range, the header ends with `, lines shown above` (all of it) or `, lines A-B shown above` (the head of it). This bounds stdout to the file size plus one header per match, for nested same-name declarations and for a minified one-line file. The `-n` line map holds `0` for a header or separator line. Lines keep their terminators, so CRLF stays CRLF.
- `not_found` lists at most `MAX_NEAREST_NAMES` unique names: names that contain the query (ignoring case) first, then source order, then an ellipsis when cut. Every name is quoted with `{:?}`, because a name comes from the file and may hold control bytes.
- `--max-lines` and `--last-lines` apply to the selected text. An elision marker counts toward the bound: `--max-lines N` means at most N lines in total, marker included, with N=1 as the one carve-out. Header lines count as lines. `-n` numbers come from the source: `compose_line_maps` maps the bounded text back to the selected text, and then to the source. Test `symbol_read_max_lines_bounds_the_slice_with_its_marker_included` pins it.

### Cache identity (`cache.rs`)

`CacheKeyParams` has a `symbol: Option<&str>` field. The same record serves `read_cache`, `write_cache` and `cache_key`, so a component is in the key on both paths or on neither. The hash input is `cache_schema_v{N}|{symbol_part}{canonical path}|{mtime secs}|{mode}|...`. `symbol_part` is empty for a whole-file read.

Why the name goes first and has a length prefix: the name is free text. If it came last, a file whose own name spells the trailing numeric fields could make a whole-file input read exactly like a symbol input, and the two reads would share an entry (silent wrong stdout). With the name first, a whole-file input has an absolute path after the version, and that never begins with `symbol:`. Inside the symbol family the length prefix fixes where the name ends. Tests: `test_cache_key_symbol_cannot_collide_with_a_file_name_that_spells_it`, `test_cache_key_without_symbol_is_unchanged`, `test_cache_key_separates_symbol_reads`.

`CacheEntry.notice: Option<String>` stores the stderr notice. It uses `serde(default, skip_serializing_if)`, so old entries load with `None`. `cached_symbol_read` does not serve a hit without a notice. A symbol read writes its entry with `view_differs: true` and no mode or tier. Only a read that succeeded writes an entry: a failed read writes nothing. A cache write failure is not fatal. When `--show-stats` is on and a hit lacks token counts, the counts are made again from the file.

## Anti-Patterns

### KB-AP-1, a partial scan read as a complete one

`truncated` is set when the walk reaches the node cap or the depth cap. Declarations before the cap stay in `decls`; those after it are absent. The reader uses the flag in two places. A successful read adds "more declarations with this name may exist" to the notice (`notice_for`). A failed read skips the name list and says "syntax-node or depth cap" (`not_found`). `DeclScan` has no field for the kind of cap, so the message names both. Do not add one: PR 3 relies on `truncated` keeping its signature and meaning. Any new consumer (PR 3) must carry the flag the same way and must not print a partial set as if it were the file.

### KB-AP-2, expecting a name from the C family

The generic rule needs `child_by_field_name("name")`. In C and C++ the function name sits under `declarator`. The scan returns nothing for those nodes, and a symbol read exits 1 with "(none)" or only the names it can find. `a_language_whose_declarations_have_no_name_field_gives_an_empty_scan` pins the empty scan. A fix needs a per-language name rule in `decl.rs`. It must also keep `name_bytes` as the range of the name, because PR 3 depends on it.

## Gotchas

- **KB-INV-1**: the key has no language field, so `--language` does not separate entries for one file. The key does include mtime in seconds, the line-number flag and the truncation options. Do not add a component to `CacheKeyParams` without a test in `cli_cache.rs`: a wrong key serves another run's stdout without an error.
- **KB-INV-2**: the notice is loss-bearing, so it is unconditional and is not behind the debug gate. A lossless notice would be debug-gated, but this one never is. Do not move it behind `debug_log!`. Tests drive the real binary through `common::skim_sandboxed`, which gives each child its own parser cache and turns analytics off. A test that passes warm and fails cold is a cache-state problem: isolate the cache.
- **KB-INV-3**: the local rule uses the nearest ancestor, so a variable in a nested block of a function body is a local, and so is a variable in an arrow function body. A `const` at the top level of a module is not.
- Cap stop: `not_found` and the match notice both name the "syntax-node or depth cap", because `DeclScan.truncated` is true for either cap. Test `no_match_in_a_file_deeper_than_the_depth_cap_does_not_blame_only_the_node_cap` pins the depth case.
- `--max-lines` and `--last-lines` notice: the notice is built before the bounds run, so it states the whole selected range (for example "lines 3-8 shown") even when the bound hid some of those lines. The marker in stdout gives the number of cut lines.
- `SKIM_PASSTHROUGH=1` ignores `--symbol` and prints the whole file.
- No-FILE stdin: `skim --symbol NAME < file` has no FILE, so the "FILE argument is required" check fires first and the exit is 1. Only an explicit `-` reaches `validate_symbol_args` and exits 2.
- The printed range is the declaration node only. Attributes, doc comments and decorators above it are not in the range.
- A parse failure on the main path (`Parser::parse`) is a generic `SkimError::ParseError`, not a `SymbolError`: it prints with `Error:` and exits 2.

## Key Files

- `crates/rskim-core/src/decl.rs`: the declaration rule (`declarations`, `enclosing_declaration`, `Declaration`, `DeclScan`).
- `crates/rskim-core/src/types.rs`: `Parser::new_for_extension` and `Parser::parse_bounded`.
- `crates/rskim-core/src/lib.rs`: re-exports `DeclScan`, `Declaration`, `declarations`, `enclosing_declaration`.
- `crates/rskim-core/tests/decl.rs`: per-language kinds, locals, caps, TSX, `parse_bounded`.
- `crates/rskim/src/symbol.rs`: `select`, `render`, `not_found`, `SymbolError`, `SymbolRead`.
- `crates/rskim/src/process.rs`: `process_symbol_file`, `cached_symbol_read`, `bound_and_number`, `write_symbol_result`.
- `crates/rskim/src/main.rs`: the `--symbol` flag, `validate_symbol_args`, `run_symbol_read`, the `SymbolError` exit path.
- `crates/rskim/src/cache.rs`: `CacheKeyParams.symbol`, `cache_key`, `CacheEntry.notice`.
- `crates/rskim/tests/cli_symbol.rs` and `cli_cache.rs`: the case table and the cache identity.
- `docs/usage.md`: the "Symbol Read" section for users.

## Related

- `.devflow/features/file-wrapper-fidelity/KNOWLEDGE.md`: elision markers, the unconditional loss-bearing notice class, byte fidelity.
- `.devflow/features/analytics/KNOWLEDGE.md`: why a symbol read records no row; `SKIM_CACHE_DIR` and test isolation.
- `.devflow/features/cmd-search/KNOWLEDGE.md`: the search layer that uses `enclosing_declaration` and `parse_bounded`.
- `crates/rskim-core/src/ast_walk.rs`: `AstWalkIter` and `AstWalkConfig`, the walk primitive that `declarations` uses, and the cap constants.
