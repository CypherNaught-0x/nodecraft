# Custom text actions: the expression language

A custom text action (#165) is a portable definition: typed inputs, parameters with defaults,
and one expression per output. `nodecraft_actions::expr` checks and runs it. This page is the
language reference; the reasons behind the design are in
[the decision](decisions/2026-10-18-custom-text-expressions.md).

## Values

| Kind | Literals | Notes |
|---|---|---|
| whole number | `0`, `42`, `-7` | 64-bit; overflow is an error, never a wrap |
| text | `"hi"`, `"a\"b"`, `"\u{1F600}"` | escapes `\" \\ \n \t \r \u{…}` |
| flag | `true`, `false` | |
| list | `[1, "a", [2]]` | items may mix kinds; a trailing comma is allowed |

Text is Unicode: lengths, indices and slices count characters (Unicode scalar values), never
bytes. There are no floating-point numbers, so results never depend on rounding.

## Syntax

```text
let name = expr in expr          # bind a name for the body
if cond then expr else expr      # cond must be a flag
a or b, a and b, not a           # flags only; and/or short-circuit
== !=                            # any two values; different kinds are unequal
< <= > >=                        # two numbers or two texts
+                                # numbers add; texts and lists concatenate
- * / %                          # numbers; / and % truncate toward zero
-x                               # negation
x[i]                             # list item or text character; negative i counts from the end
f(a, b)                          # a built-in function
# comment to the end of the line
```

Names are ASCII identifiers (letters, digits, `_`; at most 64 bytes). Declared inputs,
parameters and outputs are lowercase. An expression may read its action's inputs and
parameters, and names bound with `let`; `it` and `index` exist only inside `map` and `filter`.

## Functions

| Function | Result |
|---|---|
| `len(x)` | characters of a text, items of a list |
| `upper(t)`, `lower(t)` | Unicode case mapping (may change the length) |
| `trim(t)`, `trim_start(t)`, `trim_end(t)` | without surrounding whitespace |
| `chars(t)` | list of one-character texts |
| `contains(x, v)` | text contains text, or list contains value |
| `starts_with(t, s)`, `ends_with(t, s)` | flags |
| `count(x, v)` | non-overlapping occurrences in a text, or equal items in a list |
| `index_of(x, v)` | first character or item index, or `-1` |
| `replace(t, from, to)` | every occurrence; `from` must not be empty |
| `split(t, sep)` | list of texts; `sep` must not be empty |
| `lines(t)` | list of lines (`\n` or `\r\n`) |
| `join(list, sep)` | the list's texts, separated |
| `slice(x, start, end?)` | characters or items `[start, end)`; negative counts from the end; out-of-range bounds clamp |
| `repeat(x, n)` | a text or list `n` times (`n` ≥ 0) |
| `reverse(x)` | characters or items in reverse order |
| `sort(list)`, `unique(list)` | numbers, texts or flags of one kind; `unique` keeps first occurrences |
| `first(x)`, `last(x)` | first or last character or item; an error when empty |
| `text(x)` | a number or flag as text (text stays as it is) |
| `int(x)` | a whole number from text (surrounding whitespace allowed); a number stays as it is |
| `range(a, b)` | numbers `a` up to, not including, `b` |
| `min(a, b)`, `max(a, b)` | two numbers or two texts |
| `pad_start(t, width, c)`, `pad_end(t, width, c)` | padded with the one character `c` to `width` characters |
| `map(list, expr)` | `expr` per item, with `it` and `index` (from 0) in scope |
| `filter(list, cond)` | the items for which `cond` is `true` |

## Budgets

| Budget | Limit |
|---|---|
| expression source | 16 KiB; 64 KiB across a definition |
| tokens and syntax-tree nodes | 8,192 |
| nesting (parentheses, calls, `let`/`if`) | 32 |
| syntax-tree depth (including operator chains like `a + b + c`) | 96 |
| fuel per run | 10,000,000 by default, at most 50,000,000; one budget across all outputs |
| any value produced | 4 MiB (text bytes; 8 per number or flag; lists 16 per item plus their items) |
| list items | 16,384 |
| all inputs together | 4 MiB; input lists nest at most 32 deep |
| values held at once (names in scope, left operands, evaluated arguments, partial lists) | 16 MiB together |

Every evaluation step costs at least one fuel unit; text and list work also costs one unit per
16 bytes touched. Results that would exceed a size budget are refused before they are built
where the size is known in advance (`replace`, `repeat`, `join`, `range`, padding); repeating
something empty is instant. Counts are judged in 64 bits, so native and wasm builds agree.
Cancellation is checked every 4,096 fuel units. The limits keep the deepest allowed expression
within a 1 MiB stack even in debug builds (a test checks this).

Case mapping and whitespace follow the Unicode tables of the Rust release NodeCraft is built
with, so `upper`, `lower` and `trim` can change slightly across NodeCraft versions.

## Errors

An error is a fixed reason plus a byte offset into the expression, and for definitions the
declared input, parameter or output it concerns. Errors never contain input or parameter values,
which may be private prompts.

## Definitions

```json
{
  "schema": 1,
  "id": "user.slugify",
  "version": 2,
  "label": "Slugify titles",
  "inputs": [{"name": "titles", "kind": "text_list"}, {"name": "prefix", "kind": "text"}],
  "params": [{"name": "sep", "kind": "text", "default": "-"}, {"name": "keep", "kind": "int", "default": 12}],
  "outputs": [
    {"name": "slugs", "kind": "text_list", "expr": "map(titles, prefix + slice(join(split(lower(trim(it)), \" \"), sep), 0, keep))"},
    {"name": "summary", "kind": "text", "expr": "text(len(titles)) + \" titles\""}
  ]
}
```

- `schema` is the wire version (1). Unknown fields and newer schemas are refused. Schema 1 is
  provisional: parameters will move to the shared parameter contract (#162) with a schema bump
  and a migration.
- `id` is a stable dotted lowercase id; bump `version` whenever behaviour changes.
- Input kinds: `text`, `text_list`. Parameter kinds: `text`, `int`, `bool`, each with a default
  of that kind. Output kinds: `text`, `text_list`.
- Names are unique, lowercase, and not keywords, built-in functions, `it` or `index`.
- Compiling checks every expression and that it reads only declared inputs and parameters, so
  a run never meets an unknown name.
