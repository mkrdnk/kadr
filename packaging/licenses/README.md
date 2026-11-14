# Pinned upstream notice supplements

Most records are actual public upstream legal texts omitted from published crate
archives, not reconstructed notices. `source.json` binds each such supplement
to the crate name/version/license expression, published `.cargo_vcs_info.json`
commit/path, exact-commit public URL and raw SHA-256. Shared `_texts/` files avoid
duplicating identical notices. `encoding: "json-utf8"` preserves exact notices
without a final newline; hash the decoded UTF-8 bytes, not the JSON container.

`scripts/package-licenses.py` verifies those bindings offline before embedding
the notices and provenance. It never fetches upstream during packaging. The
root package license, vendored GPUI Apache text/modification ADR, and local
cached-crate notices are collected separately.

## Five authenticated standard declarations

The following searches did not establish standalone upstream notice files.
Their records therefore use the explicitly different `declared-spdx` type:
exact versions, expressions and published `.crate` checksums, authenticated
against Cargo.lock, bind recognized standard grants by reference. The collector
retains actual archive manifests/README/authors and all actual source comment
notices, then supplies pinned SPDX canonical terms. Pathfinder's actual
copyright/license source headers are preserved despite its unavailable VCS SHA.
The status is `declared-standard-text`, never “upstream shipped this LICENSE.”
See [POLICY.md](POLICY.md) for the fixed five-case allowlist, authentication,
placeholder omission, rejection rules and limits.

| Published crate | Declared expression | Exact VCS commit | Evidence |
| --- | --- | --- | --- |
| `convert_case 0.4.0` | `MIT` | `300d9eef9e8c970e15f324afbc3df8b6a4ebbf71` | [Complete exact-commit tree](https://api.github.com/repos/rutrum/convert-case/git/trees/300d9eef9e8c970e15f324afbc3df8b6a4ebbf71?recursive=1) has no legal files; pinned README, manifest and library-source legal-section checks supplied no actual text. |
| `hexf-parse 0.2.1` | `CC0-1.0` | `4225763d744183d720f575ae96d04161b4d08ea0` | [Complete exact-commit tree](https://api.github.com/repos/lifthrasiir/hexf/git/trees/4225763d744183d720f575ae96d04161b4d08ea0?recursive=1), README, `parse/Cargo.toml` and library sources contain no actual CC0 text. |
| `simd_helpers 0.1.0` | `MIT` | `ca1a2f84aa386d758e98f8a609d990263932fb85` | [Complete exact-commit tree](https://api.github.com/repos/lu-zero/simd_helpers/git/trees/ca1a2f84aa386d758e98f8a609d990263932fb85?recursive=1), README, manifest and library-source check supplied no actual legal text. |
| `seahash 4.1.0` | `MIT` | `94b632aeac099031c373599313d5b5f0acbbaec0` | [Original GitLab recursive tree](https://gitlab.redox-os.org/api/v4/projects/redox-os%2Fseahash/repository/tree?ref=94b632aeac099031c373599313d5b5f0acbbaec0&recursive=true&per_page=100) and [same-commit GitHub mirror](https://api.github.com/repos/redox-unofficial-mirrors/seahash/git/trees/94b632aeac099031c373599313d5b5f0acbbaec0?recursive=1) contain no legal files; pinned README and library-source checks supplied no actual text. |
| `pathfinder_geometry 0.5.1` | `MIT/Apache-2.0` | `a5e98fac00f433fe2ff4b2135383d82491b8bfc5` | Declared `servo/pathfinder` commit/tree API returns 422; pinned manifest/license raw URLs return 404. Same-commit `pcwalton/pathfinder` raw URLs also return 404. No different revision was substituted. |

The original repository-search evidence above remains for provenance; it is not
used to negate authenticated published standard grants. No owner or year is
invented when an archive provides no copyright notice. Strict collection accepts
these bounded cases and continues to reject missing/conflicting/custom or
unrecognized declarations. No compatibility conclusion, complete SBOM,
corresponding-source offer or human legal approval is asserted.
