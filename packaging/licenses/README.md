# Pinned upstream notice supplements

These are actual public upstream legal texts omitted from published crate
archives, not reconstructed SPDX templates. `source.json` binds each supplement
to the crate name/version/license expression, published `.cargo_vcs_info.json`
commit/path, exact-commit public URL and raw SHA-256. Shared `_texts/` files avoid
duplicating identical notices. `encoding: "json-utf8"` preserves exact notices
without a final newline; hash the decoded UTF-8 bytes, not the JSON container.

`scripts/package-licenses.py` verifies those bindings offline before embedding
the notices and provenance. It never fetches upstream during packaging. The
root package license, vendored GPUI Apache text/modification ADR, and local
cached-crate notices are collected separately.

## Unresolved published-source gaps

Do not fabricate a successful supplement for these packages. At the checked
commits, the following searches did not establish an actual missing notice text.
Public candidate packaging fails closed on them via `--strict`; an audit-only
inventory can still be generated without that flag. Explicit
`KADR_ALLOW_INCOMPLETE_NOTICES=1` permits only unsigned, marked, non-distributable
local QA artifacts; the release workflow forces strict mode and rejects those
statuses before any artifact upload.

| Published crate | Declared expression | Exact VCS commit | Evidence |
| --- | --- | --- | --- |
| `convert_case 0.4.0` | `MIT` | `300d9eef9e8c970e15f324afbc3df8b6a4ebbf71` | [Complete exact-commit tree](https://api.github.com/repos/rutrum/convert-case/git/trees/300d9eef9e8c970e15f324afbc3df8b6a4ebbf71?recursive=1) has no legal files; pinned README, manifest and library-source legal-section checks supplied no actual text. |
| `hexf-parse 0.2.1` | `CC0-1.0` | `4225763d744183d720f575ae96d04161b4d08ea0` | [Complete exact-commit tree](https://api.github.com/repos/lifthrasiir/hexf/git/trees/4225763d744183d720f575ae96d04161b4d08ea0?recursive=1), README, `parse/Cargo.toml` and library sources contain no actual CC0 text. |
| `simd_helpers 0.1.0` | `MIT` | `ca1a2f84aa386d758e98f8a609d990263932fb85` | [Complete exact-commit tree](https://api.github.com/repos/lu-zero/simd_helpers/git/trees/ca1a2f84aa386d758e98f8a609d990263932fb85?recursive=1), README, manifest and library-source check supplied no actual legal text. |
| `seahash 4.1.0` | `MIT` | `94b632aeac099031c373599313d5b5f0acbbaec0` | [Original GitLab recursive tree](https://gitlab.redox-os.org/api/v4/projects/redox-os%2Fseahash/repository/tree?ref=94b632aeac099031c373599313d5b5f0acbbaec0&recursive=true&per_page=100) and [same-commit GitHub mirror](https://api.github.com/repos/redox-unofficial-mirrors/seahash/git/trees/94b632aeac099031c373599313d5b5f0acbbaec0?recursive=1) contain no legal files; pinned README and library-source checks supplied no actual text. |
| `pathfinder_geometry 0.5.1` | `MIT/Apache-2.0` | `a5e98fac00f433fe2ff4b2135383d82491b8bfc5` | Declared `servo/pathfinder` commit/tree API returns 422; pinned manifest/license raw URLs return 404. Same-commit `pcwalton/pathfinder` raw URLs also return 404. No different revision was substituted. |

Declared metadata/authors are retained in the generated inventory, but they are
not presented as a recovered upstream copyright notice. Resolve these cases
through attributable upstream source/licensing evidence or a reviewed dependency
change before generating distributable candidates. No compatibility conclusion,
complete SBOM, corresponding-source offer or full license audit is asserted here.
