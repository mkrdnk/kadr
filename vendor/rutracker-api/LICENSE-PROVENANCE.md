# rutracker-api 0.2.1 license provenance

`LICENSE-MIT` and `LICENSE-APACHE` are canonical standard-license texts
supplied locally for upstream's declared alternative grant. **They are not
upstream license files**, recovered notices, or evidence that upstream shipped
these files. Classification: **declared-standard-text**.

## Authenticated upstream declaration and attribution

- Upstream repository: <https://github.com/johnlepikhin/rutracker-api>.
- Published package: `rutracker-api` version `0.2.1`.
- Published archive:
  <https://static.crates.io/crates/rutracker-api/rutracker-api-0.2.1.crate>.
- Verified archive SHA-256:
  `6e292fc0929b96a65a9796272b01d5eb69c6776a0e3d59697086d3a9738747cf`.
- Published `.cargo_vcs_info.json` identifies commit
  `f1586eb7cb4a895c22903c3ce6972c071cf61324` and an empty `path_in_vcs`
  (repository root).
- Both published `Cargo.toml` and `Cargo.toml.orig` declare exactly
  `license = "MIT OR Apache-2.0"`.
- The actual published author attribution is
  `Evgenii Lepikhin <johnlepikhin@gmail.com>`.
  This is preserved as manifest attribution, not an inferred copyright notice.

The published original manifest and README byte-match the files at the
identified upstream commit:

- [Cargo.toml](https://github.com/johnlepikhin/rutracker-api/blob/f1586eb7cb4a895c22903c3ce6972c071cf61324/Cargo.toml)
- [README.md](https://github.com/johnlepikhin/rutracker-api/blob/f1586eb7cb4a895c22903c3ce6972c071cf61324/README.md)

The README's `License` section states:

```text
Licensed under either of

 * Apache License, Version 2.0
 * MIT license

at your option.
```

The published archive and the upstream tree at that commit contain no actual
standalone license files. Review of all fetched upstream branch/tag history
(nine reachable commits) likewise found no license/notice file paths.
The declaration above is upstream evidence; the standard texts below have a
separate source. No copyright owner or year has been invented.

## Canonical standard texts and exact transformations

Both texts were retrieved over HTTPS from SPDX `license-list-data` v3.26.0,
pinned to commit `8a04f0303d95c4f932210b118588c662665bd383`:

| Local file | Canonical source |
| --- | --- |
| `LICENSE-MIT` | <https://raw.githubusercontent.com/spdx/license-list-data/8a04f0303d95c4f932210b118588c662665bd383/text/MIT.txt> |
| `LICENSE-APACHE` | <https://raw.githubusercontent.com/spdx/license-list-data/8a04f0303d95c4f932210b118588c662665bd383/text/Apache-2.0.txt> |

Following `docs/distribution.md` and `packaging/licenses/POLICY.md`, only
instructional copyright placeholders are omitted. Each transformation removes
the following exact UTF-8 byte sequence once, where `\n` denotes LF:

- MIT: `Copyright (c) <year> <copyright holders>\n\n`
- Apache-2.0 appendix: `Copyright [yyyy] [name of copyright owner]\n\n`

All other bytes are preserved, including MIT's original trailing spaces,
Apache's appendix and the final LF in each file. No permission, condition,
disclaimer or other license clause has been rewritten. The omitted placeholders
are not replaced with the author's name, a year, or a local ownership claim.

| Text | Raw canonical SHA-256 | Supplied file SHA-256 |
| --- | --- | --- |
| MIT | `6d7c9e9d7d12c8d3753bdb24417b84819ba4d6c2f025c37948113d86dc81eef9` | `dbb2557451fa0f8f21e41258b7757b969de2b033b289ab571d36d30421b5385b` |
| Apache-2.0 | `074e6e32c86a4c0ef8b3ed25b721ca23aca83df277cd88106ef7177c354615ff` | `41775bee6af0cf3bee480e3d41d4c75012ef606f1e3a7f177beced662db26b46` |

This local supplement documents the explicitly approved canonical-text treatment
for this vendored version. It does not extend the packaging collector's
declared-SPDX allowlist, change upstream's license declaration, or constitute
a legal audit or approval for public redistribution.
