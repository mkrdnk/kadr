# Decision record: authenticated standard-license declarations

## Decision and scope

Prefer actual upstream notice texts: keep local crate notices and exact-commit
workspace supplements verbatim. A published Cargo declaration of a recognized
standard license is also a grant by reference, not evidence that no grant exists
merely because the crate archive omitted a standalone LICENSE file.

The initial collector required a recovered notice file for every third-party
package. That text-only rule unnecessarily blocked five authenticated published
standard declarations. A separate `declared-spdx` provenance type now covers
**only these exact versions and expressions**:

| Package | Authenticated declaration | Canonical terms supplied |
| --- | --- | --- |
| `convert_case 0.4.0` | `MIT` | MIT |
| `hexf-parse 0.2.1` | `CC0-1.0` | CC0-1.0 |
| `simd_helpers 0.1.0` | `MIT` | MIT |
| `seahash 4.1.0` | `MIT` | MIT |
| `pathfinder_geometry 0.5.1` | `MIT/Apache-2.0` | MIT and Apache-2.0, preserving Cargo's legacy alternative-grant declaration |

This is a reviewed allowlist, not an automatic fallback for arbitrary SPDX-like
strings. Unknown/custom expressions, `NOASSERTION`, declaration conflicts,
unexpected versions, missing authenticated archives, and checksum mismatches
still fail collection. Expanding the allowlist requires an explicit review.

## Authentication and preservation

For each allowlisted case the collector:

1. Requires the checked-in crate/version/expression record to match Cargo's
   resolved metadata and the corresponding registry entry/checksum in Cargo.lock.
2. Reads the already-cached `.crate` archive, verifies its complete SHA-256
   against both sources, and inspects it without executing or unpacking code.
3. Checks the published normalized and original Cargo manifests agree on the
   package/version/license, rejects custom license-file ambiguity, and retains
   the actual published metadata, README files, author declarations and paths.
4. Preserves actual legal files and source comment notices with source paths,
   line positions and archive-member/excerpt hashes. Pathfinder's 2019/2020
   copyright and MIT/Apache source headers are copied verbatim. If an archive
   supplies no copyright notice, the report says so; no owner or year is invented.
5. Supplies canonical terms from SPDX license-list-data v3.26.0 at exact commit
   `8a04f0303d95c4f932210b118588c662665bd383`, checking the recorded raw SHA-256.
   Canonical instructional copyright-template lines are omitted from embedded
   terms rather than presented as package ownership claims. This transformation
   and both source/output hashes are recorded; no permission clause is rewritten.

For Pathfinder, the unavailable VCS revision is not an authentication failure:
the published archive checksum authenticates the manifest and actual source
headers directly. It is not represented as a recovered repository LICENSE.

Everything is offline at package time. Missing archive/source cache entries do
not cause network access or a silent relaxation. Repository assets preserve the
complete raw SPDX texts and pinned provenance for reproduction.

## Status and limits

Each affected package is labeled `declared-standard-text`, not “upstream shipped
this LICENSE” or “legal counsel approved.” Strict notice collection accepts the
authenticated standard grant plus canonical terms and all actual found notices.
The overall `texts-collected-not-audit` status remains explicitly **not** a full
linked-code SBOM, compatibility audit, source-offer assessment or production
distribution approval.

Default package/release jobs remain strict; the explicit unsigned local QA mode
is not needed to accept these five standard grants. It still cannot turn a
conflicting or unauthenticated declaration into a recognized grant. Signing,
Apple notarization, and production legal review remain separate external steps.
